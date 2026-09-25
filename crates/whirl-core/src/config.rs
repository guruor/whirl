//! The config schema, its validation, and the platform default paths.
//!
//! The schema is docs/spec/features.md Part 2; the file format, the defaults, the
//! precedence and the validation rules are docs/architecture.md 4.1 to 4.3. There
//! is exactly one implementation of all of it, used by the daemon, the worker and
//! `whirl config check`, because a config error the user must fix by hand cannot
//! have two different messages (docs/development.md section 1).
//!
//! Three properties this module owns, each because a document requires it:
//!
//! - **Zero dependencies.** The JSON reader in [`json`] is ours. It tracks line
//!   numbers, because every error and every warning has to name the line the
//!   offender is on, and it decodes the full string escape set, because Windows
//!   paths are backslash-heavy (docs/architecture.md 4.1).
//! - **`_`-prefixed keys are comments**, at every level, and are ignored by every
//!   whirl parser (docs/architecture.md 4.1).
//! - **An unknown field is a warning; a value out of range, an unknown `kind` or a
//!   missing required field is a refusal**, naming the offending key
//!   (docs/spec/features.md 2.0, docs/architecture.md 4.3).

use crate::source::Capability;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The config schema version this build writes and accepts up to
/// (docs/architecture.md 4.2, `config_schema`).
pub const CONFIG_SCHEMA: i64 = 1;

/// The source kinds the schema knows. Also the closed set an unknown `kind` is
/// reported against (docs/spec/features.md 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    Local,
    Wallhaven,
}

impl SourceKind {
    pub const ALL: [SourceKind; 2] = [SourceKind::Local, SourceKind::Wallhaven];

    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Local => "local",
            SourceKind::Wallhaven => "wallhaven",
        }
    }

    pub fn parse(name: &str) -> Option<SourceKind> {
        SourceKind::ALL.iter().copied().find(|k| k.as_str() == name)
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A config problem that refuses startup, or a `whirl config check` failure.
///
/// `field` is the offending key as a dotted path with array indices
/// (`sources[0].kind`), and `line` is its 1-based line in the file, or 0 when the
/// value came from a compiled default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub field: Option<String>,
    pub line: usize,
    pub message: String,
}

impl ConfigError {
    /// An error about a named field at a line.
    pub fn new(field: impl Into<String>, line: usize, message: impl Into<String>) -> ConfigError {
        ConfigError {
            field: Some(field.into()),
            line,
            message: message.into(),
        }
    }

    /// An error about the document itself, which has no field.
    pub fn syntax(line: usize, message: impl Into<String>) -> ConfigError {
        ConfigError {
            field: None,
            line,
            message: message.into(),
        }
    }

    /// The same error, with the config file's path in front of the message.
    pub fn with_path(mut self, path: &Path) -> ConfigError {
        self.message = format!("{}: {}", path.display(), self.message);
        self
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.field, self.line) {
            (Some(field), 0) => write!(f, "{field} (compiled default): {}", self.message),
            (Some(field), line) => write!(f, "{field} (line {line}): {}", self.message),
            (None, line) => write!(f, "line {line}: {}", self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Something accepted but worth saying out loud: an unknown field, a deprecated
/// alias, a schema newer than this build. Warnings never stop a wallpaper rotator
/// from starting (docs/architecture.md 4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub field: String,
    pub line: usize,
    pub message: String,
}

impl ConfigWarning {
    pub fn new(field: impl Into<String>, line: usize, message: impl Into<String>) -> ConfigWarning {
        ConfigWarning {
            field: field.into(),
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (line {}): {}", self.field, self.line, self.message)
    }
}

/// A parsed config, plus everything the parse wants to report without failing.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<ConfigWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    pub const ALL: [LogLevel; 5] = [
        LogLevel::Off,
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }

    pub fn parse(name: &str) -> Option<LogLevel> {
        LogLevel::ALL.iter().copied().find(|l| l.as_str() == name)
    }
}

/// `startup.mode`: re-apply the newest history entry, or rotate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    Last,
    Rotate,
}

impl StartupMode {
    pub const ALL: [StartupMode; 2] = [StartupMode::Last, StartupMode::Rotate];

    pub fn as_str(self) -> &'static str {
        match self {
            StartupMode::Last => "last",
            StartupMode::Rotate => "rotate",
        }
    }

    pub fn parse(name: &str) -> Option<StartupMode> {
        StartupMode::ALL
            .iter()
            .copied()
            .find(|m| m.as_str() == name)
    }
}

/// `display.mode`. `per-display` is accepted everywhere and refused per platform
/// at rotation time, which is `[D 5 §1.3]`'s fallback (docs/architecture.md 3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    All,
    PerDisplay,
}

impl DisplayMode {
    pub const ALL: [DisplayMode; 2] = [DisplayMode::All, DisplayMode::PerDisplay];

    pub fn as_str(self) -> &'static str {
        match self {
            DisplayMode::All => "all",
            DisplayMode::PerDisplay => "per-display",
        }
    }

    pub fn parse(name: &str) -> Option<DisplayMode> {
        DisplayMode::ALL
            .iter()
            .copied()
            .find(|m| m.as_str() == name)
    }
}

/// The wallpaper backend. `noop` runs every stage except the platform setter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Native,
    Noop,
}

impl Backend {
    pub const ALL: [Backend; 2] = [Backend::Native, Backend::Noop];

    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Native => "native",
            Backend::Noop => "noop",
        }
    }

    pub fn parse(name: &str) -> Option<Backend> {
        Backend::ALL.iter().copied().find(|b| b.as_str() == name)
    }
}

/// How a `local` source hands a file to the platform setter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMode {
    Reference,
    Copy,
}

impl LocalMode {
    pub const ALL: [LocalMode; 2] = [LocalMode::Reference, LocalMode::Copy];

    pub fn as_str(self) -> &'static str {
        match self {
            LocalMode::Reference => "reference",
            LocalMode::Copy => "copy",
        }
    }

    pub fn parse(name: &str) -> Option<LocalMode> {
        LocalMode::ALL.iter().copied().find(|m| m.as_str() == name)
    }
}

/// `schleude` and `filters` in one place: the daemon's clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub interval_seconds: u64,
    pub worker_deadline_seconds: u64,
}

/// `startup.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Startup {
    pub enabled: bool,
    pub mode: StartupMode,
    pub respect_manual: bool,
}

/// `display.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySection {
    pub mode: DisplayMode,
}

/// `filters.*`.
#[derive(Debug, Clone, PartialEq)]
pub struct Filters {
    pub max_bytes: u64,
    pub ratio_tolerance: f64,
    pub target_ratio: Option<f64>,
}

/// `state.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSection {
    pub history_entries: usize,
}

/// `dedupe.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dedupe {
    pub recent_entries: usize,
}

/// `cache.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    pub root: Option<PathBuf>,
    pub max_bytes: u64,
    pub max_files: u64,
    pub grace_seconds: u64,
    pub orphan_grace_seconds: u64,
}

/// The config, parsed and validated.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub config_schema: i64,
    /// `socket`: `None` means the platform default.
    pub socket: Option<PathBuf>,
    pub log_level: LogLevel,
    pub schedule: Schedule,
    pub startup: Startup,
    pub display: DisplaySection,
    pub min_width: u32,
    pub min_height: u32,
    pub filters: Filters,
    pub state: StateSection,
    pub dedupe: Dedupe,
    pub cache: Cache,
    pub backend: Backend,
    pub sources: Vec<SourceConfig>,
}

impl Default for Config {
    /// Every default is the value docs/architecture.md 4.2 prints, so the annotated
    /// file and the compiled defaults cannot drift.
    fn default() -> Config {
        Config {
            config_schema: CONFIG_SCHEMA,
            socket: None,
            log_level: LogLevel::Info,
            schedule: Schedule {
                interval_seconds: 1800,
                worker_deadline_seconds: 300,
            },
            startup: Startup {
                enabled: true,
                mode: StartupMode::Last,
                respect_manual: true,
            },
            display: DisplaySection {
                mode: DisplayMode::All,
            },
            min_width: 1600,
            min_height: 900,
            filters: Filters {
                max_bytes: 41_943_040,
                ratio_tolerance: 0.02,
                target_ratio: None,
            },
            state: StateSection {
                history_entries: 50,
            },
            dedupe: Dedupe { recent_entries: 50 },
            cache: Cache {
                root: None,
                max_bytes: 2_147_483_648,
                max_files: 500,
                grace_seconds: 600,
                orphan_grace_seconds: 300,
            },
            backend: Backend::Native,
            sources: Vec::new(),
        }
    }
}

/// One source entry: the shared fields of docs/spec/features.md 2.1 plus the
/// schema of its own kind.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceConfig {
    pub id: String,
    pub kind: SourceKind,
    pub weight: u32,
    /// What the source can answer. Defaults to the kind's own list and may be
    /// narrowed, never widened (docs/spec/features.md 2.5).
    pub capabilities: Vec<Capability>,
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
    pub max_bytes: Option<u64>,
    pub local: Option<LocalSource>,
    pub wallhaven: Option<WallhavenSource>,
}

impl SourceConfig {
    /// The `source:` record of docs/architecture.md 2.6, in the one place, so
    /// `status`, `sources` and `config check` cannot drift. `last` is the
    /// source's most recent outcome; this build keeps none and passes `None`,
    /// which prints `last=-`.
    pub fn record(&self, last: Option<&str>) -> crate::protocol::SourceRecord {
        crate::protocol::SourceRecord {
            id: self.id.clone(),
            kind: self.kind.as_str().to_string(),
            weight: self.weight,
            enabled: self.weight > 0,
            last: last.map(str::to_string),
            counters: Vec::new(),
            reason: None,
        }
    }

    pub fn capabilities_set(&self) -> crate::source::FilterSet {
        crate::source::FilterSet::of(&self.capabilities)
    }

    /// The capabilities a kind implements, which a config may narrow
    /// (docs/spec/features.md 2.5).
    pub fn default_capabilities(kind: SourceKind) -> Vec<Capability> {
        match kind {
            SourceKind::Local => vec![Capability::Resolution, Capability::Extension],
            SourceKind::Wallhaven => vec![
                Capability::Resolution,
                Capability::Ratio,
                Capability::Purity,
                Capability::Colors,
                Capability::Category,
            ],
        }
    }
}

