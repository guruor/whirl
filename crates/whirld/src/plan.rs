//! What this daemon actually adopted, and the flags that chose it.
//!
//! Precedence is config file, then environment, then daemon flags
//! (docs/architecture.md 4.3); the flags exist for tests
//! (docs/development.md section 7). `status` and `sources` report from here and
//! never from the config file a client re-read, because the effective values
//! differ by definition: resolved paths, `display.mode_effective`, per-source
//! `enabled`, and this process's own overrides (docs/architecture.md section 8,
//! "must never" 6).

use std::path::{Path, PathBuf};
use whirl_core::config::{Backend, Config, paths};
use whirl_core::protocol::SourceRecord;

/// The daemon's own flags. docs/architecture.md 4.3 says flags "exist only for
/// tests and for the `--backend noop` switch", and names exactly these three.
/// Everything else a test needs it can set in the environment: `WHIRL_CONFIG`,
/// `WHIRL_SOCKET`, `WHIRL_STATE_DIR`, `WHIRL_CACHE_DIR` and `WHIRL_BACKEND` are
/// the five of docs/development.md section 7.
#[derive(Debug, Default)]
pub struct Flags {
    pub config: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    pub backend: Option<Backend>,
}

impl Flags {
    pub fn parse(args: &[String]) -> Result<Flags, String> {
        let mut flags = Flags::default();
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--config" => flags.config = Some(PathBuf::from(value)),
                "--socket" => flags.socket = Some(PathBuf::from(value)),
                "--backend" => {
                    flags.backend =
                        Some(Backend::parse(value).ok_or_else(|| {
                            format!("--backend {value} is neither native nor noop")
                        })?);
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(flags)
    }
}

/// The resolved plan: every path this process opened, and the config it parsed.
#[derive(Debug)]
pub struct Effective {
    pub config_path: PathBuf,
    pub socket_path: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub backend: Backend,
    pub config: Config,
}

impl Effective {
    /// Resolve every path, read the config, and create the directories this
    /// daemon owns. A refusal here is a refusal to start (docs/architecture.md
    /// 1.5 steps 2 and 3): the message names the path and the `errno`.
    pub fn resolve(flags: &Flags) -> Result<Effective, String> {
        let config_path = flags
            .config
            .clone()
            .or_else(|| env_path("WHIRL_CONFIG"))
            .or_else(paths::config_file)
            .ok_or("no config path: neither --config, WHIRL_CONFIG nor a platform default is available")?;
        let config = load_config(&config_path)?;

        let socket_path = flags
            .socket
            .clone()
            .or_else(|| env_path("WHIRL_SOCKET"))
            .or_else(|| config.socket.clone())
            .or_else(paths::socket_file)
            .ok_or("no socket path: neither --socket, WHIRL_SOCKET, `socket` nor a platform default is available")?;

        let state_dir = env_path("WHIRL_STATE_DIR")
            .or_else(paths::state_dir)
            .ok_or(
                "no state directory: neither WHIRL_STATE_DIR nor a platform default is available",
            )?;

        let cache_dir = env_path("WHIRL_CACHE_DIR")
            .or_else(|| config.cache.root.clone())
            .or_else(paths::cache_dir)
            .ok_or("no cache directory: neither WHIRL_CACHE_DIR, `cache.root` nor a platform default is available")?;

        let backend = match &flags.backend {
            Some(backend) => *backend,
            None => match std::env::var("WHIRL_BACKEND") {
                Ok(name) => Backend::parse(&name)
                    .ok_or_else(|| format!("WHIRL_BACKEND={name:?} is neither native nor noop"))?,
                Err(_) => config.backend,
            },
        };

        create_private_dir(&state_dir)?;
        // 8.4: the cache root is probed rather than refused. A cache that cannot
        // be written is a degraded daemon (`cache_writable: 0`, 8.5), not a
        // daemon that does not start: only the state directory refuses (1.5).
        if let Err(message) = create_private_dir(&cache_dir) {
            eprintln!("whirld: warning: {message}");
        }
        writable(&state_dir)?;

        Ok(Effective {
            config_path,
            socket_path,
            state_dir,
            cache_dir,
            backend,
            config,
        })
    }

    /// The `source:` records `status`, `sources` and `config check` report, in
    /// config order (docs/architecture.md 2.10).
    pub fn source_records(&self) -> Vec<SourceRecord> {
        self.config
            .sources
            .iter()
            .map(|source| source.record(None))
            .collect()
    }

    /// Enabled sources out of all configured: the number, not the ratio
    /// (docs/architecture.md 2.10).
    pub fn sources_enabled(&self) -> usize {
        self.config
            .sources
            .iter()
            .filter(|source| source.weight > 0)
            .count()
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// Read the config, writing the annotated default first when the file is absent
/// (docs/architecture.md 4.2, docs/development.md section 7).
fn load_config(path: &Path) -> Result<Config, String> {
    if !path.exists() {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            create_private_dir(parent)?;
        }
        std::fs::write(path, Config::default_config_json()).map_err(|error| {
            format!(
                "cannot write the default config at {}: {error}",
                path.display()
            )
        })?;
        restrict_to_owner(path)?;
        eprintln!("whirld: wrote the default config at {}", path.display());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read the config at {}: {error}", path.display()))?;
    let loaded = Config::parse(&text).map_err(|error| error.with_path(path).to_string())?;
    for warning in &loaded.warnings {
        eprintln!("whirld: warning: {warning}");
    }
    Ok(loaded.config)
}

/// `0700` on the directories this daemon owns (docs/architecture.md 2.1). An
/// existing directory keeps the mode its owner chose: only a directory this
/// process created is restricted.
pub(crate) fn create_private_dir(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    restrict(path, 0o700)
}

fn restrict_to_owner(path: &Path) -> Result<(), String> {
    restrict(path, 0o600)
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot set mode {mode:o} on {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

/// 8.4: "attempt to create and remove `tmp/<run>-probe.part`", taken at start and
/// again at each rotation rather than cached, because a cache directory can
/// become unwritable while the daemon runs (a remount, a full disk, a chmod).
///
/// This is the only thing that writes into the cache root before the cache card
/// lands, and it writes nothing that outlives the call.
pub(crate) fn probe_cache_writable(cache_dir: &Path, run: u64) -> bool {
    let tmp = cache_dir.join("tmp");
    if create_private_dir(&tmp).is_err() {
        return false;
    }
    let probe = tmp.join(format!("{run}-probe.part"));
    if std::fs::write(&probe, b"").is_err() {
        return false;
    }
    std::fs::remove_file(&probe).is_ok()
}

/// 1.5 step 2: refuse to start when the state directory cannot be written, and
/// name the directory and the error.
fn writable(path: &Path) -> Result<(), String> {
    let probe = path.join(".whirl-write-probe");
    std::fs::write(&probe, b"").map_err(|error| {
        format!(
            "the state directory {} is not writable: {error}",
            path.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}
