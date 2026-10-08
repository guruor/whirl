//! The stages of a rotation, in the order docs/spec/features.md 2.5 fixes them,
//! from enumeration through to the record the daemon reads.
//!
//! One stage per function, so each is testable without the others:
//! [`enumerate`], [`stage_resolution`], [`stage_ratio`], [`stage_size`],
//! [`stage_type`], [`stage_recent`], [`store`] (the download and atomic swap of
//! docs/spec/state-and-cache.md section 3) and the setter call of
//! docs/architecture.md 1.6.
//!
//! **What is real here and what is not.** The pipeline is real: filters,
//! dedupe at both levels, `filters.max_bytes` enforced mid-stream, the header
//! sniff, the content-addressed write by `rename`, and the setter behind the
//! backend boundary. What no card has written yet is a **source**: `local` and
//! `wallhaven` are separate cards, so [`crate::sources`]' factory table is
//! empty, and a rotation against the shipped configuration fails with
//! `no_candidates` and a message that says why. The tests below supply their own
//! source and their own transport in-process, which is the only way a test-only
//! source can exist: a test-only `kind` compiled into this binary would be a
//! shipped kind.
//!
//! The stdout contract is docs/architecture.md 1.6: at most two lines,
//! `downloaded: <digest> <path>` after the rename, then
//! `set: <digest> <origin_key> <path>` after the setter returned success.

use crate::backend::{self, SetError};
use crate::sources::Sources;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use whirl_core::config::{Backend, Config, LocalMode, SourceConfig, SourceKind, paths};
use whirl_core::protocol::{self, ErrorCode, SourceRecord};
use whirl_core::source::{Candidate, EnumContext};
use whirl_core::state::{self, IndexFile, StateFile};

/// The largest header this build sniffs for the formats whose dimensions are in
/// the first bytes: PNG's `IHDR`, a JPEG's frame header, WebP's `VP8`/`VP8L`/
/// `VP8X` chunk. Section 3 step 5 asks for "the first bytes", and this is how
/// many of them are kept. Two formats are not like these: a HEIC's dimensions
/// are behind `meta`, and a JPEG's frame header can sit behind a multi-kilobyte
/// APP1/Exif segment, so each widens to [`WIDE_WINDOW`] when this window did not
/// measure it.
const HEAD_WINDOW: usize = 1024;

/// The window a stream's or a file's head is widened to when the small window
/// did not measure the bytes and the bytes themselves say they are a format
/// whose dimensions can lie further in.
///
/// Two formats need it, and [`sniffed`]'s own walk is what decides both:
///
/// - **A HEIC.** Its dimensions live in an `ispe` box inside `iprp`/`ipco`,
///   behind the `meta` box's `hdlr`, `dinf`, `pitm`, `iinf` and `iref`, so they
///   are nothing like "the first bytes" - and the first `ispe` in the file is
///   the *thumbnail* item's, which is why the walk below follows `pitm` and
///   `ipma` to the primary item rather than taking the first or the largest box
///   it finds. Measured against the thirteen `.heic` files Apple ships in
///   `/System/Library/Desktop Pictures` on macOS 26.5: the first `ispe` in
///   `ipco`, the thumbnail's, starts at 1245, 1680, 1781, 2686 and 3421, and the
///   primary item's starts 20 bytes later in each file; every `meta` box ends by
///   5220, and one `.heic` that `sips` wrote puts an `ispe` at 1063. All of them
///   are past 1024.
/// - **A JPEG.** Its frame header sits behind whatever APP segments precede it,
///   and an APP1/Exif segment is a photograph's own metadata: on the eight
///   `wallhaven` files that first exposed this, the APP1 segment declares 3.0 KB
///   to 6.0 KB, so the walk has to reach byte 3042 to 5970 while the small
///   window holds 1024. Every JPEG already in a library begins with a small JFIF
///   segment (`0xffe0`, length 16) whose frame header sits at byte 154 to 319,
///   which is why the small window measured those and not these.
///
/// 64 KiB is the worst size measured here with an order of magnitude to spare,
/// and it is the size of the read buffer in [`store`] and in `Run::reference`,
/// so a file that needs the wide window costs one wider first read and no second
/// pass.
///
/// **What has to fit is the whole `meta` box, not the `ispe`.** The walk in
/// [`boxes`] stops at the first box whose *declared* size runs past the read,
/// and [`sniff_heic`] cannot enter `meta` unless that top-level box was pushed,
/// so a `meta` box whose declared size overruns this window leaves its `ispe`
/// unread however shallowly that `ispe` sits. Measured: a file whose `meta`
/// declares 200000 bytes and whose `ispe` box starts at 58 is not measured,
/// although 58 is inside the 1024-byte window above. Every real `meta` box
/// measured here ends by 5220, so the window is an order of magnitude clear of
/// the rule it enforces. A HEIC whose `meta` box does not end inside this window
/// is still not one whirl will promise to display (features.md 2.2), and it is
/// dropped before candidacy rather than counted at a stage, which is why the
/// worker reports it on stderr and this daemon forwards that to its log.
const WIDE_WINDOW: usize = 64 * 1024;

/// A failed stage: what the daemon turns into the `ERR` code of 2.7.
#[derive(Debug, Clone)]
pub struct Failure {
    /// The argv stage name: `config`, `backend`, `source`, `download`, `set`.
    pub stage: &'static str,
    pub code: ErrorCode,
    pub message: String,
}

impl Failure {
    pub fn new(stage: &'static str, code: ErrorCode, message: impl Into<String>) -> Failure {
        Failure {
            stage,
            code,
            message: message.into(),
        }
    }

    /// stderr carries the failing stage (1.6); the daemon derives the code from
    /// it and reports the code on the wire. One line, so an operator running the
    /// worker by hand sees the same three facts the daemon does.
    pub fn report(&self) -> std::process::ExitCode {
        eprintln!(
            "stage={} code={} message={}",
            self.stage, self.code, self.message
        );
        std::process::ExitCode::from(1)
    }
}

/// What a successful rotation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub digest: String,
    pub origin_key: String,
    pub path: String,
}

impl Report {
    /// The `set:` line of docs/architecture.md 1.6, in the one place, and it is
    /// the line the daemon parses with `protocol::parse_worker_set_line`.
    pub fn set_line(&self) -> String {
        format!("set: {} {} {}", self.digest, self.origin_key, self.path)
    }

    /// The `downloaded:` line of 1.6, printed after the rename.
    pub fn downloaded_line(&self) -> String {
        format!("downloaded: {} {}", self.digest, self.path)
    }
}

// ---------------------------------------------------------------------------
// The per-source accounting of 2.6
// ---------------------------------------------------------------------------

/// The bracketed counter group of docs/architecture.md 2.6, with the stage names
/// docs/spec/features.md 2.5 fixes: `resolution`, `ratio`, `size`, `type`,
/// `dedupe`. Every rejection is counted here rather than swallowed; the stages
/// themselves return the count they removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    pub candidates: u64,
    pub admitted: u64,
    pub rejected_resolution: u64,
    pub rejected_ratio: u64,
    pub rejected_size: u64,
    pub rejected_type: u64,
    pub rejected_dedupe: u64,
}

impl Counters {
    /// The `source:` record of 2.6 for one source, bracket group included. It
    /// appears only in a `config check` response, and this is that response's
    /// only builder.
    pub fn record(
        &self,
        source: &SourceConfig,
        enabled: bool,
        reason: Option<String>,
    ) -> SourceRecord {
        SourceRecord {
            id: source.id.clone(),
            kind: source.kind.as_str().to_string(),
            weight: source.weight,
            enabled,
            last: None,
            counters: vec![
                ("candidates".to_string(), self.candidates),
                ("admitted".to_string(), self.admitted),
                ("rejected_resolution".to_string(), self.rejected_resolution),
                ("rejected_ratio".to_string(), self.rejected_ratio),
                ("rejected_size".to_string(), self.rejected_size),
                ("rejected_type".to_string(), self.rejected_type),
                ("rejected_dedupe".to_string(), self.rejected_dedupe),
            ],
            reason,
        }
    }
}

// ---------------------------------------------------------------------------
// The candidates, and the stages
// ---------------------------------------------------------------------------

/// One candidate with the source it came from: what a stage needs and what a
/// [`Candidate`] alone does not carry. The source's own section of the config is
/// found by its `id`, so the `kind` is not carried here.
#[derive(Debug, Clone)]
pub struct Seeking {
    /// The source `id` from the config, which is also the `origin_key` prefix
    /// (2.5).
    pub source: String,
    pub candidate: Candidate,
}

impl Seeking {
    /// `<source id>:<source-scoped id>`, the secondary identity of
    /// docs/spec/state-and-cache.md 4.1.
    pub fn origin_key(&self) -> String {
        format!("{}:{}", self.source, self.candidate.id)
    }
}

/// What one stage did: the survivors and the number it removed.
#[derive(Debug, Default)]
pub struct Stage {
    pub kept: Vec<Seeking>,
    pub rejected: u64,
}

/// A whole run of the pipeline over one source's candidates.
#[derive(Debug, Default)]
pub struct Filtered {
    pub kept: Vec<Seeking>,
    pub counters: Counters,
}

/// The stages of features.md 2.5 Layer 2, in its order, over one source's
/// candidates: resolution, ratio, size, type, then dedupe. Pure: no file, no
/// network, no clock.
pub fn filter_pipeline(
    config: &Config,
    platform: Platform,
    window: &Window,
    candidates: Vec<Seeking>,
) -> Filtered {
    let mut counters = Counters {
        candidates: candidates.len() as u64,
        ..Counters::default()
    };
    let input = candidates;
    let stage = stage_resolution(input, config);
    counters.rejected_resolution = stage.rejected;
    let stage = stage_ratio(stage.kept, config, config.filters.target_ratio);
    counters.rejected_ratio = stage.rejected;
    let stage = stage_size(stage.kept, config);
    counters.rejected_size = stage.rejected;
    let stage = stage_type(stage.kept, platform);
    counters.rejected_type = stage.rejected;
    let stage = stage_recent(stage.kept, window);
    counters.rejected_dedupe = stage.rejected;
    counters.admitted = stage.kept.len() as u64;
    Filtered {
        kept: stage.kept,
        counters,
    }
}

/// 2.5 step 1: `min_width` x `min_height`, global with a per-source override
/// (features.md 2.1).
///
/// A candidate that reports no dimensions passes here and is measured from its
/// header once it is downloaded: the floor is a fact about the bytes, and a
/// source that did not answer is not a source that answered "too small".
pub fn stage_resolution(input: Vec<Seeking>, config: &Config) -> Stage {
    let mut stage = Stage::default();
    for seeking in input {
        let (width, height) = floors(config, &seeking);
        let small = |value: Option<u32>, floor: u32| matches!(value, Some(value) if value < floor);
        if small(seeking.candidate.width, width) || small(seeking.candidate.height, height) {
            stage.rejected += 1;
        } else {
            stage.kept.push(seeking);
        }
    }
    stage
}

/// The floor that applies to one candidate: the source's override or the global
/// value (features.md 2.1).
pub fn floors(config: &Config, seeking: &Seeking) -> (u32, u32) {
    match config.sources.iter().find(|s| s.id == seeking.source) {
        Some(source) => (
            source.min_width.unwrap_or(config.min_width),
            source.min_height.unwrap_or(config.min_height),
        ),
        None => (config.min_width, config.min_height),
    }
}

/// 2.5 step 2: the aspect ratio against `filters.target_ratio`, with
/// `filters.ratio_tolerance` as a fraction of that target.
///
/// `None` means `any`, which is the config default and also what a build with no
/// way to read the primary display's ratio leaves in place: 4.2's `target_ratio`
/// comment says "null means any, or the primary display's ratio where the worker
/// can read it", and this worker cannot (the display is the platform layer's).
pub fn stage_ratio(input: Vec<Seeking>, config: &Config, target: Option<f64>) -> Stage {
    let mut stage = Stage::default();
    let tolerance = config.filters.ratio_tolerance;
    for seeking in input {
        match (target, seeking.candidate.width, seeking.candidate.height) {
            (Some(target), Some(width), Some(height)) if height > 0 && target > 0.0 => {
                let ratio = width as f64 / height as f64;
                if ((ratio - target) / target).abs() > tolerance {
                    stage.rejected += 1;
                } else {
                    stage.kept.push(seeking);
                }
            }
            // `any`, or nothing to compare with yet.
            _ => stage.kept.push(seeking),
        }
    }
    stage
}

/// 2.5 step 3: `filters.max_bytes`, global with a per-source override, checked
/// before the download where the source gave a size.
///
/// A candidate with no size is admitted here and capped mid-stream instead
/// (section 3 step 4): "`Content-Length` is a hint and a lying or absent header
/// must not be able to fill the disk".
pub fn stage_size(input: Vec<Seeking>, config: &Config) -> Stage {
    let mut stage = Stage::default();
    for seeking in input {
        let cap = config
            .sources
            .iter()
            .find(|s| s.id == seeking.source)
            .and_then(|s| s.max_bytes)
            .unwrap_or(config.filters.max_bytes);
        match seeking.candidate.bytes {
            Some(bytes) if bytes > cap => stage.rejected += 1,
            _ => stage.kept.push(seeking),
        }
    }
    stage
}

/// 2.5 step 4: "JPEG and PNG everywhere; WebP and HEIC per the platform note in
/// 2.2", which is that HEIC is settable on macOS only.
///
/// The extension comes from the origin's own name, and only a *known* extension
/// can reject a candidate: a URL with a query, or none at all, is settled by the
/// header sniff of section 3 step 5, which is the authority. The extension used
/// for the cache file is always the sniffed one, "so a URL ending in `.php`
/// cannot choose it" (section 3 step 5).
pub fn stage_type(input: Vec<Seeking>, platform: Platform) -> Stage {
    let mut stage = Stage::default();
    for seeking in input {
        let named = extension_of(&seeking.candidate.origin);
        match named {
            Some(ext) if !settable(platform, &ext) => stage.rejected += 1,
            _ => stage.kept.push(seeking),
        }
    }
    stage
}

/// 2.5 step 5: a candidate whose `origin_key` (or whose id) is in the recent
/// window of 4.1, which is the history ring bounded by `dedupe.recent_entries`.
///
/// The cache index is not part of this set. It is the service map
/// ([`Window::cached`]): a candidate the cache already holds is set from the
/// cache rather than dropped, so the rule this stage enforces is "not the image
/// whirl just set", and "not bytes whirl already has" costs no request and no
/// download. What the index does not hold is a miss at both halves, and is
/// fetched.
///
/// A window that holds every candidate therefore empties this stage, and that is
/// the answer 4.1 asks for rather than a case to work around: the rotation ends
/// as `no_candidates` with nothing set and the slot spent, which is
/// docs/architecture.md 1711's row 3 ("everything filtered out") and 655's
/// definition of the code. The alternative, relaxing the window until something
/// matches, would set the image the rule exists to keep off the screen, and 9's
/// table prices the larger window as "fewer repeats of an image the user liked,
/// which is a preference, not a correctness property" - a preference, not a
/// licence to break the rule.
pub fn stage_recent(input: Vec<Seeking>, window: &Window) -> Stage {
    let mut stage = Stage::default();
    for seeking in input {
        if window.contains(&seeking.origin_key()) || window.contains(&seeking.candidate.id) {
            stage.rejected += 1;
        } else {
            stage.kept.push(seeking);
        }
    }
    stage
}

/// The lower-case extension the origin names, ignoring a query or a fragment. A
/// URL's *path* is what is read: `https://host/a.jpg?cb=1` names `jpg`.
fn extension_of(origin: &str) -> Option<String> {
    let without_query = origin
        .split(['?', '#'])
        .next()
        .unwrap_or(origin)
        .trim_end_matches('/');
    let leaf = without_query.rsplit('/').next().unwrap_or(without_query);
    let (_, ext) = leaf.rsplit_once('.')?;
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// The formats the platform can actually display (features.md 2.2). An
/// implementation is passed in rather than read from `cfg!` so the Windows and
/// Linux answers are testable on a macOS machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Windows,
    Linux,
    Other,
}

pub const fn host_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        Platform::Other
    }
}

