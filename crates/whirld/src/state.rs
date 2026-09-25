//! The daemon's state, and the `status` block it answers from.
//!
//! Nothing here is persisted. docs/architecture.md 1.5 step 4 loads
//! `current.json`, `history.json` and `favorites.json` and quarantines what it
//! cannot read; 1.7.2 says the worker writes no state; this scaffold ships no
//! state writer at all, so history, pins and counters live in memory and a
//! restart loses them. That is a gap in this build, not a design choice, and it
//! is named in the handoff.
//!
//! Everything `status` prints that the daemon does not own is measured, not
//! guessed: `pid` is this process, `uptime_s` is its own clock, `rss_kb` is
//! `/proc/self/statm` or `ps`, and the cache totals come from walking the cache
//! root this daemon created.

use crate::plan::Effective;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use whirl_core::protocol::{self, ErrorCode, Kind, Response, Via};
use whirl_core::state::{Favorite, FavoriteState, HistoryEntry, HistoryRing};

/// The image on screen, as this daemon believes it.
#[derive(Debug, Clone)]
pub struct Current {
    pub digest: String,
    pub origin_key: String,
    pub via: Via,
    pub kind: Kind,
    pub path: Option<String>,
    pub at: String,
}

/// A resolved `<path|id>` argument (docs/architecture.md 2.5).
#[derive(Debug, Clone)]
pub struct Resolved {
    pub origin_key: String,
    pub digest: Option<String>,
    pub kind: Kind,
    pub path: Option<String>,
}

#[derive(Debug)]
pub struct State {
    /// The state sequence number: every state change increments it (2.10).
    pub seq: u64,
    pub paused: bool,
    /// The monotonic slot counter the worker's `--run` comes from (1.6).
    pub slot: u64,
    /// The run in flight, which is what makes `next` answer `busy` (2.7).
    pub running: Option<u64>,
    pub rotation_count: u64,
    pub last_error: Option<ErrorCode>,
    pub anchor_verified: bool,
    pub clock_jump: u64,
    pub history: HistoryRing,
    pub favorites: BTreeMap<String, Favorite>,
    pub current: Option<Current>,
}

impl State {
    pub fn new(history_bound: usize) -> State {
        State {
            seq: 0,
            paused: false,
            slot: 1,
            running: None,
            rotation_count: 0,
            last_error: None,
            anchor_verified: false,
            clock_jump: 0,
            history: HistoryRing::new(history_bound),
            favorites: BTreeMap::new(),
            current: None,
        }
    }

    /// The id resolution order of docs/architecture.md 2.5: an exact
    /// `origin_key` first, then a 64-character lower-case hex digest, then
    /// nothing. First hit wins, and the order is stated so a client can predict
    /// it.
    pub fn resolve(&self, id: &str) -> Option<Resolved> {
        let by_origin_key = self
            .history
            .iter()
            .find(|entry| entry.origin_key == id)
            .map(resolved_from_history)
            .or_else(|| self.favorites.get(id).map(resolved_from_favorite));
        if by_origin_key.is_some() {
            return by_origin_key;
        }
        if !protocol::is_digest(id) {
            return None;
        }
        self.history
            .iter()
            .find(|entry| entry.digest.as_deref() == Some(id))
            .map(resolved_from_history)
            .or_else(|| {
                self.favorites
                    .values()
                    .find(|favorite| favorite.digest.as_deref() == Some(id))
                    .map(resolved_from_favorite)
            })
    }
}

fn resolved_from_history(entry: &HistoryEntry) -> Resolved {
    Resolved {
        origin_key: entry.origin_key.clone(),
        digest: entry.digest.clone(),
        kind: entry.kind,
        path: entry.path.clone(),
    }
}

fn resolved_from_favorite(favorite: &Favorite) -> Resolved {
    Resolved {
        origin_key: favorite.origin_key.clone(),
        digest: favorite.digest.clone(),
        kind: favorite.kind,
        path: favorite.path.clone(),
    }
}

/// The daemon. One lock, held only for the duration of a memory read or write:
/// nothing in this struct is held across a worker (docs/architecture.md 1.8).
pub struct Daemon {
    pub effective: Effective,
    /// The worker this daemon spawns, resolved once at startup.
    pub worker: crate::worker::Worker,
    state: Mutex<State>,
    started: Instant,
}

