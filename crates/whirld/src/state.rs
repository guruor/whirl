//! The daemon's state, the state files it is rebuilt from, and the `status`
//! block it answers from.
//!
//! `docs/spec/state-and-cache.md` section 6 is implemented here: `load` reads
//! `current.json`, `history.json` and `favorites.json`, quarantines what it
//! cannot read (6.4), rebuilds what it must, and every state change writes the
//! file back through `crate::statefile::Store`, which is 6.3's protocol (temp
//! file, `fsync`, `rename`).
//!
//! Everything `status` prints that the daemon does not own is measured, not
//! guessed: `pid` is this process, `uptime_s` is its own clock, `rss_kb` is
//! `/proc/self/statm` or `ps`, and the cache totals come from walking the cache
//! root this daemon created.
//!
//! The sweep (5.5) hangs off this module rather than beside it, because its
//! three trigger points are state transitions: daemon start (`crate::run`), the
//! end of every rotation (`Daemon::rotation`), and a `reset` verb, which this
//! build's protocol does not have (2.5's verb set is closed and holds no such
//! verb, and 6.5's `whirl reset` is a CLI verb that has not landed). The sweep's
//! own mechanics are `crate::cache`.

use crate::cache::{self, Attempt, Protected, Reported};
use crate::events::{Bus, Event, unix_seconds};
use crate::lock::DaemonLock;
use crate::plan::Effective;
use crate::statefile::{Kind as File, Store};
use crate::worker::{Outcome, Verb, WorkerError};
use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use whirl_core::config::Config;
use whirl_core::protocol::{self, ErrorCode, Kind, Response, SetRecord, Via};
use whirl_core::state::{
    Anchor, CacheFacts, CurrentFile, Favorite, FavoriteState, FavoritesFile, HistoryEntry,
    HistoryFile, HistoryRing, StateFile,
};

/// The image on screen, as this daemon believes it.
///
/// `origin_key` and `via` are optional because `current.json`'s anchor carries
/// neither (6.1): after a restart they are recovered from the history entry with
/// the same digest, and when no entry names that digest they report `-` rather
/// than guessing an origin for the image on screen.
#[derive(Debug, Clone)]
pub struct Current {
    pub digest: String,
    pub origin_key: Option<String>,
    pub via: Option<Via>,
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
    /// The last slot claimed: what `current.json`'s `written_by` names, so the
    /// file says which run wrote it rather than which run is next.
    pub last_run: u64,
    /// The run in flight, which is what makes `next` answer `busy` (2.7).
    pub running: Option<u64>,
    pub rotation_count: u64,
    pub last_error: Option<ErrorCode>,
    pub anchor_verified: bool,
    pub clock_jump: u64,
    pub history: HistoryRing,
    pub favorites: BTreeMap<String, Favorite>,
    pub current: Option<Current>,
    /// The persisted deadline, wall clock, RFC 3339 UTC (2.10 `next_at`).
    /// Frozen while `paused`, re-armed from now by `resume` (2.9, 5.5 rule 8).
    pub next_at: Option<i64>,
    /// The cache's identity, from `cache/index.json` (2.1): what `status`'s
    /// `cache_root_id` and 6.1's `cache` block report, and the value that lets a
    /// state file tell "the cache was cleared" from "the cache is a different
    /// cache". `None` only when the index exists and cannot be read.
    pub cache_root_id: Option<String>,
    /// 5.5 step 1's outcome, and 5.4's `sweep_error` with it: one value, so
    /// `sweep_deferred` and `cache_over_reason` cannot disagree about the last
    /// attempt. `Attempt::Pending` until the first sweep runs.
    pub sweep: Attempt,
    /// 8.4's probe. Kept live: it is re-taken at every rotation, not cached.
    pub cache_writable: bool,
    /// 6.4 step 2's sticky report: the file that failed to parse, and where it
    /// was moved to.
    pub state_corrupt: Option<String>,
    pub state_quarantined: Option<String>,
    /// 6.4 step 4: a state file written by a newer whirl. Left exactly as it is.
    pub state_schema_newer: bool,
    /// 6.4 step 3: entries lost to a quarantine, until the next clean write.
    pub history_lost: bool,
    /// 6.4 step 3: `favorites.json` was quarantined, so pin state is unknown and
    /// the daemon stops changing pins.
    pub favorites_degraded: bool,
    /// Which state files this daemon may write, indexed by `File::index`. A
    /// schema newer than this build's makes its file read-only (6.4 step 4).
    pub read_only: [bool; 3],
}

