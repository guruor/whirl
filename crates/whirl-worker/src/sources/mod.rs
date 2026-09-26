//! The source factory: docs/spec/features.md 2.6's "one dispatch table", and the
//! only place that knows which `kind`s this build can actually serve.
//!
//! **Empty on purpose.** `local` and `wallhaven` are each their own card, and
//! neither has landed. Until one does, [`build`] answers `None` for every kind,
//! [`Sources::from_config`] returns a table with no entries, and a rotation fails
//! with `no_candidates` and the reason [`missing_reason`] names. That is the
//! honest state of the build: the alternative, a test-only source compiled into
//! this binary, would ship a `kind` the configuration schema does not describe.
//!
//! This module does not stand in for the sources. What it does is keep the
//! pipeline's dependency one trait wide, so a source card adds an arm to
//! [`build`] and a test in `pipeline.rs` supplies its own.

use whirl_core::config::{Config, SourceConfig, SourceKind};
use whirl_core::source::Source;

/// One configured source and the implementation that answers it. The config is
/// kept beside the implementation because `validate` and the pipeline's
/// per-source overrides (features.md 2.1) both need it.
pub struct Entry {
    pub config: SourceConfig,
    pub source: Box<dyn Source>,
}

/// Every source this build can serve, in config order.
pub struct Sources {
    entries: Vec<Entry>,
}

impl Sources {
    /// The table for a config: one entry per source whose kind has an
    /// implementation here. A source for another kind is not an error; it is a
    /// source this build cannot ask, which `config check` reports as
    /// `enabled=0` with the reason.
    pub fn from_config(config: &Config) -> Sources {
        let entries = config
            .sources
            .iter()
            .filter_map(|source| {
                build(source).map(|built| Entry {
                    config: source.clone(),
                    source: built,
                })
            })
            .collect();
        Sources { entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.config.id == id)
    }

    /// The weights of the sources in this table, which is what
    /// [`crate::pipeline::weighted_order`] draws against. A source this build
    /// cannot serve has no weight here: it can never be picked.
    pub fn weights(&self) -> Vec<u32> {
        self.entries
            .iter()
            .map(|entry| entry.config.weight)
            .collect()
    }

    /// A table built by hand, for a test that must exercise the pipeline without
    /// a shipped `kind`. Only tests can build one, because only tests may.
    #[cfg(test)]
    pub fn of(entries: Vec<Entry>) -> Sources {
        Sources { entries }
    }
}

/// The dispatch table of features.md 2.6: one arm per kind, no logic. Both arms
/// answer `None` until their card lands.
fn build(source: &SourceConfig) -> Option<Box<dyn Source>> {
    match source.kind {
        SourceKind::Local => None,
        SourceKind::Wallhaven => None,
    }
}

/// Why a source could not be asked, for `enabled=0 reason=<...>`
/// (docs/architecture.md 4.3).
pub fn missing_reason(source: &SourceConfig) -> String {
    format!(
        "no implementation for kind {} in this build",
        source.kind.as_str()
    )
}
