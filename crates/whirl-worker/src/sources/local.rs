//! The `local` source: docs/spec/features.md 2.2's directory tree, enumerated.
//!
//! This is the `local` arm of the dispatch table, and the whole of what a
//! `local` source is: the configured `paths`, the walk's bounds, the
//! include/exclude globs, and the candidate this build produces for one file.
//! `crates/whirl-core/src/source.rs` holds the trait; features.md 2.6 asks a
//! contributor for one file like this one plus one line in the factory
//! ([`crate::sources::build`]).
//!
//! **What this source does not do is filter past what 2.5 puts in it.** 2.5
//! splits filtering in two: Layer 1 is what a source declares and answers
//! ("`local` declares `resolution` and `extension`. A local filesystem can
//! answer these with a glob and a header read"), and Layer 2 is the shared
//! pipeline, applied to whatever the source returned ("a capability not declared
//! is not attempted at the source; it is applied in Layer 2. That is what keeps
//! 'a source declares its own filtering' honest"). So, here and nowhere else:
//!
//! - the `include`/`exclude` globs, which are 2.2's `extension` half, matched
//!   against the path relative to the source root, exclude winning;
//! - the header read that reports a candidate's `width`/`height`, which is 2.2's
//!   `resolution` half, "applied by reading the image header, never the
//!   filename". A file whose header cannot be read is excluded and reported, per
//!   2.2's own decision line: "a file whose header cannot be read is excluded and
//!   logged at debug, because a file we cannot measure is a file we cannot
//!   promise will display".
//!
//! Nothing else. The aspect ratio (2.5 step 2), the `max_bytes` cap (step 3), the
//! platform's content type (step 4) and both levels of dedupe (steps 5 and 6)
//! are Layer 2's and are applied by the pipeline. That is why a file under the
//! configured floor comes back as a candidate carrying the dimensions that will
//! reject it rather than disappearing here (the pipeline's own counter is what
//! `whirl config check` reports), and why the `recent` window in [`EnumContext`]
//! is not consulted: dedupe is not one of the two capabilities 2.5 gives `local`,
//! and the pipeline applies it either way.
//!
//! [`LocalMode`](whirl_core::config::LocalMode) is not consulted either: 2.2's
//! `mode` says what the worker does with the bytes (`reference` points the
//! desktop at the user's own file, `copy` materialises it in the cache), not
//! what the tree offers, and 2.2's `Candidate.origin` is the absolute path
//! either way.
//!
//! **Identity, which is the part that breaks silently.** A candidate's `id` is
//! the hex sha256 of `origin`, and `origin` is the candidate's normalised
//! absolute path. That is the rule for the `local` half of `origin_key`:
//! "`sha256` of the normalized absolute path for a `local` source, so a renamed
//! file is a new candidate rather than a silent dedupe miss"
//! (docs/architecture.md 2.5). It is a pure function of the path string alone,
//! so it cannot move under a restart, under a re-ordering of the enumeration, or
//! under a change to the file's bytes, and it moves exactly when 2.5 says it
//! must, which is when the path changes. `id = sha256(origin)` rather than two
//! hashes of the same path, because a candidate whose id and origin disagreed
//! would be a cache entry the dedupe window can no longer recognise.
//!
//! Normalisation is lexical: `~` is expanded against `HOME` (2.2: "`~` is
//! expanded"), `.` components and repeated separators are dropped, and `..` is
//! resolved against the components before it. Symlinks are deliberately *not*
//! resolved: `canonicalize` would make the id depend on where a link points, so
//! renaming the target would silently re-identify the file, and it would fail
//! for a path that does not exist yet. A path is absolute, never relative: see
//! [`Local::validate`].

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use whirl_core::config::{ConfigError, SourceConfig, paths};
use whirl_core::protocol::Sha256;
use whirl_core::source::{Candidate, Capability, EnumContext, FilterSet, Source};

use crate::pipeline::sniff;

