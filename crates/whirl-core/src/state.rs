//! State records: the history ring, favorites and the cache index.
//!
//! Shapes come from docs/architecture.md 2.6 (the positional record forms),
//! docs/architecture.md 2.10 (what `status` reports) and
//! docs/spec/state-and-cache.md 2.1 and 6 (the index and the state files).
//!
//! The records, the ring, and the three file schemas of 6.1 and 6.2 with the
//! `parse`/`encode` pair for each. Nothing in this module opens a file: the
//! shapes live here, beside the protocol they encode, and the daemon owns the
//! I/O (`crates/whirld/src/statefile.rs`, 6.3's temp-file-`fsync`-`rename`).

use crate::protocol::{Kind, Via};
use std::collections::VecDeque;

/// The history bound. Not a constant in the schema: `state.history_entries` is a
/// config key whose default is 50 (docs/architecture.md 4.2), and
/// docs/architecture.md R3 fixes the resident cost at that size.
pub const DEFAULT_HISTORY_ENTRIES: usize = 50;

/// One history entry: what was set, when, by which route, and from where.
///
/// `digest` and `path` are `None` where the platform could not report them: an
/// `external` entry whose file could not be hashed has no digest, and a
/// reference-mode set has no cache path. Both print as `-` (protocol 2.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// RFC 3339 UTC, e.g. `2026-09-25T07:41:12Z`.
    pub set_at: String,
    pub via: Via,
    pub kind: Kind,
    pub origin_key: String,
    pub digest: Option<String>,
    pub path: Option<String>,
}

impl HistoryEntry {
    /// The `entry:` line for `history` (docs/architecture.md 2.6):
    /// `entry: <set_at> <via> <kind> <origin_key> <digest|-> <path|->`.
    pub fn record_line(&self) -> String {
        format!(
            "entry: {} {} {} {} {} {}",
            self.set_at,
            self.via.as_str(),
            self.kind.as_str(),
            self.origin_key,
            self.digest.as_deref().unwrap_or("-"),
            self.path.as_deref().unwrap_or("-"),
        )
    }

    /// Parse the six fields of a history `entry:` line, after the `entry: ` prefix.
    /// The last field takes the rest of the line, spaces included (protocol 2.2).
    pub fn from_record_fields(fields: &[&str]) -> Option<HistoryEntry> {
        if fields.len() != 6 {
            return None;
        }
        Some(HistoryEntry {
            set_at: fields[0].to_string(),
            via: Via::parse(fields[1])?,
            kind: Kind::parse(fields[2])?,
            origin_key: fields[3].to_string(),
            digest: none_if_dash(fields[4]),
            path: none_if_dash(fields[5]),
        })
    }
}

/// `-` is the protocol's unset marker, never an empty string (protocol 2.6).
pub fn none_if_dash(value: &str) -> Option<String> {
    if value == "-" {
        None
    } else {
        Some(value.to_string())
    }
}

/// The history ring: newest first, bounded by `state.history_entries`.
#[derive(Debug, Clone)]
pub struct HistoryRing {
    entries: VecDeque<HistoryEntry>,
    bound: usize,
}

impl HistoryRing {
    pub fn new(bound: usize) -> HistoryRing {
        HistoryRing {
            entries: VecDeque::new(),
            bound: bound.max(1),
        }
    }

    /// Append, dropping the oldest entry if the ring is at its bound. Every set
    /// appends exactly one entry, whatever its `via` (docs/architecture.md 2.5
    /// rejects the prototype's destructive `pop_back` for `prev`).
    pub fn push(&mut self, entry: HistoryEntry) {
        if self.bound == 0 {
            return;
        }
        while self.entries.len() >= self.bound {
            self.entries.pop_back();
        }
        self.entries.push_front(entry);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bound(&self) -> usize {
        self.bound
    }

    /// Newest first.
    pub fn iter(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter()
    }

    /// The walk `prev` performs, exactly as docs/architecture.md 2.5 states it:
    ///
    /// 1. find the newest entry whose digest equals the current anchor;
    /// 2. walk older from there and stop at the first entry whose digest differs;
    /// 3. if step 1 finds nothing, start from the newest entry and apply step 2;
    /// 4. if no entry passes step 2, there is no previous entry.
    ///
    /// It removes nothing and appends nothing, so the ring a client read with
    /// `history` is the ring the next `prev` walks.
    pub fn prev_from(&self, anchor_digest: Option<&str>) -> Option<&HistoryEntry> {
        let start = anchor_digest
            .and_then(|digest| {
                self.entries
                    .iter()
                    .position(|entry| entry.digest.as_deref() == Some(digest))
            })
            .unwrap_or(0);
        self.entries
            .iter()
            .skip(start)
            .find(|entry| entry.digest.as_deref() != anchor_digest)
    }
}

impl Default for HistoryRing {
    fn default() -> HistoryRing {
        HistoryRing::new(DEFAULT_HISTORY_ENTRIES)
    }
}

/// Where a favorite's bytes are (docs/architecture.md 2.6, the favorites record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavoriteState {
    Present,
    Missing,
    Unrecoverable,
}

impl FavoriteState {
    pub fn as_str(self) -> &'static str {
        match self {
            FavoriteState::Present => "present",
            FavoriteState::Missing => "missing",
            FavoriteState::Unrecoverable => "unrecoverable",
        }
    }

    pub fn parse(name: &str) -> Option<FavoriteState> {
        match name {
            "present" => Some(FavoriteState::Present),
            "missing" => Some(FavoriteState::Missing),
            "unrecoverable" => Some(FavoriteState::Unrecoverable),
            _ => None,
        }
    }
}

/// A pin. The collection is the authoritative list; the cache index carries a copy
/// so the sweep needs one file open (docs/spec/state-and-cache.md 2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favorite {
    pub added_at: String,
    pub kind: Kind,
    pub origin_key: String,
    pub digest: Option<String>,
    pub state: FavoriteState,
    pub path: Option<String>,
}

