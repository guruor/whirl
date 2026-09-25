//! The stages of a rotation, in the order docs/spec/features.md fixes them.
//!
//! **What is real here and what is not.** The stdout contract of
//! docs/architecture.md 1.6 is real: at most two lines, `downloaded:` then
//! `set:`, and the second one only after the setter returned success. The shape
//! of the pipeline is real: a candidate, then one call to the backend boundary,
//! then the report. Everything between the candidate and that call is a
//! placeholder, because no source implementation exists yet:
//!
//! - the candidate is synthetic. It is the first enabled source's first path
//!   with a fixed leaf, and nothing is read from it;
//! - there is no download, so `downloaded:` reports the same digest and path as
//!   `set:`;
//! - no filter is applied (the pipeline filters are a later card);
//! - nothing is written to the cache: the scaffold has no cache.
//!
//! The digest is therefore a placeholder too, and a real one: it is the sha256
//! of the `origin_key`, not of the file's bytes, because there are no bytes.

use crate::backend::{self, SetError};
use whirl_core::config::{Backend, Config, SourceConfig, SourceKind, paths};
use whirl_core::protocol::{self, ErrorCode, SourceRecord};

/// A failed stage: what the daemon turns into the `ERR` code of 2.7.
#[derive(Debug, Clone)]
pub struct Failure {
    /// The argv stage name: `config`, `backend`, `source`, `set`.
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

/// `--verb rotate`: pick a candidate and set it.
pub fn rotate(config: &Config, backend: Backend) -> Result<Report, Failure> {
    let (source, path) = placeholder_candidate(config)?;
    run_to_set(source, path, backend)
}

/// `--verb set --target <path|id>`: the candidate is already chosen, by the
/// client (`set path`, `set id`) or by the daemon (`prev`), so there is nothing
/// to select. `run` is unused until the cache names part files after it.
pub fn set(
    config: &Config,
    backend: Backend,
    target: Option<&str>,
    run: u64,
) -> Result<Report, Failure> {
    let target = target.ok_or_else(|| {
        Failure::new(
            "cli",
            ErrorCode::BadArgs,
            format!("--verb set needs --target (run {run})"),
        )
    })?;
    if looks_like_a_path(target) {
        let source = first_enabled(config)?;
        return run_to_set(source, absolute(target, &source.id)?, backend);
    }
    // An id: `<source id>:<source-scoped id>` (2.5). The scaffold cannot look
    // the bytes up, so it keeps the id and rebuilds a placeholder path under the
    // source that owns the prefix.
    let source = source_for(config, target)?;
    let id = target.to_string();
    let path = placeholder_path(source)?;
    let digest = protocol::sha256_hex(id.as_bytes());
    emit_set(&digest, &id, &path, backend)
}

/// `--verb check`: the source and plan records `whirl config check` forwards.
/// The architecture fixes the rotate contract, not this output
/// (docs/development.md section 7).
///
/// `backend` is the resolved backend and not `config.backend`: 2.6's plan line
/// carries effective values, "what did the daemon actually adopt", and the two
/// differ whenever 4.3's precedence let `WHIRL_BACKEND` or `--backend` win over
/// the file.
pub fn check(config: &Config, backend: Backend) {
    for source in &config.sources {
        println!("{}", source_record(source).line());
    }
    println!("{}", protocol::plan_record(&plan_pairs(config, backend)));
}

/// The `source:` record of a configured source, from the one implementation in
/// `whirl-core` (docs/architecture.md 2.6). `last` is always `-` here: the
/// scaffold keeps no per-source outcome, and `config check` reports counters in
/// the bracketed group only.
pub fn source_record(source: &SourceConfig) -> SourceRecord {
    source.record(None)
}

/// The effective values, as the config's own dotted key paths in file order
/// (2.6). `display.mode_effective` is what the platform actually gets; with no
/// platform code yet it is the configured mode, which is honest only for `all`
/// and is why 3.7's fallback is a later card.
///
/// `backend` is passed in rather than read from `config.backend`: 2.6's values
/// are effective values, and the resolved backend is what the run adopted (4.3).
pub fn plan_pairs(config: &Config, backend: Backend) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut push = |key: &str, value: String| pairs.push((key.to_string(), value));
    push(
        "schedule.interval_seconds",
        config.schedule.interval_seconds.to_string(),
    );
    push(
        "schedule.worker_deadline_seconds",
        config.schedule.worker_deadline_seconds.to_string(),
    );
    push(
        "startup.enabled",
        u8::from(config.startup.enabled).to_string(),
    );
    push("startup.mode", config.startup.mode.as_str().to_string());
    push(
        "startup.respect_manual",
        u8::from(config.startup.respect_manual).to_string(),
    );
    push("display.mode", config.display.mode.as_str().to_string());
    push(
        "display.mode_effective",
        config.display.mode.as_str().to_string(),
    );
    push("min_width", config.min_width.to_string());
    push("min_height", config.min_height.to_string());
    push("filters.max_bytes", config.filters.max_bytes.to_string());
    push(
        "filters.ratio_tolerance",
        config.filters.ratio_tolerance.to_string(),
    );
    push(
        "filters.target_ratio",
        match config.filters.target_ratio {
            Some(ratio) => ratio.to_string(),
            None => "-".to_string(),
        },
    );
    push(
        "state.history_entries",
        config.state.history_entries.to_string(),
    );
    push(
        "dedupe.recent_entries",
        config.dedupe.recent_entries.to_string(),
    );
    push(
        "cache.root",
        match &config.cache.root {
            Some(root) => root.display().to_string(),
            None => "-".to_string(),
        },
    );
    push("cache.max_bytes", config.cache.max_bytes.to_string());
    push("cache.max_files", config.cache.max_files.to_string());
    push(
        "cache.grace_seconds",
        config.cache.grace_seconds.to_string(),
    );
    push(
        "cache.orphan_grace_seconds",
        config.cache.orphan_grace_seconds.to_string(),
    );
    push("backend", backend.as_str().to_string());
    push(
        "sources",
        config
            .sources
            .iter()
            .filter(|source| source.weight > 0)
            .count()
            .to_string(),
    );
    pairs
}