/// How many of a file's first bytes the header read looks at. The same 1024 the
/// pipeline's own sniff uses: a HEIC `ispe` box can sit behind a `meta` box, so
/// the window is wider than the 8 to 30 bytes the other three formats need.
/// This is not the decode 2.2 refuses to ship: it is the header read 2.2 puts in
/// the source's own contract.
const HEAD_BYTES: usize = 1024;

/// The `local` source of docs/spec/features.md 2.2.
#[derive(Debug, Clone)]
pub struct Local {
    /// The config's `id`, which is the prefix of every `origin_key` this source
    /// produces and the name the warnings carry.
    id: String,
    /// The configured paths, unexpanded: `~` needs `HOME`, which arrives in
    /// [`EnumContext`] and is read in `validate`.
    paths: Vec<String>,
    recursive: bool,
    max_depth: u32,
    follow_symlinks: bool,
    include: Vec<String>,
    exclude: Vec<String>,
}

/// The dispatch arm of features.md 2.6, called from
/// [`crate::sources::build`]: one line per kind, and no logic in the table.
pub fn source(config: &SourceConfig) -> Box<dyn Source> {
    Box::new(Local::of(config))
}

impl Local {
    /// The schema of 2.2, taken from the config this source was built for. A
    /// `Local` kind with no `local` section is a hand-built config that was
    /// never parsed; [`Local::validate`] refuses it, and the defaults here are
    /// 2.2's so that the refusal is the only thing that happens.
    pub fn of(config: &SourceConfig) -> Local {
        let local = config.local.clone().unwrap_or_default();
        Local {
            id: config.id.clone(),
            paths: local.paths,
            recursive: local.recursive,
            max_depth: local.max_depth,
            follow_symlinks: local.follow_symlinks,
            include: local.include,
            exclude: local.exclude,
        }
    }

    /// One configured path, expanded and normalised, or `None` when it is
    /// relative and therefore has no meaning outside the directory the user
    /// typed it in.
    fn root(
        &self,
        configured: &str,
        index: usize,
        home: Option<&Path>,
    ) -> Result<PathBuf, ConfigError> {
        match expand(configured, home) {
            Some(path) => Ok(normalise(&path)),
            None => Err(ConfigError::new(
                self.field(&format!("paths[{index}]")),
                0,
                format!(
                    "{configured} is relative: a source path is an absolute path or starts with \
                     `~`, because the daemon's working directory is not the user's and a relative \
                     path is never resolved against it (docs/architecture.md 2.5)"
                ),
            )),
        }
    }

    /// The offending key, as far as this method can name it. `validate` is handed
    /// one source and not the array it came from (the trait's signature), so the
    /// parser's own field paths (`sources[0][id=pictures].id`,
    /// crates/whirl-core/src/config.rs) are not reproducible here and the source
    /// `id`, which is unique and is what the operator searches for, stands in for
    /// the index.
    fn field(&self, key: &str) -> String {
        format!("sources[id={}].{key}", self.id)
    }

    /// features.md 2.2's `paths` rule: "A path that is missing or unreadable is a
    /// warning; it is an error only if every path fails", and
    /// docs/architecture.md 4.3's rule that such an environmental fact is the
    /// worker's, appearing as `source: <id> ... enabled=0 reason=<...>` in
    /// `status`, `sources` and `config check`.
    ///
    /// This method is the only place a source has to say "I cannot be asked at
    /// all": the trait's `enumerate` returns candidates and not a result, so a
    /// source that cannot work has to be refused here or it is indistinguishable
    /// from a source that honestly found nothing. The cost is one `stat` per
    /// configured path per rotation, which is the cost 2.2's own table charges
    /// the source, and the rule that `validate` does no I/O is read as a rule
    /// about writes and about the config's own syntax: the fact that a folder is
    /// gone is not a fact about the config file, and `whirl config check` is the
    /// tool 1.4 names for exactly this case.
    ///
    /// A relative path is refused here rather than resolved: 2.5's `bad_args`
    /// rule for `set path` is the same rule ("a relative path is `ERR bad_args`
    /// and is never resolved against the daemon's cwd, which is `/` under
    /// launchd"), and a source path is read by that same daemon.
    fn refuse(&self) -> Result<(), ConfigError> {
        if self.paths.is_empty() {
            return Err(ConfigError::new(
                self.field("paths"),
                0,
                "a local source needs at least one directory",
            ));
        }
        let home = paths::home();
        let mut failures: Vec<String> = Vec::new();
        for (index, configured) in self.paths.iter().enumerate() {
            let root = self.root(configured, index, home.as_deref())?;
            if let Some(failure) = unreadable_root(&root, self.follow_symlinks) {
                failures.push(failure);
            }
        }
        if failures.len() == self.paths.len() {
            return Err(ConfigError::new(
                self.field("paths"),
                0,
                format!("no configured path can be read: {}", failures.join("; ")),
            ));
        }
        Ok(())
    }