/// Whether a format is settable on this platform (features.md 2.2). `heic` and
/// `heif` are macOS-only: "on Windows and Linux the worker skips an HEIC file
/// with a log line, and v0.1 does not convert it".
pub fn settable(platform: Platform, ext: &str) -> bool {
    match ext {
        "jpg" | "jpeg" | "png" | "webp" => true,
        "heic" | "heif" => platform == Platform::Macos,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// The recent window (docs/spec/state-and-cache.md 4.1)
// ---------------------------------------------------------------------------

/// The recent window: the origin keys and digests a candidate is compared
/// against before it costs a request, and the cache index, which is what a
/// candidate the cache already holds is served from.
///
/// Both files are the daemon's (7.2). This process only reads them, and it
/// tolerates anything it finds: 7.1's rule is "No reader takes a lock, and every
/// reader must tolerate any file being replaced under it", and a window that
/// cannot be read is a missed preference, never a failed rotation. Nothing here
/// writes either file.
///
/// The two are not the same half of the dedupe, and the difference is the point
/// of the whole struct. The history ring is the **rejection** set: a candidate
/// in it is dropped, which is the rule that stops a `random` query setting the
/// same wallpaper twice in a week. The index is the **service** map: a candidate
/// whose `origin_key` the index names, under a file that is still there, is
/// taken from the cache instead of fetched, so bytes whirl already holds are
/// never downloaded a second time. The index is deliberately *not* part of the
/// rejection set: an image the cache already holds is not a candidate to drop,
/// it is one to set for free (4.1). An entry the sweep removed, or one that is
/// listed but whose file is gone, is served by neither half and is fetched again
/// like any other miss.
#[derive(Debug, Clone, Default)]
pub struct Window {
    keys: HashSet<String>,
    held: HashMap<String, Held>,
}

/// One index entry, reduced to the two fields the cache path is built from: the
/// digest, which is the file's own name, and the extension the name carries
/// (docs/spec/state-and-cache.md 2.1 and section 2).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    digest: String,
    ext: String,
}

impl Window {
    pub fn empty() -> Window {
        Window {
            keys: HashSet::new(),
            held: HashMap::new(),
        }
    }

    /// A window from the two files: the history ring bounded by
    /// `dedupe.recent_entries` (4.1), and every index entry that names an
    /// `origin_key`. The state directory is where the ring lives; the index lives
    /// in the cache root. Either file may be absent, unreadable, of a newer
    /// schema or malformed, and each case leaves that half empty rather than
    /// failing: the rotation falls back to fetching.
    pub fn load(state_dir: &Path, index_path: &Path, bound: usize) -> Window {
        let mut window = Window::empty();
        if let Ok(Some(text)) = read_file(&state_dir.join(state::HISTORY_FILE)) {
            match whirl_core::state::HistoryFile::parse(&text) {
                Ok(StateFile::Read(history)) => {
                    for entry in history.entries.iter().take(bound) {
                        window.add(&entry.origin_key);
                        if let Some(digest) = &entry.digest {
                            window.add(digest);
                        }
                    }
                }
                Ok(StateFile::SchemaNewer { found }) => {
                    eprintln!("warning: history.json is schema {found}, newer than this build");
                }
                Err(error) => eprintln!("warning: history.json: {error}"),
            }
        }
        if let Ok(Some(text)) = read_file(index_path) {
            match IndexFile::parse(&text) {
                Ok(StateFile::Read(index)) => {
                    for (digest, entry) in &index.entries {
                        // An entry with no `origin_key` names no candidate, so it
                        // has nothing to serve; the digest alone is not a key the
                        // selection of 4.1 can look up.
                        //
                        // One `origin_key` can name two digests: a source that
                        // re-encoded the same id has two entries for it. The map
                        // keeps one of them, chosen by the index's own digest
                        // order, and that is the one a rotation is served - so a
                        // re-encode that happens after the window has moved past
                        // the id is served the older bytes instead of being
                        // fetched. That is the price of keying the service on the
                        // id the source gave, which is the mapping the index is
                        // for and the mapping no other file holds.
                        if !entry.origin_key.is_empty() {
                            window.held.insert(
                                entry.origin_key.clone(),
                                Held {
                                    digest: digest.clone(),
                                    ext: entry.ext.clone(),
                                },
                            );
                        }
                    }
                }
                Ok(StateFile::SchemaNewer { found }) => {
                    eprintln!("warning: index.json is schema {found}, newer than this build");
                }
                Err(error) => eprintln!("warning: index.json: {error}"),
            }
        }
        window
    }

    /// A window built in memory, for a caller that already has the two sets. Used
    /// by tests only: production builds it with [`Window::load`], because the
    /// history ring and the index belong to the daemon.
    #[cfg(test)]
    pub fn of(keys: impl IntoIterator<Item = String>) -> Window {
        let mut window = Window::empty();
        for key in keys {
            window.add(&key);
        }
        window
    }

    pub fn add(&mut self, key: &str) {
        if !key.is_empty() {
            self.keys.insert(key.to_string());
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// The cache file a candidate's `origin_key` is already held under, when the
    /// index names one and the file it names really holds the bytes that name
    /// claims. This is the whole of the consultation: a `Some` is a candidate
    /// served from the cache with no request to the source (4.1).
    ///
    /// The check is the file's **bytes**, not its existence. A content-addressed
    /// name is the digest of the bytes it was written from, so section 2's third
    /// reason for the scheme - "a file whose bytes do not hash to its name was
    /// truncated or damaged by something else, and that is detectable without a
    /// second index" - is the condition of the hit rather than a report after it.
    /// A file that is gone, unreadable, or whose bytes have moved off its name is
    /// a miss, and the candidate falls through to the fetch like any other; the
    /// index entry beside it is what the sweep will reclaim (5.5).
    ///
    /// The decision here is the rotation's and not the sweep's, which is what
    /// makes an entry the sweep has taken, or one that is listed but whose file
    /// is gone, a fetch again rather than a silent nothing.
    pub fn cached(&self, cache: &Cache, origin_key: &str) -> Option<(String, PathBuf)> {
        let held = self.held.get(origin_key)?;
        let path = state::content_path(cache.root(), &held.digest, &held.ext);
        if hashes_to(&path, &held.digest) {
            return Some((held.digest.clone(), path));
        }
        None
    }

    /// The same set as a list, which is what a source is handed so it can
    /// exclude the window in its own request where it can (`EnumContext.recent`).
    /// It is the rejection set and only that: the cache index is consulted
    /// through [`Window::cached`], not handed to a source to exclude.
    pub fn keys(&self) -> Vec<String> {
        self.keys.iter().cloned().collect()
    }
}

/// Whether the file at `path` really carries the bytes its name claims: a whole
/// pass of SHA-256 that spells `digest`, the 64 lower-case hex characters the
/// cache path is built from.
///
/// This is section 2's third reason for content-addressed names, used as a
/// condition rather than as a report: "a file whose bytes do not hash to its
/// name was truncated or damaged by something else, and that is detectable
/// without a second index". A file that is gone, unreadable, or whose bytes have
/// moved off its name answers `false`, and both callers read that as a miss: the
/// hit is not taken, the candidate is fetched, and the bytes that arrive replace
/// whatever the name was holding.
fn hashes_to(path: &Path, digest: &str) -> bool {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = protocol::Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(_) => return false,
        }
    }
    hasher.hex() == digest
}

fn read_file(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The state directory of docs/architecture.md 4.3's precedence
/// (`WHIRL_STATE_DIR`, then the platform default): where the history ring the
/// recent window is built from lives (4.1). The daemon owns every file in it;
/// this process only reads.
pub fn state_directory() -> Option<PathBuf> {
    state_directory_with(&|name| std::env::var_os(name))
}

/// The same resolution with its environment supplied, so the precedence is a
/// function of three arms rather than of this process's environment, which a
/// test cannot change without racing every other test in the binary.
fn state_directory_with(lookup: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    env_path_with(lookup, "WHIRL_STATE_DIR").or_else(paths::state_dir)
}

/// An environment variable read as a path, **verbatim**: the rule the daemon
/// follows for the same names (`env_path` in crates/whirld/src/plan.rs). A
/// relative value is used against the working directory, which the worker
/// inherits from the daemon rather than replacing.
///
/// It is not filtered further on purpose. 1.2's "a relative value is treated as
/// unset" is about the four XDG variables that pick the platform defaults; 4.3
/// gives `WHIRL_STATE_DIR` and `WHIRL_CACHE_DIR` no such rule, and a process
/// that dropped a value the daemon accepted would be resolving one knob two
/// ways: the daemon reporting the directory it honoured while this one wrote
/// into the default.
fn env_path_with(lookup: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<PathBuf> {
    lookup(name).map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// The download stage: the transport, and the atomic swap of section 3
// ---------------------------------------------------------------------------

/// Where a candidate's bytes come from. The one thing behind this boundary, so
/// the whole download stage is testable without a network or a directory: a
/// candidate's `origin` is a URL for `wallhaven` and an absolute path for
/// `local`, and the transport is what turns either into a byte stream.
///
/// The stream carries no declared size on purpose. Section 3 step 4 refuses to
/// trust `Content-Length` ("a lying or absent header must not be able to fill
/// the disk"), so there is nothing here for the pipeline to be tempted by: the
/// candidate's own `bytes` is the hint the size filter uses, and the cap is
/// enforced against the bytes that actually arrive.
///
/// This is **not** the `local` source. Enumerating a directory (glob, recursion,
/// include and exclude, the header read of features.md 2.2) is that source's job
/// and a later card's; reading the bytes of a path somebody else produced is the
/// download stage, which is this card's.
pub trait Transport {
    fn open(&self, origin: &str) -> Result<Box<dyn Read>, String>;
}

/// The transport of a candidate whose origin is a path.
pub struct Paths;

impl Transport for Paths {
    fn open(&self, origin: &str) -> Result<Box<dyn Read>, String> {
        let file = fs::File::open(origin).map_err(|error| format!("{origin}: {error}"))?;
        Ok(Box::new(file))
    }
}

/// The production transport: the candidate's **origin** decides which way its
/// bytes come.
///
/// features.md 2.2 fixes the two shapes an origin has. A `local` candidate's is a
/// path on the user's disk. A `wallhaven` candidate's is the `https://` URL the
/// API's own `path` field named (crates/whirl-worker/src/sources/wallhaven.rs),
/// because that is what the API hands out and there is no local file behind it.
/// The pipeline is handed an origin and nothing else, so the choice belongs to
/// the origin's scheme and not to the source's kind, which is also what makes it
/// testable: a test drives it with a [`crate::http::Bytes`] of its own and no
/// socket, or with a path in its own scratch directory and no URL at all.
///
/// This is the whole of the defect `999ea4f5` was written for: before it, the
/// only transport in the binary was [`Paths`], and a wallhaven rotation handed
/// `fs::File::open` an `https://` URL and reported `offline` with the errno of a
/// file that never existed.
pub struct Origins<'a> {
    client: &'a dyn crate::http::Bytes,
}

impl<'a> Origins<'a> {
    /// The real transport, handed the one HTTP client this build has.
    pub fn new(client: &'a dyn crate::http::Bytes) -> Origins<'a> {
        Origins { client }
    }
}

impl Transport for Origins<'_> {
    fn open(&self, origin: &str) -> Result<Box<dyn Read>, String> {
        if !is_url(origin) {
            return Paths.open(origin);
        }
        // No key, deliberately. features.md 2.4's key authenticates a *listing*
        // (`purity`, the favourites endpoints), and the API publishes the image
        // URL itself for anyone: the proof this card came from fetched exactly
        // such a URL with the client's user agent and no key, and got 2,689,221
        // bytes back. A key sent here would be a key on a request with no
        // business carrying one.
        self.client.bytes(&crate::http::Request {
            url: origin,
            key: None,
        })
    }
}

/// Whether an origin names bytes to fetch rather than a file to open.
///
/// Two schemes and no parsing: `http` and `https` are the two the `wallhaven`
/// source can produce, and anything else is a path. A scheme is case-insensitive
/// (RFC 3986 section 3.1), so `HTTPS://` is a URL like any other.
fn is_url(origin: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        origin
            .get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    })
}

/// The platform setter, behind a trait for the one reason [`Transport`] is: the
/// call that leaves the process cannot be made in a test, and features.md 1.4's
/// behaviour on a setter that refuses ("the candidate is marked bad and one more
/// candidate is tried") has to be testable without touching a desktop.
pub trait Setter {
    fn set(&self, path: &str) -> Result<(), SetError>;
}

/// The production setter: whatever [`backend::set`] resolves for the backend the
/// daemon named.
pub struct PlatformSet(pub Backend);

impl Setter for PlatformSet {
    fn set(&self, path: &str) -> Result<(), SetError> {
        backend::set(self.0, path)
    }
}

/// The header of a downloaded file: what section 3 step 5 reads before anything
/// is published. The extension comes from here and never from the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
}

/// What a downloaded file is, or `None` when nothing in the first bytes
/// identifies one of the four formats of features.md 2.2.
pub fn sniff(bytes: &[u8]) -> Option<Header> {
    match sniffed(bytes) {
        Sniffed::Measured(header) => Some(header),
        Sniffed::BeyondWindow | Sniffed::NotAnImage => None,
    }
}

/// What the first bytes say about the four formats of features.md 2.2, with the
/// third answer a single [`sniff`] cannot carry.
///
/// A head is a window, and "the bytes did not measure" is two different facts.
/// A JPEG's frame header can sit behind a multi-kilobyte APP1/Exif segment, and
/// its segment walk then runs out of bytes while `ffd8` still says these are a
/// picture: [`Sniffed::BeyondWindow`] is that case, and a caller must not report
/// it as `not_an_image`, because the bound is the window and not the content. A
/// HEIC answers the same question through the brand test [`is_heic`] rather than
/// through this arm, because its walk cannot tell "no `meta` box" (not a picture
/// this build promises, features.md 2.2) from "a `meta` box past the head".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sniffed {
    /// The dimensions were read.
    Measured(Header),
    /// The bytes begin one of the four formats, and the walk that reads their
    /// dimensions ran past the end of what was kept.
    BeyondWindow,
    /// The bytes are not one of the four formats of features.md 2.2.
    NotAnImage,
}

fn sniffed(bytes: &[u8]) -> Sniffed {
    if let Some(header) = sniff_png(bytes) {
        return Sniffed::Measured(header);
    }
    match sniff_jpeg(bytes) {
        Sniffed::Measured(header) => return Sniffed::Measured(header),
        // A file whose first two bytes are `ffd8` is a JPEG and nothing else
        // here, so the other two walks are not asked when the JPEG's own ran
        // out of bytes.
        Sniffed::BeyondWindow => return Sniffed::BeyondWindow,
        Sniffed::NotAnImage => {}
    }
    if let Some(header) = sniff_webp(bytes) {
        return Sniffed::Measured(header);
    }
    if let Some(header) = sniff_heic(bytes) {
        return Sniffed::Measured(header);
    }
    Sniffed::NotAnImage
}

/// How many bytes the head of a file or a stream is worth keeping, given what
/// has arrived so far: [`HEAD_WINDOW`], or [`WIDE_WINDOW`] once the small window
/// is full and the bytes say they are a format whose dimensions the small window
/// did not measure.
///
/// The questions are asked in that order because each one is cheaper to answer
/// than the read it would cause. A file shorter than the small window is not
/// widened: there is nothing more to read. A PNG or a WebP answers inside the
/// small window, and so does a JPEG whose frame header is not behind a large
/// segment and a HEIC whose `ispe` happens to sit early. What is left is the two
/// cases this exists for: a HEIC whose dimensions sit behind `meta` - which is
/// every HEIC measured on this machine, at 1063 to 3421 bytes in - and a JPEG
/// whose frame header sits behind a multi-kilobyte APP1/Exif segment, at 3042 to
/// 5970 bytes in on the files that exposed this. Both callers of [`sniff`] ask
/// this and not a constant of their own, so a file the local source admits is a
/// file the pipeline can measure, and a file the local source drops is a file
/// the pipeline would drop.
pub fn head_window(head: &[u8]) -> usize {
    if head.len() < HEAD_WINDOW {
        return HEAD_WINDOW;
    }
    match sniffed(head) {
        Sniffed::Measured(_) => HEAD_WINDOW,
        // A JPEG the small window could not walk: a wider head may reach its
        // frame header, and one that outruns even the wide window is reported as
        // bounded by it rather than as bad content (`store`).
        Sniffed::BeyondWindow => WIDE_WINDOW,
        // A HEIC the small window could not measure: its brand is what says the
        // dimensions may be further in, because `sniffed` cannot walk into a
        // `meta` box the head did not hold.
        Sniffed::NotAnImage if is_heic(head) => WIDE_WINDOW,
        Sniffed::NotAnImage => HEAD_WINDOW,
    }
}

fn sniff_png(bytes: &[u8]) -> Option<Header> {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != MAGIC {
        return None;
    }
    // The IHDR chunk is the first one: length, type, then width and height.
    if &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some(Header {
        ext: "png",
        width: be32(&bytes[16..20])?,
        height: be32(&bytes[20..24])?,
    })
}

fn sniff_jpeg(bytes: &[u8]) -> Sniffed {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return Sniffed::NotAnImage;
    }
    let mut at = 2;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xff {
            return Sniffed::NotAnImage;
        }
        let marker = bytes[at + 1];
        // Fill bytes and standalone markers carry no length.
        if marker == 0xff {
            at += 1;
            continue;
        }
        if marker == 0x01 || (0xd0..=0xd9).contains(&marker) {
            at += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if length < 2 {
            return Sniffed::NotAnImage;
        }
        // SOF0..SOF15 except DHT (c4), JPG (c8) and DAC (cc) carry the frame
        // header, and it is width and height in that order at a fixed offset.
        let frame = (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
        if frame && at + 9 < bytes.len() {
            return Sniffed::Measured(Header {
                ext: "jpg",
                height: u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32,
                width: u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32,
            });
        }
        at += 2 + length;
    }
    // The walk reached the end of the bytes given before a frame header. For a
    // whole file that is a truncated picture; for a stream it is a head window
    // the photograph's own APP1/Exif segment runs past. Either way the bytes
    // said `ffd8`, so this is not a claim that they are not a JPEG.
    Sniffed::BeyondWindow
}

fn sniff_webp(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < 30 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    match &bytes[12..16] {
        b"VP8 " => {
            // The keyframe start code, then 14-bit dimensions.
            if bytes[23..26] != [0x9d, 0x01, 0x2a] {
                return None;
            }
            Some(Header {
                ext: "webp",
                width: u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3fff,
                height: u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3fff,
            })
        }
        b"VP8L" => {
            if bytes[20] != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            Some(Header {
                ext: "webp",
                width: (bits & 0x3fff) + 1,
                height: ((bits >> 14) & 0x3fff) + 1,
            })
        }
        b"VP8X" => {
            let width = 1 + (bytes[24] as u32 | (bytes[25] as u32) << 8 | (bytes[26] as u32) << 16);
            let height =
                1 + (bytes[27] as u32 | (bytes[28] as u32) << 8 | (bytes[29] as u32) << 16);
            Some(Header {
                ext: "webp",
                width,
                height,
            })
        }
        _ => None,
    }
}

/// Whether these bytes are a HEIC/HEIF file at all: the `ftyp` box of
/// ISO/IEC 14496-12 4.3 with one of the brands the HEIF family uses
/// (ISO/IEC 23008-12 Annex B).
///
/// It is the brand test alone, and it is a function of its own because
/// [`head_window`] has to ask it *before* the dimensions are found: a stream
/// widens its window on the strength of the brand, not on an answer.
pub fn is_heic(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let brand: [u8; 4] = [bytes[8], bytes[9], bytes[10], bytes[11]];
    [*b"heic", *b"heix", *b"hevc", *b"hevx", *b"mif1", *b"msf1"].contains(&brand)
}

/// The dimensions of a HEIC, from the `ispe` property of its *primary* image
/// item.
///
/// The boxes read here, all of them headers only, so no byte of image data is
/// ever read as a box:
///
/// ```text
/// ftyp
/// meta                the item tables and the properties
///   pitm              the id of the primary item
///   iprp
///     ipco            the properties, in order, 1-indexed
///       ... ispe ...  a size: width and height
///     ipma            which item carries which property
/// mdat                the coded image, never entered
/// ```
///
/// **The first `ispe` in the file is the thumbnail's, not the image's.** In all
/// thirteen of Apple's own `.heic` desktop pictures the first `ispe` in `ipco`
/// is a 1024x1024 thumbnail item and a later one is the 6016x6016 primary item,
/// and `pitm` names the primary item while `ipma` points it at a property. This
/// follows that pair. Answering from the first `ispe` byte-for-byte would be
/// worse than not answering: 1024x1024 is under the 1600x900 floor of 2.5 step
/// 1, so every one of those files would be dropped by the *filter* instead, as
/// `rejected_resolution=1` - a different bug with the same symptom, and one that
/// reads as an honest measurement.
///
/// A file whose `pitm` or `ipma` cannot be read falls back to the largest
/// `ispe`, which agrees with the primary item on every file measured here and is
/// the only defensible reading of a file that does not say which item is the
/// primary one. A file with no `ispe` anywhere is not a file whirl will promise
/// to display (features.md 2.2), so it is `None` rather than a header of zeroes:
/// zeroes would reach the resolution stage as a measured 0x0 and be reported as
/// under the floor, which is a measurement this build did not make.
fn sniff_heic(bytes: &[u8]) -> Option<Header> {
    if !is_heic(bytes) {
        return None;
    }
    let meta = boxes(bytes, 0, bytes.len())
        .into_iter()
        .find(|found| found.kind == *b"meta")?;
    // `meta` is a FullBox (ISO/IEC 14496-12 8.11.1): four bytes of version and
    // flags, then its children.
    let inside = boxes(bytes, meta.payload + 4, meta.end);
    let iprp = inside.iter().find(|found| found.kind == *b"iprp")?;
    let item = inside
        .iter()
        .find(|found| found.kind == *b"pitm")
        .and_then(|pitm| primary_item(bytes, pitm));
    let sections = boxes(bytes, iprp.payload, iprp.end);
    let owned = sections
        .iter()
        .find(|found| found.kind == *b"ipco")
        .map(|ipco| boxes(bytes, ipco.payload, ipco.end))
        .unwrap_or_default();
    let associated = item
        .and_then(|item| {
            sections
                .iter()
                .find(|found| found.kind == *b"ipma")
                .and_then(|ipma| primary_property(bytes, ipma, &owned, item))
        })
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| owned.get(index))
        .filter(|found| found.kind == *b"ispe")
        .and_then(|found| ispe_size(bytes, found));
    let (width, height) = associated.or_else(|| largest_ispe(bytes, &owned))?;
    Some(Header {
        ext: "heic",
        width,
        height,
    })
}

/// One box in a byte range: what its `type` field says it is, where its own
/// content starts, and where it ends. The payload is never read as anything but
/// the children of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoxHeader {
    kind: [u8; 4],
    /// The first byte after the header: this box's own content.
    payload: usize,
    /// One past the last byte of this box.
    end: usize,
}

/// The boxes of `bytes[from..to]`, in order, each skipped by the size it
/// declares: a 32-bit size, `1` meaning the real size is the 64-bit `largesize`
/// after the type, and `0` meaning the box runs to the end of its parent
/// (ISO/IEC 14496-12 4.2). A size smaller than the box's own header, or one that
/// runs past the parent, ends the walk.
///
/// Stopping is the point. The argument here is a *window*, not a whole file, so
/// a box that claims to be longer than what has been read is the ordinary case
/// for a truncated read, and reading past the end of the window is the one thing
/// this cannot do.
fn boxes(bytes: &[u8], from: usize, to: usize) -> Vec<BoxHeader> {
    let end = to.min(bytes.len());
    let mut out = Vec::new();
    let mut at = from;
    while at + 8 <= end {
        let (size, header) = match u64::from(be32(&bytes[at..at + 4]).unwrap_or(0)) {
            1 => match bytes.get(at + 8..at + 16).and_then(be64) {
                Some(large) => (large, 16usize),
                None => break,
            },
            0 => ((end - at) as u64, 8usize),
            declared => (declared, 8usize),
        };
        let Ok(size) = usize::try_from(size) else {
            break;
        };
        // The size is the file's own field (`largesize` is a `u64` nothing
        // bounds), and both uses below are additions to `at`, so it is bounded
        // *before* either of them. `at + size` on a `largesize` near `u64::MAX`
        // is an addition the input supplies the terms of: in debug it panics
        // (`attempt to add with overflow`, the profile `cargo test` builds), and
        // in release it wraps to a small number, which the walk below steps by -
        // landing back at the box it just read and re-reading it forever. The
        // loop keeps `at <= end`, so `next > end` is exactly `at + size > end`
        // (equivalently `size > end - at`, which is the subtraction that cannot
        // overflow) and is what "runs past the parent" in the doc comment means.
        let Some(next) = at.checked_add(size) else {
            break;
        };
        if size < header || next > end {
            break;
        }
        out.push(BoxHeader {
            kind: [bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]],
            payload: at + header,
            end: next,
        });
        at = next;
    }
    out
}

/// `ispe`'s width and height: a FullBox, so four bytes of version and flags and
/// then two 32-bit big-endian dimensions (ISO/IEC 14496-12 12.1.3).
fn ispe_size(bytes: &[u8], found: &BoxHeader) -> Option<(u32, u32)> {
    let at = found.payload + 4;
    if at + 8 > found.end {
        return None;
    }
    Some((be32(&bytes[at..at + 4])?, be32(&bytes[at + 4..at + 8])?))
}

/// The primary item's id, from `pitm`: a FullBox whose payload is a 16-bit id,
/// or a 32-bit one when the box's version is 1 or more (ISO/IEC 14496-12
/// 8.11.4).
fn primary_item(bytes: &[u8], pitm: &BoxHeader) -> Option<u32> {
    let version = *bytes.get(pitm.payload)?;
    let at = pitm.payload + 4;
    if version >= 1 {
        be32(bytes.get(at..at + 4)?)
    } else {
        Some(u32::from(*bytes.get(at)?) << 8 | u32::from(*bytes.get(at + 1)?))
    }
}

