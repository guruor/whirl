//! The source abstraction: the shape docs/spec/features.md 2.6 fixes.
//!
//! A source is a config entry, not a compiled-in provider. This module holds the
//! trait a source implements, the candidate type it returns and the capability set
//! it declares, so that the worker's source factory has one shape to dispatch to
//! and a contributor adds exactly one line to it (docs/spec/features.md 2.6).
//!
//! **No source ships in this scaffold.** `local` and `wallhaven` are separate
//! cards, exactly as the platform setters are; what exists here is the trait and
//! the schema they will plug into.

use crate::config::{ConfigError, SourceConfig};
use std::fmt;
use std::path::PathBuf;

/// One candidate an enumerating source produced.
///
/// Metadata only: `enumerate` is allowed to be lazy, and nothing but these fields
/// comes back at that stage (docs/spec/features.md 2.6). `bytes` is present only
/// where the source can answer it without the bytes themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The source-scoped id: the site's wallpaper id, or the sha256 of the
    /// normalised absolute path for a `local` source.
    pub id: String,
    /// Where the bytes come from: a URL, or an absolute path.
    pub origin: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: Option<u64>,
}

/// What a source can answer when asked (docs/spec/features.md 2.5, Layer 1).
///
/// A capability a source does not declare is not attempted at the source; the
/// shared pipeline applies it instead, and the result set is the same either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    Resolution,
    Ratio,
    Purity,
    Colors,
    Category,
    Extension,
}

impl Capability {
    /// Every capability name the schema accepts, in a fixed order so that error
    /// messages and `plan:` output cannot drift between runs.
    pub const ALL: [Capability; 6] = [
        Capability::Resolution,
        Capability::Ratio,
        Capability::Purity,
        Capability::Colors,
        Capability::Category,
        Capability::Extension,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Resolution => "resolution",
            Capability::Ratio => "ratio",
            Capability::Purity => "purity",
            Capability::Colors => "colors",
            Capability::Category => "category",
            Capability::Extension => "extension",
        }
    }

    pub fn parse(name: &str) -> Option<Capability> {
        Capability::ALL.iter().copied().find(|c| c.as_str() == name)
    }
}

/// A set of capabilities, as a bitset so that it stays a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilterSet {
    bits: u8,
}

impl FilterSet {
    /// The empty set.
    pub fn empty() -> FilterSet {
        FilterSet { bits: 0 }
    }

    /// The set containing every capability.
    pub fn full() -> FilterSet {
        FilterSet::of(&Capability::ALL)
    }

    pub fn of(caps: &[Capability]) -> FilterSet {
        let mut set = FilterSet::empty();
        for cap in caps {
            set = set.with(*cap);
        }
        set
    }

    pub fn with(self, cap: Capability) -> FilterSet {
        FilterSet {
            bits: self.bits | bit(cap),
        }
    }

    pub fn contains(self, cap: Capability) -> bool {
        self.bits & bit(cap) != 0
    }

    pub fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// The capabilities declared, in [`Capability::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }
}

fn bit(cap: Capability) -> u8 {
    1u8 << Capability::ALL.iter().position(|c| *c == cap).unwrap_or(0)
}

/// What an `enumerate` call is given.
///
/// `recent` carries the origin keys and digests of the recent window so that a
/// source which can cheaply exclude them does so; the shared pipeline still applies
/// dedupe (docs/spec/features.md 2.5, Layer 2 step 5).
#[derive(Debug, Clone, Default)]
pub struct EnumContext {
    /// The daemon's rotation id for the run this enumeration serves.
    pub run: u64,
    /// `HOME`, already resolved, for a source that expands `~` itself.
    pub home: Option<PathBuf>,
    /// Entries already seen: origin keys and digests.
    pub recent: Vec<String>,
}

/// What one `enumerate` call produced.
///
/// The candidates are 2.6's metadata-only list. The two page numbers are the
/// source's own accounting of the listing requests it made, which no stage of
/// the pipeline can see: a source that lists nothing over the wire (a local
/// filesystem) walks no pages and asks for none, so both are 0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Enumerated {
    pub candidates: Vec<Candidate>,
    /// Listing requests this enumeration actually made.
    pub pages_walked: u64,
    /// How many of the configured `pages` the walk did not make because the
    /// listing ended first: `last_page` reached, or a page that came back with
    /// no entries. A walk that made every page it was configured for skipped
    /// none, even when the listing has more pages than the config asked for:
    /// that is what `pages` means (features.md 2.3), not a skip.
    pub pages_skipped: u64,
}

impl Enumerated {
    /// The result of a source that makes no listing request of its own.
    pub fn of(candidates: Vec<Candidate>) -> Enumerated {
        Enumerated {
            candidates,
            pages_walked: 0,
            pages_skipped: 0,
        }
    }
}

/// Why an `enumerate` call could not answer.
///
/// A source that cannot be asked has to say so. The alternative, an empty
/// candidate list, is indistinguishable from a source that honestly found
/// nothing, and the operator reads the difference in the reason the rotation
/// prints (docs/architecture.md 4.3). The kind is named rather than only
/// described because it is the half a test can assert on: the message is prose
/// (and for a Wallhaven failure it is the API's own, which is not ours to keep
/// stable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceErrorKind {
    /// 401: the API wanted a key, or the key it was given is not valid.
    Unauthorized,
    /// 403: the request was refused before it was read.
    Forbidden,
    /// 429: the documented rate limit was exceeded.
    RateLimited,
    /// 404: the listing endpoint does not exist, which for a collection means
    /// the handle is wrong.
    NotFound,
    /// The request never completed: no program to make it with, no network, or
    /// a timeout.
    Unavailable,
    /// The response arrived and could not be read as the listing it claims to
    /// be.
    Malformed,
}

impl SourceErrorKind {
    /// The name of the kind, which is what a log line and a test carry.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceErrorKind::Unauthorized => "unauthorized",
            SourceErrorKind::Forbidden => "forbidden",
            SourceErrorKind::RateLimited => "rate_limited",
            SourceErrorKind::NotFound => "not_found",
            SourceErrorKind::Unavailable => "unavailable",
            SourceErrorKind::Malformed => "malformed",
        }
    }
}

/**
 * A named failure from a source, with the API's own words in the message.
 */
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    pub kind: SourceErrorKind,
    /// Prose, for the operator. It never carries an API key: see
    /// docs/architecture.md 6.3 (docs/spec/features.md 2.4).
    pub message: String,
}

impl SourceError {
    pub fn new(kind: SourceErrorKind, message: impl Into<String>) -> SourceError {
        SourceError {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for SourceError {}

/// One source, as docs/spec/features.md 2.6 describes it.
///
/// Three methods, and no I/O in `validate` or `capabilities`: `validate` rejects a
/// config that cannot work, naming the offending key, and runs where the config is
/// parsed so that the message a user sees is the same in `whirl config check` and
/// at daemon startup (docs/architecture.md 4.3).
pub trait Source {
    /// Reject a config this source cannot work with, naming the offending key.
    fn validate(&self, config: &SourceConfig) -> Result<(), ConfigError>;

    /// List candidates, or say why the listing could not be made. May be lazy:
    /// metadata only.
    fn enumerate(&self, ctx: &EnumContext) -> Result<Enumerated, SourceError>;

    /// Which Layer 1 filters this source can push into its own request.
    fn capabilities(&self) -> FilterSet;
}