    /// One candidate, or `None` when the file cannot be named or measured.
    fn candidate(&self, path: &Path, bytes: u64) -> Option<Candidate> {
        let origin = path.to_str()?.to_string();
        let header = header_of(path)?;
        Some(Candidate {
            id: digest_of(&origin),
            origin,
            width: Some(header.width),
            height: Some(header.height),
            bytes: Some(bytes),
        })
    }

    /// 2.2's `include`/`exclude`, matched against the path relative to the
    /// source root, with "exclude wins over include". An empty `include` admits
    /// nothing, because it is the whitelist and it lists nothing; the default
    /// list is the five extensions 2.2's example carries, and the extension
    /// capability 2.5 gives `local` is answered here and not by the filename's
    /// case (a `.JPG` is a JPEG on every filesystem this build runs on).
    fn admits(&self, relative: &str) -> bool {
        if self.exclude.iter().any(|glob| matches(glob, relative)) {
            return false;
        }
        self.include.iter().any(|glob| matches(glob, relative))
    }

    /// One directory's entries, in a fixed order: `read_dir`'s order is the
    /// filesystem's business, and a rotation that walked a tree differently on
    /// two runs would make every comparison between two runs a coin toss. The
    /// order is not identity (the id is the path, not the position), which is
    /// why it is safe to fix it here.
    fn walk(
        &self,
        dir: &Path,
        root: &Path,
        depth: u32,
        visited: &mut HashSet<PathBuf>,
        out: &mut Vec<Candidate>,
        skipped: &mut Skipped,
    ) {
        if depth > self.max_depth {
            return;
        }
        let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .collect(),
            Err(error) => {
                skipped.roots.push(format!("{}: {error}", dir.display()));
                return;
            }
        };
        entries.sort();
        for path in entries {
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(_) => {
                    skipped.unreadable += 1;
                    continue;
                }
            };
            if meta.is_symlink() && !self.follow_symlinks {
                // 2.2: `follow_symlinks` is "off by default to make loops
                // impossible rather than merely unlikely", and a symlinked
                // wallpaper folder "has to be named as a real path". A symlink
                // is not followed, whether it points at a directory or at a file:
                // the spec is explicit about directories and silent about files,
                // and the safe reading of `false` is "the walk never follows a
                // link", which is also the only reading under which the walk can
                // neither escape the configured tree nor loop.
                skipped.links += 1;
                continue;
            }
            let meta = if meta.is_symlink() {
                match fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(_) => {
                        skipped.unreadable += 1;
                        continue;
                    }
                }
            } else {
                meta
            };
            if meta.is_dir() {
                if !self.recursive {
                    continue;
                }
                // A loop is impossible at the walk level, not only by depth:
                // `max_depth` bounds the scan of a runaway tree (2.2), and the
                // set of directories this walk has already entered is what makes
                // a cycle terminate at the second visit instead of at the bound.
                if let Ok(real) = fs::canonicalize(&path) {
                    if !visited.insert(real) {
                        skipped.visited += 1;
                        continue;
                    }
                }
                self.walk(&path, root, depth + 1, visited, out, skipped);
            } else if meta.is_file() {
                let relative = match path.strip_prefix(root).ok().and_then(Path::to_str) {
                    Some(relative) => relative.replace(std::path::MAIN_SEPARATOR, "/"),
                    None => {
                        skipped.unreadable += 1;
                        continue;
                    }
                };
                if !self.admits(&relative) {
                    continue;
                }
                match self.candidate(&path, meta.len()) {
                    Some(candidate) => out.push(candidate),
                    None => skipped.unreadable += 1,
                }
            }
            // Anything else (a socket, a fifo, a device) is not a file 2.2's
            // `include` list can name, and is not counted as a failure: it is
            // not an image and never was.
        }
    }
}