/// The `local` source schema (docs/spec/features.md 2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSource {
    pub paths: Vec<String>,
    pub recursive: bool,
    pub max_depth: u32,
    pub follow_symlinks: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub mode: LocalMode,
}

impl Default for LocalSource {
    fn default() -> LocalSource {
        LocalSource {
            paths: Vec::new(),
            recursive: true,
            max_depth: 8,
            follow_symlinks: false,
            include: ["*.jpg", "*.jpeg", "*.png", "*.heic", "*.webp"]
                .map(str::to_string)
                .to_vec(),
            exclude: ["*/.git/*", "*/screenshots/*", "*.tmp"]
                .map(str::to_string)
                .to_vec(),
            mode: LocalMode::Reference,
        }
    }
}

/// The `wallhaven` source schema (docs/spec/features.md 2.3). Every key is a
/// documented query parameter, so the names here are the API's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallhavenSource {
    pub query: Option<String>,
    pub categories: String,
    pub purity: String,
    pub sorting: String,
    pub order: String,
    pub ratios: Option<String>,
    pub atleast: Option<String>,
    pub colors: Option<String>,
    pub top_range: Option<String>,
    pub collection: Option<String>,
    pub pages: u32,
    /// A name, never a value (docs/spec/features.md 2.4).
    pub api_key_ref: Option<String>,
}

impl Default for WallhavenSource {
    fn default() -> WallhavenSource {
        WallhavenSource {
            query: None,
            categories: "111".to_string(),
            purity: "100".to_string(),
            sorting: "random".to_string(),
            order: "desc".to_string(),
            ratios: None,
            atleast: None,
            colors: None,
            top_range: None,
            collection: None,
            pages: 1,
            api_key_ref: None,
        }
    }
}

impl Config {
    /// Parse and validate a config document.
    ///
    /// Text in, values out: reading the file is the caller's job, because
    /// `whirl-core` does no I/O (docs/architecture.md 1.2). The daemon reads the
    /// file and adds the path to any error with [`ConfigError::with_path`], so the
    /// user sees the same message whichever side of the pipe failed.
    pub fn parse(text: &str) -> Result<LoadedConfig, ConfigError> {
        let root = json::parse(text)?;
        let mut warnings = Vec::new();
        let config = parse_root(&root, &mut warnings)?;
        Ok(LoadedConfig { config, warnings })
    }