/// The index in `ipco` that `ipma` associates with `item`, the first association
/// whose property is an `ispe`.
///
/// `ipma`'s payload is a 32-bit entry count and then one entry per item: the
/// item's id, the number of properties associated with it, and one property
/// index per association (ISO/IEC 14496-12 8.11.3.3). Two things vary and both
/// are read rather than assumed from the files measured here: the id is 16 bits
/// until the box's version reaches 1 and 32 bits after, and each property index
/// is one byte with a 7-bit index plus an `essential` bit on top, or two bytes
/// with a 15-bit index, when the low bit of the box's flags is set.
fn primary_property(
    bytes: &[u8],
    ipma: &BoxHeader,
    properties: &[BoxHeader],
    item: u32,
) -> Option<usize> {
    let version = *bytes.get(ipma.payload)?;
    let flags = *bytes.get(ipma.payload + 3)?;
    let wide = flags & 1 != 0;
    let stride = if wide { 2 } else { 1 };
    let mask = if wide { 0x7fff } else { 0x7f };
    let mut at = ipma.payload + 4;
    let entries = be32(bytes.get(at..at + 4)?)? as usize;
    at += 4;
    for _ in 0..entries {
        let listed = if version >= 1 {
            be32(bytes.get(at..at + 4)?)?
        } else {
            u32::from(*bytes.get(at)?) << 8 | u32::from(*bytes.get(at + 1)?)
        };
        at += if version >= 1 { 4 } else { 2 };
        let associations = *bytes.get(at)? as usize;
        at += 1;
        if listed != item {
            at += associations * stride;
            continue;
        }
        for index in 0..associations {
            let start = at + index * stride;
            let raw = if wide {
                u32::from(*bytes.get(start)?) << 8 | u32::from(*bytes.get(start + 1)?)
            } else {
                u32::from(*bytes.get(start)?)
            };
            let property = (raw & mask) as usize;
            let named = property
                .checked_sub(1)
                .and_then(|index| properties.get(index))
                .is_some_and(|found| found.kind == *b"ispe");
            if named {
                return Some(property);
            }
        }
        return None;
    }
    None
}

/// The largest `ispe` in `ipco`, which is what a file whose `pitm`/`ipma` pair
/// could not be read is measured by.
fn largest_ispe(bytes: &[u8], properties: &[BoxHeader]) -> Option<(u32, u32)> {
    properties
        .iter()
        .filter(|found| found.kind == *b"ispe")
        .filter_map(|found| ispe_size(bytes, found))
        .max_by_key(|(width, height)| u64::from(*width) * u64::from(*height))
}

fn be64(bytes: &[u8]) -> Option<u64> {
    let mut out = [0u8; 8];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = *bytes.get(index)?;
    }
    Some(u64::from_be_bytes(out))
}

fn be32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes([
        *bytes.first()?,
        *bytes.get(1)?,
        *bytes.get(2)?,
        *bytes.get(3)?,
    ]))
}

/// What the cache holds after a successful store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub digest: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub header: Header,
    /// `true` when the bytes were already in the cache under the same digest:
    /// section 3 step 7's hit, which unlinks the part file instead of renaming.
    pub hit: bool,
}

/// The cache root this run writes into, with the two path shapes of section 2.
/// It is the worker's half of 7.1: a file it creates, under a content-addressed
/// name, and a part file under `tmp/`. The index is the daemon's and is not
/// written here.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn at(root: impl Into<PathBuf>) -> Cache {
        Cache { root: root.into() }
    }

    /// The cache root of docs/architecture.md 4.3's precedence: `WHIRL_CACHE_DIR`
    /// then `cache.root` then the platform default. The daemon resolves the same
    /// three in `whirld::plan::resolve`, which this crate cannot link; the whole
    /// point of the three is that both processes land on the same directory.
    /// `WHIRL_CACHE_DIR` reaches this process because the daemon passes its own
    /// value through the scrub (`Worker::run`, docs/architecture.md 1.6).
    pub fn resolve(config: &Config) -> Result<Cache, Failure> {
        Cache::resolve_with(config, &|name| std::env::var_os(name))
    }

    /// The same three arms with the environment supplied, so the one arm that is
    /// not a value out of `config` can be stated by a test without mutating this
    /// process's environment (which would race every other test in the binary).
    ///
    /// A directory named here need not exist: `create` makes the root, `sha256/`
    /// and `tmp/` mode 0700 on the first write (section 3 step 2), and a root it
    /// cannot create is `cache_unwritable` (8.4), which is a degraded cache and
    /// not a refusal - the same rule the daemon's own probe follows.
    fn resolve_with(
        config: &Config,
        lookup: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Cache, Failure> {
        if let Some(path) = env_path_with(lookup, "WHIRL_CACHE_DIR") {
            return Ok(Cache::at(path));
        }
        if let Some(path) = &config.cache.root {
            return Ok(Cache::at(path));
        }
        match paths::cache_dir() {
            Some(path) => Ok(Cache::at(path)),
            None => Err(Failure::new(
                "download",
                ErrorCode::CacheUnwritable,
                "no cache directory: neither WHIRL_CACHE_DIR, `cache.root` nor a platform default is available",
            )),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join(state::INDEX_FILE)
    }

    /// Section 3 step 2: `mkdir -p` the cache root, `sha256/` and `tmp/`, mode
    /// 0700. A failure here is `cache_unwritable` (8.4).
    pub fn create(&self) -> Result<(), Failure> {
        for dir in [
            self.root.clone(),
            self.root.join("sha256"),
            state::tmp_dir(&self.root),
        ] {
            if let Err(error) = fs::create_dir_all(&dir) {
                return Err(Failure::new(
                    "download",
                    ErrorCode::CacheUnwritable,
                    format!("cannot create {}: {error}", dir.display()),
                ));
            }
            owner_only_dir(&dir);
        }
        Ok(())
    }

    /// Section 3 step 3: `tmp/<run>-<rand>.part`, created with `O_CREAT|O_EXCL`
    /// so two workers cannot collide even if the lock ever failed to do its job.
    pub fn part_path(&self, run: u64) -> PathBuf {
        let rand = nonce();
        state::tmp_dir(&self.root).join(format!("{run}-{rand:08x}.part"))
    }

    /// The cache file for a digest, whatever extension it was stored under. The
    /// index knows the extension; a caller that has only the digest (a `set id`
    /// of a digest) probes the formats of 2.2.
    pub fn find(&self, digest: &str) -> Option<(PathBuf, String)> {
        for ext in ["jpg", "png", "webp", "heic"] {
            let path = state::content_path(&self.root, digest, ext);
            if path.is_file() {
                return Some((path, ext.to_string()));
            }
        }
        None
    }
}

/// Section 3, steps 3 to 7: stream the bytes into `tmp/<run>-<rand>.part`, hash
/// them in the same pass, enforce `filters.max_bytes` mid-stream, sniff the
/// header, and publish with `rename`. A partial or failed fetch leaves nothing
/// under `sha256/`: the part file is deleted on every failure path, and the
/// sweep of 5.5 is the backstop for the one case this process cannot clean up,
/// which is being killed.
pub fn store(
    cache: &Cache,
    run: u64,
    origin: &str,
    transport: &dyn Transport,
    max_bytes: u64,
) -> Result<Stored, Failure> {
    cache.create()?;
    let mut reader = transport
        .open(origin)
        .map_err(|error| Failure::new("download", ErrorCode::Offline, error))?;
    // Section 3 step 3. The part file exists before the first byte of the body
    // is read, and no failure path below leaves it behind.
    let mut part = Part::create(cache, run)?;
    let mut hasher = protocol::Sha256::new();
    let mut head: Vec<u8> = Vec::with_capacity(HEAD_WINDOW);
    let mut window = HEAD_WINDOW;
    let mut bytes = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                part.discard();
                return Err(Failure::new(
                    "download",
                    ErrorCode::Offline,
                    format!("{origin}: {error}"),
                ));
            }
        };
        bytes += read as u64;
        // Section 3 step 4: "The moment the byte count exceeds the cap, delete
        // the part file and fail with `too_large`."
        if bytes > max_bytes {
            part.discard();
            return Err(Failure::new(
                "download",
                ErrorCode::TooLarge,
                format!("{origin}: over filters.max_bytes = {max_bytes}"),
            ));
        }
        hasher.update(&buffer[..read]);
        // Section 3 step 5's head. It starts at the small window and widens, at
        // most once, for a file the small one cannot measure (`head_window`: a
        // JPEG behind a large Exif segment, a HEIC behind `meta`). That is why
        // this takes as much of the chunk as the *current* window has room for
        // instead of a fixed count: the file that needs the wide window can
        // arrive whole in this one read, and capping the first take at 1024
        // bytes would lose the rest of it.
        let mut taken = 0;
        while taken < read {
            let room = window.saturating_sub(head.len());
            if room == 0 {
                break;
            }
            let take = room.min(read - taken);
            head.extend_from_slice(&buffer[taken..taken + take]);
            taken += take;
            window = head_window(&head);
        }
        if let Err(error) = part.write(&buffer[..read]) {
            part.discard();
            return Err(write_failure(origin, error));
        }
    }
    if let Err(error) = part.flush() {
        part.discard();
        return Err(write_failure(origin, error));
    }

    let header = match sniffed(&head) {
        Sniffed::Measured(header) => header,
        // A JPEG whose own walk ran out of head: the bytes said `ffd8`, so what
        // the reader needs is the bound that stopped the measurement, not a
        // claim that the bytes are not a picture. The code stays the protocol's
        // `not_an_image` (2.7 has one code for a sniff that produced no header,
        // and a new code would be a protocol change); the message is what
        // distinguishes the two.
        Sniffed::BeyondWindow => {
            part.discard();
            return Err(Failure::new(
                "download",
                ErrorCode::NotAnImage,
                format!("{origin}: the header is past the {window}-byte head window"),
            ));
        }
        Sniffed::NotAnImage => {
            part.discard();
            return Err(Failure::new(
                "download",
                ErrorCode::NotAnImage,
                format!("{origin}: the header is not JPEG, PNG, WebP or HEIC"),
            ));
        }
    };
    let digest = hasher.hex();
    let final_path = state::content_path(cache.root(), &digest, header.ext);
    if final_path.is_file() && hashes_to(&final_path, &digest) {
        // Section 3 step 7: "If the final path already exists, the bytes are
        // identical by construction, so `unlink` the part file and report a
        // cache hit (the daemon bumps `last_used`)."
        //
        // "By construction" is the file's own contents, not the fact that a
        // directory entry is there: the arriving bytes already hash to `digest`,
        // so the name is right, but a file something else truncated or replaced
        // under that name is not the copy the name promises. That one fails
        // [`hashes_to`] and falls through to the `rename` below, which replaces
        // it with the bytes just fetched instead of handing the damaged copy to
        // the setter. The same check decides [`Window::cached`], so the before-
        // the-request hit and this after-the-download one agree about what a hit
        // is.
        part.discard();
        return Ok(Stored {
            digest,
            path: final_path,
            bytes,
            header,
            hit: true,
        });
    }
    if let Some(parent) = final_path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            part.discard();
            return Err(Failure::new(
                "download",
                ErrorCode::CacheUnwritable,
                format!("cannot create {}: {error}", parent.display()),
            ));
        }
        owner_only_dir(parent);
    }
    if let Err((from, error)) = part.publish(&final_path) {
        return Err(Failure::new(
            "download",
            ErrorCode::CacheUnwritable,
            format!(
                "cannot rename {} to {}: {error}",
                from.display(),
                final_path.display()
            ),
        ));
    }
    Ok(Stored {
        digest,
        path: final_path,
        bytes,
        header,
        hit: false,
    })
}

/// The part file of section 3 step 3: `tmp/<run>-<rand>.part`, created with
/// `O_CREAT|O_EXCL` so two workers of one run cannot take the same name. The
/// handle and its path travel together, so every failure path can close it and
/// unlink it in one call.
struct Part {
    path: PathBuf,
    file: fs::File,
}

impl Part {
    fn create(cache: &Cache, run: u64) -> Result<Part, Failure> {
        let mut collision = PathBuf::new();
        for _ in 0..8 {
            let path = cache.part_path(run);
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    owner_only_file(&path);
                    return Ok(Part { path, file });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    collision = path;
                }
                Err(error) => {
                    return Err(Failure::new(
                        "download",
                        ErrorCode::CacheUnwritable,
                        format!("cannot create {}: {error}", path.display()),
                    ));
                }
            }
        }
        Err(Failure::new(
            "download",
            ErrorCode::CacheUnwritable,
            format!(
                "cannot create a part file in {}: the last name tried, {}, was taken",
                state::tmp_dir(cache.root()).display(),
                collision.display()
            ),
        ))
    }

    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.file.write_all(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }

    /// Section 3: "the part file is deleted on every failure path". Nothing is
    /// reported if the unlink itself fails: the sweep of 5.5 reclaims an
    /// abandoned part file, and the failure being reported is the one that
    /// matters.
    fn discard(self) {
        drop(self.file);
        let _ = fs::remove_file(&self.path);
    }

    /// Step 7's publish: `rename` within one filesystem, which is atomic, so
    /// "the final path either does not exist or is the complete file".
    fn publish(self, final_path: &Path) -> Result<(), (PathBuf, std::io::Error)> {
        let part = self.path;
        drop(self.file);
        match fs::rename(&part, final_path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&part);
                Err((part, error))
            }
        }
    }
}

/// A write error is a full disk or a cache that went away: `enospc` for the
/// first (8.1), `cache_readonly` for the second (8.4).
fn write_failure(origin: &str, error: std::io::Error) -> Failure {
    let code = match error.kind() {
        std::io::ErrorKind::StorageFull => ErrorCode::Enospc,
        _ => ErrorCode::CacheReadonly,
    };
    Failure::new("download", code, format!("{origin}: {error}"))
}

/// Whether a failure out of the download stage belongs to the candidate or to
/// the machine.
///
/// The three that belong to the candidate are the three ways a *byte stream* can
/// be unusable, and features.md 1.4 gives one rule for one bad candidate — mark
/// it bad, try the next — wherever the badness came from:
///
/// - `offline`: the transport could not produce the bytes at all, which for a
///   `wallhaven` candidate is the CDN's answer to that one URL.
/// - `not_an_image`: the bytes arrived and section 3 step 5 could not read a
///   header out of them, which is what a 4xx error page looks like from here.
/// - `too_large`: the bytes ran past the source's own cap mid-stream (section 3
///   step 4), which is a fact about that file.
///
/// A cache that cannot be written is not one of those. 1.4, "Disk full or
/// unwritable cache", says the `*.part` file is removed and "the error is
/// reported" — and every remaining candidate would meet the same obstacle, so the
/// rotation stops rather than turning one disk problem into a log line per
/// candidate.
fn candidate_failure(failure: &Failure) -> bool {
    matches!(
        failure.code,
        ErrorCode::Offline | ErrorCode::NotAnImage | ErrorCode::TooLarge
    )
}

fn owner_only_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn owner_only_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Four random bytes, from the clock and the process id: there is no random
/// source in the standard library, and section 3 step 3 asks for a name that
/// cannot collide, not for a cryptographic draw.
fn nonce() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    (nanos ^ (std::process::id().wrapping_mul(0x9e37_79b9))).wrapping_mul(0x2545_f491)
}

// ---------------------------------------------------------------------------
// The rotation
// ---------------------------------------------------------------------------

/// One run's inputs: everything the stages need that is not the config, so a
/// test can drive a whole rotation with a source and a transport of its own.
pub struct Run<'a> {
    pub config: &'a Config,
    pub setter: &'a dyn Setter,
    pub sources: &'a Sources,
    pub transport: &'a dyn Transport,
    pub cache: &'a Cache,
    pub window: &'a Window,
    pub platform: Platform,
    /// The daemon's rotation id, used for the part file's name.
    pub run: u64,
    /// The one integer draw features.md F2 allows a rotation.
    pub draw: u64,
}

/// What a candidate's bytes became: the whole of what features.md 2.2's `mode`
/// decides for a rotation, and the only two shapes a successful rotation has.
///
/// The arms are the two halves of the `mode` row: `copy` "copies into the cache
/// first", and `reference` "sets the wallpaper from the original path: zero
/// extra disk, the user's folder stays the source of truth".
enum Filled {
    /// The bytes are a cache entry under `sha256/` (docs/spec/state-and-cache.md
    /// section 3): `copy`, and every kind that is not a `local` one.
    Stored(Stored),
    /// The bytes were already in the cache, under the digest the index names for
    /// this candidate's `origin_key`, and nothing was fetched (4.1). This is the
    /// `hit` of section 3 step 7 reached *before* the request instead of after
    /// it: the same bytes, the same content-addressed path, no download.
    Cached {
        /// The digest the index records for this `origin_key`.
        digest: String,
        /// The content-addressed path that digest names, `stat`ed and there.
        path: PathBuf,
    },
    /// Nothing was written, and the platform is pointed at the candidate's own
    /// path: features.md 2.2's `reference`, the printed default.
    Referenced {
        /// The one hash of the file the `set:` line needs, from the same pass
        /// that proves the file is still readable (see [`Run::reference`]).
        digest: String,
        /// The candidate's origin, which is the user's own file.
        path: PathBuf,
    },
}

impl Filled {
    /// The `set:` line's first field. For a stored candidate it is the digest
    /// of the bytes under `sha256/`; for a referenced one it is the digest of
    /// the file the platform was handed, which is what `status`'s `last_digest`
    /// documents itself to be (docs/architecture.md 2.10, "content digest of the
    /// displayed image").
    fn digest(&self) -> &str {
        match self {
            Filled::Stored(stored) => &stored.digest,
            Filled::Cached { digest, .. } => digest,
            Filled::Referenced { digest, .. } => digest,
        }
    }

    /// The path the platform is given, which is the one thing the arms
    /// disagree about: the cache file, or the user's own file.
    fn path(&self) -> &Path {
        match self {
            Filled::Stored(stored) => &stored.path,
            Filled::Cached { path, .. } => path,
            Filled::Referenced { path, .. } => path,
        }
    }
}

