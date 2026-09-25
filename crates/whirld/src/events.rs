//! The event stream of `docs/architecture.md` 2.9: the closed vocabulary, the
//! encoding, and one bounded queue per subscriber.
//!
//! Events are notifications, not data transfer: the daemon keeps no event
//! history, so a client that sees a `gap:` re-reads `status` on its own
//! connection. Every state change emits exactly one event, and a heartbeat is an
//! event and consumes a `seq`.
//!
//! The queue is bounded (R4: nothing grows without a cap) and the daemon never
//! blocks on a subscriber: a client that stops reading fills its queue, is
//! dropped from the bus, and has to reconnect. That is the only failure a
//! subscriber can cause, and it is confined to that subscriber.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use whirl_core::protocol::ErrorCode;

/// 30 s of quiet produces exactly one `heartbeat` (2.8, 2.9).
pub const HEARTBEAT_QUIET: Duration = Duration::from_secs(30);

/// One subscriber's queue. 256 `event:` lines is tens of kilobytes, so 16 stuck
/// subscribers cannot cost the daemon more than a bounded amount of memory.
const QUEUE: usize = 256;

/// One event. The vocabulary is closed: a frontend branches on the type name,
/// so a new one is a protocol change (2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A rotation began; `run` is the monotonic slot counter.
    RotateStart {
        run: u64,
    },
    /// The wallpaper changed. `path` takes the rest of the line.
    RotateOk {
        digest: String,
        origin_key: String,
        via: &'static str,
        path: Option<String>,
    },
    /// The rotation produced nothing; `code` is from 2.7.
    RotateFailed {
        code: ErrorCode,
        message: String,
    },
    /// The schedule is suspended, so `next_at` is frozen.
    Paused,
    /// The schedule is live again, with `next_at` re-armed from now.
    Resumed,
    FavoriteAdded {
        digest: String,
        origin_key: String,
    },
    FavoriteRemoved {
        digest: String,
    },
    /// A sweep completed; `hidden` is how many entries were kept only because
    /// they are pinned.
    #[allow(dead_code)] // trigger: the sweep (docs/spec/state-and-cache.md 5.5)
    CacheSwept {
        removed: u64,
        reclaimed_bytes: u64,
        hidden: u64,
    },
    /// The wall clock moved more than two intervals, so the deadline was
    /// recomputed.
    #[allow(dead_code)] // trigger: the scheduler (docs/spec/state-and-cache.md 8.6)
    ClockJump {
        seconds: i64,
    },
    /// The daemon re-read the config and it parsed.
    #[allow(dead_code)] // trigger: the rotation-time re-read (docs/architecture.md 10.5)
    ConfigReloaded,
    /// The daemon does not know what is on screen and is protecting the whole
    /// grace window (1.7.3 step 3).
    #[allow(dead_code)] // trigger: a failed readback (docs/architecture.md 1.7.3)
    AnchorUnverified,
    /// The daemon is exiting; the connection closes immediately after.
    #[allow(dead_code)] // trigger: supervision and signal handling (docs/architecture.md 1.5)
    Shutdown,
    Heartbeat {
        unix_seconds: i64,
    },
}