impl Favorite {
    /// `entry: <added_at> <kind> <origin_key> <digest|-> <state> <path|->`.
    pub fn record_line(&self) -> String {
        format!(
            "entry: {} {} {} {} {} {}",
            self.added_at,
            self.kind.as_str(),
            self.origin_key,
            self.digest.as_deref().unwrap_or("-"),
            self.state.as_str(),
            self.path.as_deref().unwrap_or("-"),
        )
    }
}

/// One entry of `cache/index.json` (docs/spec/state-and-cache.md 2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheIndexEntry {
    /// The extension taken from the sniffed header, never from the URL.
    pub ext: String,
    pub bytes: u64,
    pub first_seen: String,
    pub last_used: String,
    /// The source `id` this candidate came from.
    pub source: String,
    pub kind: Kind,
    /// The re-materialisation hint: the URL or absolute path the bytes came
    /// from (2.1). `None` where the writer did not know it: the daemon writes
    /// this entry from the worker's `set:` line of docs/architecture.md 1.6,
    /// which carries `origin_key` and the cache path and no origin (see
    /// [`IndexFile`]).
    pub origin: Option<String>,
    pub origin_key: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The cache-side copy of "some favorite record names this hash".
    pub pinned: bool,
}

// ---------------------------------------------------------------------------
// The state files (docs/spec/state-and-cache.md section 6)
// ---------------------------------------------------------------------------

/// The `schema` integer this build writes and understands. A file whose schema
/// is greater than this one is a downgrade rather than corruption, and is left
/// exactly as it is (6.4 step 4).
pub const SCHEMA: u64 = 1;

/// The three file names, so the daemon and the quarantine message of 6.4 spell
/// them the same way (`state_corrupt: history.json`).
pub const CURRENT_FILE: &str = "current.json";
pub const HISTORY_FILE: &str = "history.json";
pub const FAVORITES_FILE: &str = "favorites.json";

/// A state file that could not be understood. The caller's answer is the
/// quarantine of 6.4, so this carries a message for the log and nothing else:
/// the decision needs no structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFileError {
    pub message: String,
}

impl StateFileError {
    fn new(message: impl Into<String>) -> StateFileError {
        StateFileError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StateFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// What reading a state file found. The third case is not an error and not a
/// value: it is a file from a newer whirl, which this build must not touch
/// (6.4 step 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFile<T> {
    Read(T),
    SchemaNewer { found: u64 },
}

impl<T> StateFile<T> {
    /// The value, when this build understood the file.
    pub fn value(self) -> Option<T> {
        match self {
            StateFile::Read(value) => Some(value),
            StateFile::SchemaNewer { .. } => None,
        }
    }
}

/// `state/current.json` (docs/spec/state-and-cache.md 6.1): the live scalars and
/// the display anchor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CurrentFile {
    pub seq: u64,
    pub written_at: String,
    pub written_by: Option<String>,
    pub paused: bool,
    pub rotation_count: u64,
    /// The persisted deadline, wall clock, RFC 3339 UTC (5.5 rule 1).
    pub next_at: Option<String>,
    pub last_error: Option<String>,
    pub anchor: Option<Anchor>,
    pub cache: Option<CacheFacts>,
}

/// `anchor` in `current.json`: what whirl believes the platform is displaying.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Anchor {
    pub digest: String,
    /// `None` when there are no bytes of whirl's own (a `reference`-mode set).
    pub cached_path: Option<String>,
    pub set_at: Option<String>,
    pub display_mode: Option<String>,
}

/// The `cache` block of `current.json`, which is also what `status` reports.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CacheFacts {
    pub root_id: Option<String>,
    pub files: u64,
    pub bytes: u64,
    pub over_cap_bytes: u64,
    pub over_cap_files: u64,
}

/// `state/history.json` (docs/spec/state-and-cache.md 6.2): the ring, newest
/// first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryFile {
    pub seq: u64,
    pub written_at: String,
    pub entries: Vec<HistoryEntry>,
}

/// `state/favorites.json` (docs/spec/state-and-cache.md 6.2): the pin records.
/// The authoritative pin list is this file, and the cache index carries a copy
/// (2.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FavoritesFile {
    pub seq: u64,
    pub written_at: String,
    pub entries: Vec<Favorite>,
}

impl CurrentFile {
    pub fn parse(text: &str) -> Result<StateFile<CurrentFile>, StateFileError> {
        let root = document(text, CURRENT_FILE)?;
        let schema = schema_of(&root)?;
        if schema > SCHEMA {
            return Ok(StateFile::SchemaNewer { found: schema });
        }
        let mut file = CurrentFile {
            seq: opt_int(&root, "seq")?.unwrap_or(0),
            written_at: opt_str(&root, "written_at")?.unwrap_or_default(),
            written_by: opt_str(&root, "written_by")?,
            paused: opt_bool(&root, "paused")?.unwrap_or(false),
            rotation_count: opt_int(&root, "rotation_count")?.unwrap_or(0),
            next_at: opt_str(&root, "next_at")?,
            last_error: opt_str(&root, "last_error")?,
            anchor: None,
            cache: None,
        };
        if let Some(anchor) = optional(&root, "anchor")? {
            file.anchor = Some(Anchor {
                digest: req_str(&anchor, "digest")?,
                cached_path: opt_str(&anchor, "cached_path")?,
                set_at: opt_str(&anchor, "set_at")?,
                display_mode: opt_str(&anchor, "display_mode")?,
            });
        }
        if let Some(cache) = optional(&root, "cache")? {
            file.cache = Some(CacheFacts {
                root_id: opt_str(&cache, "root_id")?,
                files: opt_int(&cache, "files")?.unwrap_or(0),
                bytes: opt_int(&cache, "bytes")?.unwrap_or(0),
                over_cap_bytes: opt_int(&cache, "over_cap_bytes")?.unwrap_or(0),
                over_cap_files: opt_int(&cache, "over_cap_files")?.unwrap_or(0),
            });
        }
        Ok(StateFile::Read(file))
    }

