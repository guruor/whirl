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

/// One source, as docs/spec/features.md 2.6 describes it.
///
/// Three methods, and no I/O in `validate` or `capabilities`: `validate` rejects a
/// config that cannot work, naming the offending key, and runs where the config is
/// parsed so that the message a user sees is the same in `whirl config check` and
/// at daemon startup (docs/architecture.md 4.3).
pub trait Source {
    /// Reject a config this source cannot work with, naming the offending key.
    fn validate(&self, config: &SourceConfig) -> Result<(), ConfigError>;

    /// List candidates. May be lazy: metadata only.
    fn enumerate(&self, ctx: &EnumContext) -> Vec<Candidate>;

    /// Which Layer 1 filters this source can push into its own request.
    fn capabilities(&self) -> FilterSet;
}