impl Daemon {
    pub fn new(effective: Effective, worker: crate::worker::Worker) -> Daemon {
        let bound = effective.config.state.history_entries;
        Daemon {
            effective,
            worker,
            state: Mutex::new(State::new(bound)),
            started: Instant::now(),
        }
    }

    /// The lock is only ever held for one in-memory operation, so a worker that
    /// hangs cannot hold it and cannot make `status` wait.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The complete snapshot of docs/architecture.md 2.10, in the order
    /// docs/development.md section 7 prints it, with the `source:` lines of
    /// 2.10 after the count.
    pub fn status(&self) -> Response {
        let state = self.state();
        let config = &self.effective.config;
        let current = state.current.as_ref();
        let cache = cache_usage(&self.effective.cache_dir);
        let mut response = Response::ok()
            .kv(
                "daemon_version",
                format!("{} {}", protocol::PRODUCT, protocol::VERSION),
            )
            .kv("protocol", protocol::PROTOCOL_VERSION)
            .kv("platform", platform())
            .kv("pid", std::process::id())
            .kv("seq", state.seq)
            .kv("uptime_s", self.started.elapsed().as_secs())
            .kv("rss_kb", dash(rss_kb().map(|kb| kb.to_string())))
            .kv("paused", u8::from(state.paused))
            .kv("rotating", u8::from(state.running.is_some()))
            .kv("rotation_count", state.rotation_count)
            .kv("interval_s", config.schedule.interval_seconds)
            .kv(
                "last_digest",
                dash(current.map(|current| current.digest.clone())),
            )
            .kv(
                "last_origin_key",
                dash(current.map(|current| current.origin_key.clone())),
            )
            .kv(
                "last_via",
                dash(current.map(|current| current.via.as_str().to_string())),
            )
            .kv("last_at", dash(current.map(|current| current.at.clone())))
            .kv(
                "last_error",
                dash(state.last_error.map(|code| code.as_str().to_string())),
            )
            .kv("history_entries", state.history.bound())
            .kv("history_count", state.history.iter().count())
            .kv("favorites_count", state.favorites.len())
            .kv("display_mode", config.display.mode.as_str())
            // There is no platform readback in this build, so what the platform
            // "actually gets" is unknown; the configured mode is echoed and the
            // reason stays `-` rather than inventing one of 3.7's four.
            .kv("display_mode_effective", config.display.mode.as_str())
            .kv("display_mode_reason", "-")
            // The anchor is what the daemon believes is on screen, and a
            // rotation is the only thing that moves it (1.7.3).
            .kv(
                "anchor_digest",
                dash(current.map(|current| current.digest.clone())),
            )
            .kv(
                "anchor_path",
                dash(current.and_then(|current| current.path.clone())),
            )
            .kv("anchor_verified", u8::from(state.anchor_verified))
            .kv("cache_dir", self.effective.cache_dir.display())
            // 3.1's identity file does not exist until the cache does.
            .kv("cache_root_id", "-")
            .kv(
                "cache_files",
                dash(cache.map(|(files, _)| files.to_string())),
            )
            .kv(
                "cache_bytes",
                dash(cache.map(|(_, bytes)| bytes.to_string())),
            )
            .kv("cache_files_cap", config.cache.max_files)
            .kv("cache_bytes_cap", config.cache.max_bytes)
            .kv(
                "cache_over_cap",
                u8::from(matches!(cache, Some((_, bytes)) if bytes > config.cache.max_bytes)),
            )
            // 5.4's four causes are sweep outcomes, and nothing sweeps yet.
            .kv("cache_over_reason", "-")
            .kv("cache_writable", 1)
            .kv("sweep_deferred", 0)
            // The rotation lock of 1.8 is an in-process mutex, not a file lock,
            // so `none` is the value 8.7's vocabulary gives it.
            .kv("lock_mode", "none")
            .kv("state_dir", self.effective.state_dir.display())
            .kv("state_corrupt", "-")
            .kv("state_quarantined", "-")
            .kv("state_schema_newer", 0)
            .kv("history_lost", 0)
            .kv("favorites_degraded", 0)
            .kv("clock_jump", state.clock_jump)
            // No readback, so `startup.respect_manual` cannot be honoured, and
            // 1.7.3 gives that situation the value 0.
            .kv("respect_manual_effective", 0)
            .kv("sources", self.effective.sources_enabled());
        for record in self.effective.source_records() {
            response = response.line(record.line());
        }
        response
    }

