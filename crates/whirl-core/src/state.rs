//! State records: the history ring, favorites and the cache index.
//!
//! Shapes come from docs/architecture.md 2.6 (the positional record forms),
//! docs/architecture.md 2.10 (what `status` reports) and
//! docs/spec/state-and-cache.md 2.1 and 6 (the index and the state files).
//!
//! This scaffold holds the records and the ring in memory. Persisting them is
//! another card: nothing in this module opens a file, and the daemon ships no
//! state writer yet.

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
    pub origin: String,
    pub origin_key: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The cache-side copy of "some favorite record names this hash".
    pub pinned: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