/// `downloaded:` then the setter, then `set:` (docs/architecture.md 1.6).
fn run_to_set(source: &SourceConfig, path: String, backend: Backend) -> Result<Report, Failure> {
    let origin_key = format!("{}:{}", source.id, protocol::sha256_hex(path.as_bytes()));
    let digest = protocol::sha256_hex(origin_key.as_bytes());
    emit_set(&digest, &origin_key, &path, backend)
}

fn emit_set(
    digest: &str,
    origin_key: &str,
    path: &str,
    backend: Backend,
) -> Result<Report, Failure> {
    // The rename happened before this line in a real worker; here the line is
    // the whole download stage.
    println!("downloaded: {digest} {path}");
    backend::set(backend, path)
        .map_err(|error: SetError| Failure::new("set", error.code, error.message))?;
    println!("set: {digest} {origin_key} {path}");
    Ok(Report {
        digest: digest.to_string(),
        origin_key: origin_key.to_string(),
        path: path.to_string(),
    })
}

/// The first enabled source in config order: weight 0 disables an entry
/// (docs/architecture.md 4.2), and file order is what makes the choice
/// predictable.
fn first_enabled(config: &Config) -> Result<&SourceConfig, Failure> {
    config
        .sources
        .iter()
        .find(|source| source.weight > 0)
        .ok_or_else(|| {
            Failure::new(
                "source",
                ErrorCode::NoCandidates,
                "no source is enabled: every entry in `sources` has weight 0",
            )
        })
}

/// The source a `set id` target belongs to, by the `<source id>:` prefix (2.5).
fn source_for<'a>(config: &'a Config, target: &str) -> Result<&'a SourceConfig, Failure> {
    let prefix = target.split_once(':').map(|(prefix, _)| prefix);
    let found = prefix.and_then(|prefix| {
        config
            .sources
            .iter()
            .find(|source| source.id == prefix && source.weight > 0)
    });
    found
        .or_else(|| config.sources.iter().find(|source| source.weight > 0))
        .ok_or_else(|| {
            Failure::new(
                "source",
                ErrorCode::NoCandidates,
                "no source is enabled: every entry in `sources` has weight 0",
            )
        })
}

/// The scaffold's candidate: a synthetic path under an enabled source. Nothing
/// is read from it, which is why the kind has to be `local`: a `wallhaven`
/// source needs the network and this build has no HTTP client.
fn placeholder_candidate(config: &Config) -> Result<(&SourceConfig, String), Failure> {
    let source = first_enabled(config)?;
    let path = placeholder_path(source)?;
    Ok((source, path))
}

fn placeholder_path(source: &SourceConfig) -> Result<String, Failure> {
    if source.kind != SourceKind::Local {
        return Err(Failure::new(
            "source",
            ErrorCode::NoCandidates,
            format!(
                "source {} is a {} source and this scaffold has no HTTP client, so it cannot produce a candidate",
                source.id,
                source.kind.as_str()
            ),
        ));
    }
    let local = source.local.as_ref().ok_or_else(|| {
        Failure::new(
            "source",
            ErrorCode::BadConfig,
            format!("source {} has no local schema", source.id),
        )
    })?;
    let first = local.paths.first().ok_or_else(|| {
        Failure::new(
            "source",
            ErrorCode::BadConfig,
            format!("source {} has no paths", source.id),
        )
    })?;
    let base = absolute(first, &source.id)?;
    Ok(format!(
        "{}/whirl-scaffold-candidate.jpg",
        base.trim_end_matches('/')
    ))
}

/// `~` is the user's home directory, as docs/spec/features.md 2.2 has it, and a
/// record's path is absolute on POSIX (2.6).
fn absolute(path: &str, source_id: &str) -> Result<String, Failure> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = paths::home().ok_or_else(|| {
            Failure::new(
                "source",
                ErrorCode::BadConfig,
                "`~` cannot be expanded: HOME is not set",
            )
        })?;
        return Ok(format!("{}/{}", home.display(), rest));
    }
    if path.starts_with('/') || looks_like_a_windows_path(path) {
        return Ok(path.to_string());
    }
    Err(Failure::new(
        "source",
        ErrorCode::BadConfig,
        format!("source {source_id} path {path:?} is not absolute"),
    ))
}

fn looks_like_a_path(value: &str) -> bool {
    value.starts_with('/') || value.starts_with("~/") || looks_like_a_windows_path(value)
}

fn looks_like_a_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}