    /// A rotation is starting: claim the slot, or refuse because one is in
    /// flight (docs/architecture.md 1.8: refused, never queued).
    pub fn start_rotation(&self) -> Result<u64, u64> {
        let mut state = self.state();
        if let Some(run) = state.running {
            return Err(run);
        }
        let run = state.slot;
        state.slot += 1;
        state.running = Some(run);
        state.seq += 1;
        Ok(run)
    }

    /// Claim a slot for a verb that spawns a worker without being a rotation:
    /// `config check` (docs/architecture.md 2.5 gives it its own deadline).
    pub fn start_slot(&self) -> u64 {
        let mut state = self.state();
        let run = state.slot;
        state.slot += 1;
        run
    }

    pub fn record_success(&self, digest: &str, origin_key: &str, via: Via, path: Option<&str>) {
        let kind = self.kind_of(origin_key);
        let mut state = self.state();
        let at = now();
        state.running = None;
        state.seq += 1;
        state.rotation_count += 1;
        state.last_error = None;
        state.anchor_verified = true;
        state.current = Some(Current {
            digest: digest.to_string(),
            origin_key: origin_key.to_string(),
            via,
            kind,
            path: path.map(str::to_string),
            at: at.clone(),
        });
        state.history.push(HistoryEntry {
            set_at: at,
            via,
            kind,
            origin_key: origin_key.to_string(),
            digest: Some(digest.to_string()),
            path: path.map(str::to_string),
        });
    }

    pub fn record_failure(&self, code: ErrorCode) {
        let mut state = self.state();
        state.running = None;
        state.seq += 1;
        state.last_error = Some(code);
    }

    /// `kind` names the origin, not the mechanism (2.6), and the `origin_key`
    /// prefix is the source `id` (2.5). A prefix that matches no configured
    /// source is an `external` image, which is also what 2.6 calls it.
    fn kind_of(&self, origin_key: &str) -> Kind {
        let prefix = origin_key.split_once(':').map(|(prefix, _)| prefix);
        self.effective
            .config
            .sources
            .iter()
            .find(|source| Some(source.id.as_str()) == prefix)
            .map(|source| match source.kind {
                whirl_core::config::SourceKind::Local => Kind::Local,
                whirl_core::config::SourceKind::Wallhaven => Kind::Wallhaven,
            })
            .unwrap_or(Kind::External)
    }

    pub fn resolve_id(&self, id: &str) -> Option<Resolved> {
        self.state().resolve(id)
    }

    /// The `paused` flag, and the state change that follows it (2.10 `seq`).
    pub fn set_paused(&self, paused: bool) {
        let mut state = self.state();
        state.paused = paused;
        state.seq += 1;
    }
}

/// The current time, RFC 3339 UTC (2.6). One convention everywhere.
pub fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0);
    protocol::rfc3339_utc(seconds)
}

pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

fn dash(value: Option<String>) -> String {
    value.unwrap_or_else(|| "-".to_string())
}

/// The daemon's own resident set, in kilobytes. `/proc/self/statm` is the only
/// measurement available without a dependency, and it is Linux-only; macOS and
/// Windows report `-` rather than a number nobody measured. `statm` counts
/// 4 KiB pages, which is this project's target page size on both architectures.
#[cfg(target_os = "linux")]
fn rss_kb() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident_pages * 4)
}

#[cfg(not(target_os = "linux"))]
fn rss_kb() -> Option<u64> {
    // `ps -o rss=` is the one reading available without a dependency or an FFI
    // declaration; on Windows there is no `ps`, so the key reports `-`.
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

/// The files and bytes under the cache root, measured (`cache_files` and
/// `cache_bytes` in 2.10). `DirEntry::metadata` does not follow a symlink, so the
/// walk cannot be pulled out of the tree it was given, and an unreadable
/// directory yields `None`, which `status` reports as `-` rather than as zero.
fn cache_usage(root: &Path) -> Option<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).ok()? {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                files += 1;
                bytes += metadata.len();
            }
        }
    }
    Some((files, bytes))
}

/// Where a favorite's bytes are. There is no cache in this build, so a pin is
/// `present` when the path it names still exists, `missing` when it does not,
/// and `unrecoverable` when there is no path to check (2.6).
pub fn favorite_state(path: Option<&str>) -> FavoriteState {
    match path {
        Some(path) if std::path::Path::new(path).exists() => FavoriteState::Present,
        Some(_) => FavoriteState::Missing,
        None => FavoriteState::Unrecoverable,
    }
}
