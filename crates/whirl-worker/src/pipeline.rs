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
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use whirl_core::config::{Backend, Config, LocalMode, SourceConfig, SourceKind, paths};
use whirl_core::protocol::{self, ErrorCode, SourceRecord};
use whirl_core::source::{Candidate, EnumContext};
use whirl_core::state::{self, IndexFile, StateFile};

/// The largest header this build sniffs. A HEIC `ispe` box can sit behind a
/// `meta` box, so the window is wider than the 8 to 30 bytes the other three
/// formats need; section 3 step 5 asks for "the first bytes", and this is how
/// many of them are kept.
const HEAD_WINDOW: usize = 1024;

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

/// 2.5 step 5, the cheap half of dedupe: a candidate whose `origin_key` (or
/// whose id) is in the recent window of 4.1, "the whole history ring (50 entries
/// by default) plus the current index".
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
/// against before it costs a request.
///
/// It is built from the history ring and the cache index, which are the daemon's
/// files (7.2). This process only reads them, and it tolerates anything it finds:
/// 7.1's rule is "No reader takes a lock, and every reader must tolerate any file
/// being replaced under it", and a window that cannot be read is a missed
/// preference, never a failed rotation. Nothing here writes either file.
#[derive(Debug, Clone, Default)]
pub struct Window {
    keys: HashSet<String>,
}

impl Window {
    pub fn empty() -> Window {
        Window {
            keys: HashSet::new(),
        }
    }

    /// A window from the two files, bounded by `dedupe.recent_entries` (4.1: the
    /// whole ring, and the index). The state directory is where the ring lives;
    /// the index lives in the cache root.
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
                        window.add(digest);
                        window.add(&entry.origin_key);
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