impl Run<'_> {
    /// `--verb rotate`: pick a source by weight, pick a candidate, set it.
    ///
    /// The walk follows features.md 1.4: a source that yields nothing
    /// admissible is left and the remaining sources are tried in descending
    /// weight order, at most one enumeration per source per rotation; a
    /// candidate whose bytes cannot be had, and a candidate the setter refuses,
    /// are both marked bad and the next candidate is tried ([`candidate_failure`]
    /// is the line between that and a cache failure, which stops the rotation);
    /// and if nothing is admissible at all the rotation fails with
    /// `no_candidates`.
    ///
    /// The end state is 1.4's "No network" item 2 rather than a `no_candidates`:
    /// if candidates were found and every one of them failed on its bytes, the
    /// failure reported is the last download failure, so `status` gains the
    /// `offline` that section names instead of a message blaming the filters.
    pub fn rotate(&self) -> Result<Report, Failure> {
        let order = weighted_order(&self.sources.weights(), self.draw);
        if order.is_empty() {
            // Name the reason, not a guess between the two: a source with no
            // implementation in this build and a source configured at weight 0
            // are different operator mistakes (4.3).
            let unserved: Vec<String> = self
                .config
                .sources
                .iter()
                .filter(|source| self.sources.get(&source.id).is_none())
                .map(crate::sources::missing_reason)
                .collect();
            let message = if unserved.is_empty() {
                "no source is enabled: every entry in `sources` has weight 0".to_string()
            } else {
                unserved.join("; ")
            };
            return Err(Failure::new("source", ErrorCode::NoCandidates, message));
        }
        let mut bad: HashSet<String> = HashSet::new();
        let mut set_failures: Vec<SetError> = Vec::new();
        let mut fill_failures: Vec<Failure> = Vec::new();
        let mut reasons: Vec<String> = Vec::new();
        for index in order {
            let entry = &self.sources.entries()[index];
            let candidates = match self.enumerate(entry) {
                Ok(candidates) => candidates,
                Err(reason) => {
                    eprintln!("warning: source {} is disabled: {reason}", entry.config.id);
                    // 2.6's second form of the record: `enabled=0` with the
                    // source's own `reason` and no counter group, because
                    // nothing was counted. The reason is the source's own words
                    // and free-form prose, which is why 2.6 makes it the
                    // record's last field: it takes the rest of the line.
                    reasons.push(disabled_record(&entry.config, reason));
                    continue;
                }
            };
            let filtered = filter_pipeline(self.config, self.platform, self.window, candidates);
            if filtered.kept.is_empty() {
                // The exhaustion case of 4.1 is this line: a source offered
                // candidates and the recent window removed all of them, so
                // nothing is set and the reason carries the counters. The
                // daemon has already spent the slot, and 1711's row 3 keeps it
                // that way rather than inventing a retry.
                //
                // The reason is 2.6's `source:` record and not a sentence about
                // it, because the per-stage counts live in exactly one place:
                // "23 candidates, 0 admitted" is true and useless, since it
                // cannot say whether resolution, ratio, size, type or the recent
                // window refused them. `whirl config check` prints this same
                // record for the same source and is 1.4's diagnostic, but a
                // client that only ever calls `next` never sees it, so the
                // failure it receives carries the counters instead. The numbers
                // are the same numbers `check` prints because this is the same
                // builder, which is what lets a reader put the two lines side by
                // side.
                reasons.push(
                    filtered
                        .counters
                        .record(&entry.config, entry.config.weight > 0, None)
                        .line(),
                );
                continue;
            }
            for seeking in filtered.kept {
                if bad.contains(&seeking.candidate.id) {
                    continue;
                }
                let filled = match self.fill(&seeking, &mut reasons) {
                    Ok(Some(filled)) => filled,
                    Ok(None) => continue,
                    // features.md 1.4's per-candidate rule, where what failed is
                    // the bytes rather than the setter: "the bad cache entry is
                    // deleted, the candidate id is marked bad for the rest of
                    // the run, one more candidate is tried". The failure is this
                    // candidate's and not the rotation's, which is what makes
                    // 1.4's "No network" rules true rather than decorative: a
                    // source that does not need the network is still tried, and
                    // one image the CDN will not serve no longer takes the whole
                    // rotation with it.
                    Err(failure) if candidate_failure(&failure) => {
                        eprintln!("warning: {}: {}", failure.stage, failure.message);
                        bad.insert(seeking.candidate.id.clone());
                        fill_failures.push(failure);
                        continue;
                    }
                    // A cache that cannot be written is not this candidate's
                    // problem, and every remaining candidate would meet the same
                    // one: 1.4, "Disk full or unwritable cache" says the part
                    // file is removed and "the error is reported", once.
                    Err(failure) => return Err(failure),
                };
                // 1.6's first line belongs to the store, and neither a
                // reference-mode candidate nor one served from the cache reached
                // it: nothing was downloaded, so nothing is claimed to have been.
                if let Filled::Stored(_) = &filled {
                    println!("{}", self.report(&seeking, &filled).downloaded_line());
                }
                let target = filled.path().display().to_string();
                match self.setter.set(&target) {
                    Ok(()) => {
                        let report = self.report(&seeking, &filled);
                        println!("{}", report.set_line());
                        return Ok(report);
                    }
                    Err(error) => {
                        // features.md 1.4: the candidate is marked bad for the
                        // rest of the run, one more candidate is tried, and
                        // state-and-cache section 3's failure table adds the
                        // other half for a stored candidate: "it stays a cache
                        // entry, and the worker reports `set_failed`". A
                        // referenced candidate has no entry to stay, which is
                        // the mode's own consequence and not a case here.
                        eprintln!("warning: the setter refused {target}: {}", error.message);
                        bad.insert(seeking.candidate.id.clone());
                        set_failures.push(error);
                        if set_failures.len() >= 2 {
                            let last = set_failures.pop().expect("just pushed");
                            return Err(Failure::new("set", last.code, last.message));
                        }
                    }
                }
            }
        }
        if let Some(last) = set_failures.pop() {
            return Err(Failure::new("set", last.code, last.message));
        }
        // 1.4, "No network" item 2 is the end state of the rule above: a rotation
        // whose every candidate failed on its bytes reports that failure, so
        // `status` gains the `offline` the section names, rather than a
        // `no_candidates` that would blame the filters for a download.
        if let Some(last) = fill_failures.pop() {
            return Err(last);
        }
        Err(Failure::new(
            "source",
            ErrorCode::NoCandidates,
            format!(
                "no source produced an admissible image ({})",
                reasons.join("; ")
            ),
        ))
    }

    /// features.md 2.2's `mode` for the source that offered a candidate, or
    /// `None` for a kind that has no `mode` to honour.
    ///
    /// `mode` is a key of the `local` section and of nothing else (features.md
    /// 2.2), so a `wallhaven` URL answers `None`: it is not the user's own file,
    /// there is no original path to reference, and its bytes take the cache
    /// route. A `local` source with no `local` section at all answers the same
    /// value the schema's default does, which is the value
    /// `crate::sources::local::Local::of` reads: a hand-built config that was
    /// never parsed gets the documented default rather than the other arm.
    fn mode_of(&self, source: &str) -> Option<LocalMode> {
        let configured = self
            .config
            .sources
            .iter()
            .find(|configured| configured.id == source)?;
        if configured.kind != SourceKind::Local {
            return None;
        }
        Some(
            configured
                .local
                .as_ref()
                .map(|local| local.mode)
                .unwrap_or(LocalMode::Reference),
        )
    }

    /// The bytes of a candidate: where they go, and the two rejections the
    /// bytes themselves can cause.
    ///
    /// The mode decides the route and nothing else in this function: features.md
    /// 2.2's `reference` is [`Run::reference`], and everything else is the store
    /// of docs/spec/state-and-cache.md section 3. That the default is
    /// `reference` is why this branch is not the rare one: a `local` source that
    /// says nothing about `mode` does not copy the user's library.
    ///
    /// The cache is consulted first, and only for the arm that would store:
    /// 4.1's service map. A candidate whose `origin_key` the index names under a
    /// file that is still there is taken from the cache without a request, which
    /// is section 3 step 7's hit reached before the download instead of after it.
    /// The other two answers are the same miss they always were: no entry, or an
    /// entry whose file is gone, falls through to [`store`] and is fetched.
    ///
    /// `Ok(None)` is the backstop of 2.5 step 1 for a candidate whose source did
    /// not report its dimensions: the header is the authority, and a file under
    /// the floor is not admissible. It is a rejection rather than a failure, so
    /// the rotation moves on. The bytes stay a cache entry: they are a valid
    /// image, 5.5's sweep owns removal, and deleting a file a cache hit may
    /// share is worse than keeping a small one.
    fn fill(
        &self,
        seeking: &Seeking,
        reasons: &mut Vec<String>,
    ) -> Result<Option<Filled>, Failure> {
        if matches!(self.mode_of(&seeking.source), Some(LocalMode::Reference)) {
            return self.reference(seeking).map(Some);
        }
        // 4.1: the index is asked before the request is made, not after it. The
        // floor of 2.5 step 1 is not re-applied here: the source's own dimensions
        // admitted this candidate at the resolution stage, and the file was
        // admitted the first time it was stored, so a second measurement would
        // read the same bytes to reach the same answer.
        if let Some((digest, path)) = self.window.cached(self.cache, &seeking.origin_key()) {
            return Ok(Some(Filled::Cached { digest, path }));
        }
        let cap = self
            .config
            .sources
            .iter()
            .find(|source| source.id == seeking.source)
            .and_then(|source| source.max_bytes)
            .unwrap_or(self.config.filters.max_bytes);
        let stored = store(
            self.cache,
            self.run,
            &seeking.candidate.origin,
            self.transport,
            cap,
        )?;
        let (min_width, min_height) = floors(self.config, seeking);
        if stored.header.width < min_width || stored.header.height < min_height {
            reasons.push(format!(
                "{}: {}x{} is under the {}x{} floor",
                seeking.candidate.origin,
                stored.header.width,
                stored.header.height,
                min_width,
                min_height
            ));
            return Ok(None);
        }
        Ok(Some(Filled::Stored(stored)))
    }

    /// features.md 2.2's `reference`: the platform is pointed at the candidate's
    /// own path and nothing is written, so `cache/sha256/` gains no file and
    /// `tmp/` gains none either.
    ///
    /// Two things this deliberately does not do, both of them the mode's own
    /// consequences rather than omissions. The floor is not re-applied: the
    /// source read the header to produce the candidate at all
    /// (`crate::sources::local::Local::candidate` answers `None` for a file it
    /// cannot measure) and 2.5's resolution stage admitted it on those
    /// dimensions, so a second measurement here would read the same bytes to
    /// reach the same answer. `filters.max_bytes` is not enforced either: it is
    /// the download cap of 2.5 step 3, and there is no download to bound.
    ///
    /// The file is still opened, for the one field the record needs and for the
    /// only way this process can know the file is still there. `set:`'s first
    /// field is a digest by the wire grammar
    /// (`protocol::parse_worker_set_line` refuses anything else), so the bytes
    /// are hashed in one pass and the open failure is the failure
    /// [`Run::set_reference`] reports for the same act on the same kind of
    /// path. `state-and-cache` 6.2 has a `-` in that field for a reference-mode
    /// set; writing it would need `whirl-core`'s grammar and its parser, and
    /// this crate writes neither.
    fn reference(&self, seeking: &Seeking) -> Result<Filled, Failure> {
        let origin = seeking.candidate.origin.as_str();
        let mut reader = self
            .transport
            .open(origin)
            .map_err(|error| Failure::new("set", ErrorCode::NotFound, error))?;
        let mut hasher = protocol::Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => hasher.update(&buffer[..read]),
                Err(error) => {
                    return Err(Failure::new(
                        "set",
                        ErrorCode::NotFound,
                        format!("{origin}: {error}"),
                    ));
                }
            }
        }
        Ok(Filled::Referenced {
            digest: hasher.hex(),
            path: PathBuf::from(origin),
        })
    }

    /// The record the daemon reads for a successful set: the digest, the
    /// candidate's `origin_key` and the published path.
    ///
    /// "Published" is the path the platform was given, which is the cache file
    /// for a stored candidate and the user's own file for a referenced one. Both
    /// are what the daemon wants: `cache::record` writes no index entry for a
    /// path outside the cache root, naming this very case
    /// (crates/whirld/src/cache.rs).
    fn report(&self, seeking: &Seeking, filled: &Filled) -> Report {
        Report {
            digest: filled.digest().to_string(),
            origin_key: seeking.origin_key(),
            path: filled.path().display().to_string(),
        }
    }

    /// One source's candidates, or the reason it could not be asked. The recent
    /// window is handed over so a source that can exclude it in its own request
    /// does (features.md 2.5, Layer 1 and 2).
    fn enumerate(&self, entry: &crate::sources::Entry) -> Result<Vec<Seeking>, String> {
        entry
            .source
            .validate(&entry.config)
            .map_err(|error| error.to_string())?;
        let ctx = EnumContext {
            run: self.run,
            home: paths::home(),
            recent: self.window.keys(),
        };
        let enumerated = entry
            .source
            .enumerate(&ctx)
            .map_err(|error| error.to_string())?;
        Ok(enumerated
            .candidates
            .into_iter()
            .map(|candidate| Seeking {
                source: entry.config.id.clone(),
                candidate,
            })
            .collect())
    }

    /// `--verb set --target <path|id>`.
    ///
    /// A path is referenced rather than copied (docs/architecture.md 6.4: "the
    /// platform is pointed at the user's own file"), so there is no cache write
    /// and the digest is the one hash of the file that the record needs. An id
    /// resolves against the cache: a digest whose file is there is set where it
    /// is, which is what makes a re-materialised favorite land at the same path
    /// (state-and-cache 2.1). Re-materialising from a recorded origin needs the
    /// source that owns it, and this build has no route from an id back to its
    /// origin: an id resolves here only when the cache holds it.
    pub fn set(&self, target: &str) -> Result<Report, Failure> {
        if looks_like_a_path(target) {
            return self.set_reference(target);
        }
        if protocol::is_digest(target) {
            return match self.cache.find(target) {
                Some((path, _)) => {
                    let report = self.set_a_path(target, &format!("external:{target}"), &path)?;
                    Ok(report)
                }
                None => Err(Failure::new(
                    "set",
                    ErrorCode::NotFound,
                    format!("no cached file for digest {target}"),
                )),
            };
        }
        Err(Failure::new(
            "set",
            ErrorCode::NotFound,
            format!(
                "{target} is not a cached digest: re-materialising an origin_key needs the source that owns it, and this build has no route from an id back to its origin"
            ),
        ))
    }

    fn set_reference(&self, target: &str) -> Result<Report, Failure> {
        let path = absolute(target)?;
        let file = fs::File::open(&path).map_err(|error| {
            Failure::new("set", ErrorCode::NotFound, format!("{path}: {error}"))
        })?;
        let mut hasher = protocol::Sha256::new();
        let mut reader = std::io::BufReader::new(file);
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => hasher.update(&buffer[..read]),
                Err(error) => {
                    return Err(Failure::new(
                        "set",
                        ErrorCode::NotFound,
                        format!("{path}: {error}"),
                    ));
                }
            }
        }
        // 2.6: for an `external` entry `origin_key` is `external:<sha256 of the
        // absolute path>`.
        let origin_key = format!("external:{}", protocol::sha256_hex(path.as_bytes()));
        self.set_a_path(&hasher.hex(), &origin_key, Path::new(&path))
    }

    fn set_a_path(&self, digest: &str, origin_key: &str, path: &Path) -> Result<Report, Failure> {
        self.setter
            .set(&path.display().to_string())
            .map_err(|error| Failure::new("set", error.code, error.message))?;
        let report = Report {
            digest: digest.to_string(),
            origin_key: origin_key.to_string(),
            path: path.display().to_string(),
        };
        println!("{}", report.set_line());
        Ok(report)
    }
}

/// `~` is the user's home directory (features.md 2.2), and a record's path is
/// absolute on POSIX (2.6).
fn absolute(path: &str) -> Result<String, Failure> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = paths::home().ok_or_else(|| {
            Failure::new(
                "set",
                ErrorCode::BadArgs,
                "`~` cannot be expanded: HOME is not set",
            )
        })?;
        return Ok(format!("{}/{rest}", home.display()));
    }
    if path.starts_with('/') || looks_like_a_windows_path(path) {
        return Ok(path.to_string());
    }
    Err(Failure::new(
        "set",
        ErrorCode::BadArgs,
        format!("{path:?} is not an absolute path"),
    ))
}

fn looks_like_a_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("~/")
        || looks_like_a_windows_path(value)
        // A bare file name with an extension is still a path the user typed, and
        // the answer is "not absolute" rather than "not a cached digest": the
        // two mistakes are different and deserve different messages. A digest
        // and an `origin_key` name no extension, so neither is caught here.
        || extension_of(value).is_some()
}

fn looks_like_a_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// features.md F2: "Each rotation picks one source by integer weight, then one
/// candidate from it." The first index is that draw; the rest follow in
/// descending weight order, which is the order 1.4 walks when a source answered
/// with nothing admissible. Ties keep config order.
pub fn weighted_order(weights: &[u32], draw: u64) -> Vec<usize> {
    let enabled: Vec<usize> = (0..weights.len())
        .filter(|index| weights[*index] > 0)
        .collect();
    if enabled.is_empty() {
        return Vec::new();
    }
    let total: u64 = enabled.iter().map(|index| weights[*index] as u64).sum();
    let point = draw % total;
    let mut cumulative = 0u64;
    let mut first = enabled[0];
    for index in &enabled {
        cumulative += weights[*index] as u64;
        if point < cumulative {
            first = *index;
            break;
        }
    }
    let mut order = vec![first];
    let mut rest: Vec<usize> = enabled
        .into_iter()
        .filter(|index| *index != first)
        .collect();
    rest.sort_by(|a, b| weights[*b].cmp(&weights[*a]).then(a.cmp(b)));
    order.extend(rest);
    order
}