impl Source for Local {
    fn validate(&self, _config: &SourceConfig) -> Result<(), ConfigError> {
        self.refuse()
    }

    /// Every candidate the configured paths hold, at most one per file, in a
    /// fixed order. 2.2's `paths` is "one or more directories", so this is the
    /// union of every path that can be walked; a path that cannot is a warning
    /// (see [`Local::refuse`]) and the rest is enumerated regardless.
    fn enumerate(&self, ctx: &EnumContext) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        let mut skipped = Skipped::default();
        for (index, configured) in self.paths.iter().enumerate() {
            let Ok(root) = self.root(configured, index, ctx.home.as_deref()) else {
                skipped.roots.push(format!("{configured}: a relative path"));
                continue;
            };
            if let Some(failure) = unreadable_root(&root, self.follow_symlinks) {
                skipped.roots.push(failure);
                continue;
            }
            let mut visited: HashSet<PathBuf> = HashSet::new();
            if let Ok(real) = fs::canonicalize(&root) {
                visited.insert(real);
            }
            self.walk(&root, &root, 1, &mut visited, &mut out, &mut skipped);
        }
        skipped.report(&self.id);
        out.sort_by(|left, right| left.origin.cmp(&right.origin));
        out
    }

    /// The two capabilities 2.5 gives `local`, and the same list
    /// `SourceConfig::default_capabilities` assigns the kind: "`local` declares
    /// `resolution` and `extension`. A local filesystem can answer these with a
    /// glob and a header read."
    fn capabilities(&self) -> FilterSet {
        FilterSet::of(&[Capability::Resolution, Capability::Extension])
    }
}

/// What one enumeration set aside, so that a skipped file is reported rather
/// than silently missing (2.2: "excluded and logged at debug") and so that the
/// report cannot grow with the size of the tree: one line per configured path
/// that failed, plus one line for everything inside them.
#[derive(Debug, Default)]
struct Skipped {
    /// One entry per configured path that could not be walked at all.
    roots: Vec<String>,
    /// Files whose bytes, header or name could not be read.
    unreadable: u64,
    /// Entries that are symlinks, with `follow_symlinks` false.
    links: u64,
    /// Directories the walk had already entered, which is where a cycle ends.
    visited: u64,
}

impl Skipped {
    fn report(&self, id: &str) {
        for note in &self.roots {
            eprintln!("warning: source {id}: {note}");
        }
        let total = self.unreadable + self.links + self.visited;
        if total > 0 {
            eprintln!(
                "warning: source {id}: entries skipped: {} unreadable, {} symlink, {} revisited",
                self.unreadable, self.links, self.visited
            );
        }
    }
}

/// Why a configured path cannot be walked, or `None` when it can be. One
/// `stat`, which is 2.2's own cost line for a path.
fn unreadable_root(root: &Path, follow_symlinks: bool) -> Option<String> {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(error) => return Some(format!("{}: {error}", root.display())),
    };
    if meta.is_symlink() {
        if !follow_symlinks {
            return Some(format!(
                "{} is a symlink and follow_symlinks is false; name the real path instead \
                 (features.md 2.2)",
                root.display()
            ));
        }
        let meta = match fs::metadata(root) {
            Ok(meta) => meta,
            Err(error) => return Some(format!("{}: {error}", root.display())),
        };
        if !meta.is_dir() {
            return Some(format!("{} is not a directory", root.display()));
        }
        return None;
    }
    if !meta.is_dir() {
        return Some(format!("{} is not a directory", root.display()));
    }
    None
}