    pub fn encode(&self) -> String {
        let mut text = String::new();
        text.push_str("{\n");
        text.push_str(&format!("  \"schema\": {SCHEMA},\n"));
        text.push_str(&format!("  \"seq\": {},\n", self.seq));
        text.push_str(&format!(
            "  \"written_at\": {},\n",
            string(&self.written_at)
        ));
        text.push_str(&format!(
            "  \"written_by\": {},\n",
            optional_string(self.written_by.as_deref())
        ));
        text.push_str(&format!(
            "  \"paused\": {},\n",
            if self.paused { "true" } else { "false" }
        ));
        text.push_str(&format!("  \"rotation_count\": {},\n", self.rotation_count));
        text.push_str(&format!(
            "  \"next_at\": {},\n",
            optional_string(self.next_at.as_deref())
        ));
        text.push_str(&format!(
            "  \"last_error\": {},\n",
            optional_string(self.last_error.as_deref())
        ));
        match &self.anchor {
            None => text.push_str("  \"anchor\": null,\n"),
            Some(anchor) => {
                text.push_str("  \"anchor\": {\n");
                text.push_str(&format!("    \"digest\": {},\n", string(&anchor.digest)));
                text.push_str(&format!(
                    "    \"cached_path\": {},\n",
                    optional_string(anchor.cached_path.as_deref())
                ));
                text.push_str(&format!(
                    "    \"set_at\": {},\n",
                    optional_string(anchor.set_at.as_deref())
                ));
                text.push_str(&format!(
                    "    \"display_mode\": {},\n",
                    optional_string(anchor.display_mode.as_deref())
                ));
                text.push_str("    \"displays\": null\n");
                text.push_str("  },\n");
            }
        }
        match &self.cache {
            None => text.push_str("  \"cache\": null\n"),
            Some(cache) => {
                text.push_str("  \"cache\": {\n");
                text.push_str(&format!(
                    "    \"root_id\": {},\n",
                    optional_string(cache.root_id.as_deref())
                ));
                text.push_str(&format!("    \"files\": {},\n", cache.files));
                text.push_str(&format!("    \"bytes\": {},\n", cache.bytes));
                text.push_str(&format!(
                    "    \"over_cap_bytes\": {},\n",
                    cache.over_cap_bytes
                ));
                text.push_str(&format!(
                    "    \"over_cap_files\": {}\n",
                    cache.over_cap_files
                ));
                text.push_str("  }\n");
            }
        }
        text.push_str("}\n");
        text
    }
}

impl HistoryFile {
    pub fn parse(text: &str) -> Result<StateFile<HistoryFile>, StateFileError> {
        let root = document(text, HISTORY_FILE)?;
        let schema = schema_of(&root)?;
        if schema > SCHEMA {
            return Ok(StateFile::SchemaNewer { found: schema });
        }
        let mut file = HistoryFile {
            seq: opt_int(&root, "seq")?.unwrap_or(0),
            written_at: opt_str(&root, "written_at")?.unwrap_or_default(),
            entries: Vec::new(),
        };
        for (index, node) in entries_of(&root)?.iter().enumerate() {
            file.entries.push(history_entry(node, index)?);
        }
        Ok(StateFile::Read(file))
    }

    pub fn encode(&self) -> String {
        let mut text = format!(
            "{{\n  \"schema\": {SCHEMA},\n  \"seq\": {},\n  \"written_at\": {},\n  \"entries\": [",
            self.seq,
            string(&self.written_at)
        );
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&format!(
                concat!(
                    "\n    {{\n",
                    "      \"kind\": {},\n",
                    "      \"origin_key\": {},\n",
                    "      \"digest\": {},\n",
                    "      \"cached_path\": {},\n",
                    "      \"set_at\": {},\n",
                    "      \"via\": {}\n",
                    "    }}"
                ),
                string(entry.kind.as_str()),
                string(&entry.origin_key),
                optional_string(entry.digest.as_deref()),
                optional_string(entry.path.as_deref()),
                string(&entry.set_at),
                string(entry.via.as_str())
            ));
        }
        text.push_str(if self.entries.is_empty() {
            "]\n}\n"
        } else {
            "\n  ]\n}\n"
        });
        text
    }
}

impl FavoritesFile {
    pub fn parse(text: &str) -> Result<StateFile<FavoritesFile>, StateFileError> {
        let root = document(text, FAVORITES_FILE)?;
        let schema = schema_of(&root)?;
        if schema > SCHEMA {
            return Ok(StateFile::SchemaNewer { found: schema });
        }
        let mut file = FavoritesFile {
            seq: opt_int(&root, "seq")?.unwrap_or(0),
            written_at: opt_str(&root, "written_at")?.unwrap_or_default(),
            entries: Vec::new(),
        };
        for (index, node) in entries_of(&root)?.iter().enumerate() {
            let fields = want_object(node, &format!("entries[{index}]"))?;
            let path = opt_str_in(fields, "cached_path")?;
            // `state` is recomputed rather than believed: whether the bytes are
            // there is a fact about the disk right now (2.6, features.md 1.2),
            // and a value written before the file was deleted would be a lie.
            file.entries.push(Favorite {
                added_at: req_str_in(fields, "added_at")?,
                kind: kind_of(&req_str_in(fields, "kind")?, index)?,
                origin_key: req_str_in(fields, "origin_key")?,
                digest: opt_str_in(fields, "digest")?,
                state: crate::state::favorite_state_of(path.as_deref()),
                path,
            });
        }
        Ok(StateFile::Read(file))
    }