impl Event {
    /// The type name, exactly as 2.9's table spells it.
    pub fn name(&self) -> &'static str {
        match self {
            Event::RotateStart { .. } => "rotate_start",
            Event::RotateOk { .. } => "rotate_ok",
            Event::RotateFailed { .. } => "rotate_failed",
            Event::Paused => "paused",
            Event::Resumed => "resumed",
            Event::FavoriteAdded { .. } => "favorite_added",
            Event::FavoriteRemoved { .. } => "favorite_removed",
            Event::CacheSwept { .. } => "cache_swept",
            Event::ClockJump { .. } => "clock_jump",
            Event::ConfigReloaded => "config_reloaded",
            Event::AnchorUnverified => "anchor_unverified",
            Event::Shutdown => "shutdown",
            Event::Heartbeat { .. } => "heartbeat",
        }
    }

    /// One `event:` line, terminator included so the caller cannot forget it.
    pub fn line(&self, seq: u64) -> String {
        let fields = match self {
            Event::RotateStart { run } => format!(" {run}"),
            Event::RotateOk {
                digest,
                origin_key,
                via,
                path,
            } => format!(
                " {digest} {origin_key} {via} {}",
                path.as_deref().unwrap_or("-")
            ),
            Event::RotateFailed { code, message } => format!(" {} {message}", code.as_str()),
            Event::FavoriteAdded { digest, origin_key } => format!(" {digest} {origin_key}"),
            Event::FavoriteRemoved { digest } => format!(" {digest}"),
            Event::CacheSwept {
                removed,
                reclaimed_bytes,
                hidden,
            } => format!(" {removed} {reclaimed_bytes} {hidden}"),
            Event::ClockJump { seconds } => format!(" {seconds}"),
            Event::Heartbeat { unix_seconds } => format!(" {unix_seconds}"),
            Event::Paused
            | Event::Resumed
            | Event::ConfigReloaded
            | Event::AnchorUnverified
            | Event::Shutdown => String::new(),
        };
        format!("event: {seq} {}{fields}\n", self.name())
    }
}

/// Every subscriber, and when the daemon last said anything.
pub struct Bus {
    subscribers: Mutex<Vec<SyncSender<String>>>,
    /// The last line the daemon sent to anyone. A heartbeat is produced from
    /// this rather than from a timer: "30 s of quiet" is a property of the
    /// daemon, not of one connection, so two subscribers produce one heartbeat
    /// between them.
    quiet_since: Mutex<Instant>,
}

impl Bus {
    pub fn new() -> Bus {
        Bus {
            subscribers: Mutex::new(Vec::new()),
            quiet_since: Mutex::new(Instant::now()),
        }
    }

    /// Join the stream. The receiver is bounded: see `QUEUE`.
    pub fn subscribe(&self) -> Receiver<String> {
        let (sender, receiver) = sync_channel(QUEUE);
        self.subscribers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(sender);
        receiver
    }

    fn subscribers(&self) -> &Mutex<Vec<SyncSender<String>>> {
        &self.subscribers
    }

    /// Send one line to every subscriber and restart the quiet period. Never
    /// blocks and never fails the caller: a subscriber whose queue is full is
    /// removed, and its connection ends when it notices the channel is gone.
    pub fn broadcast(&self, line: String) {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        subscribers.retain(|subscriber| match subscriber.try_send(line.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        });
        self.touch();
    }

    /// Claim the right to emit this quiet period's heartbeat. At most one caller
    /// gets `true` per `HEARTBEAT_QUIET`, so two subscribers connected at once
    /// still produce exactly one heartbeat.
    pub fn claim_heartbeat(&self) -> bool {
        let mut quiet_since = self
            .quiet_since
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if quiet_since.elapsed() < HEARTBEAT_QUIET {
            return false;
        }
        *quiet_since = Instant::now();
        true
    }

    fn touch(&self) {
        let mut quiet_since = self
            .quiet_since
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *quiet_since = Instant::now();
    }
}