    /// The same set as a list, which is what a source is handed so it can
    /// exclude the window in its own request where it can (`EnumContext.recent`).
    pub fn keys(&self) -> Vec<String> {
        self.keys.iter().cloned().collect()
    }
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
    sniff_png(bytes)
        .or_else(|| sniff_jpeg(bytes))
        .or_else(|| sniff_webp(bytes))
        .or_else(|| sniff_heic(bytes))
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

fn sniff_jpeg(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut at = 2;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xff {
            return None;
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
            return None;
        }
        // SOF0..SOF15 except DHT (c4), JPG (c8) and DAC (cc) carry the frame
        // header, and it is width and height in that order at a fixed offset.
        let frame = (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
        if frame && at + 9 < bytes.len() {
            return Some(Header {
                ext: "jpg",
                height: u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32,
                width: u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32,
            });
        }
        at += 2 + length;
    }
    None
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

fn sniff_heic(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    let brand: [u8; 4] = [bytes[8], bytes[9], bytes[10], bytes[11]];
    let heic: [[u8; 4]; 6] = [*b"heic", *b"heix", *b"hevc", *b"hevx", *b"mif1", *b"msf1"];
    if !heic.contains(&brand) {
        return None;
    }
    // `ispe` carries the dimensions: 4-byte size, `ispe`, the 4 version-and-flags
    // bytes every FullBox has, then width and height as 32-bit big-endian. A
    // scan is what the header window allows; the boxes walk by their own sizes
    // and a file that hides `ispe` deeper than the window is not one whirl will
    // promise to display.
    let mut at = 0;
    while at + 20 <= bytes.len() {
        if &bytes[at + 4..at + 8] == b"ispe" {
            return Some(Header {
                ext: "heic",
                width: be32(&bytes[at + 12..at + 16]).unwrap_or(0),
                height: be32(&bytes[at + 16..at + 20]).unwrap_or(0),
            });
        }
        at += 1;
    }
    None
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
        if head.len() < HEAD_WINDOW {
            let take = (HEAD_WINDOW - head.len()).min(read);
            head.extend_from_slice(&buffer[..take]);
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

    let header = match sniff(&head) {
        Some(header) => header,
        None => {
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
    if final_path.is_file() {
        // Section 3 step 7: "If the final path already exists, the bytes are
        // identical by construction, so `unlink` the part file and report a
        // cache hit (the daemon bumps `last_used`)."
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
            Filled::Referenced { digest, .. } => digest,
        }
    }

    /// The path the platform is given, which is the one thing the two arms
    /// disagree about: the cache file, or the user's own file.
    fn path(&self) -> &Path {
        match self {
            Filled::Stored(stored) => &stored.path,
            Filled::Referenced { path, .. } => path,
        }
    }
}

impl Run<'_> {
    /// `--verb rotate`: pick a source by weight, pick a candidate, set it.
    ///
    /// The walk follows features.md 1.4: a source that yields nothing
    /// admissible is left and the remaining sources are tried in descending
    /// weight order, at most one enumeration per source per rotation; a setter
    /// failure marks the candidate bad and one more candidate is tried; and if
    /// nothing is admissible at all the rotation fails with `no_candidates`.
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
        let mut reasons: Vec<String> = Vec::new();
        for index in order {
            let entry = &self.sources.entries()[index];
            let candidates = match self.enumerate(entry) {
                Ok(candidates) => candidates,
                Err(reason) => {
                    eprintln!("warning: source {} is disabled: {reason}", entry.config.id);
                    reasons.push(format!("{}: {reason}", entry.config.id));
                    continue;
                }
            };
            let filtered = filter_pipeline(self.config, self.platform, self.window, candidates);
            if filtered.kept.is_empty() {
                reasons.push(format!(
                    "{}: {:?} candidates, {:?} admitted",
                    entry.config.id, filtered.counters.candidates, filtered.counters.admitted
                ));
                continue;
            }
            for seeking in filtered.kept {
                if bad.contains(&seeking.candidate.id) {
                    continue;
                }
                let filled = match self.fill(&seeking, &mut reasons)? {
                    Some(filled) => filled,
                    None => continue,
                };
                // 1.6's first line belongs to the store, and a reference-mode
                // candidate reached the setter without one: nothing was
                // downloaded, so nothing is claimed to have been.
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
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::rc::Rc;
    use whirl_core::config::{Config, ConfigError};
    use whirl_core::source::{Capability, Enumerated, FilterSet, Source, SourceError};

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

    /// A HEIC: the file type box, then the `ispe` box that carries the size.
    fn heic(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"heic");
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"mif1");
        bytes.extend_from_slice(&20u32.to_be_bytes());
        bytes.extend_from_slice(b"ispe");
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
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
        Run {
            config,
            setter,
            sources,
            transport,
            cache,
            window,
            platform: Platform::Macos,
            run: 1,
            draw: 0,
        }
    }

    // -- the sniff, per format ----------------------------------------------

    /// The harness writes paths into a config body, and a path with a backslash
    /// in it is why that is a quoting job and not string concatenation: written
    /// raw, the config parser stops at `unknown escape '\U'`. On the Windows
    /// runner the temporary directory is `C:\Users\...`, which is how eleven
    /// pipeline tests failed there and nowhere else (run 36157542930 on
    /// t_ddb890aa). This pins the escaping on the platform the test runs on.
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
    fn a_disabled_source_prints_a_reason_and_no_counter_group() {
        // A kind with no arm in the dispatch table. `local` has one now, so the
        // example is `wallhaven`, which does not
        // (crates/whirl-worker/src/sources/mod.rs).
        let config = Config::parse(
            "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
             \"wallhaven\", \"weight\": 3, \"query\": \"landscape\" } ]\n}\n",
        )
        .expect("the fixture config parses")
        .config;
        let line = disabled_record(
            &config.sources[0],
            crate::sources::missing_reason(&config.sources[0]),
        );
        assert_eq!(
            line,
            "source: space wallhaven weight=3 enabled=0 last=- reason=no implementation for kind wallhaven in this build"
        );
        let parsed = protocol::parse_source_record(&line).expect("the daemon reads it");
        assert!(!parsed.enabled);
        assert!(parsed.counters.is_empty());
        assert_eq!(
            parsed.reason.as_deref(),
            Some("no implementation for kind wallhaven in this build")
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
            // other entry, so the mutation survives (t_ddb890aa, run 3).
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