    pub fn encode(&self) -> String {
        let mut text = format!(
            "{{\n  \"schema\": {SCHEMA},\n  \"seq\": {},\n  \"written_at\": {},\n  \"entries\": [",
            self.seq,
            string(&self.written_at)
        );
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&format!(
                concat!(
                    "\n    {{\n",
                    "      \"kind\": {},\n",
                    "      \"origin_key\": {},\n",
                    "      \"digest\": {},\n",
                    "      \"cached_path\": {},\n",
                    "      \"added_at\": {}\n",
                    "    }}"
                ),
                string(entry.kind.as_str()),
                string(&entry.origin_key),
                optional_string(entry.digest.as_deref()),
                optional_string(entry.path.as_deref()),
                string(&entry.added_at)
            ));
        }
        text.push_str(if self.entries.is_empty() {
            "]\n}\n"
        } else {
            "\n  ]\n}\n"
        });
        text
    }
}

/// `cache/index.json`: the cache's own metadata (docs/spec/state-and-cache.md
/// 2.1). The shape lives here with the other records; the daemon owns the file
/// (7.2) and the worker reads it, read-only, for the recent window of 4.1.
///
/// Three fields of an entry are absent in what this build writes, and the reason
/// is upstream rather than a preference here: `origin`, `width` and `height`
/// come from the bytes, and the report the daemon builds an entry from is the
/// two-line stdout contract of docs/architecture.md 1.6 (`downloaded:` and
/// `set: <digest> <origin_key> <via> <path>`), which carries neither. The
/// daemon holds no image bytes (docs/architecture.md 1.9) and does not sniff the
/// file, so those three are written as `null` and are recovered when the
/// digest's file is next read. 2.1's sentence that "the worker reports the
/// digest and the origin" describes an origin the record form does not carry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexFile {
    pub seq: u64,
    pub written_at: String,
    /// Minted when the cache directory is created; a state file whose recorded
    /// `root_id` differs has to be re-materialised (2.1).
    pub root_id: String,
    /// Keyed by the 64-hex digest, which is also the file's name.
    pub entries: std::collections::BTreeMap<String, CacheIndexEntry>,
    /// Entries whose file is missing, kept so `whirl status` can report them
    /// before the next sweep reclaims them (2.1).
    pub dangling: Vec<String>,
}

/// The file name of the index, inside the cache root (section 2).
pub const INDEX_FILE: &str = "index.json";

impl IndexFile {
    pub fn parse(text: &str) -> Result<StateFile<IndexFile>, StateFileError> {
        let root = document(text, INDEX_FILE)?;
        let schema = schema_of(&root)?;
        if schema > SCHEMA {
            return Ok(StateFile::SchemaNewer { found: schema });
        }
        let mut file = IndexFile {
            seq: opt_int(&root, "seq")?.unwrap_or(0),
            written_at: opt_str(&root, "written_at")?.unwrap_or_default(),
            root_id: opt_str(&root, "root_id")?.unwrap_or_default(),
            entries: std::collections::BTreeMap::new(),
            dangling: Vec::new(),
        };
        if let Some(node) = field(&root, "entries") {
            if !node.is_null() {
                for (digest, entry) in want_object(node, "entries")? {
                    file.entries
                        .insert(digest.clone(), index_entry(digest, entry)?);
                }
            }
        }
        if let Some(node) = field(&root, "dangling") {
            if !node.is_null() {
                for (index, item) in node
                    .as_array()
                    .ok_or_else(|| StateFileError::new("dangling: not an array"))?
                    .iter()
                    .enumerate()
                {
                    file.dangling
                        .push(item.as_str().map(str::to_string).ok_or_else(|| {
                            type_mismatch(&format!("dangling[{index}]"), "a string", item)
                        })?);
                }
            }
        }
        Ok(StateFile::Read(file))
    }

    pub fn encode(&self) -> String {
        let mut text = format!(
            "{{\n  \"schema\": {SCHEMA},\n  \"seq\": {},\n  \"written_at\": {},\n  \"root_id\": {},\n  \"entries\": {{",
            self.seq,
            string(&self.written_at),
            string(&self.root_id)
        );
        for (index, (digest, entry)) in self.entries.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&format!(
                concat!(
                    "\n    {}: {{\n",
                    "      \"ext\": {},\n",
                    "      \"bytes\": {},\n",
                    "      \"first_seen\": {},\n",
                    "      \"last_used\": {},\n",
                    "      \"source\": {},\n",
                    "      \"kind\": {},\n",
                    "      \"origin\": {},\n",
                    "      \"origin_key\": {},\n",
                    "      \"width\": {},\n",
                    "      \"height\": {},\n",
                    "      \"pinned\": {}\n",
                    "    }}"
                ),
                string(digest),
                string(&entry.ext),
                entry.bytes,
                string(&entry.first_seen),
                string(&entry.last_used),
                string(&entry.source),
                string(entry.kind.as_str()),
                optional_string(entry.origin.as_deref()),
                string(&entry.origin_key),
                optional_int(entry.width.map(u64::from)),
                optional_int(entry.height.map(u64::from)),
                if entry.pinned { "true" } else { "false" }
            ));
        }
        text.push_str(if self.entries.is_empty() {
            "},\n"
        } else {
            "\n  },\n"
        });
        text.push_str("  \"dangling\": [");
        for (index, digest) in self.dangling.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&format!("\n    {}", string(digest)));
        }
        text.push_str(if self.dangling.is_empty() {
            "]\n}\n"
        } else {
            "\n  ]\n}\n"
        });
        text
    }

    /// The entry for a digest, if the index knows it.
    pub fn entry(&self, digest: &str) -> Option<&CacheIndexEntry> {
        self.entries.get(digest)
    }
}