/// The current time in whole seconds, which is the heartbeat's one field.
pub fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2.9's table, as bytes. Every variant is constructed here, so a new event
    /// type cannot reach the stream without a line shape being decided, and a
    /// renamed field cannot pass.
    #[test]
    fn every_event_encodes_to_its_documented_line() {
        let cases: [(Event, &str); 13] = [
            (
                Event::RotateStart { run: 4211 },
                "event: 184 rotate_start 4211\n",
            ),
            (
                Event::RotateOk {
                    digest: "ab".repeat(32),
                    origin_key: "pictures:1cc4".to_string(),
                    via: "source",
                    path: Some("sha256/ab/ab/ab.jpg".to_string()),
                },
                "event: 185 rotate_ok abababababababababababababababababababababababababababababababab pictures:1cc4 source sha256/ab/ab/ab.jpg\n",
            ),
            (
                Event::RotateFailed {
                    code: ErrorCode::SetFailed,
                    message: "the platform refused the set".to_string(),
                },
                "event: 195 rotate_failed set_failed the platform refused the set\n",
            ),
            (Event::Paused, "event: 186 paused\n"),
            (Event::Resumed, "event: 187 resumed\n"),
            (
                Event::FavoriteAdded {
                    digest: "cd".repeat(32),
                    origin_key: "space:ab12cd".to_string(),
                },
                "event: 1 favorite_added cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd space:ab12cd\n",
            ),
            (
                Event::FavoriteRemoved {
                    digest: "cd".repeat(32),
                },
                "event: 2 favorite_removed cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\n",
            ),
            (
                Event::CacheSwept {
                    removed: 12,
                    reclaimed_bytes: 34_816_000,
                    hidden: 1,
                },
                "event: 189 cache_swept 12 34816000 1\n",
            ),
            (
                Event::ClockJump { seconds: 7200 },
                "event: 190 clock_jump 7200\n",
            ),
            (Event::ConfigReloaded, "event: 188 config_reloaded\n"),
            (Event::AnchorUnverified, "event: 191 anchor_unverified\n"),
            (Event::Shutdown, "event: 1 shutdown\n"),
            (
                Event::Heartbeat {
                    unix_seconds: 1_790_324_533,
                },
                "event: 192 heartbeat 1790324533\n",
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.line(seq_of(expected)), expected, "{:?}", event.name());
            // The type name is the second field, which is what a client
            // branches on.
            assert!(
                expected.starts_with(&format!("event: {} {}", seq_of(expected), event.name())),
                "{expected:?}"
            );
        }
    }

    fn seq_of(line: &str) -> u64 {
        line.split(' ')
            .nth(1)
            .expect("a seq")
            .parse()
            .expect("a number")
    }

    #[test]
    fn a_rotation_ok_without_a_path_reports_a_dash() {
        // A `reference`-mode local set has no cache path, and 2.6 spells that
        // `-` everywhere else.
        let event = Event::RotateOk {
            digest: "ab".repeat(32),
            origin_key: "pictures:1cc4".to_string(),
            via: "manual",
            path: None,
        };
        assert!(event.line(1).ends_with(" manual -\n"), "{}", event.line(1));
    }

    #[test]
    fn a_slow_subscriber_is_dropped_rather_than_blocking_the_daemon() {
        let bus = Bus::new();
        let slow = bus.subscribe();
        let quick = bus.subscribe();
        for seq in 1..=(QUEUE as u64 + 10) {
            bus.broadcast(format!("event: {seq} heartbeat 0\n"));
        }
        // The quick reader drains as it goes, so it is still subscribed; the
        // slow one filled its queue and was dropped.
        assert_eq!(
            quick.iter().count(),
            QUEUE,
            "the bounded queue is what a reader gets"
        );
        assert!(bus.subscribers().lock().expect("the bus").is_empty());
        drop(slow);
    }

    #[test]
    fn one_quiet_period_produces_one_heartbeat() {
        let bus = Bus::new();
        // Freshly constructed: the first caller would have to wait 30 s, and no
        // test waits 30 s, so this asserts the claim is exclusive and cheap
        // rather than sleeping through the interval.
        assert!(!bus.claim_heartbeat(), "not quiet for 30 s yet");
        let first = bus.subscribe();
        let second = bus.subscribe();
        bus.broadcast("event: 1 paused\n".to_string());
        assert_eq!(
            first.try_recv().expect("the first subscriber"),
            "event: 1 paused\n"
        );
        assert_eq!(
            second.try_recv().expect("the second subscriber"),
            "event: 1 paused\n"
        );
        assert!(
            !bus.claim_heartbeat(),
            "a broadcast restarts the quiet period"
        );
    }
}