    /// The annotated default file the daemon writes when `config.json` is missing
    /// (docs/architecture.md 4.2). It is the document's own example, verbatim, so
    /// that the file a user gets and the file the architecture prints are the same
    /// file.
    pub fn default_config_json() -> &'static str {
        DEFAULT_CONFIG_JSON
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// The line each key in the file was found on, so that an ordering rule violated
/// by two keys can name both lines.
type Lines = BTreeMap<String, usize>;

fn at(field: &str, lines: &Lines) -> String {
    match lines.get(field) {
        Some(line) => format!(" (line {line})"),
        None => " (compiled default)".to_string(),
    }
}

fn parse_root(root: &json::Node, warnings: &mut Vec<ConfigWarning>) -> Result<Config, ConfigError> {
    let entries = root.as_object().ok_or_else(|| {
        // The document's own shape, not a field: there is no key to name.
        ConfigError::syntax(
            root.line,
            format!(
                "the config must be a JSON object, found a {}",
                root.kind_name()
            ),
        )
    })?;
    let mut lines = Lines::new();
    let mut reader = ObjReader::new("", entries, &mut lines);
    let mut config = Config::default();

    if let Some(node) = reader.field("config_schema") {
        let field = reader.path("config_schema");
        config.config_schema = want_int(node, &field)?;
        if config.config_schema > CONFIG_SCHEMA {
            warnings.push(ConfigWarning::new(
                field,
                node.line,
                format!(
                    "config_schema {} is newer than this build's {CONFIG_SCHEMA}; the file is \
                     read, reported as state_schema_newer, and never rewritten",
                    config.config_schema
                ),
            ));
        }
    }
    if let Some(node) = reader.field("socket") {
        let field = reader.path("socket");
        config.socket = want_opt_path(node, &field)?;
    }
    if let Some(node) = reader.field("log_level") {
        let field = reader.path("log_level");
        config.log_level = want_enum(
            node,
            &field,
            &LogLevel::ALL.map(LogLevel::as_str),
            LogLevel::parse,
        )?;
    }
    if let Some(node) = reader.field("schedule") {
        let field = reader.path("schedule");
        let mut schedule = ObjReader::child(&field, node, reader.lines())?;
        if let Some(node) = schedule.field("interval_seconds") {
            let field = schedule.path("interval_seconds");
            config.schedule.interval_seconds = want_u64(node, &field)?;
        }
        if let Some(node) = schedule.field("worker_deadline_seconds") {
            let field = schedule.path("worker_deadline_seconds");
            config.schedule.worker_deadline_seconds = want_u64(node, &field)?;
        }
        schedule.finish(warnings);
    }
    if let Some(node) = reader.field("startup") {
        let field = reader.path("startup");
        let mut startup = ObjReader::child(&field, node, reader.lines())?;
        if let Some(node) = startup.field("enabled") {
            let field = startup.path("enabled");
            config.startup.enabled = want_bool(node, &field)?;
        }
        if let Some(node) = startup.field("mode") {
            let field = startup.path("mode");
            config.startup.mode = want_enum(
                node,
                &field,
                &StartupMode::ALL.map(StartupMode::as_str),
                StartupMode::parse,
            )?;
        }
        if let Some(node) = startup.field("respect_manual") {
            let field = startup.path("respect_manual");
            config.startup.respect_manual = want_bool(node, &field)?;
        }
        startup.finish(warnings);
    }
    if let Some(node) = reader.field("display") {
        let field = reader.path("display");
        let mut display = ObjReader::child(&field, node, reader.lines())?;
        if let Some(node) = display.field("mode") {
            let field = display.path("mode");
            config.display.mode = want_enum(
                node,
                &field,
                &DisplayMode::ALL.map(DisplayMode::as_str),
                DisplayMode::parse,
            )?;
        }
        display.finish(warnings);
    }
    if let Some(node) = reader.field("min_width") {
        let field = reader.path("min_width");
        config.min_width = want_u32(node, &field)?;
    }
    if let Some(node) = reader.field("min_height") {
        let field = reader.path("min_height");
        config.min_height = want_u32(node, &field)?;
    }
    if let Some(node) = reader.field("filters") {
        let field = reader.path("filters");
        let mut filters = ObjReader::child(&field, node, reader.lines())?;
        if let Some(node) = filters.field("max_bytes") {
            let field = filters.path("max_bytes");
            config.filters.max_bytes = want_u64(node, &field)?;
        }
        if let Some(node) = filters.field("ratio_tolerance") {
            let field = filters.path("ratio_tolerance");
            config.filters.ratio_tolerance = want_f64(node, &field)?;
            if config.filters.ratio_tolerance < 0.0 {
                return Err(ConfigError::new(
                    field,
                    node.line,
                    "ratio_tolerance is a fraction of the target ratio and cannot be negative",
                ));
            }
        }
        if let Some(node) = filters.field("target_ratio") {
            let field = filters.path("target_ratio");
            config.filters.target_ratio = want_opt_f64(node, &field)?;
        }
        filters.finish(warnings);
    }
    if let Some(node) = reader.field("state") {
        let field = reader.path("state");
        let mut state = ObjReader::child(&field, node, reader.lines())?;
        if let Some(node) = state.field("history_entries") {
            let field = state.path("history_entries");
            config.state.history_entries = want_usize(node, &field)?;
        }
        state.finish(warnings);
    }
    if let Some(node) = reader.field("dedupe") {
        let field = reader.path("dedupe");
        let mut dedupe = ObjReader::child(&field, node, reader.lines())?;
        if let Some(node) = dedupe.field("recent_entries") {
            let field = dedupe.path("recent_entries");
            config.dedupe.recent_entries = want_usize(node, &field)?;
        }
        dedupe.finish(warnings);
    }
    // `keep` is the legacy name for `cache.max_files` (docs/architecture.md 4.1):
    // accepted, deprecated, never written back. It is a root-level name, and a
    // `keep` inside `cache` is read after this block, so the nearer scope wins if
    // a file carries both.
    if let Some(node) = reader.field("keep") {
        let field = reader.path("keep");
        config.cache.max_files = want_u64(node, &field)?;
        warnings.push(ConfigWarning::new(
            field,
            node.line,
            "`keep` is accepted and deprecated; the key is `cache.max_files`",
        ));
    }
    if let Some(node) = reader.field("cache") {
        let field = reader.path("cache");
        let mut cache = ObjReader::child(&field, node, reader.lines())?;
        // `cache_dir` is the legacy alias for `cache.root`, accepted and deprecated
        // (docs/architecture.md 4.1).
        let root_node = cache
            .field("root")
            .map(|node| (node, false))
            .or_else(|| cache.field("cache_dir").map(|node| (node, true)));
        if let Some((node, deprecated)) = root_node {
            let field = if deprecated {
                cache.path("cache_dir")
            } else {
                cache.path("root")
            };
            config.cache.root = want_opt_path(node, &field)?;
            if let Some(root) = &config.cache.root {
                if !root.is_absolute() {
                    return Err(ConfigError::new(
                        field,
                        node.line,
                        format!(
                            "{} is a relative path; cache.root must be absolute",
                            root.display()
                        ),
                    ));
                }
            }
            if deprecated {
                warnings.push(ConfigWarning::new(
                    field,
                    node.line,
                    "`cache_dir` is accepted and deprecated; the key is `cache.root`",
                ));
            }
        }
        if let Some(node) = cache.field("max_bytes") {
            let field = cache.path("max_bytes");
            config.cache.max_bytes = want_u64(node, &field)?;
        }
        // `keep` is the legacy alias for `cache.max_files`.
        let max_files_node = cache
            .field("max_files")
            .map(|node| (node, false))
            .or_else(|| cache.field("keep").map(|node| (node, true)));
        if let Some((node, deprecated)) = max_files_node {
            let field = if deprecated {
                cache.path("keep")
            } else {
                cache.path("max_files")
            };
            config.cache.max_files = want_u64(node, &field)?;
            if deprecated {
                warnings.push(ConfigWarning::new(
                    field,
                    node.line,
                    "`keep` is accepted and deprecated; the key is `cache.max_files`",
                ));
            }
        }
        if let Some(node) = cache.field("grace_seconds") {
            let field = cache.path("grace_seconds");
            config.cache.grace_seconds = want_u64(node, &field)?;
        }
        if let Some(node) = cache.field("orphan_grace_seconds") {
            let field = cache.path("orphan_grace_seconds");
            config.cache.orphan_grace_seconds = want_u64(node, &field)?;
        }
        cache.finish(warnings);
    }
    if let Some(node) = reader.field("backend") {
        let field = reader.path("backend");
        config.backend = want_enum(
            node,
            &field,
            &Backend::ALL.map(Backend::as_str),
            Backend::parse,
        )?;
    }
    // The sources are parsed after `finish`, so the reader has released its
    // borrow of `lines` before `parse_source` records the line of each of its
    // own keys: two `&mut Lines` cannot be alive at once.
    let mut sources: Option<(String, &json::Node)> = None;
    if let Some(node) = reader.field("sources") {
        sources = Some((reader.path("sources"), node));
    }
    reader.finish(warnings);
    if let Some((field, node)) = sources {
        let elements = node.as_array().ok_or_else(|| {
            ConfigError::new(
                &field,
                node.line,
                format!("expected an array of sources, found a {}", node.kind_name()),
            )
        })?;
        for (index, element) in elements.iter().enumerate() {
            config
                .sources
                .push(parse_source(index, element, &mut lines, warnings)?);
        }
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for source in &config.sources {
            if let Some(first) = seen.insert(source.id.as_str(), 0) {
                let _ = first;
                return Err(ConfigError::new(
                    format!("sources[id={}]", source.id),
                    *lines
                        .get(&format!("sources[id={}].id", source.id))
                        .unwrap_or(&0),
                    format!(
                        "duplicate source id {:?}; ids are unique because an id is the prefix of \
                         every origin_key the source produces",
                        source.id
                    ),
                ));
            }
        }
    }

    validate_ordering(&config, &lines)?;
    Ok(config)
}

fn parse_source(
    index: usize,
    node: &json::Node,
    lines: &mut Lines,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<SourceConfig, ConfigError> {
    let path = format!("sources[{index}]");
    let entries = node.as_object().ok_or_else(|| {
        ConfigError::new(
            &path,
            node.line,
            format!("expected a source object, found a {}", node.kind_name()),
        )
    })?;
    let mut reader = ObjReader::new(&path, entries, lines);

    let id_field = reader.path("id");
    let id = match reader.field("id") {
        Some(node) => {
            let id = want_string(node, &id_field)?;
            if id.is_empty() {
                return Err(ConfigError::new(&id_field, node.line, "id is empty"));
            }
            if id.len() > 64 {
                return Err(ConfigError::new(
                    &id_field,
                    node.line,
                    format!("id is {} bytes; the limit is 64", id.len()),
                ));
            }
            if let Some(bad) = id
                .chars()
                .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')))
            {
                return Err(ConfigError::new(
                    &id_field,
                    node.line,
                    format!(
                        "id {id:?} contains {bad:?}; an id matches [A-Za-z0-9._:-]+ because it is \
                         a non-final field of the `source:` and `entry:` records"
                    ),
                ));
            }
            id
        }
        None => {
            return Err(ConfigError::new(
                &id_field,
                node.line,
                "missing required field `id`",
            ));
        }
    };
    reader
        .lines
        .insert(format!("{path}[id={id}].id"), node.line);

    let kind_field = reader.path("kind");
    let kind = match reader.field("kind") {
        Some(node) => {
            let value = want_string(node, &kind_field)?;
            SourceKind::parse(&value).ok_or_else(|| {
                ConfigError::new(
                    &kind_field,
                    node.line,
                    format!(
                        "unknown source kind {value:?} for source {id:?}; known kinds are {}",
                        known_kinds()
                    ),
                )
            })?
        }
        None => {
            return Err(ConfigError::new(
                &kind_field,
                node.line,
                format!(
                    "missing required field `kind` for source {id:?}; known kinds are {}",
                    known_kinds()
                ),
            ));
        }
    };
    reader
        .lines
        .insert(format!("{path}[id={id}].kind"), node.line);

    let mut source = SourceConfig {
        id,
        kind,
        weight: 1,
        capabilities: SourceConfig::default_capabilities(kind),
        min_width: None,
        min_height: None,
        max_bytes: None,
        local: None,
        wallhaven: None,
    };

    if let Some(node) = reader.field("weight") {
        let field = reader.path("weight");
        let weight = want_int(node, &field)?;
        if weight < 0 {
            return Err(ConfigError::new(
                field,
                node.line,
                format!(
                    "weight {weight} is negative; 0 disables a source, negative is not a value"
                ),
            ));
        }
        source.weight = u32::try_from(weight).map_err(|_| {
            ConfigError::new(
                reader.path("weight"),
                node.line,
                format!("weight {weight} does not fit in 32 bits"),
            )
        })?;
    }
    if let Some(node) = reader.field("capabilities") {
        let field = reader.path("capabilities");
        let names = want_string_array(node, &field)?;
        let mut caps = Vec::new();
        for name in &names {
            let cap = Capability::parse(name).ok_or_else(|| {
                ConfigError::new(
                    &field,
                    node.line,
                    format!(
                        "unknown capability {name:?}; known capabilities are {}",
                        Capability::ALL.map(Capability::as_str).join(", ")
                    ),
                )
            })?;
            if !SourceConfig::default_capabilities(kind).contains(&cap) {
                return Err(ConfigError::new(
                    &field,
                    node.line,
                    format!(
                        "source {:?} of kind {kind} cannot declare the capability \
                         {name:?}; it may narrow {}, never widen it",
                        source.id,
                        SourceConfig::default_capabilities(kind)
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            caps.push(cap);
        }
        source.capabilities = caps;
    }
    if let Some(node) = reader.field("min_width") {
        let field = reader.path("min_width");
        source.min_width = Some(want_u32(node, &field)?);
    }
    if let Some(node) = reader.field("min_height") {
        let field = reader.path("min_height");
        source.min_height = Some(want_u32(node, &field)?);
    }
    if let Some(node) = reader.field("max_bytes") {
        let field = reader.path("max_bytes");
        source.max_bytes = Some(want_u64(node, &field)?);
    }
    match kind {
        SourceKind::Local => source.local = Some(parse_local(&mut reader, node)?),
        SourceKind::Wallhaven => source.wallhaven = Some(parse_wallhaven(&mut reader)?),
    }
    // A key the schema does not have is a warning, not a refusal (4.3), and a
    // source's keys belong to the kind it declared, so an unknown one is
    // reported with the source's path: `sources[0].purity`.
    reader.finish(warnings);
    Ok(source)
}

fn known_kinds() -> String {
    SourceKind::ALL
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_local(
    reader: &mut ObjReader<'_, '_>,
    node: &json::Node,
) -> Result<LocalSource, ConfigError> {
    let mut local = LocalSource::default();
    let paths_field = reader.path("paths");
    match reader.field("paths") {
        Some(node) => {
            let paths = want_string_array(node, &paths_field)?;
            if paths.is_empty() {
                return Err(ConfigError::new(
                    &paths_field,
                    node.line,
                    "the `paths` array is empty; a local source needs at least one directory",
                ));
            }
            for path in &paths {
                if path.is_empty() {
                    return Err(ConfigError::new(
                        &paths_field,
                        node.line,
                        "a `paths` entry is empty",
                    ));
                }
            }
            local.paths = paths;
        }
        None => {
            return Err(ConfigError::new(
                &paths_field,
                node.line,
                "missing required field `paths` for a local source",
            ));
        }
    }
    if let Some(node) = reader.field("recursive") {
        let field = reader.path("recursive");
        local.recursive = want_bool(node, &field)?;
    }
    if let Some(node) = reader.field("max_depth") {
        let field = reader.path("max_depth");
        let depth = want_int(node, &field)?;
        if depth < 1 {
            return Err(ConfigError::new(
                field,
                node.line,
                format!("max_depth {depth} is not a bound; it must be at least 1"),
            ));
        }
        local.max_depth = u32::try_from(depth).unwrap_or(u32::MAX);
    }
    if let Some(node) = reader.field("follow_symlinks") {
        let field = reader.path("follow_symlinks");
        local.follow_symlinks = want_bool(node, &field)?;
    }
    if let Some(node) = reader.field("include") {
        let field = reader.path("include");
        local.include = want_string_array(node, &field)?;
    }
    if let Some(node) = reader.field("exclude") {
        let field = reader.path("exclude");
        local.exclude = want_string_array(node, &field)?;
    }
    if let Some(node) = reader.field("mode") {
        let field = reader.path("mode");
        local.mode = want_enum(
            node,
            &field,
            &LocalMode::ALL.map(LocalMode::as_str),
            LocalMode::parse,
        )?;
    }
    Ok(local)
}

fn parse_wallhaven(reader: &mut ObjReader<'_, '_>) -> Result<WallhavenSource, ConfigError> {
    let mut wallhaven = WallhavenSource::default();
    if let Some(node) = reader.field("query") {
        let field = reader.path("query");
        wallhaven.query = want_opt_string(node, &field)?;
    }
    if let Some(node) = reader.field("categories") {
        let field = reader.path("categories");
        wallhaven.categories = want_flags(node, &field, 3)?;
    }
    if let Some(node) = reader.field("purity") {
        let field = reader.path("purity");
        wallhaven.purity = want_flags(node, &field, 3)?;
    }
    if let Some(node) = reader.field("sorting") {
        let field = reader.path("sorting");
        wallhaven.sorting = want_enum(
            node,
            &field,
            &[
                "date_added",
                "relevance",
                "random",
                "views",
                "favorites",
                "toplist",
            ],
            |name| {
                [
                    "date_added",
                    "relevance",
                    "random",
                    "views",
                    "favorites",
                    "toplist",
                ]
                .iter()
                .copied()
                .find(|s| *s == name)
                .map(str::to_string)
            },
        )?;
    }
    if let Some(node) = reader.field("order") {
        let field = reader.path("order");
        wallhaven.order = want_enum(node, &field, &["desc", "asc"], |name| {
            ["desc", "asc"]
                .iter()
                .copied()
                .find(|o| *o == name)
                .map(str::to_string)
        })?;
    }
    if let Some(node) = reader.field("ratios") {
        let field = reader.path("ratios");
        wallhaven.ratios = want_opt_string(node, &field)?;
    }
    if let Some(node) = reader.field("atleast") {
        let field = reader.path("atleast");
        wallhaven.atleast = want_opt_string(node, &field)?;
    }
    if let Some(node) = reader.field("colors") {
        let field = reader.path("colors");
        wallhaven.colors = want_opt_string(node, &field)?;
    }
    if let Some(node) = reader.field("top_range") {
        let field = reader.path("top_range");
        wallhaven.top_range = want_opt_string(node, &field)?;
    }
    if let Some(node) = reader.field("collection") {
        let field = reader.path("collection");
        wallhaven.collection = want_opt_string(node, &field)?;
    }
    if let Some(node) = reader.field("pages") {
        let field = reader.path("pages");
        let pages = want_int(node, &field)?;
        if !(1..=5).contains(&pages) {
            return Err(ConfigError::new(
                field,
                node.line,
                format!(
                    "pages {pages} is outside 1..=5; one page is 24 results and the API allows 45 \
                     requests per minute"
                ),
            ));
        }
        wallhaven.pages = u32::try_from(pages).unwrap_or(1);
    }
    if let Some(node) = reader.field("api_key_ref") {
        let field = reader.path("api_key_ref");
        let value = want_opt_string(node, &field)?;
        if let Some(value) = &value {
            // A key in a config file is a key in every backup of that file
            // (docs/spec/features.md 2.4): the value must be a label.
            if looks_like_a_key(value) {
                return Err(ConfigError::new(
                    &field,
                    node.line,
                    "api_key_ref holds a NAME, never a value; a key in the config file is a key in \
                     every backup of it. Use WHIRL_WALLHAVEN_API_KEY or the platform's own store",
                ));
            }
        }
        wallhaven.api_key_ref = value;
    }
    Ok(wallhaven)
}

/// A 32-hex-character-or-longer run of key-ish characters is treated as a value,
/// not as a label. Labels are short and dotted or colon-prefixed
/// (`keychain:whirl-wallhaven`).
fn looks_like_a_key(value: &str) -> bool {
    value.len() >= 32
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        && value.chars().any(|c| c.is_ascii_digit())
}

/// A `categories`/`purity` style flag string: exactly `width` characters of 0/1.
fn want_flags(node: &json::Node, field: &str, width: usize) -> Result<String, ConfigError> {
    let value = want_string(node, field)?;
    if value.len() != width || !value.chars().all(|c| c == '0' || c == '1') {
        return Err(ConfigError::new(
            field,
            node.line,
            format!("{value:?} is not {width} flags of 0 or 1"),
        ));
    }
    Ok(value)
}

/// The ordering rules of docs/architecture.md 4.3. Each violation names the key,
/// its line, and the value it was compared against.
fn validate_ordering(config: &Config, lines: &Lines) -> Result<(), ConfigError> {
    if config.cache.max_bytes < config.filters.max_bytes {
        return Err(ConfigError::new(
            "cache.max_bytes",
            *lines.get("cache.max_bytes").unwrap_or(&0),
            format!(
                "{} is less than filters.max_bytes = {}{}; the cache must hold at least one \
                 admissible image",
                config.cache.max_bytes,
                config.filters.max_bytes,
                at("filters.max_bytes", lines)
            ),
        ));
    }
    if config.cache.max_files < 2 {
        return Err(ConfigError::new(
            "cache.max_files",
            *lines.get("cache.max_files").unwrap_or(&0),
            format!("{} is less than 2", config.cache.max_files),
        ));
    }
    if config.state.history_entries < 1 {
        return Err(ConfigError::new(
            "state.history_entries",
            *lines.get("state.history_entries").unwrap_or(&0),
            format!("{} is less than 1", config.state.history_entries),
        ));
    }
    if config.dedupe.recent_entries < 1 {
        return Err(ConfigError::new(
            "dedupe.recent_entries",
            *lines.get("dedupe.recent_entries").unwrap_or(&0),
            format!("{} is less than 1", config.dedupe.recent_entries),
        ));
    }
    if config.schedule.interval_seconds < 60 {
        return Err(ConfigError::new(
            "schedule.interval_seconds",
            *lines.get("schedule.interval_seconds").unwrap_or(&0),
            format!("{} is less than 60", config.schedule.interval_seconds),
        ));
    }
    if config.schedule.worker_deadline_seconds < 60 {
        return Err(ConfigError::new(
            "schedule.worker_deadline_seconds",
            *lines.get("schedule.worker_deadline_seconds").unwrap_or(&0),
            format!(
                "{} is less than 60",
                config.schedule.worker_deadline_seconds
            ),
        ));
    }
    Ok(())
}

/// One JSON object, and which of its fields have been consumed.
///
/// `find` records the line of every key it returns, which is what lets an ordering
/// violation name the line of the key that caused it, and `finish` turns every
/// unconsumed key into a warning, which is what turns a typo into a log line
/// instead of a refusal (docs/architecture.md 4.3).
struct ObjReader<'a, 'b> {
    path: String,
    entries: &'a [(String, json::Node)],
    seen: Vec<bool>,
    lines: &'b mut Lines,
}

impl<'a, 'b> ObjReader<'a, 'b> {
    fn new(
        path: &str,
        entries: &'a [(String, json::Node)],
        lines: &'b mut Lines,
    ) -> ObjReader<'a, 'b> {
        ObjReader {
            path: path.to_string(),
            entries,
            seen: vec![false; entries.len()],
            lines,
        }
    }

    fn child(
        path: &str,
        node: &'a json::Node,
        lines: &'b mut Lines,
    ) -> Result<ObjReader<'a, 'b>, ConfigError> {
        let entries = node.as_object().ok_or_else(|| {
            ConfigError::new(
                path,
                node.line,
                format!("expected an object, found a {}", node.kind_name()),
            )
        })?;
        Ok(ObjReader::new(path, entries, lines))
    }

    fn path(&self, key: &str) -> String {
        if self.path.is_empty() {
            key.to_string()
        } else {
            format!("{}.{key}", self.path)
        }
    }

    fn lines(&mut self) -> &mut Lines {
        self.lines
    }

    fn field(&mut self, key: &str) -> Option<&'a json::Node> {
        for (index, (name, value)) in self.entries.iter().enumerate() {
            if name == key && !self.seen[index] {
                self.seen[index] = true;
                self.lines.insert(self.path(key), value.line);
                return Some(value);
            }
        }
        None
    }

    /// A source object's fields belong to whichever kind it declared, so the
    /// unknown-key warning says so rather than listing nothing.
    fn finish(self, warnings: &mut Vec<ConfigWarning>) {
        for (index, (name, value)) in self.entries.iter().enumerate() {
            if self.seen[index] || name.starts_with('_') {
                continue;
            }
            warnings.push(ConfigWarning::new(
                self.path(name),
                value.line,
                "unknown field, ignored: it is not in the config schema",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Typed accessors. Each one names the field and the line when it refuses.
// ---------------------------------------------------------------------------

fn want_string(node: &json::Node, field: &str) -> Result<String, ConfigError> {
    match node.as_str() {
        Some(value) => Ok(value.to_string()),
        None => Err(type_error(node, field, "a string")),
    }
}

fn want_opt_string(node: &json::Node, field: &str) -> Result<Option<String>, ConfigError> {
    if node.is_null() {
        return Ok(None);
    }
    want_string(node, field).map(Some)
}

fn want_opt_path(node: &json::Node, field: &str) -> Result<Option<PathBuf>, ConfigError> {
    Ok(want_opt_string(node, field)?.map(PathBuf::from))
}

fn want_string_array(node: &json::Node, field: &str) -> Result<Vec<String>, ConfigError> {
    let elements = node
        .as_array()
        .ok_or_else(|| type_error(node, field, "an array of strings"))?;
    let mut out = Vec::with_capacity(elements.len());
    for element in elements {
        out.push(want_string(element, field)?);
    }
    Ok(out)
}

fn want_bool(node: &json::Node, field: &str) -> Result<bool, ConfigError> {
    node.as_bool()
        .ok_or_else(|| type_error(node, field, "true or false"))
}

fn want_int(node: &json::Node, field: &str) -> Result<i64, ConfigError> {
    match node.as_num() {
        Some(value) if value.fract() == 0.0 && value.is_finite() => Ok(value as i64),
        Some(value) => Err(ConfigError::new(
            field,
            node.line,
            format!("{value} is not an integer"),
        )),
        None => Err(type_error(node, field, "an integer")),
    }
}

fn want_u32(node: &json::Node, field: &str) -> Result<u32, ConfigError> {
    let value = want_int(node, field)?;
    u32::try_from(value).map_err(|_| {
        ConfigError::new(
            field,
            node.line,
            format!("{value} is outside 0..={}", u32::MAX),
        )
    })
}

fn want_u64(node: &json::Node, field: &str) -> Result<u64, ConfigError> {
    let value = want_int(node, field)?;
    u64::try_from(value).map_err(|_| {
        ConfigError::new(
            field,
            node.line,
            format!("{value} is outside 0..={}", u64::MAX),
        )
    })
}

fn want_usize(node: &json::Node, field: &str) -> Result<usize, ConfigError> {
    let value = want_int(node, field)?;
    usize::try_from(value).map_err(|_| {
        ConfigError::new(
            field,
            node.line,
            format!("{value} is outside 0..={}", usize::MAX),
        )
    })
}

fn want_f64(node: &json::Node, field: &str) -> Result<f64, ConfigError> {
    match node.as_num() {
        Some(value) if value.is_finite() => Ok(value),
        Some(_) => Err(ConfigError::new(
            field,
            node.line,
            "the value is not finite",
        )),
        None => Err(type_error(node, field, "a number")),
    }
}

fn want_opt_f64(node: &json::Node, field: &str) -> Result<Option<f64>, ConfigError> {
    if node.is_null() {
        return Ok(None);
    }
    want_f64(node, field).map(Some)
}

fn want_enum<T>(
    node: &json::Node,
    field: &str,
    known: &[&str],
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, ConfigError> {
    let value = want_string(node, field)?;
    parse(&value).ok_or_else(|| {
        ConfigError::new(
            field,
            node.line,
            format!(
                "unknown value {value:?}; known values are {}",
                known.join(", ")
            ),
        )
    })
}

fn type_error(node: &json::Node, field: &str, expected: &str) -> ConfigError {
    ConfigError::new(
        field,
        node.line,
        format!("expected {expected}, found a {}", node.kind_name()),
    )
}

// ---------------------------------------------------------------------------
// Platform default paths (docs/spec/state-and-cache.md section 1)
// ---------------------------------------------------------------------------

/// The platform default paths, in one place because two crates need them: the
/// daemon resolves the config file, the socket, the state directory and the cache
/// root, and the CLI resolves the socket. A second copy of these rules is the bug
/// the single-copy rule exists to prevent (docs/development.md section 1).
///
/// No I/O happens here and no platform API is called: these are path rules.
pub mod paths {
    use std::env;
    use std::path::PathBuf;

    /// `HOME`, or `USERPROFILE` on Windows.
    pub fn home() -> Option<PathBuf> {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    /// An XDG variable, read as an absolute path only: a relative value is
    /// treated as unset, which is what the specification requires
    /// (docs/spec/state-and-cache.md 1.2).
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn xdg(name: &str) -> Option<PathBuf> {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    #[cfg(target_os = "macos")]
    pub fn support_dir() -> Option<PathBuf> {
        home().map(|home| home.join("Library/Application Support/whirl"))
    }

    #[cfg(target_os = "macos")]
    pub fn config_file() -> Option<PathBuf> {
        support_dir().map(|dir| dir.join("config.json"))
    }

    #[cfg(target_os = "macos")]
    pub fn socket_file() -> Option<PathBuf> {
        support_dir().map(|dir| dir.join("whirl.sock"))
    }

    #[cfg(target_os = "macos")]
    pub fn state_dir() -> Option<PathBuf> {
        support_dir().map(|dir| dir.join("state"))
    }

    #[cfg(target_os = "macos")]
    pub fn cache_dir() -> Option<PathBuf> {
        home().map(|home| home.join("Library/Caches/whirl"))
    }

    #[cfg(target_os = "macos")]
    pub fn log_file() -> Option<PathBuf> {
        home().map(|home| home.join("Library/Logs/whirl/whirl.log"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn config_file() -> Option<PathBuf> {
        let base = xdg("XDG_CONFIG_HOME").or_else(|| home().map(|home| home.join(".config")))?;
        Some(base.join("whirl/config.json"))
    }

    /// `$XDG_RUNTIME_DIR/whirl.sock`, falling back to
    /// `$XDG_STATE_HOME/whirl/whirl.sock` when it is unset (the socket is a
    /// runtime file, and the state directory is the documented second choice).
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn socket_file() -> Option<PathBuf> {
        if let Some(runtime) = xdg("XDG_RUNTIME_DIR") {
            return Some(runtime.join("whirl.sock"));
        }
        state_dir().map(|dir| dir.join("whirl.sock"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn state_dir() -> Option<PathBuf> {
        let base =
            xdg("XDG_STATE_HOME").or_else(|| home().map(|home| home.join(".local/state")))?;
        Some(base.join("whirl"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn cache_dir() -> Option<PathBuf> {
        let base = xdg("XDG_CACHE_HOME").or_else(|| home().map(|home| home.join(".cache")))?;
        Some(base.join("whirl"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn log_file() -> Option<PathBuf> {
        state_dir().map(|dir| dir.join("whirl.log"))
    }

    /// Windows: `%APPDATA%` and `%LOCALAPPDATA%`.
    ///
    /// Deviation from docs/spec/state-and-cache.md 1.3, stated rather than hidden:
    /// the spec requires `SHGetKnownFolderPath` with `KNOWNFOLDERID` constants and
    /// forbids reading these variables out of the environment. Calling Win32 needs
    /// either a dependency or an FFI declaration, and v0.1 has neither in
    /// `whirl-core`; the variables are the same strings by default, which is the
    /// spec's own observation. This is the scaffold's one Windows path deviation.
    #[cfg(windows)]
    pub fn appdata() -> Option<PathBuf> {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    #[cfg(windows)]
    pub fn local_appdata() -> Option<PathBuf> {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home().map(|home| home.join("AppData/Local")))
    }

    #[cfg(windows)]
    pub fn config_file() -> Option<PathBuf> {
        appdata().map(|dir| dir.join("whirl/config.json"))
    }

    #[cfg(windows)]
    pub fn socket_file() -> Option<PathBuf> {
        // The real transport is a named pipe, `\\.\pipe\whirl-<user>` (2.1), and a
        // named pipe is not a path. This returns the pipe name so that error
        // messages can name the thing that is missing.
        let user = env::var("USERNAME").unwrap_or_else(|_| "unknown".to_string());
        let sanitised: String = user
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .take(32)
            .collect();
        Some(PathBuf::from(format!("\\\\.\\pipe\\whirl-{sanitised}")))
    }

    #[cfg(windows)]
    pub fn state_dir() -> Option<PathBuf> {
        local_appdata().map(|dir| dir.join("whirl/state"))
    }

    #[cfg(windows)]
    pub fn cache_dir() -> Option<PathBuf> {
        local_appdata().map(|dir| dir.join("whirl/cache"))
    }

    #[cfg(windows)]
    pub fn log_file() -> Option<PathBuf> {
        local_appdata().map(|dir| dir.join("whirl/whirl.log"))
    }
}

// ---------------------------------------------------------------------------
// The JSON reader
// ---------------------------------------------------------------------------

/// A JSON reader with line numbers, written against the standard library alone.
///
/// It is strict where strictness pays (an unterminated string, a trailing comma, a
/// control character in a string, a duplicate key, trailing content after the
/// document) because those are the mistakes that would otherwise be read as
/// something else, and lenient about nothing else.
mod json {
    use super::ConfigError;

    #[derive(Debug, Clone, PartialEq)]
    pub(super) struct Node {
        pub(super) value: Value,
        pub(super) line: usize,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub(super) enum Value {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Node>),
        Obj(Vec<(String, Node)>),
    }

    impl Node {
        pub(super) fn kind_name(&self) -> &'static str {
            match self.value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Num(_) => "number",
                Value::Str(_) => "string",
                Value::Arr(_) => "array",
                Value::Obj(_) => "object",
            }
        }

        pub(super) fn is_null(&self) -> bool {
            matches!(self.value, Value::Null)
        }

        pub(super) fn as_str(&self) -> Option<&str> {
            match &self.value {
                Value::Str(value) => Some(value),
                _ => None,
            }
        }

        pub(super) fn as_bool(&self) -> Option<bool> {
            match self.value {
                Value::Bool(value) => Some(value),
                _ => None,
            }
        }

        pub(super) fn as_num(&self) -> Option<f64> {
            match self.value {
                Value::Num(value) => Some(value),
                _ => None,
            }
        }

        pub(super) fn as_array(&self) -> Option<&[Node]> {
            match &self.value {
                Value::Arr(elements) => Some(elements),
                _ => None,
            }
        }

        pub(super) fn as_object(&self) -> Option<&[(String, Node)]> {
            match &self.value {
                Value::Obj(entries) => Some(entries),
                _ => None,
            }
        }
    }

    /// Parse one JSON document.
    pub(super) fn parse(text: &str) -> Result<Node, ConfigError> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
            line: 1,
        };
        parser.skip_ws();
        let node = parser.value()?;
        parser.skip_ws();
        if parser.pos < parser.bytes.len() {
            return Err(ConfigError::syntax(
                parser.line,
                "trailing content after the config object",
            ));
        }
        Ok(node)
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
        line: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.pos).copied()
        }

        fn bump(&mut self) -> Option<u8> {
            let byte = self.peek()?;
            self.pos += 1;
            if byte == b'\n' {
                self.line += 1;
            }
            Some(byte)
        }

        fn skip_ws(&mut self) {
            while matches!(
                self.peek(),
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
            ) {
                self.bump();
            }
        }

        fn expect(&mut self, byte: u8, what: &str) -> Result<(), ConfigError> {
            match self.peek() {
                Some(found) if found == byte => {
                    self.bump();
                    Ok(())
                }
                Some(found) => Err(ConfigError::syntax(
                    self.line,
                    format!(
                        "expected {what} ({:?}), found {:?}",
                        byte as char, found as char
                    ),
                )),
                None => Err(ConfigError::syntax(
                    self.line,
                    format!(
                        "expected {what} ({:?}), found the end of the file",
                        byte as char
                    ),
                )),
            }
        }

        fn value(&mut self) -> Result<Node, ConfigError> {
            self.skip_ws();
            let line = self.line;
            let value = match self.peek() {
                Some(b'{') => self.object()?,
                Some(b'[') => self.array()?,
                Some(b'"') => Value::Str(self.string()?),
                Some(b't') => {
                    self.literal("true")?;
                    Value::Bool(true)
                }
                Some(b'f') => {
                    self.literal("false")?;
                    Value::Bool(false)
                }
                Some(b'n') => {
                    self.literal("null")?;
                    Value::Null
                }
                Some(b'-') | Some(b'0'..=b'9') => Value::Num(self.number()?),
                Some(found) => {
                    return Err(ConfigError::syntax(
                        line,
                        format!("unexpected {:?} where a value was expected", found as char),
                    ));
                }
                None => {
                    return Err(ConfigError::syntax(
                        line,
                        "expected a value, found the end of the file",
                    ));
                }
            };
            Ok(Node { value, line })
        }

        fn object(&mut self) -> Result<Value, ConfigError> {
            self.expect(b'{', "an object")?;
            let mut entries: Vec<(String, Node)> = Vec::new();
            self.skip_ws();
            if self.peek() == Some(b'}') {
                self.bump();
                return Ok(Value::Obj(entries));
            }
            loop {
                self.skip_ws();
                let key = self.string().map_err(|error| {
                    ConfigError::syntax(self.line, format!("in an object key: {}", error.message))
                })?;
                self.skip_ws();
                self.expect(b':', "a colon after the key")?;
                let value = self.value()?;
                if entries.iter().any(|(name, _)| name == &key) {
                    return Err(ConfigError::syntax(
                        value.line,
                        format!("duplicate key {key:?}"),
                    ));
                }
                entries.push((key, value));
                self.skip_ws();
                match self.peek() {
                    Some(b',') => {
                        self.bump();
                        self.skip_ws();
                        if self.peek() == Some(b'}') {
                            return Err(ConfigError::syntax(
                                self.line,
                                "trailing comma in an object",
                            ));
                        }
                    }
                    Some(b'}') => {
                        self.bump();
                        return Ok(Value::Obj(entries));
                    }
                    _ => return Err(ConfigError::syntax(self.line, "expected \",\" or \"}\"")),
                }
            }
        }

        fn array(&mut self) -> Result<Value, ConfigError> {
            self.expect(b'[', "an array")?;
            let mut elements = Vec::new();
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.bump();
                return Ok(Value::Arr(elements));
            }
            loop {
                elements.push(self.value()?);
                self.skip_ws();
                match self.peek() {
                    Some(b',') => {
                        self.bump();
                        self.skip_ws();
                        if self.peek() == Some(b']') {
                            return Err(ConfigError::syntax(
                                self.line,
                                "trailing comma in an array",
                            ));
                        }
                    }
                    Some(b']') => {
                        self.bump();
                        return Ok(Value::Arr(elements));
                    }
                    _ => return Err(ConfigError::syntax(self.line, "expected \",\" or \"]\"")),
                }
            }
        }

        fn literal(&mut self, word: &str) -> Result<(), ConfigError> {
            for expected in word.bytes() {
                match self.bump() {
                    Some(found) if found == expected => {}
                    _ => {
                        return Err(ConfigError::syntax(
                            self.line,
                            format!("expected the literal {word:?}"),
                        ));
                    }
                }
            }
            Ok(())
        }

        fn string(&mut self) -> Result<String, ConfigError> {
            let start = self.line;
            self.expect(b'"', "a string")?;
            let mut out = String::new();
            loop {
                match self.bump() {
                    None => {
                        return Err(ConfigError::syntax(start, "unterminated string"));
                    }
                    Some(b'"') => return Ok(out),
                    Some(b'\\') => {
                        let escape = self
                            .bump()
                            .ok_or_else(|| ConfigError::syntax(self.line, "unterminated escape"))?;
                        match escape {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'b' => out.push('\u{8}'),
                            b'f' => out.push('\u{c}'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let high = self.hex4()?;
                                let code = if (0xd800..0xdc00).contains(&high) {
                                    // A surrogate pair: \uD83D\uDE00.
                                    self.expect(b'\\', "a low surrogate")?;
                                    self.expect(b'u', "a low surrogate")?;
                                    let low = self.hex4()?;
                                    if !(0xdc00..0xe000).contains(&low) {
                                        return Err(ConfigError::syntax(
                                            self.line,
                                            "a high surrogate must be followed by a low one",
                                        ));
                                    }
                                    let combined =
                                        0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
                                    char::from_u32(combined).ok_or_else(|| {
                                        ConfigError::syntax(self.line, "invalid surrogate pair")
                                    })?
                                } else {
                                    char::from_u32(high).ok_or_else(|| {
                                        ConfigError::syntax(
                                            self.line,
                                            format!("\\u{high:04x} is not a character"),
                                        )
                                    })?
                                };
                                out.push(code);
                            }
                            other => {
                                return Err(ConfigError::syntax(
                                    self.line,
                                    format!("unknown escape \\{:?}", other as char),
                                ));
                            }
                        }
                    }
                    Some(byte) if byte < 0x20 => {
                        // The line of the character itself: `bump` has already
                        // counted this newline, so a newline reports the line it
                        // ended rather than the one it started.
                        let line = if byte == b'\n' {
                            self.line - 1
                        } else {
                            self.line
                        };
                        return Err(ConfigError::syntax(
                            line,
                            "an unescaped control character in a string",
                        ));
                    }
                    Some(_byte) => {
                        // A byte of a multi-byte sequence is copied through and the
                        // string is checked once, at the end, which keeps a split
                        // character from being mangled in the middle.
                        let start = self.pos - 1;
                        let mut end = self.pos;
                        while let Some(byte) = self.peek() {
                            if byte < 0x80 || byte >= 0x20 && byte != b'"' && byte != b'\\' {
                                break;
                            }
                            end += 1;
                            self.bump();
                        }
                        let chunk = std::str::from_utf8(&self.bytes[start..end]).map_err(|_| {
                            ConfigError::syntax(self.line, "the string is not valid UTF-8")
                        })?;
                        out.push_str(chunk);
                    }
                }
            }
        }

        fn hex4(&mut self) -> Result<u32, ConfigError> {
            let mut value = 0u32;
            for _ in 0..4 {
                let byte = self
                    .bump()
                    .ok_or_else(|| ConfigError::syntax(self.line, "unterminated \\u escape"))?;
                let digit = (byte as char).to_digit(16).ok_or_else(|| {
                    ConfigError::syntax(self.line, "\\u must be followed by 4 hex digits")
                })?;
                value = value * 16 + digit;
            }
            Ok(value)
        }

        fn number(&mut self) -> Result<f64, ConfigError> {
            let start = self.pos;
            let line = self.line;
            if self.peek() == Some(b'-') {
                self.bump();
            }
            while matches!(
                self.peek(),
                Some(b'0'..=b'9') | Some(b'.') | Some(b'e') | Some(b'E') | Some(b'+') | Some(b'-')
            ) {
                self.bump();
            }
            let text = std::str::from_utf8(&self.bytes[start..self.pos])
                .map_err(|_| ConfigError::syntax(line, "the number is not valid UTF-8"))?;
            text.parse::<f64>()
                .map_err(|_| ConfigError::syntax(line, format!("{text:?} is not a number")))
        }
    }
}

/// The file the daemon writes when `config.json` is missing: the annotated example
/// of docs/architecture.md 4.2, verbatim.
const DEFAULT_CONFIG_JSON: &str = r#"{
  "_comment_1": "whirl configuration. JSON, UTF-8, no comments in the syntax: any key whose first",
  "_comment_2": "character is an underscore is a comment and is ignored by every whirl parser, at",
  "_comment_3": "every level. Every key below is written at its default value, so deleting a line",
  "_comment_4": "keeps the default and there is no key whose absence means something different.",
  "_comment_5": "Precedence, lowest first: compiled defaults, this file, the environment",
  "_comment_6": "(WHIRL_CONFIG, WHIRL_SOCKET, WHIRL_STATE_DIR, WHIRL_CACHE_DIR, WHIRL_BACKEND,",
  "_comment_7": "WHIRL_WALLHAVEN_API_KEY), then daemon flags, which exist for tests only.",
  "_comment_8": "Two legacy names are still accepted and log a deprecation warning: `keep` for",
  "_comment_9": "cache.max_files and `cache_dir` for cache.root. Neither is written back.",

  "config_schema": 1,
  "_comment_config_schema": "Bumped only when a key changes meaning. A file with a newer value is read, reported in status.state_schema_newer, and not rewritten.",

  "socket": null,
  "_comment_socket": "null means the platform default: ~/Library/Application Support/whirl/whirl.sock on macOS, $XDG_RUNTIME_DIR/whirl.sock on Linux (falling back to $XDG_STATE_HOME/whirl/whirl.sock), \\\\.\\pipe\\whirl-<user> on Windows. An absolute path here is used verbatim. The daemon refuses to start if the resolved path does not fit the platform's sun_path (104 bytes on macOS).",

  "log_level": "info",
  "_comment_log_level": "off | error | warn | info | debug. The log is a file the daemon owns; it is never an interface.",

  "schedule": {
    "interval_seconds": 1800,
    "worker_deadline_seconds": 300,
    "_comment_interval_seconds": "The rotation interval. The OS scheduler starts the daemon; this timer lives in the daemon and the deadline is a wall-clock comparison, so a laptop that slept through three slots rotates once on wake and then returns to the grid.",
    "_comment_worker_deadline_seconds": "A worker that has not finished in this many seconds gets SIGTERM, then SIGKILL 5 s later, and the slot is spent. 300 s is ~90x the measured worst case and covers a fully expired download."
  },

  "startup": {
    "enabled": true,
    "mode": "last",
    "respect_manual": true,
    "_comment_enabled": "Rotate once when the daemon starts, so a login restores the wallpaper.",
    "_comment_mode": "last | rotate. `last` re-applies the newest history entry and spends no download.",
    "_comment_respect_manual": "When the platform can read back the current image and it is not the one whirl set, record it as an `external` history entry and do not overwrite it. Degrades to false, reported as status.respect_manual_effective, where the platform cannot read back."
  },

  "display": {
    "mode": "all",
    "_comment_mode": "all | per-display. `all` is one image on every display. `per-display` is honoured only where the platform documents a per-display setter a one-rotation worker can reach (Windows, sway, generic X11) and is refused in config check elsewhere: GNOME cannot do it, KDE is out of scope, and macOS is unverified, where it is accepted and runs as all with status.display_mode_reason naming why."
  },

  "min_width": 1600,
  "min_height": 900,
  "_comment_min_width": "The shared resolution floor, applied by reading the image header. The specs name no global default; this is the prototype's value (prototype/wh-rotate/example-config.json), and most users should set it to their own panel's resolution. A source may override it with its own min_width/min_height.",

  "filters": {
    "max_bytes": 41943040,
    "ratio_tolerance": 0.02,
    "target_ratio": null,
    "_comment": "max_bytes is 40 MiB, the largest file the pipeline will admit. ratio_tolerance is a fraction of the target ratio. target_ratio null means any, or the primary display's ratio where the worker can read it."
  },

  "state": {
    "history_entries": 50,
    "_comment_history_entries": "The history ring. 50 entries at roughly 341 bytes each in the index, so the daemon's resident state stays ~8 KB."
  },

  "dedupe": {
    "recent_entries": 50,
    "_comment_recent_entries": "How many recent entries a candidate is compared against. Larger than the ring it is not, and a larger window costs only hashes."
  },

  "cache": {
    "root": null,
    "max_bytes": 2147483648,
    "max_files": 500,
    "grace_seconds": 600,
    "orphan_grace_seconds": 300,
    "_comment_root": "null means the platform cache root (~/Library/Caches/whirl, $XDG_CACHE_HOME/whirl, %LOCALAPPDATA%\\whirl\\cache). An alias `cache_dir` is also accepted, absolute paths only. config check refuses a root whose filesystem does not support flock, and reports lock_mode: excl_file if a weaker lock had to be used.",
    "_comment_max_bytes": "2 GiB. Whichever of the two caps binds first evicts. config check refuses cache.max_bytes < filters.max_bytes, because that config has a guaranteed permanent overshoot.",
    "_comment_max_files": "500. Both caps exist because a count cap does not bound bytes: the same 500 files span 60x in size across samples, and 500 files is ~1.5-1.9 GB at the sampled means.",
    "_comment_grace_seconds": "A cache file newer than this is never reclaimed, so an in-flight set cannot have its file deleted underneath it.",
    "_comment_orphan_grace_seconds": "How long a file with no index entry survives before the sweep reclaims it. This is what covers a worker killed between its rename and its report."
  },

  "backend": "native",
  "_comment_backend": "native | noop. `noop` runs every stage except the platform setter, so a whole rotation can be exercised in a test or in CI without touching the desktop. Also settable as WHIRL_BACKEND=noop or --backend noop.",

  "sources": [
    {
      "_comment_kind": "local: one or more directories, filtered by a header read and a glob. The source entries below are the two schemas from docs/spec/features.md 2.2 and 2.3, at their defaults.",
      "id": "pictures",
      "kind": "local",
      "weight": 1,
      "capabilities": ["resolution", "extension"],
      "paths": ["~/Pictures/Wallpapers", "/Volumes/Media/walls"],
      "recursive": true,
      "max_depth": 8,
      "follow_symlinks": false,
      "include": ["*.jpg", "*.jpeg", "*.png", "*.heic", "*.webp"],
      "exclude": ["*/.git/*", "*/screenshots/*", "*.tmp"],
      "min_width": 2560,
      "min_height": 1440,
      "mode": "reference",
      "_comment_id": "Unique, and the prefix of every origin_key this source produces. [A-Za-z0-9._:-]+, at most 64 bytes, because a space here would break the line protocol's framing.",
      "_comment_weight": "Relative chance of being chosen per rotation. 0 disables the source without deleting it. The daemon normalises; it does not need the weights to sum to anything.",
      "_comment_capabilities": "What the source can answer. Defaults to the kind's own list and may be narrowed, never widened: config check refuses a capability the kind does not implement. A capability not declared is applied by the shared pipeline instead, which is why the result set is the same either way.",
      "_comment_paths": "~ is expanded. A path that is missing or unreadable is a warning; it is an error only if every path fails.",
      "_comment_max_depth": "Bounds a runaway tree, such as a symlinked home directory, at a predictable scan.",
      "_comment_follow_symlinks": "Off by default so that loops are impossible rather than merely unlikely.",
      "_comment_include": "Matched against the path relative to the source root. Exclude wins. A file whose header cannot be read is excluded and logged at debug, never guessed at from its name.",
      "_comment_mode": "reference sets the wallpaper from the original path, and a deleted file becomes a broken wallpaper with no error at the time it breaks. copy copies into the cache first, which is what a removable volume or a network share needs."
    },
    {
      "id": "space",
      "kind": "wallhaven",
      "weight": 3,
      "capabilities": ["resolution", "ratio", "purity", "colors", "category"],
      "query": "space nebula",
      "categories": "111",
      "purity": "100",
      "sorting": "random",
      "order": "desc",
      "ratios": "16x9,16x10",
      "atleast": "2560x1440",
      "colors": null,
      "top_range": "1M",
      "collection": null,
      "pages": 1,
      "api_key_ref": null,
      "_comment_query": "The site's tag syntax, including +tag, -tag, @user, id:, type:, like:.",
      "_comment_categories": "Three flags: general, anime, people. 111 is all three.",
      "_comment_purity": "100 sfw, 110 sketchy, 111 nsfw. NSFW needs a valid key and fails 401 without one.",
      "_comment_sorting": "date_added | relevance | random | views | favorites | toplist.",
      "_comment_ratios": "Pushed down as `ratios`, and also applied by the shared pipeline with filters.ratio_tolerance.",
      "_comment_atleast": "Pushed down as `atleast`; the pipeline still enforces min_width x min_height, because the source's own admission is not the whole filter.",
      "_comment_top_range": "Only meaningful with sorting=toplist.",
      "_comment_collection": "A path segment instead of a search: /api/v1/collections/USERNAME/ID. That endpoint exposes only the purity filter, so every other filter is applied locally.",
      "_comment_pages": "How many 24-result pages to sample. Default 1, hard cap 5: one API call per page against a documented 45 requests per minute.",
      "_comment_api_key_ref": "A NAME, never a value. null means the key is looked up in the platform's own store, in this order: WHIRL_WALLHAVEN_API_KEY, then keychain item `whirl-wallhaven` on macOS, `secret-tool lookup service whirl key wallhaven` on Linux, Credential Manager generic credential `whirl/wallhaven` on Windows. A key in this file is a bad_config refusal, because a key in a config file is a key in every backup of that file."
    }
  ]
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The quickstart's config, verbatim from docs/development.md section 7.
    const QUICKSTART: &str = r#"{
  "config_schema": 1,
  "backend": "noop",
  "sources": [
    { "id": "pictures", "kind": "local", "weight": 1, "paths": ["~/Pictures"] }
  ]
}"#;

    fn refusal(text: &str) -> ConfigError {
        Config::parse(text).expect_err("the config must be refused")
    }

    #[test]
    fn the_annotated_default_config_is_the_schema() {
        let loaded = Config::parse(Config::default_config_json()).expect("the default config");
        assert!(
            loaded.warnings.is_empty(),
            "the generated default must not warn: {:?}",
            loaded.warnings
        );
        let config = loaded.config;
        let defaults = Config::default();
        assert_eq!(config.config_schema, defaults.config_schema);
        assert_eq!(config.log_level, defaults.log_level);
        assert_eq!(config.schedule, defaults.schedule);
        assert_eq!(config.startup, defaults.startup);
        assert_eq!(config.display, defaults.display);
        assert_eq!(config.min_width, defaults.min_width);
        assert_eq!(config.min_height, defaults.min_height);
        assert_eq!(config.filters, defaults.filters);
        assert_eq!(config.state, defaults.state);
        assert_eq!(config.dedupe, defaults.dedupe);
        assert_eq!(config.cache, defaults.cache);
        // The example carries the two documented source schemas.
        assert_eq!(config.sources.len(), 2);
        assert_eq!(config.sources[0].id, "pictures");
        assert_eq!(config.sources[0].kind, SourceKind::Local);
        assert_eq!(
            config.sources[0].local.as_ref().map(|l| l.mode),
            Some(LocalMode::Reference)
        );
        assert_eq!(
            config.sources[0].capabilities,
            vec![Capability::Resolution, Capability::Extension]
        );
        assert_eq!(config.sources[1].id, "space");
        assert_eq!(config.sources[1].kind, SourceKind::Wallhaven);
        assert_eq!(
            config.sources[1].wallhaven.as_ref().map(|w| w.pages),
            Some(1)
        );
        assert_eq!(
            config.sources[1]
                .wallhaven
                .as_ref()
                .and_then(|w| w.api_key_ref.clone()),
            None
        );
    }

    #[test]
    fn the_quickstart_config_parses_and_validates() {
        let loaded = Config::parse(QUICKSTART).expect("the quickstart config");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.backend, Backend::Noop);
        assert_eq!(loaded.config.schedule.interval_seconds, 1800);
        assert_eq!(loaded.config.state.history_entries, 50);
        let source = &loaded.config.sources[0];
        assert_eq!(source.id, "pictures");
        assert_eq!(source.kind, SourceKind::Local);
        assert_eq!(source.weight, 1);
        assert_eq!(
            source.local.as_ref().map(|l| l.paths.clone()),
            Some(vec!["~/Pictures".to_string()])
        );
        // A source that does not declare capabilities gets its kind's own list.
        assert_eq!(
            source.capabilities,
            SourceConfig::default_capabilities(SourceKind::Local)
        );
    }

    #[test]
    fn a_missing_required_field_names_the_field_and_the_line() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [\n    { \"id\": \"pictures\", \"paths\": [\"~/Pictures\"] }\n  ]\n}";
        let error = refusal(text);
        assert_eq!(error.field.as_deref(), Some("sources[0].kind"));
        assert_eq!(error.line, 4);
        let message = error.to_string();
        assert_eq!(
            message,
            "sources[0].kind (line 4): missing required field `kind` for source \"pictures\"; \
             known kinds are local, wallhaven"
        );

        // A local source without `paths` is the other required field.
        let error =
            refusal("{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\" }\n  ]\n}");
        assert_eq!(error.field.as_deref(), Some("sources[0].paths"));
        assert_eq!(error.line, 3);
        assert!(error.to_string().contains("missing required field `paths`"));

        // And a source without an id.
        let error = refusal(
            "{\n  \"sources\": [\n    { \"kind\": \"local\", \"paths\": [\"~/x\"] }\n  ]\n}",
        );
        assert_eq!(error.field.as_deref(), Some("sources[0].id"));
        assert_eq!(error.line, 3);
    }

    #[test]
    fn an_unknown_source_kind_names_the_field_the_line_and_the_known_kinds() {
        let text = "{\n  \"sources\": [\n    { \"id\": \"flickr\", \"kind\": \"flickr-set\", \"paths\": [\"~/x\"] }\n  ]\n}";
        let error = refusal(text);
        assert_eq!(
            error.to_string(),
            "sources[0].kind (line 3): unknown source kind \"flickr-set\" for source \"flickr\"; \
             known kinds are local, wallhaven"
        );
    }

    #[test]
    fn an_unknown_field_is_a_warning_naming_its_line() {
        let text = "{\n  \"config_schema\": 1,\n  \"interval\": 900,\n  \"sources\": []\n}";
        let loaded = Config::parse(text).expect("an unknown field must not refuse startup");
        assert_eq!(loaded.warnings.len(), 1);
        let warning = &loaded.warnings[0];
        assert_eq!(warning.field, "interval");
        assert_eq!(warning.line, 3);
        assert_eq!(
            warning.to_string(),
            "interval (line 3): unknown field, ignored: it is not in the config schema"
        );

        // The same rule inside a source, where a wallhaven key under a local kind
        // is the realistic typo.
        let text = "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"~/x\"], \"purity\": \"100\" }\n  ]\n}";
        let loaded = Config::parse(text).expect("an unknown field must not refuse startup");
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.warnings[0].field, "sources[0].purity");
        assert_eq!(loaded.warnings[0].line, 3);
    }

    #[test]
    fn an_ordering_violation_names_both_values_and_the_line() {
        let text = "{\n  \"filters\": {\n    \"max_bytes\": 41943040\n  },\n  \"cache\": {\n    \"max_bytes\": 1048576\n  }\n}";
        let error = refusal(text);
        assert_eq!(error.field.as_deref(), Some("cache.max_bytes"));
        assert_eq!(error.line, 6);
        assert_eq!(
            error.to_string(),
            "cache.max_bytes (line 6): 1048576 is less than filters.max_bytes = 41943040 (line 3); \
             the cache must hold at least one admissible image"
        );

        // The scalar floors, each naming its own key.
        for (text, field, line) in [
            (
                "{\n  \"schedule\": {\n    \"interval_seconds\": 30\n  }\n}",
                "schedule.interval_seconds",
                3,
            ),
            (
                "{\n  \"cache\": {\n    \"max_files\": 1\n  }\n}",
                "cache.max_files",
                3,
            ),
            (
                "{\n  \"state\": {\n    \"history_entries\": 0\n  }\n}",
                "state.history_entries",
                3,
            ),
            (
                "{\n  \"dedupe\": {\n    \"recent_entries\": 0\n  }\n}",
                "dedupe.recent_entries",
                3,
            ),
            (
                "{\n  \"schedule\": {\n    \"worker_deadline_seconds\": 10\n  }\n}",
                "schedule.worker_deadline_seconds",
                3,
            ),
        ] {
            let error = refusal(text);
            assert_eq!(error.field.as_deref(), Some(field), "{text}");
            assert_eq!(error.line, line, "{text}");
        }
    }

    #[test]
    fn wrong_types_and_out_of_range_values_are_refused_with_their_line() {
        let cases = [
            ("{\n  \"min_width\": \"1600\"\n}", "min_width", 2),
            ("{\n  \"min_width\": 1600.5\n}", "min_width", 2),
            (
                "{\n  \"startup\": {\n    \"enabled\": \"yes\"\n  }\n}",
                "startup.enabled",
                3,
            ),
            ("{\n  \"log_level\": \"loud\"\n}", "log_level", 2),
            ("{\n  \"backend\": \"magic\"\n}", "backend", 2),
            (
                "{\n  \"display\": {\n    \"mode\": \"every\"\n  }\n}",
                "display.mode",
                3,
            ),
            (
                "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"~/x\"], \"mode\": \"move\" }\n  ]\n}",
                "sources[0].mode",
                3,
            ),
            (
                "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"~/x\"], \"weight\": -1 }\n  ]\n}",
                "sources[0].weight",
                3,
            ),
            (
                "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"wallhaven\", \"pages\": 6 }\n  ]\n}",
                "sources[0].pages",
                3,
            ),
            (
                "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"wallhaven\", \"purity\": \"1\" }\n  ]\n}",
                "sources[0].purity",
                3,
            ),
            (
                "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [] }\n  ]\n}",
                "sources[0].paths",
                3,
            ),
        ];
        for (text, field, line) in cases {
            let error = refusal(text);
            assert_eq!(error.field.as_deref(), Some(field), "{text}: {error}");
            assert_eq!(error.line, line, "{text}: {error}");
        }
    }

    #[test]
    fn a_source_id_outside_the_charset_is_refused() {
        let text = "{\n  \"sources\": [\n    { \"id\": \"two words\", \"kind\": \"local\", \"paths\": [\"~/x\"] }\n  ]\n}";
        let error = refusal(text);
        assert_eq!(error.field.as_deref(), Some("sources[0].id"));
        assert_eq!(error.line, 3);
        assert!(error.to_string().contains("matches [A-Za-z0-9._:-]+"));
    }

    #[test]
    fn a_capability_a_kind_cannot_implement_is_refused() {
        let text = "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"~/x\"], \"capabilities\": [\"purity\"] }\n  ]\n}";
        let error = refusal(text);
        assert_eq!(error.field.as_deref(), Some("sources[0].capabilities"));
        assert_eq!(error.line, 3);
        assert!(error.to_string().contains("never widen it"));
    }

    #[test]
    fn a_duplicate_source_id_is_refused() {
        let text = "{\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"~/a\"] },\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"~/b\"] }\n  ]\n}";
        let error = refusal(text);
        assert!(error.to_string().contains("duplicate source id"), "{error}");
    }

    #[test]
    fn comment_keys_are_ignored_at_every_level() {
        let text = "{\n  \"_comment\": \"the whole file\",\n  \"config_schema\": 1,\n  \"cache\": {\n    \"_comment\": \"here too\",\n    \"max_files\": 10\n  },\n  \"sources\": [\n    { \"_comment_kind\": \"and here\", \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"~/x\"] }\n  ]\n}";
        let loaded = Config::parse(text).expect("comments are ignored");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.cache.max_files, 10);
    }

    #[test]
    fn the_legacy_aliases_are_accepted_and_deprecated() {
        // The validator asks the platform whether the path is absolute, and a bare
        // `/tmp/...` is not absolute on Windows: the fixture has to be absolute
        // where it runs, or this test fails for the platform's reason instead of
        // the alias's.
        let root = if cfg!(windows) {
            "C:/whirl-cache"
        } else {
            "/tmp/whirl-cache"
        };
        let text = format!(
            "{{\n  \"keep\": 100,\n  \"cache\": {{\n    \"cache_dir\": \"{root}\"\n  }}\n}}"
        );
        let loaded = Config::parse(&text).expect("aliases are accepted");
        assert_eq!(loaded.config.cache.max_files, 100);
        assert_eq!(loaded.config.cache.root, Some(PathBuf::from(root)));
        let fields: Vec<&str> = loaded.warnings.iter().map(|w| w.field.as_str()).collect();
        assert_eq!(fields, vec!["keep", "cache.cache_dir"]);
        assert!(loaded.warnings[0].to_string().contains("deprecated"));
        assert_eq!(loaded.warnings[0].line, 2);

        // A relative cache root is refused, alias or not.
        let error = refusal("{\n  \"cache\": {\n    \"root\": \"cache/here\"\n  }\n}");
        assert_eq!(error.field.as_deref(), Some("cache.root"));
        assert!(error.to_string().contains("must be absolute"));
    }

    #[test]
    fn a_key_that_looks_like_an_api_key_is_refused() {
        let text = "{\n  \"sources\": [\n    { \"id\": \"s\", \"kind\": \"wallhaven\", \"api_key_ref\": \"a1b2c3d4e5f60718293a4b5c6d7e8f90\" }\n  ]\n}";
        let error = refusal(text);
        assert_eq!(error.field.as_deref(), Some("sources[0].api_key_ref"));
        assert!(error.to_string().contains("NAME, never a value"));

        // The documented label form is accepted.
        let text = "{\n  \"sources\": [\n    { \"id\": \"s\", \"kind\": \"wallhaven\", \"api_key_ref\": \"keychain:whirl-wallhaven\" }\n  ]\n}";
        assert!(Config::parse(text).is_ok());
    }

    #[test]
    fn the_full_string_escape_set_decodes_because_windows_paths_need_it() {
        let text = "{\n  \"socket\": \"\\\\\\\\server\\\\share\\\\a.json\",\n  \"sources\": [\n    { \"id\": \"p\", \"kind\": \"local\", \"paths\": [\"C:\\\\Users\\\\x\\\\Pictures\", \"a\\tb\"] }\n  ]\n}";
        let loaded = Config::parse(text).expect("the escapes decode");
        assert_eq!(
            loaded.config.socket,
            Some(PathBuf::from("\\\\server\\share\\a.json"))
        );
        assert_eq!(
            loaded.config.sources[0]
                .local
                .as_ref()
                .map(|l| l.paths[0].clone()),
            Some("C:\\Users\\x\\Pictures".to_string())
        );
        assert_eq!(
            loaded.config.sources[0]
                .local
                .as_ref()
                .map(|l| l.paths[1].clone()),
            Some("a\tb".to_string())
        );
    }

    #[test]
    fn syntax_errors_carry_a_line() {
        let cases = [
            ("{\n  \"a\": 1,\n}", 3),
            ("{\n  \"a\": \"unterminated\n}", 2),
            ("{\n  \"a\": 1,\n  \"a\": 2\n}", 3),
            ("{\n  \"a\": 01x\n}", 2),
            ("[]", 1),
            ("{\n  \"a\": 1\n}\n{\n}", 4),
        ];
        for (text, line) in cases {
            let error = refusal(text);
            assert_eq!(error.line, line, "{text}: {error}");
            assert_eq!(error.field, None, "{text}");
            assert!(error.to_string().starts_with(&format!("line {line}: ")));
        }
        assert!(
            refusal("{\n  \"a\": 1,\n}")
                .to_string()
                .contains("trailing comma")
        );
        assert!(refusal("[]").to_string().contains("must be a JSON object"));
        assert!(
            Config::parse("{}").is_ok(),
            "an empty object is a valid config"
        );
    }

    #[test]
    fn a_newer_schema_version_warns_and_is_not_a_refusal() {
        let text = "{\n  \"config_schema\": 9\n}";
        let loaded = Config::parse(text).expect("a newer schema is read");
        assert_eq!(loaded.config.config_schema, 9);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(
            loaded.warnings[0]
                .to_string()
                .contains("state_schema_newer")
        );
    }

    #[test]
    fn the_documented_socket_and_cache_defaults_stay_none() {
        let loaded = Config::parse("{}").expect("an empty config");
        assert_eq!(loaded.config.socket, None);
        assert_eq!(loaded.config.cache.root, None);
        assert_eq!(loaded.config.backend, Backend::Native);
        assert!(loaded.config.sources.is_empty());
    }

    #[test]
    fn the_parser_reads_the_whole_document_scale() {
        // A wallhaven source with every key at a non-default value, to prove the
        // schema accepts the documented surface.
        let text = r#"{
          "sources": [
            {
              "id": "space",
              "kind": "wallhaven",
              "weight": 3,
              "capabilities": ["resolution", "ratio"],
              "query": "space nebula +tag -tag @user id:12",
              "categories": "111",
              "purity": "100",
              "sorting": "toplist",
              "order": "asc",
              "ratios": "16x9,16x10",
              "atleast": "2560x1440",
              "colors": "660000",
              "top_range": "1M",
              "pages": 5
            }
          ]
        }"#;
        let loaded = Config::parse(text).expect("the wallhaven schema");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let wallhaven = loaded.config.sources[0]
            .wallhaven
            .as_ref()
            .expect("wallhaven");
        assert_eq!(wallhaven.pages, 5);
        assert_eq!(wallhaven.sorting, "toplist");
        assert_eq!(loaded.config.sources[0].weight, 3);
    }
}