/// `sha256/aa/bb/<digest>.<ext>`: the content-addressed path of section 2, in the
/// one place, so the worker, the daemon's reconciliation and the sweep agree
/// about where an entry's file is. The two-level fanout is filesystem ergonomics
/// only, and nothing depends on it (2.1).
pub fn content_path(cache_root: &std::path::Path, digest: &str, ext: &str) -> std::path::PathBuf {
    let mut path = cache_root.join("sha256");
    if digest.len() >= 4 {
        path.push(&digest[0..2]);
        path.push(&digest[2..4]);
    }
    path.join(format!("{digest}.{ext}"))
}

/// `tmp/`: in-flight downloads, never candidates (section 2). It sits next to
/// `sha256/` so that section 3 step 7's `rename` is same-filesystem by
/// construction rather than by a runtime check.
pub fn tmp_dir(cache_root: &std::path::Path) -> std::path::PathBuf {
    cache_root.join("tmp")
}

/// One digest-keyed entry of the index, tolerant on read: a field this build
/// does not know is ignored, and the three that describe the bytes are optional
/// because a writer that did not hold them wrote `null`.
fn index_entry(
    digest: &str,
    node: &crate::config::json::Node,
) -> Result<CacheIndexEntry, StateFileError> {
    let fields = want_object(node, &format!("entries[{digest}]"))?;
    Ok(CacheIndexEntry {
        ext: req_str_in(fields, "ext")?,
        bytes: opt_int_in(fields, "bytes")?.unwrap_or(0),
        first_seen: opt_str_in(fields, "first_seen")?.unwrap_or_default(),
        last_used: opt_str_in(fields, "last_used")?.unwrap_or_default(),
        source: opt_str_in(fields, "source")?.unwrap_or_default(),
        kind: kind_of(&req_str_in(fields, "kind")?, 0)?,
        origin: opt_str_in(fields, "origin")?,
        origin_key: req_str_in(fields, "origin_key")?,
        width: opt_int_in(fields, "width")?.and_then(|value| u32::try_from(value).ok()),
        height: opt_int_in(fields, "height")?.and_then(|value| u32::try_from(value).ok()),
        pinned: opt_bool_in(fields, "pinned")?.unwrap_or(false),
    })
}

/// Where a favorite's bytes are (docs/architecture.md 2.6, the favorites record).
/// One implementation, used by the state-file reader and by `favorites`.
pub fn favorite_state_of(path: Option<&str>) -> FavoriteState {
    match path {
        Some(path) if std::path::Path::new(path).exists() => FavoriteState::Present,
        Some(_) => FavoriteState::Missing,
        None => FavoriteState::Unrecoverable,
    }
}

/// Parse one state document and require an object with an integer `schema`
/// (6.4 step 1: a file whose schema is not an integer is quarantined).
fn document(
    text: &str,
    name: &str,
) -> Result<Vec<(String, crate::config::json::Node)>, StateFileError> {
    if text.trim().is_empty() {
        return Err(StateFileError::new(format!("{name} is empty")));
    }
    let root = crate::config::json::parse(text)
        .map_err(|error| StateFileError::new(format!("{name}: {error}")))?;
    match root.as_object() {
        Some(entries) => Ok(entries.to_vec()),
        None => Err(StateFileError::new(format!(
            "{name}: the document is a {}, not an object",
            root.kind_name()
        ))),
    }
}

fn schema_of(root: &[(String, crate::config::json::Node)]) -> Result<u64, StateFileError> {
    let node =
        field(root, "schema").ok_or_else(|| StateFileError::new("schema: the key is missing"))?;
    match node.as_num() {
        // A schema is an integer: `1.5` is not one, and neither is a string.
        Some(value) if value >= 0.0 && value.fract() == 0.0 && value <= u64::MAX as f64 => {
            Ok(value as u64)
        }
        _ => Err(StateFileError::new(
            "schema: not a non-negative integer (6.4 step 1)",
        )),
    }
}

fn entries_of(
    root: &[(String, crate::config::json::Node)],
) -> Result<Vec<crate::config::json::Node>, StateFileError> {
    let node = field(root, "entries").ok_or_else(|| StateFileError::new("entries: missing"))?;
    node.as_array()
        .map(|entries| entries.to_vec())
        .ok_or_else(|| StateFileError::new("entries: not an array"))
}

fn field<'a>(
    entries: &'a [(String, crate::config::json::Node)],
    key: &str,
) -> Option<&'a crate::config::json::Node> {
    entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, node)| node)
}

fn want_object<'a>(
    node: &'a crate::config::json::Node,
    what: &str,
) -> Result<&'a [(String, crate::config::json::Node)], StateFileError> {
    node.as_object()
        .ok_or_else(|| StateFileError::new(format!("{what}: not an object")))
}