impl State {
    pub fn new(history_bound: usize) -> State {
        State {
            seq: 0,
            paused: false,
            slot: 1,
            last_run: 0,
            running: None,
            rotation_count: 0,
            last_error: None,
            anchor_verified: false,
            clock_jump: 0,
            history: HistoryRing::new(history_bound),
            favorites: BTreeMap::new(),
            current: None,
            next_at: None,
            cache_root_id: None,
            sweep: Attempt::Pending,
            cache_writable: true,
            state_corrupt: None,
            state_quarantined: None,
            state_schema_newer: false,
            history_lost: false,
            favorites_degraded: false,
            read_only: [false; 3],
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

    /// The current entry as a resolution, for `favorite` with no argument. An
    /// anchor recovered from `current.json` without a history entry to name it
    /// has no origin, so there is nothing to pin.
    pub fn current_resolved(&self) -> Option<Resolved> {
        let current = self.current.as_ref()?;
        Some(Resolved {
            origin_key: current.origin_key.clone()?,
            digest: Some(current.digest.clone()),
            kind: current.kind,
            path: current.path.clone(),
        })
    }

    /// How many seconds until `next_at`, clamped to `0..=interval`, or `None`
    /// while paused: a suspended schedule has no deadline to count down to
    /// (2.10 `next_in_s`, 5.5 rule 8). `now` is passed in rather than read here,
    /// so each state 2.10 names is asserted on a value instead of on the
    /// machine's clock.
    pub fn next_in_s(&self, now: i64, interval_seconds: u64) -> Option<i64> {
        if self.paused {
            return None;
        }
        let next_at = self.next_at?;
        let remaining = next_at - now;
        Some(remaining.clamp(0, interval_seconds as i64))
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

/// What one rotation produced, for either of its two callers: a client's
/// `next`/`set` over the control socket, or a due slot of the scheduler (2.5,
/// 5.5 rule 4). The state change has already happened by the time this is
/// returned; this is only what to *say* about it.
#[derive(Debug)]
pub enum Rotation {
    /// The wallpaper changed: 2.6's `set:` record, already parsed.
    Set(SetRecord),
    /// The rotation produced nothing: the code and message of 2.7, which are
    /// also in `last_error` and on the `rotate_failed` event.
    Failed { code: ErrorCode, message: String },
}

/// The daemon. One lock, held only for the duration of a memory read or write
/// and the small state-file write that follows one; nothing in this struct is
/// held across a worker (docs/architecture.md 1.8).
pub struct Daemon {
    pub effective: Effective,
    /// The worker this daemon spawns, resolved once at startup.
    pub worker: crate::worker::Worker,
    /// The state directory, and the only thing that writes to it (7.1).
    pub store: Store,
    /// `state/locks/daemon.lock`, taken by 1.5 step 1 before this daemon was
    /// built and held until the process ends. It is a field and not a local of
    /// `run` because `status` has to report the primitive that actually holds it
    /// (2.10 `lock_mode`), and reading that from the lock is what stops the key
    /// from being a constant.
    lock: DaemonLock,
    /// Every subscribed connection, and the quiet period a heartbeat is
    /// measured from (2.9).
    pub bus: Bus,
    state: Mutex<State>,
    started: Instant,
}

impl Daemon {
    /// Build the daemon and load the state files of 6.4.
    ///
    /// `lock` is the already-held `daemon.lock`: 1.5 step 1 runs before the state
    /// directory is even opened, so the caller takes it.
    pub fn load(effective: Effective, worker: crate::worker::Worker, lock: DaemonLock) -> Daemon {
        let store = Store::new(&effective.state_dir);
        let bound = effective.config.state.history_entries;
        let mut state = State::new(bound);

        // 8.4: the probe, at start and at each rotation, not cached.
        state.cache_writable = crate::plan::probe_cache_writable(&effective.cache_dir, state.slot);

        load_current(&store, &mut state);
        load_history(&store, &mut state);
        load_favorites(&store, &mut state);
        rebuild_current(&mut state);

        // 2.1's `root_id`, read from `index.json` rather than minted here: the
        // identity is minted on the way to a write, so there is one id per cache
        // directory and not one per reader of it. A cache that has never been
        // written has none yet, which `status` reports as `-`; the startup sweep
        // (5.5's first trigger) writes it microseconds later. An index that cannot
        // be read is not fatal at startup (the sweep reports it as 5.4's
        // `sweep_error`).
        match cache::load(&effective.cache_dir, unix_seconds()) {
            Ok(index) if !index.root_id.is_empty() => {
                state.cache_root_id = Some(index.root_id.clone())
            }
            Ok(_) => {}
            Err(message) => eprintln!("whirld: index.json: {message}"),
        }

        // 2.10 `next_at`: the persisted deadline, or a fresh one. A first run
        // has no file to read it from.
        let interval = effective.config.schedule.interval_seconds as i64;
        if state.next_at.is_none() && !state.paused {
            state.next_at = Some(unix_seconds() + interval);
        }

        Daemon {
            effective,
            worker,
            store,
            lock,
            bus: Bus::new(),
            state: Mutex::new(state),
            started: Instant::now(),
        }
    }

    /// The lock is only ever held for one in-memory operation plus the state-file
    /// write that operation implies, so a worker that hangs cannot hold it and
    /// cannot make `status` wait.
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
        // 5.4's check is the definition of these totals: the files under
        // `sha256/`, measured now, against the two caps. One walk answers
        // `cache_files`, `cache_bytes`, `cache_over_cap` and the reason, so the
        // four keys cannot disagree with each other.
        let cache = cache::survey(&self.effective.cache_dir).ok();
        let protected = protected_set(&state);
        let over = cache.as_ref().and_then(|survey| {
            if state.favorites_degraded {
                // 5.3 and 6.4 step 3 both say this without a condition on the
                // caps: while the pin set is unreadable the status line reports
                // `cache_over_reason: favorites_degraded` "rather than a bound it
                // is not enforcing". The reason is a property of the degraded
                // state, so it is reported the moment the state exists, and
                // `cache_over_cap` stays the separate fact that a cap is
                // exceeded.
                Some(cache::Cause::FavoritesDegraded)
            } else if state.sweep != Attempt::Ran {
                // 5.4: the four causes are exhaustive, so a sweep that did not
                // enforce the bound is named rather than exempted.
                (survey.bytes > config.cache.max_bytes || survey.files > config.cache.max_files)
                    .then_some(cache::Cause::SweepError)
            } else {
                cache::over_cause(
                    survey,
                    config.cache.max_bytes,
                    config.cache.max_files,
                    &protected,
                    unix_seconds(),
                    config.cache.grace_seconds,
                )
            }
        });
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
            .kv("next_at", dash(state.next_at.map(protocol::rfc3339_utc)))
            .kv(
                "next_in_s",
                dash(
                    state
                        .next_in_s(unix_seconds(), config.schedule.interval_seconds)
                        .map(|seconds| seconds.to_string()),
                ),
            )
            .kv(
                "last_digest",
                dash(current.map(|current| current.digest.clone())),
            )
            .kv(
                "last_origin_key",
                dash(current.and_then(|current| current.origin_key.clone())),
            )
            .kv(
                "last_via",
                dash(current.and_then(|current| current.via.map(|via| via.as_str().to_string()))),
            )
            .kv("last_at", dash(current.map(|current| or_dash(&current.at))))
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
            // The anchor is what the daemon believes is on screen (1.7.3).
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
            // 2.1 mints the root id inside `index.json`, which this daemon is the
            // writer of (7.2) and reads at startup.
            .kv("cache_root_id", dash(state.cache_root_id.clone()))
            .kv(
                "cache_files",
                dash(cache.as_ref().map(|survey| survey.files.to_string())),
            )
            .kv(
                "cache_bytes",
                dash(cache.as_ref().map(|survey| survey.bytes.to_string())),
            )
            .kv("cache_files_cap", config.cache.max_files)
            .kv("cache_bytes_cap", config.cache.max_bytes)
            .kv("cache_over_cap", u8::from(over.is_some()))
            .kv(
                "cache_over_reason",
                over.map(|cause| cause.as_str().to_string())
                    .unwrap_or_else(|| "-".to_string()),
            )
            .kv("cache_writable", u8::from(state.cache_writable))
            .kv("sweep_deferred", u8::from(state.sweep.deferred()))
            // The primitive actually holding `state/locks/daemon.lock`, read from
            // the lock this daemon took at startup (2.10's `lock_mode` row: two
            // values, no third, and the value names the primitive and not the
            // fact that locking happens). `none` was here before the
            // reconciliation: 1.8's in-process mutex is a different lock, and no
            // rule in either document gives it a `lock_mode`.
            .kv("lock_mode", self.lock.mode().as_str())
            .kv("state_dir", self.effective.state_dir.display())
            .kv("state_corrupt", dash(state.state_corrupt.clone()))
            .kv("state_quarantined", dash(state.state_quarantined.clone()))
            .kv("state_schema_newer", u8::from(state.state_schema_newer))
            .kv("history_lost", u8::from(state.history_lost))
            .kv("favorites_degraded", u8::from(state.favorites_degraded))
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

    /// Join the event stream of 2.9: the sequence number to publish, and the
    /// queue this connection reads.
    ///
    /// The registration and the sequence read happen under the state lock, so an
    /// event can never be emitted between the number a client is told and the
    /// moment it is listening. That is what makes `subscribed:` trustworthy.
    pub fn subscribe(&self) -> (u64, Receiver<String>) {
        let state = self.state();
        let seq = state.seq;
        let receiver = self.bus.subscribe();
        (seq, receiver)
    }

    /// Publish one event and take the next sequence number. Called with the
    /// state lock already held by every state change: one transition, one event,
    /// one number (2.9).
    fn announce(state: &mut State, bus: &Bus, event: Event) -> u64 {
        state.seq += 1;
        let seq = state.seq;
        bus.broadcast(event.line(seq));
        seq
    }

    /// A rotation is starting: claim the slot, or refuse because one is in
    /// flight (docs/architecture.md 1.8: refused, never queued). The claim is
    /// the state transition, so it is also where `rotate_start` goes out.
    pub fn start_rotation(&self) -> Result<u64, u64> {
        let mut state = self.state();
        if let Some(run) = state.running {
            return Err(run);
        }
        let run = state.slot;
        state.slot += 1;
        state.last_run = run;
        state.running = Some(run);
        // 8.4: the cache probe is re-taken at each rotation, not cached.
        state.cache_writable = crate::plan::probe_cache_writable(&self.effective.cache_dir, run);
        // `running` is part of what a client reads from `status`, but 6.1 lists
        // no such field, so this state change writes no file: the event is the
        // only record of it. One event per transition, and this is the start.
        Self::announce(&mut state, &self.bus, Event::RotateStart { run });
        Ok(run)
    }

    /// Claim a slot for a verb that spawns a worker without being a rotation:
    /// `config check` (docs/architecture.md 2.5 gives it its own deadline).
    pub fn start_slot(&self) -> u64 {
        let mut state = self.state();
        let run = state.slot;
        state.slot += 1;
        state.last_run = run;
        run
    }

    /// A rotation succeeded: the anchor moves, the ring gains an entry, and both
    /// files are written back (6.2, 6.3).
    pub fn record_success(&self, digest: &str, origin_key: &str, via: Via, path: Option<&str>) {
        let kind = self.kind_of(origin_key);
        let mut state = self.state();
        let at = now();
        state.running = None;
        state.rotation_count += 1;
        state.last_error = None;
        state.anchor_verified = true;
        state.current = Some(Current {
            digest: digest.to_string(),
            origin_key: Some(origin_key.to_string()),
            via: Some(via),
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
        Self::announce(
            &mut state,
            &self.bus,
            Event::RotateOk {
                digest: digest.to_string(),
                origin_key: origin_key.to_string(),
                via: via.as_str(),
                path: path.map(str::to_string),
            },
        );
        self.save_current(&state);
        self.save_history(&mut state);
    }

    /// A rotation failed. `last_error` is the code a client reads, and the
    /// message rides the event (2.9 `rotate_failed`).
    pub fn record_failure(&self, code: ErrorCode, message: &str) {
        let mut state = self.state();
        state.running = None;
        state.last_error = Some(code);
        Self::announce(
            &mut state,
            &self.bus,
            Event::RotateFailed {
                code,
                message: message.to_string(),
            },
        );
        self.save_current(&state);
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

    /// The `plan:` line of 2.6 for this daemon, from the one implementation
    /// beside the schema (`Config::plan_pairs`).
    ///
    /// `whirl config check` prints this same line, and 2.6's claim is that "what
    /// did the daemon actually adopt" is answerable from the line alone, so the
    /// daemon records it at a rotation instead of keeping a second opinion about
    /// the effective values.
    pub fn plan_line(&self) -> String {
        protocol::plan_record(&self.effective.config.plan_pairs(self.effective.backend))
    }

    /// 8.7's reporting half, for the one verb that reports it besides `status`:
    /// 4.2's `cache.root` comment has `config check` "report `lock_mode:
    /// excl_file` if a weaker lock had to be used". `None` under `flock`, because
    /// 2.5's `config check` row lists that body exhaustively.
    pub fn lock_line(&self) -> Option<String> {
        self.lock.report_line()
    }

    /// The deadline a worker gets: `schedule.worker_deadline_seconds` (1.7.1).
    pub fn worker_deadline(&self) -> Duration {
        Duration::from_secs(self.effective.config.schedule.worker_deadline_seconds)
    }

    /// One rotation, in the slot `run` the caller claimed, with its outcome
    /// recorded. This is the whole of the rotation path, shared by its two
    /// callers so that a scheduled slot and a client's `next` cannot diverge:
    /// `crate::socket` writes the `set:` line or the `ERR` to the client, and
    /// `crate::scheduler` logs the outcome for a slot nobody asked for.
    ///
    /// The worker is spawned outside the state lock (1.8), and `record_success`
    /// or `record_failure` is what clears `running` again.
    pub fn rotation(&self, run: u64, via: Via, verb: Verb, target: Option<&str>) -> Rotation {
        let deadline = self.worker_deadline();
        let mut reported = None;
        let outcome = match self.worker.run(verb, target, run, deadline) {
            Ok(Outcome::Set(record)) => {
                self.record_success(
                    &record.digest,
                    &record.origin_key,
                    via,
                    record.path.as_deref(),
                );
                // 7.3 step 4's input: the three fields the worker's `set:` line
                // carries, with the kind from the source the daemon spawned. An
                // empty path is a `set:` line without one, which is not a cache
                // path and so records no index entry.
                reported = Some(Reported {
                    digest: record.digest.clone(),
                    origin_key: record.origin_key.clone(),
                    path: record.path.clone().unwrap_or_default(),
                    kind: self.kind_of(&record.origin_key),
                });
                Rotation::Set(record)
            }
            // A rotation verb answered with `source:`/`plan:` lines is not the
            // `set:` of 2.5 or 2.6, so its output could not be parsed into the
            // answer the client asked for: `worker_failed` (2.7). The lines are
            // in the message because they are all the evidence there is.
            Ok(Outcome::Lines(lines)) => {
                let message = format!(
                    "the worker answered a rotation with check lines: {}",
                    lines.join(" | ")
                );
                self.record_failure(ErrorCode::WorkerFailed, &message);
                Rotation::Failed {
                    code: ErrorCode::WorkerFailed,
                    message,
                }
            }
            Err(WorkerError::Timeout) => {
                let message = "the worker did not finish inside schedule.worker_deadline_seconds";
                self.record_failure(ErrorCode::Timeout, message);
                Rotation::Failed {
                    code: ErrorCode::Timeout,
                    message: message.to_string(),
                }
            }
            Err(WorkerError::Failed { code, message }) => {
                self.record_failure(code, &message);
                Rotation::Failed { code, message }
            }
        };
        // 7.3 step 4 and 5.5's second trigger: the index entry, then the sweep,
        // after the worker has exited. A failed rotation runs the sweep too
        // ("including failed ones") and records no entry, because there is
        // nothing the worker reported.
        self.finish_rotation(reported.as_ref());
        outcome
    }

    /// Spend the slot of a due rotation: the deadline advances by whole
    /// intervals (5.5 rule 4) and is persisted *before* the worker is spawned,
    /// because the slot is spent whether or not the rotation inside it succeeds
    /// (1.7.2: a crash mid-rotation has "the slot consumed").
    ///
    /// Only a due slot calls this. A client's `next` is not a slot on the grid,
    /// so it leaves `next_at` where it is: 2.12's table re-anchors on the
    /// rotation that a slot ran, not on every rotation.
    pub fn spend_slot(&self, next_at: i64) {
        let mut state = self.state();
        state.next_at = Some(next_at);
        self.save_current(&state);
    }

    /// 5.5 rule 5: one line in the daemon's log, one more `clock_jump` for
    /// `status` (2.10), the deadline re-anchored from the wall clock, and the
    /// event on the stream (2.9). Not a rotation, so no `rotate_*` event.
    pub fn record_clock_jump(&self, seconds: i64, next_at: i64) {
        let mut state = self.state();
        state.clock_jump += 1;
        state.next_at = Some(next_at);
        Self::announce(&mut state, &self.bus, Event::ClockJump { seconds });
        self.save_current(&state);
        eprintln!(
            "whirld: clock jump: the wall clock moved {seconds} s, next_at re-anchored to {}",
            protocol::rfc3339_utc(next_at)
        );
    }

    /// 5.5's sweep: one implementation, three trigger points, and it always runs
    /// while holding the rotation lock (7.2), which is what makes it unable to
    /// race a download.
    ///
    /// **Triggers.** `crate::run` calls this after the socket is bound and before
    /// the first slot; [`Daemon::finish_rotation`] calls it at the end of every
    /// rotation, including failed ones; and 5.5's third trigger, a `reset` verb,
    /// has no call site in this build, because 2.5's verb set is closed and holds
    /// no such verb and 6.5's `whirl reset` is a CLI verb that has not landed.
    /// Never on a timer: "a timer is a resident thing to wake up for".
    pub fn sweep(&self) {
        let config = cache::CacheConfig::from(&self.effective.config.cache);
        // Step 1: the rotation lock, non-blocking. The worker's own half of 7.2
        // (it takes `rotate.lock` for its run) is not in this build:
        // `crates/whirl-worker` has no lock module yet, so a hand-run worker is
        // the only other process this can meet.
        let guard = match crate::lock::take_rotate(&self.effective.state_dir) {
            Ok(Some(guard)) => guard,
            Ok(None) => {
                // 5.5 step 7's one line, with `deferred=1`: the shape is fixed,
                // and two numbers of it are measured rather than left empty.
                let measured = cache::survey(&self.effective.cache_dir).ok();
                eprintln!(
                    "sweep files={} bytes={} removed=0 reclaimed=0 orphans=0 deferred=1",
                    measured
                        .as_ref()
                        .map(|measured| measured.files.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    measured
                        .as_ref()
                        .map(|measured| measured.bytes.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                );
                self.record_sweep(Attempt::Deferred, None);
                return;
            }
            Err(message) => {
                eprintln!("whirld: sweep failed: {message}");
                self.record_sweep(Attempt::Failed, None);
                return;
            }
        };
        let protected = {
            let state = self.state();
            protected_set(&state)
        };
        let now = unix_seconds();
        match cache::sweep(&self.effective.cache_dir, &config, &protected, now) {
            Ok(swept) => {
                // Step 7: one line, in the shape 5.5 prints.
                eprintln!(
                    "sweep files={} bytes={} removed={} reclaimed={} orphans={} deferred=0",
                    swept.files, swept.bytes, swept.removed, swept.reclaimed, swept.orphans
                );
                self.record_sweep(Attempt::Ran, Some(swept));
            }
            Err(message) => {
                eprintln!("whirld: sweep failed: {message}");
                self.record_sweep(Attempt::Failed, None);
            }
        }
        drop(guard);
    }

    /// What the last sweep attempt did, for 2.10's `sweep_deferred`, 5.4's
    /// `sweep_error` and 2.9's `cache_swept`. One place, so the three cannot
    /// disagree about the attempt they are all describing.
    fn record_sweep(&self, attempt: Attempt, swept: Option<cache::Swept>) {
        let mut state = self.state();
        state.sweep = attempt;
        if state.cache_root_id.is_none() && attempt == Attempt::Ran {
            // The write this attempt has just made mints the id when the cache had
            // none, so this is the value `index.json` now carries.
            if let Ok(index) = cache::load(&self.effective.cache_dir, unix_seconds()) {
                if !index.root_id.is_empty() {
                    state.cache_root_id = Some(index.root_id);
                }
            }
        }
        if let Some(swept) = swept {
            Self::announce(
                &mut state,
                &self.bus,
                Event::CacheSwept {
                    // 2.9's `<removed>` is every file this sweep unlinked, across
                    // 5.5's steps 3, 4 and 5; the log line keeps the three
                    // apart, and the event is the one number a subscriber wants.
                    removed: swept.removed + swept.reclaimed + swept.orphans,
                    reclaimed_bytes: swept.freed_bytes,
                    hidden: swept.hidden,
                },
            );
        }
    }

    /// 7.3 step 4's daemon half, after the worker has exited: the index entry for
    /// what the worker reported (7.2 makes the daemon the writer of
    /// `cache/index.json`), then the sweep. Both run after every rotation,
    /// including a failed one (5.5).
    fn finish_rotation(&self, reported: Option<&Reported>) {
        if let Some(reported) = reported {
            let protected = {
                let state = self.state();
                protected_set(&state)
            };
            let now = unix_seconds();
            match cache::record(&self.effective.cache_dir, reported, &protected, now) {
                // The sweep writes the file; an entry recorded here is already in
                // it, and 5.5 step 6 writes it again on every sweep anyway.
                Ok(_) => {}
                Err(message) => eprintln!("whirld: index entry not written: {message}"),
            }
        }
        self.sweep();
    }

    pub fn resolve_id(&self, id: &str) -> Option<Resolved> {
        self.state().resolve(id)
    }

    /// One heartbeat, for the quiet period a connection claimed. A heartbeat is
    /// an event and consumes a `seq` like any other (2.9); it is not a state
    /// change, so it writes no file.
    pub fn heartbeat(&self) {
        let mut state = self.state();
        Self::announce(
            &mut state,
            &self.bus,
            Event::Heartbeat {
                unix_seconds: unix_seconds(),
            },
        );
    }

    /// The `paused` flag, the state change that follows it, and the freeze or
    /// re-arm of `next_at` (2.9, 5.5 rule 8).
    pub fn set_paused(&self, paused: bool) {
        let mut state = self.state();
        state.paused = paused;
        if !paused {
            // 2.9 `resumed`: the schedule is live again, `next_at` re-armed from
            // now (`crate::schedule::rearmed_from`, the same rule the scheduler
            // reads it from, so `resume` and a fresh start cannot disagree).
            // While paused `next_at` keeps its value, frozen: a suspended
            // schedule has no deadline to count down to.
            let interval = self.effective.config.schedule.interval_seconds;
            state.next_at = Some(crate::schedule::rearmed_from(unix_seconds(), interval));
        }
        Self::announce(
            &mut state,
            &self.bus,
            if paused {
                Event::Paused
            } else {
                Event::Resumed
            },
        );
        self.save_current(&state);
    }

    /// A pin was written. `None` when this id is already pinned, which is the
    /// `already: 1` case and not a state change.
    pub fn add_favorite(&self, resolved: &Resolved) -> bool {
        let mut state = self.state();
        if state.favorites.contains_key(&resolved.origin_key) {
            return false;
        }
        let favorite = Favorite {
            added_at: now(),
            kind: resolved.kind,
            origin_key: resolved.origin_key.clone(),
            digest: resolved.digest.clone(),
            state: crate::state::favorite_state(resolved.path.as_deref()),
            path: resolved.path.clone(),
        };
        state
            .favorites
            .insert(resolved.origin_key.clone(), favorite);
        let digest = resolved.digest.clone().unwrap_or_else(|| "-".to_string());
        Self::announce(
            &mut state,
            &self.bus,
            Event::FavoriteAdded {
                digest: digest.clone(),
                origin_key: resolved.origin_key.clone(),
            },
        );
        self.save_favorites(&state);
        true
    }

    /// A pin was removed. `None` when the id resolves to nothing.
    pub fn remove_favorite(&self, id: &str) -> Option<String> {
        let mut state = self.state();
        let removed = state
            .resolve(id)
            .and_then(|resolved| state.favorites.remove(&resolved.origin_key));
        match removed {
            None => None,
            Some(favorite) => {
                let digest = favorite.digest.unwrap_or_else(|| "-".to_string());
                Self::announce(
                    &mut state,
                    &self.bus,
                    Event::FavoriteRemoved {
                        digest: digest.clone(),
                    },
                );
                self.save_favorites(&state);
                Some(digest)
            }
        }
    }

    /// 6.4 step 3: while `favorites.json` is degraded, pin-changing verbs are
    /// refused and the message carries the quarantine path.
    pub fn favorites_degraded_message(&self) -> Option<String> {
        let state = self.state();
        if !state.favorites_degraded {
            return None;
        }
        Some(
            state
                .state_quarantined
                .clone()
                .unwrap_or_else(|| "favorites.json".to_string()),
        )
    }

    /// Write `current.json` (6.1) as it stands. A file this build is too old for
    /// is left alone (6.4 step 4).
    fn save_current(&self, state: &State) {
        if state.read_only[File::Current.index()] {
            return;
        }
        let config: &Config = &self.effective.config;
        let file = CurrentFile {
            seq: state.seq,
            written_at: now(),
            written_by: Some(format!(
                "{}/{} pid={} run={}",
                protocol::PRODUCT,
                protocol::VERSION,
                std::process::id(),
                state.last_run
            )),
            paused: state.paused,
            rotation_count: state.rotation_count,
            next_at: state.next_at.map(protocol::rfc3339_utc),
            last_error: state.last_error.map(|code| code.as_str().to_string()),
            anchor: state.current.as_ref().map(|current| Anchor {
                digest: current.digest.clone(),
                cached_path: current.path.clone(),
                set_at: Some(current.at.clone()),
                display_mode: Some(config.display.mode.as_str().to_string()),
            }),
            cache: cache::survey(&self.effective.cache_dir)
                .ok()
                .map(|survey| CacheFacts {
                    // 2.1: the identity and the totals both come from the cache's
                    // own files, and this daemon is the writer of both (7.2).
                    root_id: state.cache_root_id.clone(),
                    files: survey.files,
                    bytes: survey.bytes,
                    over_cap_bytes: u64::from(survey.bytes > config.cache.max_bytes),
                    over_cap_files: u64::from(survey.files > config.cache.max_files),
                }),
        };
        self.write(File::Current, file.encode());
    }

    /// Write `history.json` (6.2): newest first, bounded at
    /// `state.history_entries`. A clean write is what clears `history_lost`
    /// (6.4 step 3).
    fn save_history(&self, state: &mut State) {
        if state.read_only[File::History.index()] {
            return;
        }
        let file = HistoryFile {
            seq: state.seq,
            written_at: now(),
            entries: state.history.iter().cloned().collect(),
        };
        if self.write(File::History, file.encode()) {
            state.history_lost = false;
        }
    }

    /// Write `favorites.json` (6.2). This is the one irreversible file, and the
    /// authoritative pin list.
    fn save_favorites(&self, state: &State) {
        if state.read_only[File::Favorites.index()] {
            return;
        }
        let file = FavoritesFile {
            seq: state.seq,
            written_at: now(),
            entries: state.favorites.values().cloned().collect(),
        };
        self.write(File::Favorites, file.encode());
    }

    /// One state-file write, logged rather than fatal: 8.5 refuses to start on a
    /// state directory that cannot be written, but a write that fails while the
    /// daemon runs keeps the last good state and keeps serving
    /// (docs/architecture.md 7.4, the disk-full row).
    fn write(&self, kind: File, text: String) -> bool {
        match self.store.write(kind, &text) {
            Ok(()) => true,
            Err(message) => {
                eprintln!("whirld: state write failed: {message}");
                false
            }
        }
    }
}

/// The current time, RFC 3339 UTC (2.6). One convention everywhere.
pub fn now() -> String {
    protocol::rfc3339_utc(unix_seconds())
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

/// A timestamp that a state file left empty prints as `-` like every other unset
/// value (2.6).
fn or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

// ---------------------------------------------------------------------------
// Loading (docs/spec/state-and-cache.md 6.4)
// ---------------------------------------------------------------------------

/// Read and apply `current.json`. A file that cannot be read or parsed is
/// quarantined and its scalars fall back to what `history.json` can supply.
fn load_current(store: &Store, state: &mut State) {
    let text = match store.read(File::Current) {
        Ok(None) => return,
        Ok(Some(text)) => text,
        Err(message) => {
            eprintln!("whirld: {message}");
            quarantine(store, state, File::Current);
            return;
        }
    };
    match CurrentFile::parse(&text) {
        Ok(StateFile::Read(file)) => {
            state.paused = file.paused;
            state.rotation_count = file.rotation_count;
            state.next_at = file
                .next_at
                .as_deref()
                .and_then(protocol::parse_rfc3339_utc);
            if file.next_at.is_some() && state.next_at.is_none() {
                // A deadline this build cannot read is not a deadline it may
                // schedule against: it re-arms from now instead of guessing.
                eprintln!("whirld: current.json: next_at is not an RFC 3339 UTC timestamp");
            }
            state.last_error = file.last_error.as_deref().and_then(ErrorCode::parse);
            if let Some(anchor) = file.anchor {
                // The anchor is the display record (5.3) and the reason this
                // file exists. `origin_key` and `via` are not in it (6.1), so
                // they are recovered from the history entry with the same
                // digest once history is loaded.
                state.current = Some(Current {
                    digest: anchor.digest,
                    origin_key: None,
                    via: None,
                    kind: Kind::External,
                    path: anchor.cached_path,
                    at: anchor.set_at.unwrap_or_default(),
                });
                state.anchor_verified = true;
            }
        }
        Ok(StateFile::SchemaNewer { found }) => {
            eprintln!(
                "whirld: current.json was written by schema {found} and this build understands {}: leaving it alone",
                whirl_core::state::SCHEMA
            );
            state.read_only[File::Current.index()] = true;
            state.state_schema_newer = true;
        }
        Err(error) => {
            eprintln!("whirld: current.json: {error}");
            quarantine(store, state, File::Current);
        }
    }
}

/// Read and apply `history.json`. A quarantine starts from an empty ring and
/// says so, because a ring that vanished silently is the failure mode 6.4 step 2
/// exists to prevent.
fn load_history(store: &Store, state: &mut State) {
    let text = match store.read(File::History) {
        Ok(None) => return,
        Ok(Some(text)) => text,
        Err(message) => {
            eprintln!("whirld: {message}");
            quarantine(store, state, File::History);
            state.history_lost = true;
            return;
        }
    };
    match HistoryFile::parse(&text) {
        Ok(StateFile::Read(file)) => {
            // Newest first, as written; the ring bounds it again in case the
            // file was edited by hand.
            for entry in file.entries.into_iter().rev() {
                state.history.push(entry);
            }
        }
        Ok(StateFile::SchemaNewer { found }) => {
            eprintln!(
                "whirld: history.json was written by schema {found} and this build understands {}: leaving it alone",
                whirl_core::state::SCHEMA
            );
            state.read_only[File::History.index()] = true;
            state.state_schema_newer = true;
        }
        Err(error) => {
            eprintln!("whirld: history.json: {error}");
            quarantine(store, state, File::History);
            state.history_lost = true;
        }
    }
}

/// Read and apply `favorites.json`. This is the escalation of 6.4 step 3: a
/// corrupt pin file is not an empty pin set, it is a daemon that stops changing
/// pins and protects the whole cache until the user resolves it.
fn load_favorites(store: &Store, state: &mut State) {
    let text = match store.read(File::Favorites) {
        Ok(None) => return,
        Ok(Some(text)) => text,
        Err(message) => {
            eprintln!("whirld: {message}");
            quarantine(store, state, File::Favorites);
            state.favorites_degraded = true;
            return;
        }
    };
    match FavoritesFile::parse(&text) {
        Ok(StateFile::Read(file)) => {
            for favorite in file.entries {
                state
                    .favorites
                    .insert(favorite.origin_key.clone(), favorite);
            }
        }
        Ok(StateFile::SchemaNewer { found }) => {
            eprintln!(
                "whirld: favorites.json was written by schema {found} and this build understands {}: leaving it alone",
                whirl_core::state::SCHEMA
            );
            state.read_only[File::Favorites.index()] = true;
            state.state_schema_newer = true;
        }
        Err(error) => {
            eprintln!("whirld: favorites.json: {error}");
            quarantine(store, state, File::Favorites);
            state.favorites_degraded = true;
        }
    }
}

/// 6.4 step 1: rename the file out of the way and report it. A quarantine that
/// itself fails (a read-only directory) is still reported as corruption rather
/// than becoming a startup failure: the daemon continues with defaults and the
/// status line says so.
fn quarantine(store: &Store, state: &mut State, kind: File) {
    state.state_corrupt = Some(kind.name().to_string());
    match store.quarantine(kind, unix_seconds()) {
        Ok(path) => state.state_quarantined = Some(path.display().to_string()),
        Err(message) => eprintln!("whirld: {message}"),
    }
}

/// 6.4 step 3 and 6.1: `current.json` is derived, so a missing or quarantined
/// one is rebuilt from `history.json`'s newest entry. The digest is the join
/// between the anchor and the entry that named it.
fn rebuild_current(state: &mut State) {
    let newest = state.history.iter().next().cloned();
    let anchor = state.current.take();
    state.current = match (anchor, newest) {
        (Some(anchor), Some(entry)) => {
            let named = entry.digest.as_deref() == Some(anchor.digest.as_str())
                // A `reference`-mode entry has no digest, so the path is the
                // only identity it can be matched on.
                || (entry.path.is_some() && entry.path == anchor.path);
            Some(Current {
                origin_key: named.then(|| entry.origin_key.clone()),
                via: named.then_some(entry.via),
                kind: if named { entry.kind } else { Kind::External },
                at: if anchor.at.is_empty() {
                    entry.set_at.clone()
                } else {
                    anchor.at.clone()
                },
                digest: anchor.digest,
                path: anchor.path,
            })
        }
        (Some(anchor), None) => Some(Current {
            digest: anchor.digest,
            origin_key: None,
            via: None,
            kind: Kind::External,
            path: anchor.path,
            at: anchor.at,
        }),
        (None, Some(entry)) => {
            // Rebuilt from history: the file's anchor is gone, so nothing has
            // been read back in this run (1.7.3).
            state.anchor_verified = false;
            Some(Current {
                digest: entry.digest.clone().unwrap_or_default(),
                origin_key: Some(entry.origin_key.clone()),
                via: Some(entry.via),
                kind: entry.kind,
                path: entry.path.clone(),
                at: entry.set_at.clone(),
            })
        }
        (None, None) => None,
    };
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

/// 5.3's protected set, resolved from the state this daemon holds. Two keys per
/// rule, a digest and a path, because either can be the only one available: an
/// index entry carries the digest, a `Favorite` carries both, and a file the
/// daemon died before recording carries only a path.
///
/// 5.3's escalation is here too: while `favorites.json` is quarantined
/// (`favorites_degraded: 1`, 6.4) the pin set cannot be read, so the whole cache
/// is protected.
fn protected_set(state: &State) -> Protected {
    let mut protected = Protected {
        degraded: state.favorites_degraded,
        ..Protected::default()
    };
    if let Some(current) = &state.current {
        protected.anchor_digest = Some(current.digest.clone());
        if let Some(path) = &current.path {
            protected.anchor_paths.insert(path.clone());
        }
    }
    for favorite in state.favorites.values() {
        // 5.3: "every `favorites.json` entry with a materialised `cached_path`
        // and digest". Storing both means a hand-edited file can make the two
        // disagree, and the union still protects what either names.
        if let Some(digest) = &favorite.digest {
            protected.pinned_digests.insert(digest.clone());
        }
        if let Some(path) = &favorite.path {
            protected.pinned_paths.insert(path.clone());
        }
    }
    protected
}

/// Where a favorite's bytes are. There is no cache in this build, so a pin is
/// `present` when the path it names still exists, `missing` when it does not,
/// and `unrecoverable` when there is no path to check (2.6).
pub fn favorite_state(path: Option<&str>) -> FavoriteState {
    whirl_core::state::favorite_state_of(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{Attempt, Mode};
    use std::path::{Path, PathBuf};
    use whirl_core::config::Backend;

    /// 2.10's `next_in_s` row, one assertion per state it names, with `now`
    /// passed in: a live deadline counts down, a deadline already passed is `0`
    /// and never negative, a deadline further away than one interval is capped
    /// at the interval, a suspended schedule has no deadline to count down to
    /// (`-`, which is `None` here), and a missing deadline is `-` too.
    ///
    /// Each assertion fails on a wrong value of the right shape: dropping the
    /// clamp gives `-7` for the passed deadline and `99_999` for the distant
    /// one, and returning `Some(0)` for a paused daemon (a plausible reading of
    /// "no countdown") fails the two `None` assertions.
    #[test]
    fn next_in_s_is_the_countdown_of_2_10_in_every_state() {
        let mut state = State::new(50);
        assert_eq!(
            state.next_in_s(1_000, 1800),
            None,
            "no persisted deadline: 2.10's `-` while the schedule is unset"
        );

        state.next_at = Some(2_800);
        assert_eq!(state.next_in_s(1_000, 1800), Some(1_800));
        assert_eq!(
            state.next_in_s(1_799, 1800),
            Some(1_001),
            "a second later the countdown is a second shorter"
        );
        assert_eq!(
            state.next_in_s(2_807, 1800),
            Some(0),
            "a deadline already passed counts down to 0, not below it"
        );
        // A deadline further away than one interval (a hand-edited state file)
        // is capped at the interval rather than reported as-is.
        state.next_at = Some(1_000_000);
        assert_eq!(
            state.next_in_s(900_000, 1800),
            Some(1_800),
            "capped at one interval"
        );

        state.next_at = Some(2_800);
        state.paused = true;
        assert_eq!(
            state.next_in_s(1_200, 1800),
            None,
            "a suspended schedule has no deadline to count down to (5.5 rule 8)"
        );
    }

    /// `resume` re-arms from now (2.5, 5.5 rule 8), so the countdown straight
    /// after it is the full interval; `pause` freezes the deadline where it was
    /// rather than advancing it, so the value is unchanged by the pause.
    #[test]
    fn a_re_armed_deadline_counts_down_from_the_full_interval() {
        let mut state = State::new(50);
        state.next_at = Some(crate::schedule::rearmed_from(1_000, 1800));
        assert_eq!(state.next_in_s(1_000, 1800), Some(1_800));
        state.paused = true;
        state.paused = false;
        assert_eq!(
            state.next_in_s(1_000, 1800),
            Some(1_800),
            "the flag does not move the deadline"
        );
    }

    /// The `lock_mode` value of a `status` response (2.10).
    fn lock_mode(daemon: &Daemon) -> String {
        daemon
            .status()
            .lines()
            .iter()
            .find_map(|line| line.strip_prefix("lock_mode: ").map(str::to_string))
            .expect("lock_mode is in the 2.10 key set")
    }

    /// A daemon over a directory of its own, holding a lock whose OS answer is
    /// supplied by `attempt`: the two values of 2.10's row are otherwise not both
    /// reachable on a machine whose filesystems all support `flock`.
    fn daemon(dir: &Path, attempt: Attempt) -> Daemon {
        daemon_with(dir, attempt, Config::default())
    }

    /// The same, with the config of the test's choosing: the caps of 5.1 and the
    /// graces of 5.3 are the inputs every eviction decision is made from.
    fn daemon_with(dir: &Path, attempt: Attempt, config: Config) -> Daemon {
        let state_dir = dir.join("state");
        let effective = Effective {
            config_path: dir.join("config.json"),
            socket_path: dir.join("whirl.sock"),
            state_dir: state_dir.clone(),
            cache_dir: dir.join("cache"),
            backend: Backend::Noop,
            config,
        };
        let worker = crate::worker::Worker::new(
            PathBuf::from("whirl-worker"),
            effective.config_path.clone(),
            Backend::Noop,
            effective.state_dir.clone(),
            effective.cache_dir.clone(),
        );
        let lock = crate::lock::take_as(&state_dir, attempt).expect("the lock is taken");
        Daemon::load(effective, worker, lock)
    }

    /// 2.10's `lock_mode` row: "the primitive actually holding
    /// `state/locks/daemon.lock` ... There is no third value". The value changes
    /// with the primitive and comes from the lock object, so `none` (the literal
    /// this key used to be) and any other constant fails one of these two
    /// assertions whichever constant it is.
    #[test]
    fn status_reports_the_primitive_that_holds_the_daemon_lock() {
        let root = std::env::temp_dir().join(format!("whirl-lock-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // Two directories: 8.7's fallback file is exclusive, so one directory
        // cannot hold a `daemon.lock` of each mode.
        let flock = daemon(&root.join("flock"), Attempt::Acquired);
        assert_eq!(lock_mode(&flock), Mode::Flock.as_str());
        assert_eq!(lock_mode(&flock), "flock");

        let exclusive = daemon(&root.join("excl"), Attempt::Unsupported);
        assert_eq!(lock_mode(&exclusive), Mode::ExclFile.as_str());
        assert_eq!(lock_mode(&exclusive), "excl_file");

        drop(flock);
        drop(exclusive);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One `key: value` of a `status` response, as a client reads it (2.4).
    fn kv(response: &Response, key: &str) -> String {
        response
            .lines()
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{key}: ")).map(str::to_string))
            .unwrap_or_else(|| panic!("{key} is in the 2.10 key set: {:?}", response.lines()))
    }

    /// Indexed cache files under a scratch cache root: a file with no index entry
    /// is 5.5 step 3's orphan and is not what these tests are about, so each file
    /// gets the entry that makes it an entry step 5 may evict. `last_used` runs
    /// oldest first, which is the order step 5 evicts in.
    fn plant_images(cache_dir: &Path, digests: &[String]) {
        let mut index = whirl_core::state::IndexFile {
            seq: 0,
            written_at: "2026-01-01T00:00:00Z".to_string(),
            root_id: "test-root".to_string(),
            entries: BTreeMap::new(),
            dangling: Vec::new(),
        };
        for (position, digest) in digests.iter().enumerate() {
            let path = whirl_core::state::content_path(cache_dir, digest, "jpg");
            std::fs::create_dir_all(path.parent().expect("a digest directory"))
                .expect("the parents");
            std::fs::write(&path, vec![b'x'; 100]).expect("a cache file");
            index.entries.insert(
                digest.clone(),
                whirl_core::state::CacheIndexEntry {
                    ext: "jpg".to_string(),
                    bytes: 100,
                    first_seen: "2026-01-01T00:00:00Z".to_string(),
                    last_used: format!("2026-01-0{}T00:00:00Z", position + 1),
                    source: "test".to_string(),
                    kind: Kind::Local,
                    origin: None,
                    origin_key: "test:1".to_string(),
                    width: None,
                    height: None,
                    pinned: false,
                },
            );
        }
        cache::write(cache_dir, &index).expect("the planted index");
    }

    /// 2.10's `sweep_deferred` and 5.4's `sweep_error` are two readings of one
    /// attempt, so a cache over its cap reports the sweep that did not run rather
    /// than a bound that held: the same daemon is put in each of the states
    /// `Attempt` can be in before a sweep finishes, and then allowed to sweep for
    /// real through `Daemon::sweep` (5.5 step 1's lock included).
    ///
    /// A `status` that reported `cache_over_cap: 0` for a cache it never
    /// corrected, or `sweep_deferred: 1` for a sweep that ran and failed, fails
    /// one of these assertions; the two keys are read from one attempt, so they
    /// cannot disagree about it.
    #[test]
    fn status_reports_a_sweep_that_did_not_run_and_then_the_bound_it_enforced() {
        let root = std::env::temp_dir().join(format!("whirl-sweep-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mut config = Config::default();
        config.cache.max_files = 2;
        // 5.3 protects "any cache file created within `cache.grace_seconds`", and
        // every file this test writes was created seconds ago: 0 is the smallest
        // window that lets the cap bind at all, and 4.3 puts no floor on it.
        config.cache.grace_seconds = 0;
        let daemon = daemon_with(&root, Attempt::Acquired, config);

        let cache_dir = root.join("cache");
        std::fs::create_dir_all(&cache_dir).expect("the cache root");
        let digests: Vec<String> = ['1', '2', '3']
            .iter()
            .map(|byte| byte.to_string().repeat(64))
            .collect();
        plant_images(&cache_dir, &digests);

        // Nothing has swept yet, and the cache is over a cap: 5.4's four causes
        // are exhaustive, so this is `sweep_error` rather than an exemption.
        let before = daemon.status();
        assert_eq!(kv(&before, "cache_files"), "3");
        assert_eq!(
            kv(&before, "cache_over_cap"),
            "1",
            "three files against a cap of two"
        );
        assert_eq!(kv(&before, "cache_over_reason"), "sweep_error");
        assert_eq!(
            kv(&before, "sweep_deferred"),
            "0",
            "1 is 5.5 step 1's case and nothing else"
        );

        // 5.5 step 1: someone else held `rotate.lock`.
        daemon.state().sweep = cache::Attempt::Deferred;
        let deferred = daemon.status();
        assert_eq!(kv(&deferred, "sweep_deferred"), "1");
        assert_eq!(kv(&deferred, "cache_over_reason"), "sweep_error");

        // 5.5's other way to not finish: it ran and failed (8.1's disk-full case).
        daemon.state().sweep = cache::Attempt::Failed;
        let failed = daemon.status();
        assert_eq!(
            kv(&failed, "sweep_deferred"),
            "0",
            "a failed sweep is a different fact (2.10)"
        );
        assert_eq!(kv(&failed, "cache_over_reason"), "sweep_error");

        // And the sweep that does run, through the real rotation lock.
        daemon.sweep();
        let after = daemon.status();
        assert_eq!(
            kv(&after, "cache_files"),
            "2",
            "5.5 step 5 evicted down to `cache.max_files`"
        );
        assert_eq!(kv(&after, "cache_over_cap"), "0");
        assert_eq!(kv(&after, "cache_over_reason"), "-");
        assert_eq!(kv(&after, "sweep_deferred"), "0");
        assert!(
            !whirl_core::state::content_path(&cache_dir, &digests[0], "jpg").exists(),
            "the oldest `last_used` went"
        );
        assert!(
            whirl_core::state::content_path(&cache_dir, &digests[2], "jpg").exists(),
            "the newest is still there"
        );

        drop(daemon);
        let _ = std::fs::remove_dir_all(&root);
    }
}
