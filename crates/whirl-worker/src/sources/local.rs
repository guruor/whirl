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
//! must, which is when the path changes. The inputs it does *not* have are worth
//! naming, because each one is a way to get this wrong: not the run's slot
//! counter (`EnumContext::run`), not the candidate's position in the walk, not
//! the file's size, mtime or bytes, and not the source's `id`, which is already
//! the prefix the pipeline gives the `origin_key` (`format!("{}:{}", source,
//! candidate.id)`, crates/whirl-worker/src/pipeline.rs) and would otherwise
//! appear twice in it. `id = sha256(origin)` rather than two hashes of the same
//! path, because a candidate whose id and origin disagreed would be a cache
//! entry the dedupe window can no longer recognise.
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

    /// 2.2's `include`/`exclude`: "Matched against the path relative to the
    /// source root. Exclude wins over include." An empty `include` admits
    /// nothing, because it is the whitelist and it lists nothing; the default
    /// list is the five extensions 2.2's example carries, so the extension
    /// capability 2.5 gives `local` is answered here and not by the filename's
    /// case (a `.JPG` is a JPEG on every filesystem this build runs on).
    ///
    /// The text a pattern sees carries a leading separator
    /// (`/walls/sub/screenshots/one.png` for a root of `walls`). That is what
    /// makes 2.2's own defaults reach a `.git` or `screenshots` directory that
    /// sits directly under the configured path: `*/.git/*` needs a separator
    /// before `.git` to match against, and `.git/cached.png` has none, so
    /// without the leading separator the default list would exclude only
    /// *nested* ones and an operator's `screenshots/` folder would be rotated
    /// onto the desktop. The path is still the one "relative to the source
    /// root"; the root is simply spelled `/`.
    fn admits(&self, relative: &str) -> bool {
        let text = format!("/{relative}");
        if self.exclude.iter().any(|glob| matches(glob, &text)) {
            return false;
        }
        self.include.iter().any(|glob| matches(glob, &text))
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
        sources_of(&[&dir.join("walls")], extra)
    }

    /// The same table over other paths, in order: 2.2's `paths` is "one or more
    /// directories" and the order is the config's.
    fn sources_of(paths: &[&Path], extra: &str) -> Sources {
        let listed = paths
            .iter()
            .map(|path| quoted(&path.display().to_string()))
            .collect::<Vec<String>>()
            .join(", ");
        parsed(&format!(
            "{{\n  \"config_schema\": 1,\n  \"sources\": [ {{ \"id\": \"pictures\", \"kind\": \
             \"local\", \"weight\": 1, \"paths\": [{listed}]{extra} }} ]\n}}\n"
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

    fn ids(candidates: &[Candidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect()
    }

    /// The id of the candidate at one path, or a panic naming what the
    /// enumeration returned: a rename changes which candidate is which, so a
    /// test that looked a candidate up by position would pass for the wrong
    /// reason.
    fn id_of(candidates: &[Candidate], path: &Path) -> String {
        let origin = path.display().to_string();
        candidates
            .iter()
            .find(|candidate| candidate.origin == origin)
            .unwrap_or_else(|| panic!("no candidate for {origin} in {:?}", ids(candidates)))
            .id
            .clone()
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

    /// 2.5's split, and the line that decides it: "the source says what it can
    /// answer, the pipeline answers the rest", with "Layer 2, the shared
    /// pipeline" opening its numbered list with the resolution floor. A file
    /// below 4.2's 1600x900 is therefore still this source's candidate; the
    /// pipeline's `rejected_resolution` counter is what says why it was dropped,
    /// and filtering it here would make that counter unreachable while hiding
    /// the image from the reason the operator is shown.
    #[test]
    fn a_file_below_the_pipelines_resolution_floor_is_still_a_candidate() {
        let dir = scratch("below-floor");
        let file = dir.join("walls").join("small.png");
        plant(&file, 400, 300);

        let candidates = enumerate(&table(&dir, ""), &[]);
        assert_eq!(
            origins(&candidates),
            vec![file.display().to_string()],
            "the floor is 2.5 Layer 2's, not this source's"
        );
        assert_eq!(candidates[0].width, Some(400), "the header is still read");
        assert_eq!(candidates[0].height, Some(300));
    }

    /// The identity rule frozen against an external tool: these are the digests
    /// `printf %s <path> | shasum -a 256` printed, so the rule under test is
    /// "hex sha256 of the path string" and not "whatever this function computes".
    /// If someone folds the source's `id` in, or hashes the bytes instead, or
    /// switches to another encoding, this fails on a string and not on a machine.
    #[test]
    fn the_identity_rule_is_the_sha256_of_the_path() {
        assert_eq!(
            digest_of("/walls/one.png"),
            "d4a5e565d5b01bad165d63acae1bf0516764ffcd73343f23640c4a923e901f93"
        );
        assert_eq!(
            digest_of("/walls/nested/two.png"),
            "65e718c5c979fc5110f85cb6b0d56fea90a563642c3e466da73639dbc3e88b55"
        );
        assert_eq!(
            digest_of("/Users/a/Pictures/Wallpapers/aurora.jpg"),
            "a078510443d68578500b264842feff18bb0bd73d4bfefb00902cd5d55b33f1a0"
        );
        assert_eq!(digest_of("").len(), 64, "it is always a full hex sha256");
    }

    /// An id is the hash of the path and of nothing else. `Sha256` hashes a
    /// file's *bytes* for the cache and the dedupe, so the failure this guards
    /// is an id that became content-addressed: the same picture renamed would
    /// then be a dedupe miss (2.5's own reason for the rule), and a re-encoded
    /// file would be a second candidate.
    #[test]
    fn the_id_is_not_the_digest_of_the_file() {
        let dir = scratch("not-content");
        let file = dir.join("walls").join("one.png");
        plant(&file, 2560, 1440);
        let mut hasher = Sha256::new();
        hasher.update(&fs::read(&file).expect("the fixture's bytes"));
        let bytes = hasher.hex();

        let candidates = enumerate(&table(&dir, ""), &[]);
        assert_eq!(candidates.len(), 1);
        assert_ne!(
            candidates[0].id, bytes,
            "the content digest is the cache's identity (3.3), not the candidate's"
        );
        assert_eq!(candidates[0].id, digest_of(&candidates[0].origin));
    }

    /// Two enumerations of the same directory yield the same ids, and the run
    /// number does not enter them: `EnumContext::run` is the daemon's own slot
    /// counter (1.6), so a source that folded it in would re-identify every file
    /// of every rotation, and the cache would fill with the same picture.
    #[test]
    fn two_enumerations_yield_the_same_ids_whatever_the_run_number() {
        let dir = scratch("stability");
        let one = dir.join("walls").join("one.png");
        let two = dir.join("walls").join("nested").join("two.png");
        plant(&one, 2560, 1440);
        plant(&two, 2000, 1200);
        let sources = table(&dir, "");

        let first = enumerate(&sources, &[]);
        let second = enumerate(&sources, &[]);
        assert_eq!(first.len(), 2);
        assert_eq!(
            ids(&first),
            ids(&second),
            "a restart is a second enumeration of the same tree"
        );
        let later_run = sources.entries()[0].source.enumerate(&EnumContext {
            run: 4096,
            home: Some(std::env::temp_dir()),
            recent: Vec::new(),
        });
        assert_eq!(
            ids(&first),
            ids(&later_run),
            "the run counter is not identity"
        );
        assert_eq!(id_of(&first, &one), digest_of(&one.display().to_string()));
    }

    /// The enumeration's order is not identity. A file appearing before the
    /// others in the sorted walk moves every position and no id, which is what
    /// makes the cache survive a file being added.
    #[test]
    fn the_id_does_not_depend_on_the_enumeration_order() {
        let dir = scratch("order");
        let walls = dir.join("walls");
        let late = walls.join("zulu.png");
        plant(&late, 2560, 1440);
        plant(&walls.join("mike.png"), 2000, 1200);
        let sources = table(&dir, "");

        let before = enumerate(&sources, &[]);
        assert_eq!(origins(&before), {
            let mut expected = vec![
                walls.join("mike.png").display().to_string(),
                walls.join("zulu.png").display().to_string(),
            ];
            expected.sort();
            expected
        });

        // A file that sorts first, so every position moves by one.
        let early = walls.join("alpha.png");
        plant(&early, 3000, 2000);
        let after = enumerate(&sources, &[]);
        assert_eq!(after.len(), 3);
        assert_eq!(id_of(&after, &late), id_of(&before, &late));
        assert_eq!(
            id_of(&after, &walls.join("mike.png")),
            digest_of(&walls.join("mike.png").display().to_string())
        );
    }

    /// The id changes when the path changes and only then: a rewrite of the
    /// same path is the same candidate (its bytes are the cache's business), and
    /// a rename is a new one, which is 2.5's stated purpose for the rule.
    #[test]
    fn the_id_follows_the_path_and_not_the_bytes() {
        let dir = scratch("path-not-bytes");
        let walls = dir.join("walls");
        let first = walls.join("one.png");
        plant(&first, 2560, 1440);
        let before = enumerate(&table(&dir, ""), &[]);

        // Same path, different bytes and different dimensions.
        plant(&first, 1920, 1080);
        let rewritten = enumerate(&table(&dir, ""), &[]);
        assert_eq!(rewritten.len(), 1);
        assert_eq!(
            id_of(&rewritten, &first),
            id_of(&before, &first),
            "unread or changed content is not a new candidate"
        );
        assert_eq!(
            rewritten[0].width,
            Some(1920),
            "and the candidate carries the new metadata"
        );

        // The same bytes at a new path.
        let renamed = walls.join("two.png");
        fs::rename(&first, &renamed).expect("the rename");
        let after = enumerate(&table(&dir, ""), &[]);
        assert_eq!(after.len(), 1);
        assert_ne!(
            id_of(&after, &renamed),
            id_of(&before, &first),
            "2.5: a renamed file is a new candidate rather than a silent dedupe miss"
        );
        assert_eq!(
            id_of(&after, &renamed),
            digest_of(&renamed.display().to_string())
        );
    }

    // Item 3's edge cases, one test each. features.md 2.2 and
    // architecture.md 4.3 decide them; where the spec is silent the test says
    // what was chosen and why. What a skipped file is *reported* as is asserted
    // where the worker's stderr can be read (crates/whirl-worker/tests/argv.rs);
    // what is asserted here is what the enumeration returned.

    /// The empty directory. `enumerate` yields nothing and `validate` is `Ok`,
    /// which is the distinction 1.4 draws: a source that honestly found nothing
    /// is `no_candidates` at the stage (1.4's "the source answered but the
    /// filter pipeline left nothing"), and a source that cannot be asked is
    /// `enabled=0 reason=<...>` (4.3). Both are pinned at the process level.
    #[test]
    fn an_empty_directory_enumerates_to_nothing_and_is_not_a_failure() {
        let dir = scratch("empty");
        fs::create_dir_all(dir.join("walls")).expect("the source directory");
        let sources = table(&dir, "");
        let entry = &sources.entries()[0];
        assert!(
            entry.source.validate(&entry.config).is_ok(),
            "an empty directory is not an error: 2.2 refuses a path that is missing or \
             unreadable, not one that is merely empty"
        );
        assert!(enumerate(&sources, &[]).is_empty());
    }

    /// 2.2's `paths` rule: "a path that is missing or unreadable is a warning;
    /// it is an error only if every path fails". One missing root leaves the
    /// source enabled and the other root enumerated; both missing is the refusal
    /// 4.3 renders as `enabled=0 reason=<...>`, which is what the daemon's
    /// `config check` shows.
    #[test]
    fn a_missing_path_is_a_failure_only_when_every_path_misses() {
        let dir = scratch("missing");
        let present = dir.join("walls");
        let absent = dir.join("gone");
        plant(&present.join("one.png"), 2560, 1440);

        let half = sources_of(&[&present, &absent], "");
        let entry = &half.entries()[0];
        assert!(
            entry.source.validate(&entry.config).is_ok(),
            "one of two paths is a warning, not a refusal"
        );
        assert_eq!(
            origins(&enumerate(&half, &[])),
            vec![present.join("one.png").display().to_string()],
            "and the rest of the tree is enumerated anyway"
        );

        let both = sources_of(&[&absent, &dir.join("also-gone")], "");
        let entry = &both.entries()[0];
        let reason = entry
            .source
            .validate(&entry.config)
            .expect_err("every path fails")
            .to_string();
        assert!(
            reason.contains("sources[id=pictures].paths"),
            "the reason names the key: {reason}"
        );
        assert!(
            reason.contains(&absent.display().to_string()),
            "and the path that failed: {reason}"
        );
        // The errno text is the platform's, so the test asks the platform for it
        // rather than hardcoding Unix's: `unreadable_root` reports the same
        // `symlink_metadata` failure this line provokes.
        let why = std::fs::symlink_metadata(&absent)
            .expect_err("the path is still missing")
            .to_string();
        assert!(
            reason.contains(&why),
            "and why, in this platform's own words ({why}): {reason}"
        );
    }

    /// Absolute paths only. The parser accepts a relative path (it is a string
    /// to it), so the guard is [`Local::validate`]: it refuses the value and
    /// names the key, which is what 4.3's `enabled=0 reason=<...>` renders. The
    /// reading is 2.5's own rule for `set path`: a relative path is never
    /// resolved against the daemon's working directory, which is `/` under
    /// launchd and therefore not the directory the operator typed in.
    #[test]
    fn a_relative_path_is_refused_and_never_resolved_against_the_working_directory() {
        let relative = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"pictures\", \
                        \"kind\": \"local\", \"weight\": 1, \"paths\": [\"walls\"] } ]\n}\n";
        let config = Config::parse(relative)
            .expect("the parser takes a relative path: it is a string to the parser")
            .config;
        assert_eq!(
            config.sources[0]
                .local
                .as_ref()
                .expect("a local section")
                .paths,
            vec!["walls".to_string()],
            "nothing normalised it away before the source saw it"
        );

        let sources = Sources::from_config(&config);
        let entry = &sources.entries()[0];
        let reason = entry
            .source
            .validate(&entry.config)
            .expect_err("a relative path is refused")
            .to_string();
        assert!(
            reason.contains("sources[id=pictures].paths[0]"),
            "the key is named: {reason}"
        );
        assert!(
            reason.contains("walls is relative"),
            "and so is the value: {reason}"
        );
    }

    /// 2.2: "`~` is expanded". The home it expands against is the one
    /// [`EnumContext`] carries, not this process's own environment, because the
    /// daemon that reads the config is not necessarily the shell that wrote it
    /// (the same reason 2.5 refuses a relative `set path`); the pipeline passes
    /// `paths::home()`.
    #[test]
    fn a_tilde_path_expands_against_the_home_the_context_carries() {
        let dir = scratch("tilde");
        let home = dir.join("home");
        let file = home.join("walls").join("one.png");
        plant(&file, 2560, 1440);

        let sources = sources_of(&[Path::new("~/walls")], "");
        let candidates = sources.entries()[0].source.enumerate(&EnumContext {
            run: 1,
            home: Some(home.clone()),
            recent: Vec::new(),
        });
        assert_eq!(
            origins(&candidates),
            vec![file.display().to_string()],
            "`~` is expanded against `EnumContext::home`"
        );
        assert_eq!(
            id_of(&candidates, &file),
            digest_of(&file.display().to_string()),
            "and the id is the expanded path's, so a config that moves is a cache that \
             moves with it"
        );
    }

    /// 2.2's `include`/`exclude`, which are this source's answer to Layer 1's
    /// `extension` capability ("a local filesystem can answer these with a glob
    /// and a header read", 2.5). The default lists are
    /// `whirl_core::config::LocalSource`'s: five extensions, and `*/.git/*` and
    /// `*/screenshots/*` excluded. The extension, not the bytes, is what decides
    /// here, so `vector.svg` is not a candidate even though its contents are a
    /// PNG, and `UPPER.PNG` is one because a filename's case is not a type.
    #[test]
    fn the_default_include_list_admits_five_extensions_and_names_two_exclusions() {
        let dir = scratch("globs-default");
        let walls = dir.join("walls");
        plant(&walls.join("kept.png"), 2560, 1440);
        plant(&walls.join("also.jpg"), 2000, 1200);
        plant(&walls.join("UPPER.PNG"), 2000, 1200);
        plant(&walls.join("vector.svg"), 2000, 1200);
        plant(&walls.join("notes.txt"), 2000, 1200);
        plant(&walls.join(".git").join("cached.png"), 2000, 1200);
        plant(&walls.join("screenshots").join("wall.png"), 2000, 1200);

        let mut expected = vec![
            walls.join("UPPER.PNG").display().to_string(),
            walls.join("also.jpg").display().to_string(),
            walls.join("kept.png").display().to_string(),
        ];
        expected.sort();
        assert_eq!(origins(&enumerate(&table(&dir, ""), &[])), expected);
    }

    /// "Exclude wins over include" (2.2), with both lists configured: the file
    /// both lists name is not a candidate, and one that only `include` names is.
    #[test]
    fn a_configured_exclude_wins_over_the_include_that_admits_the_same_file() {
        let dir = scratch("globs-config");
        let walls = dir.join("walls");
        plant(&walls.join("kept.png"), 2560, 1440);
        plant(&walls.join("skip-me.png"), 2000, 1200);
        let sources = table(
            &dir,
            ", \"include\": [\"*.png\"], \"exclude\": [\"*skip-*\"]",
        );
        assert_eq!(
            origins(&enumerate(&sources, &[])),
            vec![walls.join("kept.png").display().to_string()]
        );
    }

    /// 2.2: "a file whose header cannot be read is excluded and logged at debug,
    /// because a file we cannot measure is a file we cannot promise will
    /// display". A file the include list names whose bytes are not an image is
    /// the first of the two ways that happens, and it must not take the
    /// enumeration down with it.
    #[test]
    fn a_file_whose_header_cannot_be_read_is_not_a_candidate() {
        let dir = scratch("no-header");
        let walls = dir.join("walls");
        plant(&walls.join("good.png"), 2560, 1440);
        fs::write(walls.join("liar.png"), b"not a PNG, whatever the name says").expect("a file");
        fs::write(walls.join("empty.png"), b"").expect("an empty file");

        assert_eq!(
            origins(&enumerate(&table(&dir, ""), &[])),
            vec![walls.join("good.png").display().to_string()],
            "a zero-byte file and a file of the wrong bytes are both unmeasurable"
        );
    }

    /// The other way a header cannot be read: the file cannot be opened at all.
    /// The mode is the fixture: if this ever runs as a user who can read
    /// anything, the fixture's own assertion fails rather than the test passing
    /// for the wrong reason.
    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_read_is_skipped_and_the_rest_is_enumerated() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("locked");
        let walls = dir.join("walls");
        let good = walls.join("good.png");
        let locked = walls.join("locked.png");
        plant(&good, 2560, 1440);
        plant(&locked, 2000, 1200);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("a mode");
        assert!(
            fs::read(&locked).is_err(),
            "the fixture has to be unreadable for this test to mean anything"
        );

        assert_eq!(
            origins(&enumerate(&table(&dir, ""), &[])),
            vec![good.display().to_string()]
        );
    }

    /// The symlink case. 2.2 is explicit about the *directory* half
    /// ("`follow_symlinks`: off by default to make loops impossible rather than
    /// merely unlikely", and "a symlinked wallpaper folder has to be named as a
    /// real path") and silent about a symlinked *file*. The reading this source
    /// takes, and why: with `follow_symlinks` false the walk follows no link at
    /// all. Following file links while refusing directory ones would let a file
    /// outside the configured tree become a candidate, which is the same escape
    /// the directory rule closes; and the safe reading is also the simpler one to
    /// state. With the flag on, a followed link's candidate carries the path the
    /// config named, which is the link.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_not_followed_until_follow_symlinks_is_on() {
        use std::os::unix::fs::symlink;

        let dir = scratch("symlink-file");
        let real = dir.join("elsewhere").join("real.png");
        plant(&real, 2560, 1440);
        let link = dir.join("walls").join("linked.png");
        fs::create_dir_all(dir.join("walls")).expect("the source directory");
        symlink(&real, &link).expect("the link");

        let refused = enumerate(&table(&dir, ""), &[]);
        assert!(
            refused.is_empty(),
            "follow_symlinks is false (2.2's default): {:?}",
            origins(&refused)
        );

        let followed = enumerate(&table(&dir, ", \"follow_symlinks\": true"), &[]);
        assert_eq!(origins(&followed), vec![link.display().to_string()]);
        assert_eq!(
            followed[0].width,
            Some(2560),
            "the header read follows the link, and the identity stays the path that was named"
        );
    }

    /// The same rule for a symlinked directory: not walked, and with the flag on,
    /// walked under the link's own path.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_not_walked_either() {
        use std::os::unix::fs::symlink;

        let dir = scratch("symlink-dir");
        let walls = dir.join("walls");
        plant(&walls.join("real.png"), 2560, 1440);
        plant(&dir.join("elsewhere").join("linked.png"), 2000, 1200);
        symlink(dir.join("elsewhere"), walls.join("link")).expect("the link");

        assert_eq!(
            origins(&enumerate(&table(&dir, ""), &[])),
            vec![walls.join("real.png").display().to_string()],
            "a symlinked folder has to be named as a real path (2.2)"
        );

        let followed = enumerate(&table(&dir, ", \"follow_symlinks\": true"), &[]);
        let mut expected = vec![
            walls.join("link").join("linked.png").display().to_string(),
            walls.join("real.png").display().to_string(),
        ];
        expected.sort();
        assert_eq!(origins(&followed), expected);
    }

    /// A configured root that is a symlink is the same decision one level up:
    /// with `follow_symlinks` false the source cannot be walked at all, so it is
    /// refused by name and the operator sees `enabled=0 reason=<...>` instead of a
    /// source that silently finds nothing.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_is_refused_and_the_reason_is_the_flag() {
        use std::os::unix::fs::symlink;

        let dir = scratch("symlink-root");
        let real = dir.join("real");
        plant(&real.join("walls").join("one.png"), 2560, 1440);
        let link = dir.join("linked");
        symlink(&real, &link).expect("the link");

        let sources = sources_of(&[&link], "");
        let entry = &sources.entries()[0];
        let reason = entry
            .source
            .validate(&entry.config)
            .expect_err("a symlinked root is refused")
            .to_string();
        assert!(
            reason.contains(&link.display().to_string()) && reason.contains("follow_symlinks"),
            "the reason names the path and the key that would change the answer: {reason}"
        );

        let followed = sources_of(&[&link], ", \"follow_symlinks\": true");
        let entry = &followed.entries()[0];
        assert!(entry.source.validate(&entry.config).is_ok());
        assert_eq!(
            origins(&enumerate(&followed, &[])),
            vec![link.join("walls").join("one.png").display().to_string()]
        );
    }

    /// A loop is impossible at the walk level, not merely by the depth bound: with
    /// `follow_symlinks` on, `max_depth` would make a cycle terminate eventually,
    /// and the set of directories this walk has already entered is what makes it
    /// terminate at the second visit. The bound here is 40 so that a failure of
    /// the visited set shows up as repeated candidates and not as a stack.
    #[cfg(unix)]
    #[test]
    fn a_followed_loop_terminates_at_the_second_visit() {
        use std::os::unix::fs::symlink;

        let dir = scratch("loop");
        let walls = dir.join("walls");
        plant(&walls.join("one.png"), 2560, 1440);
        symlink(&walls, walls.join("again")).expect("the loop");

        let candidates = enumerate(
            &table(&dir, ", \"follow_symlinks\": true, \"max_depth\": 40"),
            &[],
        );
        assert_eq!(
            candidates.len(),
            1,
            "one file, one candidate, however many times the tree can be re-entered: {:?}",
            origins(&candidates)
        );
    }

    /// 2.2's `recursive` and `max_depth` ("the default is true with a bound", in
    /// the schema's own words), so a rotation cannot scan a runaway tree: the
    /// bounds are the spec's and not a policy of this file.
    #[test]
    fn the_walk_honours_recursive_and_the_depth_bound() {
        let dir = scratch("depth");
        let walls = dir.join("walls");
        plant(&walls.join("top.png"), 2560, 1440);
        plant(&walls.join("one").join("mid.png"), 2000, 1200);
        plant(&walls.join("one").join("two").join("deep.png"), 2000, 1200);

        assert_eq!(
            origins(&enumerate(&table(&dir, ", \"recursive\": false"), &[])),
            vec![walls.join("top.png").display().to_string()],
            "`recursive` false is one level"
        );
        assert_eq!(
            origins(&enumerate(&table(&dir, ", \"max_depth\": 1"), &[])),
            vec![walls.join("top.png").display().to_string()],
            "and the bound counts the same way"
        );
        let mut expected = vec![
            walls.join("top.png").display().to_string(),
            walls.join("one").join("mid.png").display().to_string(),
        ];
        expected.sort();
        assert_eq!(
            origins(&enumerate(&table(&dir, ", \"max_depth\": 2"), &[])),
            expected
        );
        assert_eq!(
            enumerate(&table(&dir, ""), &[]).len(),
            3,
            "the default bound (8) reaches all of it"
        );
    }
}