/// A field that may be absent or `null`: both mean "unset", which is what the
/// state files write for a value a fresh daemon has not set.
fn optional(
    root: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<Option<Vec<(String, crate::config::json::Node)>>, StateFileError> {
    match field(root, key) {
        None => Ok(None),
        Some(node) if node.is_null() => Ok(None),
        Some(node) => Ok(Some(want_object(node, key)?.to_vec())),
    }
}

fn opt_str_in(
    entries: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<Option<String>, StateFileError> {
    match field(entries, key) {
        None => Ok(None),
        Some(node) if node.is_null() => Ok(None),
        Some(node) => node
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| type_mismatch(key, "a string", node)),
    }
}

fn opt_str(
    root: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<Option<String>, StateFileError> {
    opt_str_in(root, key)
}

fn req_str_in(
    entries: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<String, StateFileError> {
    opt_str_in(entries, key)?
        .ok_or_else(|| StateFileError::new(format!("{key}: the key is missing")))
}

fn req_str(
    root: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<String, StateFileError> {
    req_str_in(root, key)
}

fn opt_int(
    root: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<Option<u64>, StateFileError> {
    opt_int_in(root, key)
}

fn opt_int_in(
    entries: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<Option<u64>, StateFileError> {
    match field(entries, key) {
        None => Ok(None),
        Some(node) if node.is_null() => Ok(None),
        Some(node) => match node.as_num() {
            Some(value) if value >= 0.0 && value.fract() == 0.0 && value <= u64::MAX as f64 => {
                Ok(Some(value as u64))
            }
            _ => Err(type_mismatch(key, "a non-negative integer", node)),
        },
    }
}

fn opt_bool(
    root: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<Option<bool>, StateFileError> {
    opt_bool_in(root, key)
}

fn opt_bool_in(
    entries: &[(String, crate::config::json::Node)],
    key: &str,
) -> Result<Option<bool>, StateFileError> {
    match field(entries, key) {
        None => Ok(None),
        Some(node) if node.is_null() => Ok(None),
        Some(node) => node
            .as_bool()
            .map(Some)
            .ok_or_else(|| type_mismatch(key, "a boolean", node)),
    }
}

fn type_mismatch(key: &str, expected: &str, node: &crate::config::json::Node) -> StateFileError {
    StateFileError::new(format!(
        "{key}: expected {expected}, found {}",
        node.kind_name()
    ))
}

/// A history entry's `via`, tolerant on read and exact on write.
///
/// `docs/spec/state-and-cache.md` 6.2 spells this field `rotate`, `prev`, `set`,
/// `startup`, `external`; `docs/architecture.md` 2.6 fixes the protocol's closed
/// vocabulary as `source`, `manual`, `prev`, `startup`, `recovered`. This build
/// writes the protocol's values, so a state file and a `history` response cannot
/// disagree, and accepts both when reading, so a file written by a reader of the
/// other list is not called corrupt. The conflict is raised in the handoff
/// rather than settled here.
fn via_of(name: &str, index: usize) -> Result<Via, StateFileError> {
    match name {
        "rotate" | "source" => Some(Via::Source),
        "set" | "manual" => Some(Via::Manual),
        "prev" => Some(Via::Prev),
        "startup" | "external" => Some(Via::Startup),
        "recovered" => Some(Via::Recovered),
        _ => None,
    }
    .ok_or_else(|| StateFileError::new(format!("entries[{index}].via: unknown value {name:?}")))
}

fn kind_of(name: &str, index: usize) -> Result<Kind, StateFileError> {
    Kind::parse(name).ok_or_else(|| {
        StateFileError::new(format!("entries[{index}].kind: unknown value {name:?}"))
    })
}

fn history_entry(
    node: &crate::config::json::Node,
    index: usize,
) -> Result<HistoryEntry, StateFileError> {
    let fields = want_object(node, &format!("entries[{index}]"))?;
    let kind = kind_of(&req_str_in(fields, "kind")?, index)?;
    Ok(HistoryEntry {
        set_at: req_str_in(fields, "set_at")?,
        via: via_of(&req_str_in(fields, "via")?, index)?,
        kind,
        origin_key: req_str_in(fields, "origin_key")?,
        digest: opt_str_in(fields, "digest")?,
        path: opt_str_in(fields, "cached_path")?,
    })
}

/// A JSON string literal, escaped rather than trusted: a state file carries
/// paths a user chose, and a path may contain a quote or a control character.
fn string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

fn optional_string(value: Option<&str>) -> String {
    match value {
        Some(value) => string(value),
        None => "null".to_string(),
    }
}

/// An optional integer in a document this build writes: `null` where the writer
/// did not know the value, which is the only unset marker in a JSON file (6.3).
fn optional_int(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_current_file_round_trips_through_its_own_encoding() {
        let file = CurrentFile {
            seq: 7,
            written_at: "2026-09-25T07:41:12Z".to_string(),
            written_by: Some("whirl/0.1.0 pid=4211 run=3".to_string()),
            paused: true,
            rotation_count: 40,
            next_at: Some("2026-09-25T08:11:12Z".to_string()),
            last_error: Some("source \"a\" timed out".to_string()),
            anchor: Some(Anchor {
                digest: "a".repeat(64),
                cached_path: Some("/cache/objects/aa/".to_string() + &"a".repeat(64) + ".jpg"),
                set_at: Some("2026-09-25T07:41:12Z".to_string()),
                display_mode: Some("copy".to_string()),
            }),
            cache: Some(CacheFacts {
                root_id: Some("f2b7a1c0-0000-4000-8000-000000000000".to_string()),
                files: 312,
                bytes: 41_943_040,
                over_cap_bytes: 0,
                over_cap_files: 0,
            }),
        };
        let parsed = CurrentFile::parse(&file.encode()).expect("a state file this build wrote");
        assert_eq!(parsed, StateFile::Read(file));
    }

    #[test]
    fn a_history_file_round_trips_with_its_order() {
        let file = HistoryFile {
            seq: 9,
            written_at: "2026-09-25T07:41:12Z".to_string(),
            entries: vec![
                HistoryEntry {
                    set_at: "2026-09-25T07:41:12Z".to_string(),
                    via: Via::Manual,
                    kind: Kind::External,
                    origin_key: "external:b84a".to_string(),
                    digest: Some("b".repeat(64)),
                    path: Some("/tmp/one.jpg".to_string()),
                },
                HistoryEntry {
                    set_at: "2026-09-25T07:11:12Z".to_string(),
                    via: Via::Source,
                    kind: Kind::Wallhaven,
                    origin_key: "a:17".to_string(),
                    digest: None,
                    path: None,
                },
            ],
        };
        let parsed = HistoryFile::parse(&file.encode())
            .expect("this build's own encoding")
            .value()
            .expect("a schema this build understands");
        assert_eq!(parsed, file, "newest first, and nothing reordered");
    }

    #[test]
    fn a_favorite_file_needs_no_state_field_and_recomputes_one() {
        // `state` is a fact about the disk, so a file that claims `present` for
        // a path that is not there must not be believed (2.6).
        let text = r#"{
          "schema": 1,
          "seq": 2,
          "written_at": "2026-09-25T07:41:12Z",
          "entries": [
            {
              "kind": "local",
              "origin_key": "a:17",
              "digest": null,
              "cached_path": "/nonexistent/whirl-test-object.jpg",
              "added_at": "2026-09-01T00:00:00Z",
              "state": "present"
            }
          ]
        }"#;
        let parsed = FavoritesFile::parse(text)
            .expect("a readable file")
            .value()
            .expect("schema 1");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].state, FavoriteState::Missing);
        assert_eq!(parsed.entries[0].added_at, "2026-09-01T00:00:00Z");
    }

    #[test]
    fn a_newer_schema_is_read_only_rather_than_corrupt() {
        // 6.4 step 4: a downgrade leaves the file alone. The three readers agree.
        let newer = r#"{"schema": 2, "seq": 1, "next_at": null}"#;
        assert_eq!(
            CurrentFile::parse(newer),
            Ok(StateFile::SchemaNewer { found: 2 })
        );
        assert_eq!(
            HistoryFile::parse(newer),
            Ok(StateFile::SchemaNewer { found: 2 })
        );
        assert_eq!(
            FavoritesFile::parse(newer),
            Ok(StateFile::SchemaNewer { found: 2 })
        );
    }

    #[test]
    fn corruption_is_refused_rather_than_defaulted() {
        // 6.4 step 1: a file that does not parse, an empty file, and a schema
        // that is not an integer are all quarantine, not a fresh start.
        for bad in [
            "",
            "   \n",
            "{\"schema\": 1, \"seq\": 1",
            "{\"schema\": \"one\", \"seq\": 1}",
            "{\"schema\": 1.5}",
            "{\"schema\": 1, \"paused\": \"yes\"}",
            "[1, 2, 3]",
            "{\"schema\": 1, \"entries\": {}}",
            "{\"schema\": 1, \"entries\": [{\"kind\": \"image\"}]}",
            "{\"schema\": 1, \"entries\": [{\"kind\": \"gif\", \"via\": \"set\", \"set_at\": \"x\", \"origin_key\": \"a:1\"}]}",
            "{\"seq\": 1}",
        ] {
            assert!(
                CurrentFile::parse(bad).is_err()
                    || HistoryFile::parse(bad).is_err()
                    || FavoritesFile::parse(bad).is_err(),
                "{bad:?} was accepted"
            );
        }
        // Missing keys that are merely unset are not corruption: `next_at` and
        // `last_error` are absent in a fresh file, and the object is still read.
        let sparse = r#"{"schema": 1}"#;
        let parsed = CurrentFile::parse(sparse).expect("a sparse file").value();
        assert_eq!(parsed, Some(CurrentFile::default()));
    }

    #[test]
    fn the_two_via_vocabularies_both_read() {
        // 6.2's list and 2.6's list disagree; a reader accepts both, and writes
        // the protocol's (the handoff quotes both).
        let text = |via: &str| {
            format!(
                r#"{{"schema": 1, "entries": [{{"kind": "local", "via": "{via}",
                   "set_at": "2026-09-25T07:41:12Z", "origin_key": "a:1"}}]}}"#
            )
        };
        let read = |via: &str| {
            HistoryFile::parse(&text(via))
                .expect("readable")
                .value()
                .expect("schema 1")
                .entries[0]
                .via
        };
        assert_eq!(read("rotate"), Via::Source);
        assert_eq!(read("source"), Via::Source);
        assert_eq!(read("set"), Via::Manual);
        assert_eq!(read("manual"), Via::Manual);
        assert_eq!(read("external"), Via::Startup);
        assert_eq!(read("startup"), Via::Startup);
        assert_eq!(read("prev"), Via::Prev);
        assert_eq!(read("recovered"), Via::Recovered);
        assert!(HistoryFile::parse(&text("sideways")).is_err());
    }

    #[test]
    fn a_written_document_is_json_this_build_can_read_back() {
        // The encoder is hand-written, so the one property that matters is that
        // the reader accepts it: a path with a quote, a backslash and a newline
        // is the case a naive encoder gets wrong.
        let nasty = "/tmp/a\"b\\c\nd.jpg";
        let file = FavoritesFile {
            seq: 1,
            written_at: "2026-09-25T07:41:12Z".to_string(),
            entries: vec![Favorite {
                added_at: "2026-09-25T07:41:12Z".to_string(),
                kind: Kind::External,
                origin_key: "external:zz".to_string(),
                digest: None,
                state: FavoriteState::Unrecoverable,
                path: None,
            }],
        };
        let text = file.encode();
        assert!(text.contains("external:zz"));
        let parsed = FavoritesFile::parse(&text).expect("its own encoding");
        assert_eq!(parsed.value(), Some(file));

        let anchor = Anchor {
            digest: "c".repeat(64),
            cached_path: Some(nasty.to_string()),
            set_at: None,
            display_mode: None,
        };
        let file = CurrentFile {
            anchor: Some(anchor.clone()),
            ..CurrentFile::default()
        };
        let parsed = CurrentFile::parse(&file.encode()).expect("its own encoding");
        assert_eq!(parsed.value().and_then(|file| file.anchor), Some(anchor));
    }

    fn entry(digest: &str) -> HistoryEntry {
        HistoryEntry {
            set_at: "2026-09-25T07:41:12Z".to_string(),
            via: Via::Source,
            kind: Kind::Local,
            origin_key: format!("pictures:{digest}"),
            digest: Some(digest.to_string()),
            path: Some(format!("/tmp/{digest}.jpg")),
        }
    }

    #[test]
    fn the_ring_is_newest_first_and_bounded() {
        let mut ring = HistoryRing::new(3);
        for digest in ["a", "b", "c", "d"] {
            ring.push(entry(digest));
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.bound(), 3);
        let digests: Vec<_> = ring
            .iter()
            .map(|e| e.digest.clone().unwrap_or_default())
            .collect();
        assert_eq!(digests, vec!["d", "c", "b"]);
    }

    #[test]
    fn prev_walks_one_entry_older_each_time() {
        let mut ring = HistoryRing::new(50);
        for digest in ["a", "b", "c"] {
            ring.push(entry(digest));
        }
        // The last push is the newest, so the ring is newest-first: c, b, a.
        // Step 1 finds it, step 2 steps to "b".
        let first = ring.prev_from(Some("c")).expect("a previous entry");
        assert_eq!(first.digest.as_deref(), Some("b"));
        // The ring did not move: the second prev from the new anchor gives "a".
        assert_eq!(ring.len(), 3);
        let second = ring.prev_from(Some("b")).expect("a previous entry");
        assert_eq!(second.digest.as_deref(), Some("a"));
        // Nothing older than "a".
        assert!(ring.prev_from(Some("a")).is_none());
    }

    #[test]
    fn prev_from_an_unknown_anchor_starts_at_the_newest_entry() {
        let mut ring = HistoryRing::new(50);
        for digest in ["a", "b"] {
            ring.push(entry(digest));
        }
        // The ring is [b, a]: "b" is the newest.
        // Step 3: an anchor that is not in the ring, an external image for example.
        let prev = ring
            .prev_from(Some("not-in-history"))
            .expect("newest entry");
        assert_eq!(prev.digest.as_deref(), Some("b"));
    }

    #[test]
    fn a_history_record_round_trips() {
        let original = entry("d435840c");
        let line = original.record_line();
        assert!(line.starts_with("entry: 2026-09-25T07:41:12Z source local pictures:d435840c "));
        let fields: Vec<&str> = line["entry: ".len()..].split(' ').collect();
        assert_eq!(HistoryEntry::from_record_fields(&fields), Some(original));
    }

    #[test]
    fn a_record_without_a_path_or_digest_round_trips_as_dashes() {
        let original = HistoryEntry {
            set_at: "2026-09-25T06:58:02Z".to_string(),
            via: Via::Startup,
            kind: Kind::External,
            origin_key: "external:d25a845d".to_string(),
            digest: None,
            path: None,
        };
        let line = original.record_line();
        assert!(line.ends_with(" - -"), "unset fields print as -: {line}");
        let fields: Vec<&str> = line["entry: ".len()..].split(' ').collect();
        assert_eq!(HistoryEntry::from_record_fields(&fields), Some(original));
    }

    #[test]
    fn a_path_with_spaces_survives_the_record() {
        let original = HistoryEntry {
            set_at: "2026-09-25T06:58:02Z".to_string(),
            via: Via::Manual,
            kind: Kind::Local,
            origin_key: "pictures:abc".to_string(),
            digest: Some("abc".to_string()),
            path: Some("/Users/some one/Pictures/a b.jpg".to_string()),
        };
        let line = original.record_line();
        let rest = &line["entry: ".len()..];
        // The last field takes the rest of the line, so split into five then keep one.
        let mut fields: Vec<&str> = rest.splitn(6, ' ').collect();
        assert_eq!(fields.len(), 6);
        fields[5] = fields[5].trim_start();
        assert_eq!(HistoryEntry::from_record_fields(&fields), Some(original));
    }

    /// The index of 2.1 round-trips through its own encoder, including an entry
    /// whose three byte-derived fields the writer did not know.
    #[test]
    fn the_index_round_trips_through_its_encoder() {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "ab12cd34".to_string(),
            CacheIndexEntry {
                ext: "jpg".to_string(),
                bytes: 3_822_331,
                first_seen: "2026-09-24T22:10:03Z".to_string(),
                last_used: "2026-09-25T07:41:12Z".to_string(),
                source: "space".to_string(),
                kind: Kind::Wallhaven,
                origin: None,
                origin_key: "wallhaven:ab12cd".to_string(),
                width: None,
                height: None,
                pinned: true,
            },
        );
        let index = IndexFile {
            seq: 41,
            written_at: "2026-09-25T07:41:12Z".to_string(),
            root_id: "8f1d0c2e".to_string(),
            entries,
            dangling: vec!["c0ffee".to_string()],
        };
        let text = index.encode();
        assert!(text.contains("\"origin\": null"), "{text}");
        assert!(text.contains("\"width\": null"), "{text}");
        match IndexFile::parse(&text) {
            Ok(StateFile::Read(parsed)) => assert_eq!(parsed, index),
            other => panic!("the index reads back: {other:?}"),
        }
    }

    /// An index from a newer build is left alone, the same rule as the state
    /// files (6.4 step 4) and the same one case.
    #[test]
    fn an_index_from_a_newer_build_is_not_read() {
        let text = "{\"schema\": 2, \"entries\": {}}";
        assert_eq!(
            IndexFile::parse(text),
            Ok(StateFile::SchemaNewer { found: 2 })
        );
    }

    /// The content-addressed path of section 2 is one function, and the fanout
    /// is the first four hex characters.
    #[test]
    fn the_content_path_is_the_digest_under_the_two_level_fanout() {
        let root = std::path::Path::new("/cache");
        assert_eq!(
            content_path(root, "ab12cd34ef", "jpg"),
            std::path::PathBuf::from("/cache/sha256/ab/12/ab12cd34ef.jpg")
        );
        assert_eq!(
            tmp_dir(root),
            std::path::PathBuf::from("/cache/tmp"),
            "tmp/ sits next to sha256/ so the rename of section 3 is same-filesystem"
        );
    }
}
