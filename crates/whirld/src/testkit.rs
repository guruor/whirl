//! Test scaffolding for the daemon's own unit tests, and nothing else.
//!
//! This module is compiled only under `cfg(test)`, so no line here is part of
//! the daemon, and `cargo build` sees none of it. It exists because three
//! modules (`state`, `scheduler`, `socket`) need a real `Daemon` in-process:
//! building one means an `Effective`, a `Worker` and a `DaemonLock`, and the
//! same twelve lines written three times is the defect a shared builder
//! prevents.
//!
//! `Scratch` is the half of "a test leaves nothing behind" that is mechanical:
//! every directory this module hands out is removed when the guard drops, and a
//! test that declares the `Scratch` before its `Daemon` drops the daemon first
//! (Rust drops locals in reverse declaration order), so the lock file and the
//! handler threads are gone before the tree is unlinked.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use whirl_core::config::{Backend, Config};

use crate::lock::Attempt;
use crate::plan::Effective;
use crate::state::Daemon;
use crate::worker::Worker;

/// A directory of this test's own, removed on drop.
pub struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("whirl-test-{name}-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        Scratch { dir }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A daemon over `dir`, with `program` as its worker, holding the lock the way
/// `attempt` says.
///
/// Nothing here reads the process environment: `Effective` is built directly,
/// which is 4.3's precedence bypassed on purpose, so two tests in the same
/// binary cannot fight over `WHIRL_CONFIG`. The daemon lock is really taken
/// (8.7's `flock` direction), so `status` reports the primitive this machine can
/// answer with; `Attempt::Unsupported` is the other value of that row, and the
/// rotation lock is set to match because 8.7 makes the fallback a property of
/// the filesystem rather than of one lock.
///
/// `config_text` is written to `dir/config.json` when it is not `None`. That is
/// 4.2's annotated default for a test whose rotation re-reads a real document;
/// `None` is for a test written against the file being absent, where
/// `reconfigure` records `bad_config` instead.
///
/// The one thing this cannot do is 8.7's `excl_file` fallback:
/// `Daemon::rotate_attempt` is private to `state`, so a caller that wants it
/// sets it itself (the same way the daemon lock's `Attempt` was passed in).
pub fn build(
    dir: &Path,
    attempt: Attempt,
    config: Config,
    program: PathBuf,
    config_text: Option<&str>,
) -> Daemon {
    let state_dir = dir.join("state");
    let cache_dir = dir.join("cache");
    let config_path = dir.join("config.json");
    if let Some(text) = config_text {
        fs::write(&config_path, text).expect("the config file");
    }
    let effective = Effective {
        config_path,
        socket_path: dir.join("whirl.sock"),
        state_dir: state_dir.clone(),
        cache_dir: cache_dir.clone(),
        backend: Backend::Noop,
        config: RwLock::new(config),
    };
    let worker = Worker::new(
        program,
        effective.config_path.clone(),
        Backend::Noop,
        state_dir.clone(),
        cache_dir,
    );
    let lock = crate::lock::take_as(&state_dir, attempt).expect("the daemon lock");
    Daemon::load(effective, worker, lock)
}

/// The builder with a real `config.json` behind it: a rotation re-reads a
/// document rather than recording `bad_config`, which is what a test of the
/// rotation path wants unless it is about the config being unreadable.
pub fn daemon(dir: &Path, config: Config, program: PathBuf) -> Daemon {
    build(
        dir,
        Attempt::Acquired,
        config,
        program,
        Some(Config::default_config_json()),
    )
}

/// The builder with the daemon's real worker program and no config file, so an
/// existing test written against that shape keeps it.
pub fn daemon_holding(dir: &Path, attempt: Attempt, config: Config) -> Daemon {
    build(dir, attempt, config, PathBuf::from("whirl-worker"), None)
}

/// A daemon with no worker at all: a path that does not exist, so a rotation
/// fails at the spawn and every test that is not about the worker says so.
pub fn daemon_without_a_worker(dir: &Path, config: Config) -> Daemon {
    daemon(dir, config, dir.join("no-worker"))
}

/// A daemon whose worker reports `set: <digest> <origin_key> <path>` and exits
/// 0: the success shape of 1.6, so a rotation has something to record.
pub fn daemon_with_a_set(
    dir: &Path,
    config: Config,
    digest: &str,
    origin_key: &str,
    path: &str,
) -> Daemon {
    let program = script(
        dir,
        "worker-set.sh",
        &format!("#!/bin/sh\nprintf '%s\\n' 'set: {digest} {origin_key} {path}'\n"),
    );
    daemon(dir, config, program)
}

/// An executable `/bin/sh` script inside `dir`. Any path works as a worker, and a
/// script is the cheapest one that can print a `set:` line.
pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("a script");
    let mut permissions = fs::metadata(&path).expect("the script").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("an executable script");
    path
}

/// A 64-character lower-case hex digest, which is what `set id` and the worker's
/// `set:` line both require (2.6): `is_digest` reads `0-9a-f` and nothing else.
pub fn digest(seed: char) -> String {
    seed.to_string().repeat(64)
}