/// A file's first [`HEAD_BYTES`] bytes, sniffed. `None` covers every failure the
/// same way, because 2.2's decision is the same for all of them: a file we
/// cannot measure is a file we cannot promise will display.
fn header_of(path: &Path) -> Option<crate::pipeline::Header> {
    let mut file = File::open(path).ok()?;
    let mut head = vec![0u8; HEAD_BYTES];
    let mut filled = 0;
    while filled < HEAD_BYTES {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return None,
        }
    }
    sniff(&head[..filled])
}

/// `~` and `~/`, expanded against `HOME` (2.2: "`~` is expanded"). `None` for a
/// relative path, which has no meaning to a process whose working directory is
/// not the user's.
fn expand(configured: &str, home: Option<&Path>) -> Option<PathBuf> {
    if configured == "~" {
        return home.map(Path::to_path_buf);
    }
    for prefix in ["~/", "~\\"] {
        if let Some(rest) = configured.strip_prefix(prefix) {
            return home.map(|home| home.join(rest));
        }
    }
    let path = Path::new(configured);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

/// The normalised absolute path of the identity rule: `.` dropped, `..`
/// resolved against the components before it, repeated separators collapsed, and
/// no symlink resolution (a link's target is not the path the config named, and
/// `canonicalize` needs the file to exist).
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// The `local` half of `origin_key`: the hex sha256 of the candidate's
/// normalised absolute path, which is `origin` itself, so an id and an origin
/// that disagreed are not expressible (docs/architecture.md 2.5).
fn digest_of(origin: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(origin.as_bytes());
    hasher.hex()
}

/// 2.2's globs, with the two wildcards its own examples use: `*` for any run of
/// characters, `?` for one. `*` crosses a separator, because the defaults
/// (`*.jpg`, `*/.git/*`) are matched against a path relative to the root and a
/// walk that is `recursive` by default has separators in every match: with `*`
/// stopping at `/`, the default `include` list would match a file in the root
/// and nothing below it. Matching ignores ASCII case, for the reason
/// [`Local::admits`] gives: the extension names a type, and the case of a
/// filename is not part of a type.
fn matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let text: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(star) = star {
            p = star + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::Sources;
    use whirl_core::config::{Config, SourceKind};

    /// A directory of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whirl-local-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    /// A PNG with dimensions: the magic and the IHDR chunk, which is all the
    /// header read looks at. The same fixture
    /// `crates/whirl-worker/src/pipeline.rs` uses, for the same reason: this
    /// build validates a header and never decodes (features.md 1.4).
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes
    }

    /// A file with dimensions, and anything above it.
    fn plant(path: &Path, width: u32, height: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("a directory");
        }
        fs::write(path, png(width, height)).expect("the fixture is written");
    }

    /// A JSON string literal, because a path becomes one in the config body and
    /// a Windows path is full of backslashes.
    fn quoted(value: &str) -> String {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }

    fn parsed(body: &str) -> Sources {
        let config = Config::parse(body)
            .expect("the fixture config parses")
            .config;
        Sources::from_config(&config)
    }

    /// The table the binary itself would build, through
    /// [`Sources::from_config`] and not a hand-made entry, so a test cannot pass
    /// against a source the dispatch table does not ship (features.md 2.6).
    fn table(dir: &Path, extra: &str) -> Sources {
        parsed(&format!(
            "{{\n  \"config_schema\": 1,\n  \"sources\": [ {{ \"id\": \"pictures\", \"kind\": \
             \"local\", \"weight\": 1, \"paths\": [{}]{extra} }} ]\n}}\n",
            quoted(&dir.join("walls").display().to_string())
        ))
    }

    /// One enumeration of the table's first source.
    fn enumerate(sources: &Sources, recent: &[&str]) -> Vec<Candidate> {
        sources.entries()[0].source.enumerate(&EnumContext {
            run: 1,
            home: Some(std::env::temp_dir()),
            recent: recent.iter().map(|key| key.to_string()).collect(),
        })
    }

    fn origins(candidates: &[Candidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|candidate| candidate.origin.clone())
            .collect()
    }

    #[test]
    fn the_local_kind_is_in_the_dispatch_table_and_wallhaven_is_not() {
        let dir = scratch("dispatch");
        plant(&dir.join("walls").join("one.png"), 2000, 1200);

        let local = table(&dir, "");
        assert_eq!(local.entries().len(), 1, "the kind has an implementation");
        assert_eq!(local.entries()[0].config.id, "pictures");
        assert_eq!(local.weights(), vec![1], "and so it can be drawn");

        let wallhaven = parsed(
            "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
             \"wallhaven\", \"weight\": 3, \"query\": \"landscape\" } ]\n}\n",
        );
        assert!(
            wallhaven.entries().is_empty(),
            "a kind with no card yet is not served, and `config check` says so with \
             `missing_reason`"
        );
    }

    #[test]
    fn every_configured_directory_is_enumerated_in_a_fixed_order() {
        let dir = scratch("roots");
        let first = dir.join("walls");
        let second = dir.join("more");
        plant(&first.join("nested").join("a.png"), 2000, 1200);
        plant(&first.join("b.png"), 2000, 1200);
        plant(&second.join("c.png"), 2000, 1200);

        let one_root = enumerate(&table(&dir, ""), &[]);
        let mut expected = vec![
            first.join("b.png").display().to_string(),
            first.join("nested").join("a.png").display().to_string(),
        ];
        expected.sort();
        assert_eq!(origins(&one_root), expected, "one configured directory");

        let sources = parsed(&format!(
            "{{\n  \"config_schema\": 1,\n  \"sources\": [ {{ \"id\": \"pictures\", \"kind\": \
             \"local\", \"weight\": 1, \"paths\": [{}, {}] }} ]\n}}\n",
            quoted(&first.display().to_string()),
            quoted(&second.display().to_string())
        ));
        let mut expected = vec![
            second.join("c.png").display().to_string(),
            first.join("b.png").display().to_string(),
            first.join("nested").join("a.png").display().to_string(),
        ];
        expected.sort();
        assert_eq!(
            origins(&enumerate(&sources, &[])),
            expected,
            "2.2's `paths` is \"one or more directories\": both roots, in the one order a \
             second run would repeat"
        );
    }

    #[test]
    fn the_capabilities_are_the_two_the_spec_gives_the_kind() {
        let dir = scratch("capabilities");
        plant(&dir.join("walls").join("one.png"), 2000, 1200);
        let sources = table(&dir, "");
        assert_eq!(
            sources.entries()[0].source.capabilities(),
            FilterSet::of(&[Capability::Resolution, Capability::Extension]),
            "2.5: \"local declares resolution and extension\""
        );
        assert_eq!(
            sources.entries()[0].source.capabilities(),
            FilterSet::of(&SourceConfig::default_capabilities(SourceKind::Local)),
            "and the kind's own default list, which a config may narrow but never widen"
        );
    }

    #[test]
    fn the_recent_window_is_the_pipelines_business_not_the_sources() {
        let dir = scratch("recent");
        let file = dir.join("walls").join("one.png");
        plant(&file, 2000, 1200);
        let sources = table(&dir, "");
        let key = format!("pictures:{}", digest_of(&file.display().to_string()));
        assert_eq!(
            enumerate(&sources, &[key.as_str()]).len(),
            1,
            "dedupe is 2.5 Layer 2 step 5 and not one of the two capabilities 2.5 gives \
             `local`: the source returns the candidate and the pipeline counts the rejection"
        );
    }

    #[test]
    fn a_candidate_carries_the_path_the_dimensions_and_the_size() {
        let dir = scratch("candidate");
        let file = dir.join("walls").join("one.png");
        plant(&file, 2560, 1440);

        let candidates = enumerate(&table(&dir, ""), &[]);
        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.origin, file.display().to_string());
        assert_eq!(
            candidate.width,
            Some(2560),
            "read from the header, never the name"
        );
        assert_eq!(candidate.height, Some(1440));
        assert_eq!(
            candidate.bytes,
            Some(png(2560, 1440).len() as u64),
            "2.5 step 3's `stat`, which a source can answer without the bytes"
        );
        assert_eq!(
            candidate.id,
            digest_of(&candidate.origin),
            "the id and the origin cannot disagree"
        );
    }
}