/// The rotation's one draw (features.md F2), from the clock, the process and the
/// rotation id: no random source is needed, and the value only has to be
/// unpredicted by the previous rotation.
pub fn draw(run: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let mut mixed = nanos ^ run.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (std::process::id() as u64);
    // splitmix64's finaliser, so a one-nanosecond step moves every bit.
    mixed ^= mixed >> 30;
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^= mixed >> 27;
    mixed = mixed.wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

// ---------------------------------------------------------------------------
// `--verb check`
// ---------------------------------------------------------------------------

/// `--verb check`: the source and plan records `whirl config check` forwards
/// (docs/development.md section 7).
///
/// This is features.md 2.5's "Filter reporting": "`whirl config check` prints,
/// per source, the candidates found and the candidates removed per stage". A
/// source whose kind has no implementation, or whose config this build cannot
/// work with, prints `enabled=0` and the reason rather than a counter group of
/// zeros, because it never enumerated (2.6's group is optional in the form).
pub fn check(
    config: &Config,
    backend: Backend,
    sources: &Sources,
    window: &Window,
    platform: Platform,
) {
    for source in &config.sources {
        match sources.get(&source.id) {
            None => println!(
                "{}",
                disabled_record(source, crate::sources::missing_reason(source))
            ),
            Some(entry) => match entry.source.validate(&entry.config) {
                // 4.3: "The worker runs the semantic check ... and prints
                // `enabled=0 reason=<...>`", naming the offending key.
                Err(error) => println!("{}", disabled_record(source, error.to_string())),
                Ok(()) => match entry.source.enumerate(&EnumContext {
                    run: 0,
                    home: paths::home(),
                    recent: window.keys(),
                }) {
                    Err(error) => println!("{}", disabled_record(source, error.to_string())),
                    Ok(enumerated) => {
                        let candidates = enumerated
                            .candidates
                            .into_iter()
                            .map(|candidate| Seeking {
                                source: entry.config.id.clone(),
                                candidate,
                            })
                            .collect();
                        let filtered = filter_pipeline(config, platform, window, candidates);
                        println!(
                            "{}",
                            filtered
                                .counters
                                .record(source, source.weight > 0, None)
                                .line()
                        );
                    }
                },
            },
        }
    }
    // One implementation of the plan line, in `whirl-core` beside the schema:
    // the daemon records the same line for a scheduled rotation, so the two can
    // never disagree about the key order (2.6).
    println!("{}", protocol::plan_record(&config.plan_pairs(backend)));
}

/// A source that could not be asked: `enabled=0` and the reason, with no counter
/// group, because nothing was counted.
fn disabled_record(source: &SourceConfig, reason: String) -> String {
    let mut record = source.record(None);
    record.enabled = false;
    record.reason = Some(reason);
    record.line()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};
    use std::io::Cursor;
    use std::rc::Rc;
    use whirl_core::config::{Config, ConfigError};
    use whirl_core::protocol::{Kind, Via};
    use whirl_core::source::{Capability, Enumerated, FilterSet, Source, SourceError};
    use whirl_core::state::{CacheIndexEntry, HistoryEntry, HistoryFile, IndexFile};

    // -- the two things a test owns: a source and a byte source -------------

    /// A source that answers with a list the test wrote. It is not a `kind`:
    /// features.md 2.6's dispatch table is [`crate::sources`]'s, and nothing here
    /// is compiled into the binary's own table. That is how a test-only source
    /// can exist at all.
    struct Fixture {
        candidates: Vec<Candidate>,
        seen_recent: Rc<RefCell<Vec<String>>>,
        broken: Option<String>,
    }

    impl Fixture {
        fn with(candidates: Vec<Candidate>) -> Fixture {
            Fixture {
                candidates,
                seen_recent: Rc::new(RefCell::new(Vec::new())),
                broken: None,
            }
        }

        /// The handle a test keeps to see what `enumerate` was handed.
        fn handle(&self) -> Rc<RefCell<Vec<String>>> {
            Rc::clone(&self.seen_recent)
        }
    }

    impl Source for Fixture {
        fn validate(&self, _config: &SourceConfig) -> Result<(), ConfigError> {
            match &self.broken {
                Some(message) => Err(ConfigError::new(
                    "sources[0].paths[0]",
                    3,
                    message.to_string(),
                )),
                None => Ok(()),
            }
        }

        fn enumerate(&self, ctx: &EnumContext) -> Result<Enumerated, SourceError> {
            *self.seen_recent.borrow_mut() = ctx.recent.clone();
            Ok(Enumerated::of(self.candidates.clone()))
        }

        fn capabilities(&self) -> FilterSet {
            FilterSet::of(&[Capability::Resolution, Capability::Extension])
        }
    }

    /// The bytes a candidate's origin resolves to. A whole rotation can then run
    /// with no filesystem of its own beyond the cache root, and a fetch failure
    /// is a missing key rather than a chmod.
    struct Bytes(HashMap<String, Vec<u8>>);

    impl Bytes {
        fn of(pairs: &[(&str, Vec<u8>)]) -> Bytes {
            Bytes(
                pairs
                    .iter()
                    .map(|(origin, bytes)| (origin.to_string(), bytes.clone()))
                    .collect(),
            )
        }
    }

    impl Transport for Bytes {
        fn open(&self, origin: &str) -> Result<Box<dyn Read>, String> {
            match self.0.get(origin) {
                Some(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
                None => Err(format!("{origin}: no such file")),
            }
        }
    }

    /// The HTTP client a test hands [`Origins`]: the bodies it wrote, and every
    /// request it was asked for.
    ///
    /// This is the whole of "the test suite stays offline" for the URL route.
    /// Nothing in it opens a socket, and a URL it has no body for is a failure
    /// rather than a real request, so a test that quietly reached the network
    /// would go red instead of slow.
    struct Recorded {
        bodies: HashMap<String, Vec<u8>>,
        seen: RefCell<Vec<(String, Option<String>)>>,
    }

    impl Recorded {
        fn of(pairs: &[(&str, Vec<u8>)]) -> Recorded {
            Recorded {
                bodies: pairs
                    .iter()
                    .map(|(url, bytes)| (url.to_string(), bytes.clone()))
                    .collect(),
                seen: RefCell::new(Vec::new()),
            }
        }

        /// The requests, in order: the URL, and the key if one was sent.
        fn requests(&self) -> Vec<(String, Option<String>)> {
            self.seen.borrow().clone()
        }
    }

    impl crate::http::Bytes for Recorded {
        fn bytes(&self, request: &crate::http::Request<'_>) -> Result<Box<dyn Read>, String> {
            self.seen
                .borrow_mut()
                .push((request.url.to_string(), request.key.map(str::to_string)));
            match self.bodies.get(request.url) {
                Some(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
                None => Err(format!("{}: 404", request.url)),
            }
        }
    }

    fn read_all(mut reader: Box<dyn Read>) -> Vec<u8> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .expect("the fixture body reads to its end");
        bytes
    }

    /// features.md 2.2's two origin shapes, and the choice between them: an
    /// `https://` origin is *fetched* and a path is *opened*, decided from the
    /// origin alone.
    ///
    /// The defect this closes was precisely that the choice did not exist: the
    /// only transport in the binary was [`Paths`], so a `wallhaven` candidate's
    /// `https://` URL reached `fs::File::open` and the rotation reported the errno
    /// of a file that never existed (`999ea4f5`). Neither direction can pass by
    /// accident here — the URL is not a file on this machine, and the path is
    /// something the recorded client has never heard of — so a transport that
    /// picked one route for both would fail on one of the two halves.
    ///
    /// The key is asserted to be absent as well: the API hands the image URL out
    /// to anyone, and 2.4's key authenticates a listing.
    #[test]
    fn the_transport_is_chosen_by_the_origin() {
        let dir = scratch("origin-route");
        let path = dir.join("pictures").join("one.png");
        fs::create_dir_all(path.parent().expect("a parent")).expect("the fixture directory");
        let file_bytes = long_png(1600, 900, 16);
        fs::write(&path, &file_bytes).expect("the fixture file");

        let url = "https://w.wallhaven.cc/full/83/wallhaven-83dp81.png";
        let url_bytes = long_png(1600, 901, 16);
        let client = Recorded::of(&[(url, url_bytes.clone())]);
        let transport = Origins::new(&client);

        assert_eq!(
            read_all(transport.open(url).expect("the URL is fetched")),
            url_bytes
        );
        assert_eq!(
            client.requests(),
            vec![(url.to_string(), None)],
            "one request for the image, and no key on it"
        );

        let origin = path.display().to_string();
        assert_eq!(
            read_all(transport.open(&origin).expect("the path is opened")),
            file_bytes
        );
        assert_eq!(
            client.requests().len(),
            1,
            "a path is not a request: the recorded client never saw it"
        );
    }

    // -- minimal files of each format, enough for the sniff -----------------

    /// A PNG with dimensions: the magic and the IHDR chunk, which is all section
    /// 3 step 5 reads.
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

    /// A JPEG with dimensions: an APP0 segment, then a frame header.
    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend_from_slice(&[0xff, 0xe0, 0x00, 0x10]);
        bytes.extend_from_slice(b"JFIF\0");
        bytes.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
        bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        bytes
    }

    /// The payload of an APP1/Exif segment of `len` bytes: the Exif identifier
    /// (`Exif\0\0`, JEITA CP-3451 4.7.2), then filler, so the segment has the
    /// shape a camera or a wallpaper service writes. `len` is the payload length;
    /// the segment's declared length is `len + 2` ([`jpeg_segment`]).
    fn exif(len: usize) -> Vec<u8> {
        let mut payload = b"Exif\0\0".to_vec();
        assert!(len >= payload.len(), "an Exif payload holds its identifier");
        let pad = len - payload.len();
        filler(&mut payload, pad);
        payload
    }

    /// The payload of an APP2/ICC segment of `len` bytes: the ICC signature
    /// (`ICC_PROFILE\0`, ICC.1:2004-10 Annex B.4), then filler.
    fn icc(len: usize) -> Vec<u8> {
        let mut payload = b"ICC_PROFILE\0".to_vec();
        assert!(len >= payload.len(), "an ICC payload holds its signature");
        let pad = len - payload.len();
        filler(&mut payload, pad);
        payload
    }

    /// One JPEG segment as a real encoder writes it: `ff<marker>`, the declared
    /// length, then the payload. ISO/IEC 10918-1 B.1.1.4: the length field counts
    /// itself and the payload.
    fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let declared = payload.len() + 2;
        assert!(
            declared <= u16::MAX as usize,
            "a JPEG segment's length field is 16 bits"
        );
        let mut bytes = vec![0xff, marker];
        bytes.extend_from_slice(&(declared as u16).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    /// A JPEG in the shape a photograph from a camera or a wallpaper service has,
    /// rather than the minimal JFIF one above: `ffd8`, then the APP segments the
    /// test names (APP1/Exif, APP2/ICC, both skipped when empty), then the frame
    /// header, then `image` bytes of coded data. The shape is the point: a
    /// photograph's frame header sits behind its own metadata, which is
    /// kilobytes, instead of at byte 30.
    fn jpeg_photo(width: u16, height: u16, app1: &[u8], app2: &[u8], image: usize) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        if !app1.is_empty() {
            bytes.extend_from_slice(&jpeg_segment(0xe1, app1));
        }
        if !app2.is_empty() {
            bytes.extend_from_slice(&jpeg_segment(0xe2, app2));
        }
        bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        filler(&mut bytes, image);
        bytes
    }

    fn webp_header(chunk: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(chunk);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(body);
        while bytes.len() < 30 {
            bytes.push(0);
        }
        bytes
    }

    /// Lossless WebP: the 14-bit packed dimensions.
    fn webp_lossless(width: u32, height: u32) -> Vec<u8> {
        let mut body = vec![0x2f];
        let bits = ((width - 1) & 0x3fff) | (((height - 1) & 0x3fff) << 14);
        body.extend_from_slice(&bits.to_le_bytes());
        webp_header(b"VP8L", &body)
    }

    /// Extended WebP: 24-bit canvas dimensions.
    fn webp_extended(width: u32, height: u32) -> Vec<u8> {
        let mut body = vec![0u8; 4];
        let (w, h) = (width - 1, height - 1);
        body.extend_from_slice(&[w as u8, (w >> 8) as u8, (w >> 16) as u8]);
        body.extend_from_slice(&[h as u8, (h >> 8) as u8, (h >> 16) as u8]);
        webp_header(b"VP8X", &body)
    }

    /// A box: a 32-bit size, a type, and the payload.
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    /// A box that declares its real size in the 64-bit `largesize` field: a
    /// 32-bit size of `1`, the type, then the size the file chooses (ISO/IEC
    /// 14496-12 4.2). The payload is whatever the caller passes; the declared
    /// size is the caller's number, not the payload's length, which is the point
    /// of the tests that use it.
    fn boxed_large(kind: &[u8; 4], largesize: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = 1u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(&largesize.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    /// The file type box every HEIC starts with: a major brand and a compatible
    /// one, which is all `is_heic` reads.
    fn ftyp() -> Vec<u8> {
        let mut payload = b"heic".to_vec();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(b"mif1");
        boxed(b"ftyp", &payload)
    }

    /// An `ispe` property: version and flags, then width and height.
    fn ispe(width: u32, height: u32) -> Vec<u8> {
        let mut payload = vec![0u8; 4];
        payload.extend_from_slice(&width.to_be_bytes());
        payload.extend_from_slice(&height.to_be_bytes());
        boxed(b"ispe", &payload)
    }

    /// `pitm`, version 0: the primary item's id.
    fn pitm(item: u16) -> Vec<u8> {
        let mut payload = vec![0u8; 4];
        payload.extend_from_slice(&item.to_be_bytes());
        boxed(b"pitm", &payload)
    }

    /// `ipma`, version 0 with one-byte property indices: one associated property
    /// per item, as `(item id, property index)`, the index 1-based against
    /// `ipco`'s children.
    fn ipma(entries: &[(u16, u8)]) -> Vec<u8> {
        let mut payload = vec![0u8; 4];
        payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (item, property) in entries {
            payload.extend_from_slice(&item.to_be_bytes());
            payload.push(1);
            payload.push(*property);
        }
        boxed(b"ipma", &payload)
    }

    /// A HEIC in the shape Apple's own files have: `ftyp`, then `meta` with
    /// `pitm` (the primary item is 2), an `iprp` whose `ipco` holds one `ispe`
    /// per item and whose `ipma` gives item 1 the first and item 2 the second,
    /// then `mdat` with a well-formed `ispe` box *inside its payload* - image
    /// data that reads like a box header, which a byte scan of the window could
    /// have answered with. `filler` is a box of padding between `pitm` and
    /// `iprp`, which is how a test pushes the properties past the small window.
    fn heic_of(thumbnail: (u32, u32), primary: (u32, u32), filler: usize) -> Vec<u8> {
        let ipco = boxed(
            b"ipco",
            &[ispe(thumbnail.0, thumbnail.1), ispe(primary.0, primary.1)].concat(),
        );
        let iprp = boxed(b"iprp", &[ipco, ipma(&[(1, 1), (2, 2)])].concat());
        let meta = boxed(
            b"meta",
            &[
                vec![0, 0, 0, 0], // the FullBox version and flags
                pitm(2),
                boxed(b"free", &vec![0u8; filler]),
                iprp,
            ]
            .concat(),
        );
        [ftyp(), meta, boxed(b"mdat", &ispe(2048, 1536))].concat()
    }

    /// The two-item shape: a 1024x1024 thumbnail item and the primary one, which
    /// is what every `.heic` measured on this machine carries.
    fn heic(width: u32, height: u32) -> Vec<u8> {
        heic_of((1024, 1024), (width, height), 0)
    }

    fn filler(bytes: &mut Vec<u8>, extra: usize) {
        bytes.extend(std::iter::repeat_n(0u8, extra));
    }

    // -- the harness --------------------------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whirl-worker-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    /// 4.3's order for the cache root, arm by arm: the environment beats the
    /// file, the file beats the compiled default, and the platform default is
    /// what is left. `WHIRL_CACHE_DIR` is the one arm that is not a value out of
    /// the config, so it arrives as a lookup: a test that set this process's own
    /// environment would race every other test in this binary, and the daemon
    /// passes its own value to this process anyway (`Worker::run`, 1.6).
    #[test]
    fn the_cache_root_follows_4_3s_precedence() {
        let dir = scratch("cache-precedence");
        let from_the_file = config(&dir, "");
        let environment = dir.join("from-environment");
        let from_the_environment = |name: &str| match name {
            "WHIRL_CACHE_DIR" => Some(OsString::from(environment.clone())),
            _ => None,
        };

        assert_eq!(
            Cache::resolve_with(&from_the_file, &from_the_environment)
                .expect("a cache")
                .root(),
            environment.as_path(),
            "the environment wins over the `cache.root` the file sets"
        );
        assert_eq!(
            Cache::resolve_with(&from_the_file, &|_| None)
                .expect("a cache")
                .root(),
            dir.join("cache").as_path(),
            "`cache.root` wins over the compiled default"
        );

        let bare = Config::parse("{\n  \"sources\": []\n}\n")
            .expect("a config with no cache root")
            .config;
        assert_eq!(bare.cache.root, None, "the empty arm is really empty");
        assert_eq!(
            Cache::resolve_with(&bare, &|_| None)
                .expect("a cache")
                .root(),
            paths::cache_dir()
                .expect("this platform has a cache directory")
                .as_path(),
            "the compiled default is the floor"
        );
    }

    /// The state directory's two arms, resolved the same way (4.3 gives it no
    /// config-file arm, because the config lives in the state directory's own
    /// tree). Tested here rather than through a real state file because the
    /// resolution is the thing the scrub could have dropped.
    #[test]
    fn the_state_directory_prefers_the_environment_over_the_platform_default() {
        let dir = scratch("state-precedence");
        let environment = dir.join("from-environment");
        assert_eq!(
            state_directory_with(&|name| match name {
                "WHIRL_STATE_DIR" => Some(OsString::from(environment.clone())),
                _ => None,
            }),
            Some(environment),
            "the environment wins"
        );
        assert_eq!(
            state_directory_with(&|_| None),
            paths::state_dir(),
            "the platform default is what is left"
        );
    }

    /// A directory the environment names need not exist: section 3 step 2 makes
    /// the cache root, `sha256/` and `tmp/` mode 0700 on the first write. This is
    /// the state directory's rule too (the daemon creates it at startup,
    /// crates/whirld/src/plan.rs), so it is the one `WHIRL_CACHE_DIR` follows.
    #[test]
    fn a_cache_root_that_does_not_exist_is_created_owner_only() {
        let root = scratch("cache-create").join("a").join("cache");
        assert!(!root.exists(), "the fixture starts with no cache root");

        Cache::at(&root)
            .create()
            .expect("the directories are created");

        for directory in [root.clone(), root.join("sha256"), root.join("tmp")] {
            assert!(directory.is_dir(), "{} is a directory", directory.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [&root, &root.join("sha256"), &root.join("tmp")] {
                let mode = std::fs::metadata(directory)
                    .expect("the directory")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o700, "{} is owner-only", directory.display());
            }
        }
    }

    /// A config with one `local` source whose path is fake: the pipeline never
    /// reads it, because [`Bytes`] answers the origins. The floors are low and
    /// the cap is small so a fixture image can be either side of both.
    ///
    /// `source_keys` lands inside the source's own object, which is where
    /// `local.mode` lives (features.md 2.2); `extra` lands after the `sources`
    /// array, which is where a test adds a top-level key. Two arguments rather
    /// than one because a key in the wrong object is not the key under test.
    fn config_with(dir: &Path, extra: &str, source_keys: &str) -> Config {
        // A path becomes a string literal in the body, so it is escaped rather
        // than trusted: on Windows `dir` is `C:\Users\...`, and an unescaped
        // `\U` is a syntax error at the config parser (which decodes the full
        // escape set precisely because Windows paths need it,
        // crates/whirl-core/src/config.rs). The two lines are the ones
        // crates/whirl-worker/tests/argv.rs uses for the same reason.
        let quoted = |path: PathBuf| {
            let path = path.display().to_string();
            format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
        };
        let body = format!(
            "{{\n  \"config_schema\": 1,\n  \"backend\": \"noop\",\n  \"min_width\": 16,\n  \
             \"min_height\": 16,\n  \"filters\": {{ \"max_bytes\": 4096, \"ratio_tolerance\": 0.02, \
             \"target_ratio\": null }},\n  \"cache\": {{ \"root\": {} }},\n  \
             \"sources\": [ {{ \"id\": \"pictures\", \"kind\": \"local\", \"weight\": 1, \
             \"paths\": [{}]{source_keys} }} ]{extra}\n}}\n",
            quoted(dir.join("cache")),
            quoted(dir.join("pictures"))
        );
        Config::parse(&body)
            .expect("the fixture config parses")
            .config
    }

    /// The fixture config with no key beyond the schema, and in particular no
    /// `mode`: what the shipped schema writes and what the default is.
    fn config(dir: &Path, extra: &str) -> Config {
        config_with(dir, extra, "")
    }

    /// The fixture config in `copy`: the arm that stores.
    ///
    /// Every test below that asserts a cache file, a part file or a download
    /// failure is a claim about the store, and features.md 2.2's default is the
    /// other arm: leaving them on the default made them assert one mode's
    /// behaviour under the other one's config, which is the shape of the defect
    /// `mode` was read for in the first place. The two tests that are about
    /// `mode` itself name it in their own body, one arm each.
    fn copy_config(dir: &Path) -> Config {
        config_with(dir, "", ", \"mode\": \"copy\"")
    }

    fn candidate(id: &str, origin: &str, width: u32, height: u32, bytes: u64) -> Candidate {
        Candidate {
            id: id.to_string(),
            origin: origin.to_string(),
            width: Some(width),
            height: Some(height),
            bytes: Some(bytes),
        }
    }

    /// The same candidate as the pipeline sees it: tagged with the config's
    /// `pictures` source, which is what the per-source overrides key on.
    fn seeking(id: &str, origin: &str, width: u32, height: u32, bytes: u64) -> Seeking {
        Seeking {
            source: "pictures".to_string(),
            candidate: candidate(id, origin, width, height, bytes),
        }
    }

    /// The whole pipeline, in one process: [`crate::sources::Entry`] with a
    /// [`Fixture`] behind it.
    fn table(config: &Config, source: Fixture) -> crate::sources::Sources {
        crate::sources::Sources::of(vec![crate::sources::Entry {
            config: config.sources[0].clone(),
            source: Box::new(source),
        }])
    }

    fn empty_window(dir: &Path) -> Window {
        Window::load(&dir.join("state"), &dir.join("cache/index.json"), 50)
    }

    fn count_files(dir: &Path) -> usize {
        let Ok(entries) = fs::read_dir(dir) else {
            return 0;
        };
        let mut total = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += count_files(&path);
            } else {
                total += 1;
            }
        }
        total
    }

    fn long_png(width: u32, height: u32, extra: usize) -> Vec<u8> {
        let mut bytes = png(width, height);
        filler(&mut bytes, extra);
        bytes
    }

    fn noop_run<'a>(
        config: &'a Config,
        sources: &'a crate::sources::Sources,
        transport: &'a dyn Transport,
        cache: &'a Cache,
        window: &'a Window,
        setter: &'a dyn Setter,
    ) -> Run<'a> {
        noop_run_of(config, sources, transport, cache, window, setter, 1)
    }

    /// The same run with the rotation id of the caller: a rotation's id names
    /// its part file (`tmp/<run>-<rand>.part`, 1.6), so two consecutive
    /// rotations are two ids, which is what the window test needs.
    #[allow(clippy::too_many_arguments)]
    fn noop_run_of<'a>(
        config: &'a Config,
        sources: &'a crate::sources::Sources,
        transport: &'a dyn Transport,
        cache: &'a Cache,
        window: &'a Window,
        setter: &'a dyn Setter,
        run: u64,
    ) -> Run<'a> {
        Run {
            config,
            setter,
            sources,
            transport,
            cache,
            window,
            platform: Platform::Macos,
            run,
            draw: 0,
        }
    }

    // -- the sniff, per format ----------------------------------------------

    /// The harness writes paths into a config body, and a path with a backslash
    /// in it is why that is a quoting job and not string concatenation: written
    /// raw, the config parser stops at `unknown escape '\U'`. On the Windows
    /// runner the temporary directory is `C:\Users\...`, which is how eleven
    /// pipeline tests failed there and nowhere else (run 36157542930; fixed in
    /// `d24ddfc7`). This pins the escaping on the platform the test runs on.
    #[test]
    fn the_fixture_config_escapes_a_path_with_a_backslash_in_it() {
        // Absolute where the test runs and carrying a backslash either way:
        // `cache.root` must be absolute, and `/tmp/...` is only root-relative
        // on Windows, so the Windows runner needs its own literal.
        #[cfg(windows)]
        let dir = PathBuf::from(r"C:\Users\whirl\worker-7");
        #[cfg(not(windows))]
        let dir = PathBuf::from(r"/tmp/whirl\worker-7");
        let parsed = config(&dir, "");
        assert_eq!(parsed.cache.root, Some(dir.join("cache")));
        let paths = parsed
            .sources
            .first()
            .expect("the fixture source")
            .local
            .as_ref()
            .expect("local")
            .paths
            .clone();
        assert_eq!(paths, vec![dir.join("pictures").display().to_string()]);
    }

    #[test]
    fn the_sniff_names_each_format_and_its_dimensions() {
        assert_eq!(
            sniff(&png(1600, 900)),
            Some(Header {
                ext: "png",
                width: 1600,
                height: 900
            })
        );
        assert_eq!(
            sniff(&jpeg(1920, 1080)),
            Some(Header {
                ext: "jpg",
                width: 1920,
                height: 1080
            })
        );
        assert_eq!(
            sniff(&webp_lossless(800, 600)),
            Some(Header {
                ext: "webp",
                width: 800,
                height: 600
            })
        );
        assert_eq!(
            sniff(&webp_extended(1024, 768)),
            Some(Header {
                ext: "webp",
                width: 1024,
                height: 768
            })
        );
        assert_eq!(
            sniff(&heic(4032, 3024)),
            Some(Header {
                ext: "heic",
                width: 4032,
                height: 3024
            })
        );
        // Nothing here is an image, and a name is not evidence.
        assert_eq!(sniff(b"<html>not a picture</html>"), None);
        assert_eq!(sniff(&[]), None);
        assert_eq!(sniff(&png(1600, 900)[..12]), None, "half a header");
    }

    /// The HEIC half of the sniff, in the shape this machine's files have: the
    /// first `ispe` in `ipco` is a 1024x1024 thumbnail item and the primary
    /// item's is later. Answering with the first one is not a near miss: 1024x1024
    /// is under the 1600x900 floor of 2.5 step 1, so the file would be dropped by
    /// the *filter* as `rejected_resolution=1`, which `whirl config check` reports
    /// as an honest measurement of a file that is not small at all.
    #[test]
    fn the_heic_sniff_answers_with_the_primary_items_ispe_and_not_the_first_one() {
        assert_eq!(
            sniff(&heic_of((1024, 1024), (6016, 6016), 0)),
            Some(Header {
                ext: "heic",
                width: 6016,
                height: 6016
            }),
            "the thumbnail is the first ispe in the file and the second is the image"
        );
        assert_eq!(
            sniff(&heic_of((8000, 8000), (4032, 3024), 0)),
            Some(Header {
                ext: "heic",
                width: 4032,
                height: 3024
            }),
            "and the primary item wins even when the thumbnail is larger: `pitm` \
             and `ipma` are what settle it, not the size of the box"
        );
    }

    /// `mdat` is the coded image, and image data is arbitrary bytes: this one
    /// holds a well-formed `ispe` box, at an offset a scan of the window would
    /// have read. The walk never enters it, so the fake is not an answer, and a
    /// file with `ftyp heic` and no `meta` is not a picture.
    #[test]
    fn a_heic_without_a_meta_box_is_not_measured_by_its_image_data() {
        let mut bytes = ftyp();
        bytes.extend_from_slice(&boxed(b"mdat", &ispe(2048, 1536)));
        assert!(is_heic(&bytes), "the brand is a HEIC brand");
        assert_eq!(
            sniff(&bytes),
            None,
            "a scan of the window would answer 2048x1536 from the coded image"
        );
    }

    /// A file whose `pitm`/`ipma` pair is missing is measured by its largest
    /// `ispe` rather than dropped: the pair is what makes the answer exact, not
    /// what makes it possible. Nothing measured here needs the arm, and a file
    /// that does not name a primary item still has dimensions in it.
    #[test]
    fn a_heic_without_pitm_is_measured_by_its_largest_ispe() {
        let ipco = boxed(b"ipco", &[ispe(800, 600), ispe(1600, 900)].concat());
        let iprp = boxed(b"iprp", &[ipco, ipma(&[(1, 1), (2, 2)])].concat());
        let meta = boxed(b"meta", &[vec![0, 0, 0, 0], iprp].concat());
        assert_eq!(
            sniff(&[ftyp(), meta].concat()),
            Some(Header {
                ext: "heic",
                width: 1600,
                height: 900
            })
        );
    }

    /// The window rule of section 3 step 5, on the file that made this necessary:
    /// a HEIC whose dimensions are past the 1024-byte window asks for the wide
    /// one, and the wide one measures it.
    #[test]
    fn a_heic_past_the_small_window_asks_for_the_wide_one() {
        // 4096 bytes between `pitm` and `iprp` puts the first `ispe` around 4 KiB
        // in, which is the shape of `Mac Blue.heic` (1245), `Sonoma.heic` (2686)
        // and one `sips` wrote (1063), with room to spare.
        let bytes = heic_of((1024, 1024), (6016, 6016), 4096);
        let small = &bytes[..HEAD_WINDOW];
        assert_eq!(
            sniff(small),
            None,
            "the small window cannot see the ispe, and that is where this started"
        );
        assert!(is_heic(small), "but the bytes say what they are");
        assert_eq!(
            head_window(small),
            WIDE_WINDOW,
            "so a head is worth reading further, and this is by how much"
        );
        assert_eq!(
            sniff(&bytes),
            Some(Header {
                ext: "heic",
                width: 6016,
                height: 6016
            }),
            "and the wide window measures it"
        );
    }

    /// The widening is for the two formats whose dimensions can sit past the
    /// small window and no other: everything that answers inside it keeps the
    /// 1 KiB read, which is every PNG and WebP in a library, every JPEG whose
    /// frame header is not behind a large APP segment, and every HEIC whose
    /// `ispe` happens to sit early.
    #[test]
    fn the_window_widens_only_for_a_file_the_small_window_cannot_measure() {
        assert_eq!(head_window(&png(1600, 900)), HEAD_WINDOW);
        assert_eq!(
            head_window(&jpeg(1920, 1080)),
            HEAD_WINDOW,
            "the minimal JFIF fixture answers at byte 30, which is exactly the shape \
             that hid this defect: a library of those is not a library of photographs"
        );
        assert_eq!(head_window(&webp_lossless(800, 600)), HEAD_WINDOW);
        assert_eq!(
            head_window(b"<html>not a picture</html>"),
            HEAD_WINDOW,
            "and a file that is not a picture is not a HEIC or a JPEG either"
        );
        assert_eq!(
            head_window(&heic_of((1024, 1024), (6016, 6016), 4096)[..200]),
            HEAD_WINDOW,
            "a file shorter than the small window has nothing further to read, \
             and the sniff on it is `None` for that reason: there is no ispe to \
             find and no more file to ask"
        );
        // A HEIC *longer* than the small window whose dimensions are inside it:
        // the zeroed padding here is a second `free` box after `mdat`, so the
        // first 1024 bytes hold the whole `meta` box.
        let mut long = heic(4032, 3024);
        long.extend_from_slice(&boxed(b"free", &vec![0u8; 4096]));
        assert!(
            long.len() > HEAD_WINDOW,
            "the window rule needs a long file"
        );
        assert_eq!(
            head_window(&long),
            HEAD_WINDOW,
            "the small window measured it, so there is nothing to widen for"
        );
    }

    /// The defect the eight `wallhaven` files exposed, on a fixture shaped the way
    /// a photograph is: `ffd8`, an APP1/Exif segment of 3.0 KB that declares its
    /// own length, then the frame header. The minimal [`jpeg`] fixture above
    /// begins with an APP0/JFIF segment of 16 bytes and answers at byte 30, which
    /// is why every JPEG already in a cache passed while these did not.
    #[test]
    fn a_photograph_behind_a_large_exif_segment_is_measured() {
        let photo = jpeg_photo(1920, 1080, &exif(3048), &[], 0);
        assert!(
            photo.len() > HEAD_WINDOW,
            "the Exif segment alone outruns the small window: {} bytes",
            photo.len()
        );
        let small = &photo[..HEAD_WINDOW];
        assert_eq!(
            sniff(small),
            None,
            "the small window cannot reach the frame header"
        );
        assert_eq!(
            sniffed(small),
            Sniffed::BeyondWindow,
            "and the walk ran out of head rather than the bytes being something else"
        );
        assert_eq!(
            head_window(small),
            WIDE_WINDOW,
            "so a head is worth reading further, and this is by how much"
        );
        assert_eq!(
            sniff(&photo),
            Some(Header {
                ext: "jpg",
                width: 1920,
                height: 1080
            }),
            "the bytes are a JPEG whose dimensions were only ever a window away"
        );
    }

    /// The same rule with two large pre-frame segments in front of the frame
    /// header, which is what a phone or an editor writes when it stores Exif and
    /// an ICC profile: the walk has to step past both, and neither fits in the
    /// small window.
    #[test]
    fn a_photograph_behind_exif_and_a_large_icc_segment_is_measured() {
        let photo = jpeg_photo(3840, 2160, &exif(2048), &icc(3072), 0);
        let small = &photo[..HEAD_WINDOW];
        assert_eq!(sniff(small), None, "APP1 alone is past the small window");
        assert_eq!(head_window(small), WIDE_WINDOW);
        assert_eq!(
            sniff(&photo),
            Some(Header {
                ext: "jpg",
                width: 3840,
                height: 2160
            }),
            "the walk steps past APP1 and APP2 to the frame header"
        );
    }

    /// The whole stream, not the sniff alone: section 3 step 5 reads a head that
    /// is assembled from the reads of a download, and a photograph is more than
    /// one 64 KiB read. The fixture is a real shape - 3 KB of Exif, the frame
    /// header, then 70 KB of coded image - so the head has to widen mid-stream,
    /// the file is stored under its digest, and the dimensions the old 1 KiB
    /// head could not measure are reported.
    #[test]
    fn a_streamed_photograph_is_sniffed_stored_and_reported_with_its_dimensions() {
        let dir = scratch("worker-streamed-photo");
        let photo = jpeg_photo(1920, 1080, &exif(3048), &[], 70_000);
        assert!(
            photo.len() > 64 * 1024,
            "the fixture needs more than one read of store's 64 KiB buffer: {} bytes",
            photo.len()
        );
        let cache = Cache::at(dir.join("cache"));
        let transport = Bytes::of(&[("pictures/photo.jpg", photo.clone())]);

        let stored = store(&cache, 1, "pictures/photo.jpg", &transport, 200_000)
            .expect("a photograph the small head refused is stored");

        assert_eq!(
            stored.header,
            Header {
                ext: "jpg",
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(stored.bytes, photo.len() as u64);
        assert!(!stored.hit);
        assert_eq!(
            stored.path,
            state::content_path(cache.root(), &protocol::sha256_hex(&photo), "jpg")
        );
        assert!(stored.path.is_file(), "the bytes are published");
        assert_eq!(
            count_files(&state::tmp_dir(cache.root())),
            0,
            "the part file is renamed, not left behind"
        );
    }

    /// A JPEG past even the wide window is a bound and not bad content: the same
    /// fixture one order of magnitude larger, so its APP1/Exif segment (the
    /// largest a JPEG segment can declare, `0xffff`) steps the walk to byte
    /// 65539, three bytes past the 64 KiB window. The bytes are a well-formed
    /// JPEG that a reader with the whole file measures, so the failure must name
    /// the window rather than claim the file is not an image.
    #[test]
    fn a_head_that_runs_out_mid_walk_names_the_window_and_not_bad_content() {
        let dir = scratch("worker-head-window");
        let photo = jpeg_photo(1920, 1080, &exif(65_533), &[], 4_000);
        assert!(photo.len() > WIDE_WINDOW);
        assert_eq!(
            sniff(&photo),
            Some(Header {
                ext: "jpg",
                width: 1920,
                height: 1080
            }),
            "the file is a JPEG and only the window is short"
        );
        let cache = Cache::at(dir.join("cache"));
        let transport = Bytes::of(&[("pictures/deep.jpg", photo.clone())]);

        let failure = store(&cache, 1, "pictures/deep.jpg", &transport, 200_000)
            .expect_err("the wide window cannot reach the frame header");

        assert_eq!(
            failure.code,
            ErrorCode::NotAnImage,
            "2.7 has one code for a sniff that read no header; the message tells the two apart"
        );
        assert_eq!(failure.stage, "download");
        assert!(
            failure.message.contains("65536-byte head window"),
            "the message names the bound: {}",
            failure.message
        );
        assert!(
            !failure.message.contains("not JPEG, PNG, WebP or HEIC"),
            "the bytes are a JPEG, and the message does not say otherwise: {}",
            failure.message
        );
        assert_eq!(count_files(&cache.root().join("sha256")), 0);
        assert_eq!(
            count_files(&state::tmp_dir(cache.root())),
            0,
            "the part file is deleted on this path too"
        );
    }

    /// The same bytes, both callers of the sniff. `sources/local.rs` admits a
    /// file it can measure and `store` measures what it downloaded, and both ask
    /// [`head_window`] how much of a head is worth reading, so one verdict has to
    /// come back for one file: a file this source admits is a file the download
    /// path accepts, and one it drops is one the download path refuses.
    ///
    /// The fixture is the shape that made the disagreement visible in review:
    /// `ffd8`, a 4 KB APP1/Exif segment, an APP2/ICC segment, then the frame
    /// header at byte 4032 - past the 1 KiB small window and well inside the
    /// 64 KiB one. `whirl-worker --verb set --target <this file>` took it while
    /// the same bytes over the wire came back `not_an_image`, because the small
    /// window was the only one the download path ever asked for. Both accept now,
    /// and neither refuses a head the other reads.
    #[test]
    fn a_file_admitted_locally_and_the_same_bytes_fetched_get_the_same_verdict() {
        let photo = jpeg_photo(1920, 1080, &exif(3998), &icc(24), 0);
        assert!(
            photo.len() > HEAD_WINDOW,
            "the fixture has to outrun the small window: {} bytes",
            photo.len()
        );
        let dir = scratch("worker-two-callers");
        let path = dir.join("photo.jpg");
        std::fs::write(&path, &photo).expect("the fixture file");

        // The local source's own admission: what decides whether the file is a
        // candidate at all (`local::candidate`).
        let local = crate::sources::local::header_of(&path);
        // The download path, handed the same bytes over a transport.
        let cache = Cache::at(dir.join("cache"));
        let transport = Bytes::of(&[("photo.jpg", photo.clone())]);
        let wire = store(&cache, 1, "photo.jpg", &transport, 200_000)
            .expect("the same bytes the local source admits are stored, not refused");

        assert_eq!(
            local,
            Some(Header {
                ext: "jpg",
                width: 1920,
                height: 1080
            }),
            "the local source reads the frame header behind the Exif segment"
        );
        assert_eq!(
            wire.header,
            Header {
                ext: "jpg",
                width: 1920,
                height: 1080
            },
            "and the wire path measures the same frame header"
        );
        assert_eq!(
            local.as_ref(),
            Some(&wire.header),
            "one function answers both callers"
        );
    }

    /// A box's size is the file's own number, and the walk adds it to where it
    /// is, so it is bounded before it is used. This pins the two sizes that
    /// broke that: `u64::MAX`, which panicked the debug build at the bound
    /// (`attempt to add with overflow`) and in release wrapped to a small number;
    /// and `2**64 - 20`, which wraps to exactly `0` - the walk steps back to the
    /// box it just read and re-reads it forever, in the shipped profile.
    ///
    /// A reader that stops is the rule (features.md 2.2: a header that cannot be
    /// read never becomes a candidate); a reader that dies or spins is not a
    /// rule at all.
    #[test]
    fn a_box_size_that_would_overflow_the_walk_stops_it_instead() {
        // The 64-byte shape of the card's repro: `ftyp` (20 bytes), then a box
        // whose 32-bit size is `1` and whose `largesize` is the whole of the
        // arithmetic. 20 + 0xffff_ffff_ffff_ffec is 0 modulo 2**64.
        for largesize in [u64::MAX, 0xffff_ffff_ffff_ffec] {
            let bytes = [ftyp(), boxed_large(b"junk", largesize, &[0u8; 8])].concat();
            let found = boxes(&bytes, 0, bytes.len());
            assert_eq!(
                found.len(),
                1,
                "the walk reads the ftyp box and stops at the one it cannot bound \
                 (largesize {largesize:#018x})"
            );
            assert_eq!(found[0].kind, *b"ftyp");
            assert_eq!(found[0].end, 20);
            assert_eq!(
                sniff(&bytes),
                None,
                "and a file whose header cannot be read is not measured, rather \
                 than measured as 0x0"
            );
        }
    }

    /// The bound is on the arithmetic and not on the field: a `largesize` that
    /// fits is still read and still skipped by, so a file that uses the 64-bit
    /// form for an ordinary reason is read exactly as before.
    #[test]
    fn a_box_size_that_fits_is_still_read_from_largesize() {
        let bytes = [
            ftyp(),
            boxed_large(b"free", 32, &[0u8; 16]),
            boxed(b"mdat", &[]),
        ]
        .concat();
        let kinds: Vec<[u8; 4]> = boxes(&bytes, 0, bytes.len())
            .into_iter()
            .map(|found| found.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![*b"ftyp", *b"free", *b"mdat"],
            "a 64-bit size of 32 skips its own 16-byte header and 16 payload bytes"
        );
    }

    // -- the rotation -------------------------------------------------------

    #[test]
    fn a_rotation_publishes_under_the_digest_and_reports_the_record() {
        let dir = scratch("worker-rotate");
        let config = copy_config(&dir);
        let bytes = long_png(1600, 900, 196);
        let sources = table(
            &config,
            Fixture::with(vec![candidate(
                "one",
                "pictures/one.png",
                1600,
                900,
                bytes.len() as u64,
            )]),
        );
        let transport = Bytes::of(&[("pictures/one.png", bytes.clone())]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let report = run.rotate().expect("the rotation sets the file");

        assert_eq!(report.digest, protocol::sha256_hex(&bytes));
        assert_eq!(report.origin_key, "pictures:one");
        assert_eq!(
            report.path,
            state::content_path(cache.root(), &report.digest, "png")
                .display()
                .to_string(),
            "the extension is the sniffed one, never the URL's"
        );
        assert_eq!(
            fs::read(&report.path).expect("the bytes are published"),
            bytes
        );
        assert_eq!(count_files(&state::tmp_dir(cache.root())), 0);
        // The record the daemon reads, through the daemon's own parser.
        assert_eq!(
            protocol::parse_worker_set_line(&report.set_line()),
            Some((
                report.digest.clone(),
                report.origin_key.clone(),
                report.path.clone()
            )),
            "{}",
            report.set_line()
        );
        assert!(report.downloaded_line().starts_with("downloaded: "));
    }

    /// A setter that remembers what it was given, so a test can assert the path
    /// the platform was handed rather than only the path a report names. The
    /// noop backend is enough for every other test in this module; the two
    /// below are the ones where the path itself is the claim.
    #[derive(Default)]
    struct Recorder(RefCell<Vec<String>>);

    impl Recorder {
        fn targets(&self) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    impl Setter for Recorder {
        fn set(&self, path: &str) -> Result<(), SetError> {
            self.0.borrow_mut().push(path.to_string());
            Ok(())
        }
    }

    /// features.md 2.2's `mode` at its printed default, which the fixture config
    /// gets by writing no `mode` at all: the platform is set from the
    /// candidate's own path and the cache gains nothing.
    ///
    /// The claims, in the order the row makes them. The path the setter is given
    /// is the candidate's `origin` and not a cache file, which is "sets the
    /// wallpaper from the original path". `cache/sha256/` holds what it held
    /// before, which is nothing, and `tmp/` holds nothing either: "zero extra
    /// disk". The digest is the one hash of that file, because the `set:` line's
    /// first field is a digest by the wire grammar and the daemon reads the line
    /// back (the assertion below is the daemon's own parser).
    ///
    /// features.md 105's pinning consequence is pinned here, and this is what it
    /// comes to inside the worker: a pin is a digest, the digest is the cache
    /// filename (state-and-cache 6.2), and a rotation that admits nothing to the
    /// cache leaves a pin with nothing to name. The `copy` test below is the
    /// other side of that sentence, where there is exactly one file to name.
    ///
    /// Neuter the mode branch in `Run::fill` and this test goes red on the first
    /// assertion: the setter is handed a cache file and `sha256/` holds one.
    #[test]
    fn a_reference_mode_rotation_sets_the_original_path_and_stores_nothing() {
        let dir = scratch("worker-reference");
        let config = config(&dir, "");
        assert_eq!(
            config.sources[0]
                .local
                .as_ref()
                .expect("the local section")
                .mode,
            LocalMode::Reference,
            "the fixture writes no `mode`, so this is the default the config gives"
        );
        let bytes = long_png(1600, 900, 196);
        let sources = table(
            &config,
            Fixture::with(vec![candidate(
                "one",
                "pictures/one.png",
                1600,
                900,
                bytes.len() as u64,
            )]),
        );
        let transport = Bytes::of(&[("pictures/one.png", bytes.clone())]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = Recorder::default();
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let report = run.rotate().expect("the rotation sets the file");

        assert_eq!(
            setter.targets(),
            vec!["pictures/one.png".to_string()],
            "the platform is given the candidate's own path, not a copy of it"
        );
        assert_eq!(report.path, "pictures/one.png", "{}", report.set_line());
        assert_eq!(report.origin_key, "pictures:one");
        assert_eq!(
            report.digest,
            protocol::sha256_hex(&bytes),
            "the set: line's first field is the hash of the file that was set"
        );
        assert_eq!(
            count_files(&cache.root().join("sha256")),
            0,
            "nothing entered the cache, so no file exists for a pin to name"
        );
        assert_eq!(
            count_files(&state::tmp_dir(cache.root())),
            0,
            "and no part file was created on the way"
        );
        assert_eq!(
            protocol::parse_worker_set_line(&report.set_line()),
            Some((
                report.digest.clone(),
                report.origin_key.clone(),
                report.path.clone()
            )),
            "the daemon reads the line back, user's path and all: {}",
            report.set_line()
        );
    }

    /// features.md 2.2's `copy`, one arm over from the test above and the reason
    /// the branch has two: "`copy` copies into the cache first", so the platform
    /// is given the cache file and the cache holds exactly one entry, which is
    /// the file a pin would name. Everything else about the rotation is
    /// unchanged, and this test is what says so: the same fixture, the same
    /// candidate, the other mode.
    ///
    /// `mode` is written into the config body rather than set on the parsed
    /// struct, because what the pipeline reads is the `local` section of the
    /// config it was handed. `whirl-core`'s own parse of the key is its own
    /// test (`"mode": "move"` is refused at `sources[0].mode`,
    /// crates/whirl-core/src/config.rs).
    ///
    /// Make the branch reference everything and this test goes red on the
    /// cache-file assertion: `sha256/` is empty and the setter was handed the
    /// candidate's origin.
    #[test]
    fn a_copy_mode_rotation_stores_the_bytes_and_sets_the_cache_file() {
        let dir = scratch("worker-copy");
        let config = config_with(&dir, "", ", \"mode\": \"copy\"");
        assert_eq!(
            config.sources[0]
                .local
                .as_ref()
                .expect("the local section")
                .mode,
            LocalMode::Copy,
            "the key in the body is the key the rotation reads"
        );
        let bytes = long_png(1600, 900, 196);
        let sources = table(
            &config,
            Fixture::with(vec![candidate(
                "one",
                "pictures/one.png",
                1600,
                900,
                bytes.len() as u64,
            )]),
        );
        let transport = Bytes::of(&[("pictures/one.png", bytes.clone())]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = Recorder::default();
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let report = run.rotate().expect("the rotation sets the file");

        assert_eq!(
            report.digest,
            protocol::sha256_hex(&bytes),
            "the digest of the bytes, as it was before the mode was read"
        );
        assert_eq!(
            report.path,
            state::content_path(cache.root(), &report.digest, "png")
                .display()
                .to_string(),
            "the platform is given the cache file, not the user's own"
        );
        assert_eq!(
            setter.targets(),
            vec![report.path.clone()],
            "and that path is what the setter received"
        );
        assert_eq!(
            fs::read(&report.path).expect("the bytes are published"),
            bytes
        );
        assert_eq!(
            count_files(&cache.root().join("sha256")),
            1,
            "one entry, which is the file a pin names"
        );
        assert_eq!(count_files(&state::tmp_dir(cache.root())), 0);
    }

    /// features.md 2.5 hands the recent window to the source, so a source can see
    /// what the last rotations used before it lists anything.
    #[test]
    fn the_recent_window_reaches_the_source() {
        let dir = scratch("worker-context");
        let config = config(&dir, "");
        let fixture = Fixture::with(vec![candidate("one", "pictures/one.png", 1600, 900, 100)]);
        let seen = fixture.handle();
        let sources = table(&config, fixture);
        let transport = Bytes::of(&[]);
        let cache = Cache::at(dir.join("cache"));
        let window = Window::of(vec!["pictures:known".to_string()]);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let _ = run.rotate();

        assert_eq!(
            *seen.borrow(),
            vec!["pictures:known".to_string()],
            "the source is handed the window rather than left to guess"
        );
    }

    /// 4.1's recent window across rotations, which is what nothing tested: the
    /// window is the whole history ring plus the cache index, and its purpose is
    /// that the image just set is not offered again while an unused candidate
    /// remains. A unit test of `stage_recent` cannot see this, because it hands
    /// itself a window; this one builds the window from a real state directory
    /// with the real [`Window::load`], the way the worker does at start-up, and
    /// records each rotation the way the daemon does (6.2: newest first, with
    /// the digest and the `origin_key` the worker reported).
    ///
    /// The two candidate ids differ and so do their bytes, so neither level of
    /// the dedupe can be what moves the second rotation to the other candidate:
    /// only the window can.
    ///
    /// The third rotation is the exhaustion case of 4.1, and its policy is not
    /// invented here. 4.1's rule is that every candidate in the window is
    /// dropped; architecture.md 1711's row 3 makes "everything filtered out" a
    /// `no_candidates` rotation, with `config check` as the diagnostic that
    /// names the stage that removed them; and architecture.md 655 defines the
    /// code as "every configured source yielded nothing admissible". So a window
    /// that holds every candidate means nothing is set and the slot is spent,
    /// not that the window is relaxed until something matches.
    #[test]
    fn consecutive_rotations_do_not_repeat_while_a_candidate_is_unused() {
        struct Recording {
            calls: Rc<RefCell<Vec<String>>>,
        }
        impl Setter for Recording {
            fn set(&self, path: &str) -> Result<(), SetError> {
                self.calls.borrow_mut().push(path.to_string());
                Ok(())
            }
        }

        let dir = scratch("worker-window-rotations");
        // `copy`, not the fixture's default: the last assertion is about what the
        // two rotations stored, and features.md 2.2's default is `reference`,
        // which stores nothing (see `copy_config`'s own note).
        let config = copy_config(&dir);
        let one = long_png(1600, 900, 32);
        let two = long_png(1600, 901, 32);
        let sources = table(
            &config,
            Fixture::with(vec![
                candidate("one", "pictures/one.png", 1600, 900, one.len() as u64),
                candidate("two", "pictures/two.png", 1600, 901, two.len() as u64),
            ]),
        );
        let transport = Bytes::of(&[("pictures/one.png", one), ("pictures/two.png", two)]);
        let cache = Cache::at(dir.join("cache"));
        let state_dir = dir.join("state");
        let setter = Recording {
            calls: Rc::new(RefCell::new(Vec::new())),
        };
        // The daemon's own encoder, in `whirl-core` beside the schema, so the
        // file this test writes is the file `Window::load` really reads.
        let write_history = |entries: &[HistoryEntry]| {
            fs::create_dir_all(&state_dir).expect("the state directory");
            let file = HistoryFile {
                seq: entries.len() as u64 + 1,
                written_at: "2026-09-26T00:00:00Z".to_string(),
                entries: entries.to_vec(),
            };
            fs::write(state_dir.join(state::HISTORY_FILE), file.encode())
                .expect("history.json is written");
        };
        let entry = |report: &Report| HistoryEntry {
            set_at: "2026-09-26T00:00:00Z".to_string(),
            via: Via::Source,
            kind: Kind::Local,
            origin_key: report.origin_key.clone(),
            digest: Some(report.digest.clone()),
            path: Some(report.path.clone()),
        };

        // Rotation 1: nothing has been set, so the window is empty and the
        // first candidate is admissible.
        let window = Window::load(&state_dir, &cache.index_path(), 50);
        assert!(
            !window.contains("pictures:one") && !window.contains("pictures:two"),
            "an empty state directory is an empty window"
        );
        let first = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 1)
            .rotate()
            .expect("the first rotation sets a candidate");
        assert_eq!(first.origin_key, "pictures:one");
        write_history(&[entry(&first)]);

        // Rotation 2: the window holds the image just set, so the same candidate
        // must not come back while the other one is unused.
        let window = Window::load(&state_dir, &cache.index_path(), 50);
        assert!(
            window.contains("pictures:one"),
            "the window must hold the image just set: {} is not in it",
            first.origin_key
        );
        let second = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 2)
            .rotate()
            .expect("the second rotation sets the other candidate");
        assert_eq!(second.origin_key, "pictures:two");
        assert_ne!(
            second.origin_key, first.origin_key,
            "the window must keep the image just set out of the next rotation"
        );
        write_history(&[entry(&second), entry(&first)]);

        // Rotation 3: the window now holds every candidate. 4.1's rule leaves
        // nothing admissible, and 1711's row 3 is the shape of the answer:
        // `no_candidates`, nothing set, the slot spent by the caller.
        let window = Window::load(&state_dir, &cache.index_path(), 50);
        assert!(
            window.contains("pictures:one") && window.contains("pictures:two"),
            "the window must hold every candidate that has been set"
        );
        match noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 3).rotate() {
            Err(failure) => assert_eq!(failure.code, ErrorCode::NoCandidates),
            Ok(report) => panic!(
                "an exhausted window must set nothing, not repeat: {}",
                report.set_line()
            ),
        }
        assert_eq!(
            setter.calls.borrow().len(),
            2,
            "the third rotation set nothing: the two calls are the two rotations before it"
        );
        assert_eq!(count_files(&cache.root().join("sha256")), 2);
    }

    // -- what the index is asked before a fetch (4.1) -----------------------

    /// A [`Bytes`] that also records every origin it was asked for: the
    /// accounting behind "the second rotation fetches zero bytes for that id".
    struct Counting {
        bodies: HashMap<String, Vec<u8>>,
        opened: Rc<RefCell<Vec<String>>>,
    }

    impl Counting {
        fn of(pairs: &[(&str, Vec<u8>)]) -> Counting {
            Counting {
                bodies: pairs
                    .iter()
                    .map(|(origin, bytes)| (origin.to_string(), bytes.clone()))
                    .collect(),
                opened: Rc::new(RefCell::new(Vec::new())),
            }
        }

        /// The origins this transport was asked to open, in order.
        fn opened(&self) -> Vec<String> {
            self.opened.borrow().clone()
        }
    }

    impl Transport for Counting {
        fn open(&self, origin: &str) -> Result<Box<dyn Read>, String> {
            self.opened.borrow_mut().push(origin.to_string());
            match self.bodies.get(origin) {
                Some(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
                None => Err(format!("{origin}: no such file")),
            }
        }
    }

    /// The daemon's own record for a rotation (`whirl_core::state`), so the file
    /// a test writes is the file [`Window::load`] really reads.
    fn history_entry(report: &Report) -> HistoryEntry {
        HistoryEntry {
            set_at: "2026-09-26T00:00:00Z".to_string(),
            via: Via::Source,
            kind: Kind::Local,
            origin_key: report.origin_key.clone(),
            digest: Some(report.digest.clone()),
            path: Some(report.path.clone()),
        }
    }

    /// `history.json`, written the way the daemon writes it (6.2: newest first).
    fn write_history_at(state_dir: &Path, entries: &[HistoryEntry]) {
        fs::create_dir_all(state_dir).expect("the state directory");
        let file = HistoryFile {
            seq: entries.len() as u64 + 1,
            written_at: "2026-09-26T00:00:00Z".to_string(),
            entries: entries.to_vec(),
        };
        fs::write(state_dir.join(state::HISTORY_FILE), file.encode())
            .expect("history.json is written");
    }

    /// `cache/index.json`, in the shape `whirld::cache::record` writes: the digest
    /// is the key, and the entry names the `origin_key` and the extension the
    /// file's own name carries. The other fields are the daemon's and the
    /// pipeline reads none of them.
    fn write_index_at(cache: &Cache, entries: &[(&Report, &str)]) {
        let mut index = IndexFile {
            seq: 1,
            written_at: "2026-09-26T00:00:00Z".to_string(),
            root_id: "test-root".to_string(),
            entries: BTreeMap::new(),
            dangling: Vec::new(),
        };
        for (report, ext) in entries {
            index.entries.insert(
                report.digest.clone(),
                CacheIndexEntry {
                    ext: ext.to_string(),
                    bytes: 1,
                    first_seen: "2026-09-26T00:00:00Z".to_string(),
                    last_used: "2026-09-26T00:00:00Z".to_string(),
                    source: "pictures".to_string(),
                    kind: Kind::Local,
                    origin: None,
                    origin_key: report.origin_key.clone(),
                    width: None,
                    height: None,
                    pinned: false,
                },
            );
        }
        fs::write(cache.index_path(), index.encode()).expect("the index is written");
    }

    /// A candidate whose image the cache already holds is taken from the cache,
    /// with no request to the source (4.1).
    ///
    /// Three rotations over a listing of two ids, with the window at
    /// `dedupe.recent_entries = 1` so that an id comes round again: the first two
    /// fetch, because nothing is held; the third offers the id the first one set,
    /// finds it in the index under a file that is still there, and opens no
    /// origin at all. The counting transport is the accounting, and the history
    /// and the index are written between the runs exactly as the daemon writes
    /// them (6.2), from the reports the rotations produced.
    #[test]
    fn a_candidate_the_cache_already_holds_is_set_without_a_fetch() {
        let dir = scratch("worker-held");
        let config = copy_config(&dir);
        let one = long_png(1600, 900, 32);
        let two = long_png(1600, 901, 32);
        let sources = table(
            &config,
            Fixture::with(vec![
                candidate("one", "pictures/one.png", 1600, 900, one.len() as u64),
                candidate("two", "pictures/two.png", 1600, 901, two.len() as u64),
            ]),
        );
        let transport = Counting::of(&[
            ("pictures/one.png", one.clone()),
            ("pictures/two.png", two.clone()),
        ]);
        let cache = Cache::at(dir.join("cache"));
        let state_dir = dir.join("state");
        let setter = Recorder::default();

        // Rotation 1: nothing is held, so the first candidate is fetched.
        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let first = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 1)
            .rotate()
            .expect("the first rotation fetches");
        assert_eq!(first.origin_key, "pictures:one");
        assert_eq!(transport.opened(), vec!["pictures/one.png".to_string()]);
        write_history_at(&state_dir, &[history_entry(&first)]);
        write_index_at(&cache, &[(&first, "png")]);

        // Rotation 2: the id just set is in the window, so the other candidate is
        // fetched. The window is small enough that the first id comes round on the
        // very next rotation.
        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let second = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 2)
            .rotate()
            .expect("the second rotation fetches the other candidate");
        assert_eq!(second.origin_key, "pictures:two");
        assert_eq!(
            transport.opened(),
            vec![
                "pictures/one.png".to_string(),
                "pictures/two.png".to_string()
            ]
        );
        write_history_at(&state_dir, &[history_entry(&second), history_entry(&first)]);
        write_index_at(&cache, &[(&first, "png"), (&second, "png")]);

        // Rotation 3: the window holds only the second id, so the first is offered
        // again. It is in the index, under a file that is still there, so it is
        // set from the cache and the transport is asked for nothing.
        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let third = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 3)
            .rotate()
            .expect("the third rotation serves the first candidate from the cache");
        assert_eq!(third.origin_key, "pictures:one");
        assert_eq!(third.digest, first.digest);
        assert_eq!(third.path, first.path);
        assert_eq!(
            fs::read(&first.path).expect("the cached file is readable"),
            one,
            "the bytes served from the cache are the origin's own"
        );
        assert_eq!(
            transport.opened(),
            vec![
                "pictures/one.png".to_string(),
                "pictures/two.png".to_string()
            ],
            "the third rotation fetched nothing: the index was consulted first"
        );
        assert_eq!(
            count_files(&state::tmp_dir(cache.root())),
            0,
            "a candidate served from the cache has no part file"
        );
        assert_eq!(
            setter.targets(),
            vec![first.path.clone(), second.path.clone(), first.path.clone()],
            "every rotation set something, the last one from the cache"
        );
    }

    /// The same id, whose cached file has been deleted, is fetched again: the
    /// `stat` is the rotation's decision, so a file the index lists but the disk
    /// does not have is a miss like any other (4.1).
    #[test]
    fn a_held_candidate_whose_file_is_gone_is_fetched_again() {
        let dir = scratch("worker-held-gone");
        let config = copy_config(&dir);
        let one = long_png(1600, 900, 32);
        let two = long_png(1600, 901, 32);
        let sources = table(
            &config,
            Fixture::with(vec![
                candidate("one", "pictures/one.png", 1600, 900, one.len() as u64),
                candidate("two", "pictures/two.png", 1600, 901, two.len() as u64),
            ]),
        );
        let transport = Counting::of(&[
            ("pictures/one.png", one.clone()),
            ("pictures/two.png", two.clone()),
        ]);
        let cache = Cache::at(dir.join("cache"));
        let state_dir = dir.join("state");
        let setter = Recorder::default();

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let first = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 1)
            .rotate()
            .expect("the first rotation fetches");
        write_history_at(&state_dir, &[history_entry(&first)]);
        write_index_at(&cache, &[(&first, "png")]);

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let second = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 2)
            .rotate()
            .expect("the second rotation fetches the other candidate");
        write_history_at(&state_dir, &[history_entry(&second), history_entry(&first)]);
        write_index_at(&cache, &[(&first, "png"), (&second, "png")]);

        // The index still names the first file; the disk no longer has it.
        let stored = state::content_path(cache.root(), &first.digest, "png");
        fs::remove_file(&stored).expect("the cached file is removed");
        assert!(!stored.exists(), "the fixture starts with the file gone");

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let third = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 3)
            .rotate()
            .expect("the rotation fetches the candidate again");
        assert_eq!(third.origin_key, "pictures:one");
        assert_eq!(third.digest, first.digest);
        assert_eq!(
            transport.opened(),
            vec![
                "pictures/one.png".to_string(),
                "pictures/two.png".to_string(),
                "pictures/one.png".to_string(),
            ],
            "a listed file that is gone is fetched again, not served"
        );
        assert!(
            stored.is_file(),
            "the bytes are back under their own content-addressed name"
        );
    }

    /// The same id, whose cached file is still there under its own name but no
    /// longer holds the bytes that name spells, is fetched again: the hit is the
    /// file's contents matching the index's digest, not the file's existence
    /// (4.1, and section 2's third reason for content-addressed names).
    ///
    /// The bytes that arrive replace the damaged copy under the same name, so
    /// the setter is handed the image the name promises rather than whatever was
    /// sitting under it.
    #[test]
    fn a_held_candidate_whose_file_was_replaced_is_fetched_again() {
        let dir = scratch("worker-held-replaced");
        let config = copy_config(&dir);
        let one = long_png(1600, 900, 32);
        let two = long_png(1600, 901, 32);
        let sources = table(
            &config,
            Fixture::with(vec![
                candidate("one", "pictures/one.png", 1600, 900, one.len() as u64),
                candidate("two", "pictures/two.png", 1600, 901, two.len() as u64),
            ]),
        );
        let transport = Counting::of(&[
            ("pictures/one.png", one.clone()),
            ("pictures/two.png", two.clone()),
        ]);
        let cache = Cache::at(dir.join("cache"));
        let state_dir = dir.join("state");
        let setter = Recorder::default();

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let first = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 1)
            .rotate()
            .expect("the first rotation fetches");
        write_history_at(&state_dir, &[history_entry(&first)]);
        write_index_at(&cache, &[(&first, "png")]);

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let second = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 2)
            .rotate()
            .expect("the second rotation fetches the other candidate");
        write_history_at(&state_dir, &[history_entry(&second), history_entry(&first)]);
        write_index_at(&cache, &[(&first, "png"), (&second, "png")]);

        // The index still names the first file and the file is still there under
        // that name, but something replaced its bytes in place: another image,
        // same name. It no longer hashes to what the name spells.
        let stored = state::content_path(cache.root(), &first.digest, "png");
        let tampered = long_png(1600, 902, 48);
        assert_ne!(
            protocol::sha256_hex(&tampered),
            first.digest,
            "the replacement must hash to a different name for the case to bite"
        );
        fs::write(&stored, &tampered).expect("the cached file is replaced in place");

        // The consultation is the file's bytes, so this is a miss even though the
        // `stat` succeeds.
        let window = Window::load(&state_dir, &cache.index_path(), 1);
        assert!(
            window.cached(&cache, "pictures:one").is_none(),
            "a file whose bytes do not hash to its name is not a hit"
        );

        let third = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 3)
            .rotate()
            .expect("the rotation fetches the candidate again");
        assert_eq!(third.origin_key, "pictures:one");
        assert_eq!(third.digest, first.digest);
        assert_eq!(third.path, first.path);
        assert_eq!(
            transport.opened(),
            vec![
                "pictures/one.png".to_string(),
                "pictures/two.png".to_string(),
                "pictures/one.png".to_string(),
            ],
            "a file whose bytes have moved off its name is fetched again, not served"
        );
        assert_eq!(
            fs::read(&stored).expect("the cache file is readable"),
            one,
            "the bytes that arrived replaced the damaged copy under the same name"
        );
        assert_eq!(
            setter.targets().last().expect("the third rotation set"),
            &stored.display().to_string(),
            "the setter was handed the repaired file, not the damaged one"
        );
    }

    /// The same id, whose index entry the sweep has evicted, is fetched again:
    /// the index is the service map, and an id it no longer names has nothing to
    /// serve however healthy the file for it may be (4.1).
    #[test]
    fn a_swept_entry_is_fetched_again() {
        let dir = scratch("worker-entry-swept");
        let config = copy_config(&dir);
        let one = long_png(1600, 900, 32);
        let two = long_png(1600, 901, 32);
        let sources = table(
            &config,
            Fixture::with(vec![
                candidate("one", "pictures/one.png", 1600, 900, one.len() as u64),
                candidate("two", "pictures/two.png", 1600, 901, two.len() as u64),
            ]),
        );
        let transport = Counting::of(&[
            ("pictures/one.png", one.clone()),
            ("pictures/two.png", two.clone()),
        ]);
        let cache = Cache::at(dir.join("cache"));
        let state_dir = dir.join("state");
        let setter = Recorder::default();

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let first = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 1)
            .rotate()
            .expect("the first rotation fetches");
        write_history_at(&state_dir, &[history_entry(&first)]);
        write_index_at(&cache, &[(&first, "png")]);

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        let second = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 2)
            .rotate()
            .expect("the second rotation fetches the other candidate");
        write_history_at(&state_dir, &[history_entry(&second), history_entry(&first)]);

        // The sweep's eviction write: the index no longer names the first id,
        // while the file it used to describe is untouched on disk.
        write_index_at(&cache, &[(&second, "png")]);
        let stored = state::content_path(cache.root(), &first.digest, "png");
        assert!(
            stored.is_file(),
            "the file the evicted entry described stays"
        );

        let window = Window::load(&state_dir, &cache.index_path(), 1);
        assert!(
            window.cached(&cache, "pictures:one").is_none(),
            "an id the index no longer names has nothing to serve"
        );
        let third = noop_run_of(&config, &sources, &transport, &cache, &window, &setter, 3)
            .rotate()
            .expect("the rotation fetches the candidate again");
        assert_eq!(third.origin_key, "pictures:one");
        assert_eq!(
            transport.opened(),
            vec![
                "pictures/one.png".to_string(),
                "pictures/two.png".to_string(),
                "pictures/one.png".to_string(),
            ],
            "an evicted entry is a miss, and the candidate is fetched"
        );
    }

    /// A rotation still succeeds when the index is absent, unreadable or stale:
    /// the index half of the window comes back empty and every candidate is
    /// fetched, rather than the rotation failing on a file it only reads (7.1).
    #[test]
    fn a_rotation_survives_an_index_it_cannot_read() {
        let dir = scratch("worker-index-unreadable");
        let config = copy_config(&dir);
        let bytes = long_png(1600, 900, 32);
        let sources = table(
            &config,
            Fixture::with(vec![candidate(
                "one",
                "pictures/one.png",
                1600,
                900,
                bytes.len() as u64,
            )]),
        );
        let transport = Counting::of(&[("pictures/one.png", bytes.clone())]);
        let cache = Cache::at(dir.join("cache"));
        let state_dir = dir.join("state");
        let setter = Recorder::default();
        // The window is empty for the whole test - `dedupe.recent_entries` at 0 -
        // so the candidate is offered every time and the only half under test is
        // the index.
        let window = || Window::load(&state_dir, &cache.index_path(), 0);

        // Absent: the common case, and an empty index rather than an error.
        assert!(!cache.index_path().exists());
        assert!(window().cached(&cache, "pictures:one").is_none());
        let first = noop_run_of(&config, &sources, &transport, &cache, &window(), &setter, 1)
            .rotate()
            .expect("an absent index does not fail a rotation");
        assert_eq!(transport.opened(), vec!["pictures/one.png".to_string()]);

        // Unreadable: text the schema cannot parse. The file is read, warned
        // about and ignored, and the candidate is fetched like a miss.
        fs::create_dir_all(cache.root()).expect("the cache root");
        fs::write(cache.index_path(), "{ not the index at all\n").expect("a broken index");
        assert!(window().cached(&cache, "pictures:one").is_none());
        let second = noop_run_of(&config, &sources, &transport, &cache, &window(), &setter, 2)
            .rotate()
            .expect("an unreadable index does not fail a rotation");
        assert_eq!(
            transport.opened(),
            vec![
                "pictures/one.png".to_string(),
                "pictures/one.png".to_string()
            ],
            "an unreadable index serves nothing, so the bytes are fetched"
        );
        assert_eq!(second.digest, first.digest);

        // A newer schema is a downgrade rather than corruption (6.4 step 4): it
        // is left alone, serves nothing, and the rotation still fetches.
        fs::write(cache.index_path(), "{\n  \"schema\": 2\n}\n").expect("a newer index");
        assert!(window().cached(&cache, "pictures:one").is_none());
        let third = noop_run_of(&config, &sources, &transport, &cache, &window(), &setter, 3)
            .rotate()
            .expect("a newer index does not fail a rotation");
        assert_eq!(
            transport.opened().len(),
            3,
            "three rotations, three fetches"
        );
        assert_eq!(third.digest, first.digest);
        assert_eq!(
            fs::read_to_string(cache.index_path()).expect("the newer index"),
            "{\n  \"schema\": 2\n}\n",
            "a newer schema is left exactly as it is"
        );
    }

    /// The invariant of section 3: a partial fetch never becomes a cache file.
    #[test]
    fn the_cap_is_enforced_mid_stream_and_nothing_is_published() {
        let dir = scratch("worker-cap");
        let config = copy_config(&dir);
        let bytes = long_png(1600, 900, 8192);
        // The candidate declares ten bytes; the bytes that arrive are what the
        // cap is enforced against.
        let sources = table(
            &config,
            Fixture::with(vec![candidate("big", "pictures/big.png", 1600, 900, 10)]),
        );
        let transport = Bytes::of(&[("pictures/big.png", bytes)]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let failure = run.rotate().expect_err("the cap rejects it");

        assert_eq!(failure.code, ErrorCode::TooLarge);
        assert_eq!(failure.stage, "download");
        assert_eq!(
            count_files(&cache.root().join("sha256")),
            0,
            "no cache file from a partial download"
        );
        assert_eq!(
            count_files(&state::tmp_dir(cache.root())),
            0,
            "the part file is deleted on the failure path"
        );
    }

    /// features.md 1.4's "No network" item 2 is the end state of the per-candidate
    /// rule: this is the rotation whose *only* candidate failed on its bytes, so
    /// there is nothing left to try and the failure is reported — `offline`, the
    /// code the section names, rather than a `no_candidates` that would send the
    /// operator to the filters. Nothing is published either way.
    #[test]
    fn a_fetch_that_fails_publishes_nothing_and_says_why() {
        let dir = scratch("worker-fetch");
        let config = copy_config(&dir);
        let sources = table(
            &config,
            Fixture::with(vec![candidate("gone", "pictures/gone.png", 1600, 900, 100)]),
        );
        let transport = Bytes::of(&[]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let failure = run.rotate().expect_err("nothing was fetched");

        assert_eq!(failure.code, ErrorCode::Offline);
        assert!(
            failure.message.contains("no such file"),
            "{}",
            failure.message
        );
        assert_eq!(count_files(&state::tmp_dir(cache.root())), 0);
    }

    /// The not-an-image path of section 3 step 5, driven directly rather than
    /// through the code path that shares it: a download whose bytes sniff as
    /// nothing features.md 2.2 defines fails with the code 2.7's table names
    /// (`not_an_image`), and leaves neither a cache entry nor its part file.
    #[test]
    fn a_download_that_is_not_an_image_fails_and_publishes_nothing() {
        let dir = scratch("worker-not-an-image");
        let config = copy_config(&dir);
        let sources = table(
            &config,
            Fixture::with(vec![candidate("text", "pictures/text.png", 1600, 900, 100)]),
        );
        // Plain text behind a `.png` name: the source's extension claims an
        // image, and the header sniff is the thing that decides.
        let transport = Bytes::of(&[(
            "pictures/text.png",
            b"not an image at all, only text\n".to_vec(),
        )]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let failure = run.rotate().expect_err("the header is not an image");

        assert_eq!(failure.code, ErrorCode::NotAnImage);
        assert_eq!(failure.code.as_str(), "not_an_image");
        assert_eq!(failure.stage, "download");
        assert!(
            failure.message.contains("pictures/text.png"),
            "{}",
            failure.message
        );
        assert_eq!(
            count_files(&cache.root().join("sha256")),
            0,
            "no cache entry from a download that is not an image"
        );
        assert_eq!(
            count_files(&state::tmp_dir(cache.root())),
            0,
            "the part file is deleted on the not-an-image path"
        );
    }

    /// Every rejection is counted with the stage name 2.5 fixes, rather than
    /// swallowed: the counter group of 2.6.
    #[test]
    fn each_stage_counts_what_it_removes() {
        let dir = scratch("worker-stages");
        let config = config(&dir, "");
        let candidates = vec![
            seeking("small", "pictures/small.png", 8, 8, 100),
            seeking("square", "pictures/square.png", 1000, 1000, 100),
            seeking("fat", "pictures/fat.png", 1600, 900, 9000),
            seeking("script", "pictures/script.php", 1600, 900, 100),
            seeking("known", "pictures/known.png", 1600, 900, 100),
            seeking("good", "pictures/good.png", 1600, 900, 100),
        ];
        let window = Window::of(vec!["pictures:known".to_string()]);
        let mut config = config;
        config.filters.target_ratio = Some(16.0 / 9.0);

        let filtered = filter_pipeline(&config, Platform::Macos, &window, candidates);

        assert_eq!(
            filtered.counters,
            Counters {
                candidates: 6,
                admitted: 1,
                rejected_resolution: 1,
                rejected_ratio: 1,
                rejected_size: 1,
                rejected_type: 1,
                rejected_dedupe: 1,
            }
        );
        assert_eq!(filtered.kept[0].candidate.id, "good");
        let line = filtered
            .counters
            .record(&config.sources[0], true, None)
            .line();
        assert_eq!(
            line,
            "source: pictures local weight=1 enabled=1 last=- candidates=6 admitted=1 \
             rejected_resolution=1 rejected_ratio=1 rejected_size=1 rejected_type=1 \
             rejected_dedupe=1 reason=-"
        );
        // The same record through the daemon's own parser, which is what
        // `config check` forwards.
        let parsed = protocol::parse_source_record(&line).expect("the daemon reads it");
        assert_eq!(parsed.counters.len(), 7);
        assert_eq!(parsed.reason, None);
    }

    #[test]
    fn a_source_this_build_cannot_work_with_prints_a_reason_and_no_counter_group() {
        // The second form of 2.6's optional group: `enabled=0` and a reason, with
        // no counter group, because nothing was counted. Every kind of the schema
        // has an arm in the dispatch table now, so the reason is a real refusal
        // from a source itself rather than `missing_reason`. A `wallhaven`
        // `collection` that is not `<username>/<id>` is the refusal that needs no
        // machine, no network and no API key, and reads the same on every
        // platform (a missing `local` path does not: `/x` is absolute on unix and
        // relative on Windows, which is a different refusal). `disabled_record`
        // is still the group's only builder.
        let config = Config::parse(
            "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
             \"wallhaven\", \"weight\": 1, \"collection\": \"just-a-name\" } ]\n}\n",
        )
        .expect("the fixture config parses")
        .config;
        let sources = crate::sources::Sources::from_config(&config);
        let entry = sources.get("space").expect("the kind has an arm");
        let reason = entry
            .source
            .validate(&entry.config)
            .expect_err("the handle is not <username>/<id>")
            .to_string();
        let line = disabled_record(&config.sources[0], reason);
        assert!(
            line.starts_with("source: space wallhaven weight=1 enabled=0 last=- reason="),
            "{line}"
        );
        let parsed = protocol::parse_source_record(&line).expect("the daemon reads it");
        assert!(!parsed.enabled);
        assert!(parsed.counters.is_empty());
        let reason = parsed.reason.expect("a reason");
        assert!(
            reason.contains("just-a-name"),
            "the reason quotes what was configured: {reason}"
        );
        assert!(
            reason.contains("<username>/<id>"),
            "the reason says what the shape had to be: {reason}"
        );
        assert!(
            reason.contains("sources[id=space].collection"),
            "the reason names the offending key (4.3): {reason}"
        );
    }

    #[test]
    fn identical_bytes_under_a_second_origin_are_a_cache_hit() {
        let dir = scratch("worker-hit");
        let cache = Cache::at(dir.join("cache"));
        let bytes = long_png(1600, 900, 64);
        let transport = Bytes::of(&[
            ("pictures/one.png", bytes.clone()),
            ("pictures/two.png", bytes.clone()),
        ]);

        let first = store(&cache, 1, "pictures/one.png", &transport, 4096).expect("stored");
        let second = store(&cache, 2, "pictures/two.png", &transport, 4096).expect("stored");

        assert!(!first.hit);
        assert!(second.hit, "the digest is already in the cache");
        assert_eq!(first.path, second.path);
        assert_eq!(count_files(&cache.root().join("sha256")), 1);
        assert_eq!(count_files(&state::tmp_dir(cache.root())), 0);
    }

    /// features.md 1.4: the candidate is marked bad, one more candidate is tried,
    /// and state-and-cache section 3's table says the cache file stays.
    #[test]
    fn a_refusing_setter_keeps_the_entry_and_tries_one_more_candidate() {
        struct Refusing {
            calls: Rc<RefCell<Vec<String>>>,
        }
        impl Setter for Refusing {
            fn set(&self, path: &str) -> Result<(), SetError> {
                self.calls.borrow_mut().push(path.to_string());
                Err(SetError::new(
                    ErrorCode::SetFailed,
                    "the test setter refuses",
                ))
            }
        }

        let dir = scratch("worker-setter");
        let config = copy_config(&dir);
        let one = long_png(1600, 900, 32);
        let two = long_png(1600, 901, 32);
        let sources = table(
            &config,
            Fixture::with(vec![
                candidate("one", "pictures/one.png", 1600, 900, one.len() as u64),
                candidate("two", "pictures/two.png", 1600, 901, two.len() as u64),
            ]),
        );
        let transport = Bytes::of(&[("pictures/one.png", one), ("pictures/two.png", two)]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let calls = Rc::new(RefCell::new(Vec::new()));
        let setter = Refusing {
            calls: Rc::clone(&calls),
        };
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let failure = run.rotate().expect_err("the setter refuses both");

        assert_eq!(failure.code, ErrorCode::SetFailed);
        assert_eq!(calls.borrow().len(), 2, "one more candidate was tried");
        assert_eq!(
            count_files(&cache.root().join("sha256")),
            2,
            "a file the setter refused stays a cache entry"
        );
        assert_eq!(count_files(&state::tmp_dir(cache.root())), 0);
    }

    /// features.md 1.4's per-candidate rule where the failure is the *download*
    /// rather than the setter: the candidate whose bytes cannot be had is marked
    /// bad and one more candidate is tried, in the same source.
    ///
    /// The defect this pins is the `?` that used to sit here: `Run::rotate`
    /// propagated the first `fill` failure straight out, so a rotation against a
    /// live wallhaven source ended with `stage=download code=offline` on the first
    /// image the CDN would not serve, without ever asking for the candidate behind
    /// it. The second candidate is fetched, published and set here, and the failed
    /// one leaves nothing behind: no part file, no cache entry, and one call to
    /// the setter.
    #[test]
    fn an_unfetchable_candidate_is_marked_bad_and_the_next_one_is_tried() {
        let dir = scratch("worker-candidate-failure");
        let config = copy_config(&dir);
        let bytes = long_png(1600, 901, 32);
        let sources = table(
            &config,
            Fixture::with(vec![
                candidate("gone", "pictures/gone.png", 1600, 900, 100),
                candidate("here", "pictures/here.png", 1600, 901, bytes.len() as u64),
            ]),
        );
        let transport = Bytes::of(&[("pictures/here.png", bytes.clone())]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = Recorder::default();
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let report = run
            .rotate()
            .expect("the candidate behind the missing one is admitted");

        assert_eq!(report.origin_key, "pictures:here");
        assert_eq!(report.digest, protocol::sha256_hex(&bytes));
        assert_eq!(
            setter.targets(),
            vec![report.path.clone()],
            "the setter is handed the candidate that was fetched, once"
        );
        assert_eq!(
            count_files(&cache.root().join("sha256")),
            1,
            "only the fetched candidate is published"
        );
        assert_eq!(
            count_files(&state::tmp_dir(cache.root())),
            0,
            "the failed fetch leaves no part file"
        );
    }

    /// The line `Run::rotate` draws between a failure that belongs to the
    /// candidate and one that belongs to the machine, pinned on the predicate
    /// itself so the policy is stated in one test rather than inferred from four.
    ///
    /// features.md 1.4, "Disk full or unwritable cache": the part file is removed
    /// and "the error is reported" — once, and not per candidate, which is the
    /// whole reason the cache codes below are not in the first list.
    #[test]
    fn only_the_candidates_own_failures_are_candidate_failures() {
        for code in [
            ErrorCode::Offline,
            ErrorCode::NotAnImage,
            ErrorCode::TooLarge,
        ] {
            assert!(
                candidate_failure(&Failure::new("download", code, "test")),
                "{code:?} is the candidate's"
            );
        }
        for code in [
            ErrorCode::CacheUnwritable,
            ErrorCode::CacheReadonly,
            ErrorCode::Enospc,
        ] {
            assert!(
                !candidate_failure(&Failure::new("download", code, "test")),
                "{code:?} is the machine's, and the rotation stops"
            );
        }
    }

    /// The floor of 2.5 step 1 against the header, for a source that claimed a
    /// size it did not mean.
    #[test]
    fn a_file_under_the_floor_is_not_set() {
        let dir = scratch("worker-floor");
        let config = copy_config(&dir);
        let bytes = long_png(8, 8, 8);
        let sources = table(
            &config,
            Fixture::with(vec![candidate("lied", "pictures/lied.png", 1600, 900, 100)]),
        );
        let transport = Bytes::of(&[("pictures/lied.png", bytes)]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let failure = run.rotate().expect_err("nothing admissible");

        assert_eq!(failure.code, ErrorCode::NoCandidates);
        assert!(
            failure.message.contains("under the 16x16 floor"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn a_rotation_with_no_source_says_no_source() {
        let dir = scratch("worker-nosource");
        let config = config(&dir, "");
        let sources = crate::sources::Sources::of(Vec::new());
        let transport = Bytes::of(&[]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);

        let failure = run.rotate().expect_err("there is nothing to ask");

        assert_eq!(failure.code, ErrorCode::NoCandidates);
        assert_eq!(failure.stage, "source");
    }

    // -- the stages on their own --------------------------------------------

    #[test]
    fn the_ratio_stage_treats_the_tolerance_as_a_fraction_of_the_target() {
        let dir = scratch("worker-ratio");
        let config = config(&dir, "");
        let target = 16.0 / 9.0;
        let candidates = vec![
            seeking("sixteen-nine", "a.png", 1600, 900, 100),
            seeking("sixteen-ten", "b.png", 1600, 1000, 100),
            seeking("just-inside", "c.png", 1600, 890, 100),
            seeking("just-outside", "d.png", 1600, 880, 100),
            // The case that separates the two readings, and the reason this test
            // is named after the fractional one: 1600x886 is 1.8059 against
            // 1.7778, an absolute delta of 0.0281 that the tolerance of 0.02
            // would reject, but a fractional delta of 0.0158 that it keeps.
            // Without this candidate `(ratio - target).abs() > tolerance` and
            // `((ratio - target) / target).abs() > tolerance` agree on every
            // other entry, so the mutation survives (66b1fd7a, run 3).
            seeking("in-the-band", "e.png", 1600, 886, 100),
        ];

        let stage = stage_ratio(candidates, &config, Some(target));

        let kept: Vec<&str> = stage
            .kept
            .iter()
            .map(|seeking| seeking.candidate.id.as_str())
            .collect();
        assert_eq!(kept, vec!["sixteen-nine", "just-inside", "in-the-band"]);
        assert_eq!(stage.rejected, 2);
        // `any` is the config default, and then nothing is rejected.
        assert_eq!(stage_ratio(vec![], &config, None).rejected, 0);
    }

    #[test]
    fn the_type_stage_reads_the_platform_and_the_origins_extension() {
        assert!(settable(Platform::Macos, "heic"));
        assert!(!settable(Platform::Windows, "heic"));
        assert!(!settable(Platform::Linux, "heic"));
        assert!(settable(Platform::Linux, "png"));
        assert!(!settable(Platform::Macos, "php"));

        assert_eq!(
            extension_of("https://host/a.JPG?cb=1"),
            Some("jpg".to_string())
        );
        assert_eq!(extension_of("https://host/a.php"), Some("php".to_string()));
        assert_eq!(extension_of("https://host/image?id=3"), None);
        assert_eq!(
            extension_of("/home/me/pics/a.jpeg"),
            Some("jpeg".to_string())
        );

        let candidates = vec![
            seeking("heic", "/pics/a.heic", 1600, 900, 100),
            seeking("script", "/pics/a.php", 1600, 900, 100),
            seeking("unnamed", "https://host/image?id=3", 1600, 900, 100),
        ];
        let mac = stage_type(candidates.clone(), Platform::Macos);
        assert_eq!(mac.kept.len(), 2, "HEIC is settable on macOS");
        assert_eq!(mac.rejected, 1);
        let linux = stage_type(candidates, Platform::Linux);
        assert_eq!(linux.kept.len(), 1, "HEIC is skipped on Linux");
        assert_eq!(linux.rejected, 2);
    }

    #[test]
    fn the_recent_window_is_the_origin_key_and_the_digest() {
        let window = Window::of(vec![
            "pictures:known".to_string(),
            "a".repeat(64),
            String::new(),
        ]);
        let candidates = vec![
            seeking("known", "pictures/known.png", 1600, 900, 100),
            seeking("fresh", "pictures/fresh.png", 1600, 900, 100),
        ];
        let stage = stage_recent(candidates, &window);
        assert_eq!(stage.rejected, 1);
        assert_eq!(stage.kept[0].candidate.id, "fresh");
        // A window with no state directory behind it is an empty window, not an
        // error: 7.1's readers tolerate anything.
        assert!(!empty_window(&scratch("worker-window")).contains("pictures:known"));
    }

    #[test]
    fn weighted_order_draws_one_source_then_descends_by_weight() {
        // weights [1, 5, 3]: the draw lands inside the cumulative weight.
        assert_eq!(weighted_order(&[1, 5, 3], 0), vec![0, 1, 2]);
        assert_eq!(weighted_order(&[1, 5, 3], 1), vec![1, 2, 0]);
        assert_eq!(weighted_order(&[1, 5, 3], 8), vec![2, 1, 0]);
        assert_eq!(
            weighted_order(&[1, 5, 3], 9),
            vec![0, 1, 2],
            "the draw wraps"
        );
        // A source with weight 0 is never drawn and never listed.
        assert_eq!(weighted_order(&[0, 2], 0), vec![1]);
        assert!(weighted_order(&[0, 0], 0).is_empty());
        // Across a run of draws, the weight is a chance and not an order.
        let firsts: Vec<usize> = (0..24)
            .map(|draw| weighted_order(&[1, 2], draw)[0])
            .collect();
        assert!(firsts.contains(&0) && firsts.contains(&1), "{firsts:?}");
    }

    #[test]
    fn set_path_references_the_users_file_without_copying_it() {
        let dir = scratch("worker-set");
        let config = config(&dir, "");
        let sources = table(&config, Fixture::with(Vec::new()));
        let transport = Bytes::of(&[]);
        let cache = Cache::at(dir.join("cache"));
        let window = empty_window(&dir);
        let setter = PlatformSet(Backend::Noop);
        let run = noop_run(&config, &sources, &transport, &cache, &window, &setter);
        let bytes = long_png(1600, 900, 32);
        let target = dir.join("mine.png");
        fs::write(&target, &bytes).expect("the user's file");
        let target = target.display().to_string();

        let report = run.set(&target).expect("a reference is set");

        assert_eq!(report.digest, protocol::sha256_hex(&bytes));
        assert_eq!(
            report.origin_key,
            format!("external:{}", protocol::sha256_hex(target.as_bytes()))
        );
        assert_eq!(report.path, target);
        assert!(
            !cache.root().exists(),
            "a reference is handed over, never copied (6.4)"
        );
        // Anything else is not a path this build can resolve yet.
        assert_eq!(
            run.set("relative.png").expect_err("not absolute").code,
            ErrorCode::BadArgs
        );
        assert_eq!(
            run.set(&"f".repeat(64)).expect_err("not cached").code,
            ErrorCode::NotFound
        );
    }
}
