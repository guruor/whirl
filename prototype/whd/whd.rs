// whd - the mpd to a wallpaper rotator's mpc. Rust, std only, no crates.
//
// Architecture rule this file exists to enforce:
//   the daemon owns config, schedule, history, favorites and state.
//   the daemon NEVER owns pixels. every image operation is exec'd into a
//   disposable worker process, so the resident floor is the control plane
//   (a couple of MB) and never the largest image ever decoded.
//
// Protocol: text lines over a unix socket, one command per line, response
// terminated by OK or ACK <message>. Same shape as mpd.
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = "whd 0.1.0";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rss_mb(pid: u32) -> f64 {
    match Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<f64>()
            .unwrap_or(0.0)
            / 1024.0,
        Err(_) => 0.0,
    }
}

#[derive(Clone)]
struct Cfg {
    socket: PathBuf,
    state_dir: PathBuf,
    rotator: String,
    rotator_cfg: String,
    cache_dir: String,
    interval: u64,
    dry: bool,
}

struct State {
    paused: bool,
    rotating: bool,
    rotations: u64,
    last_path: String,
    last_line: String,
    last_at: u64,
    next_at: u64,
    history: VecDeque<String>,
    favorites: Vec<String>,
    gen: u64,
    log: VecDeque<String>,
}

enum Job {
    Rotate(Sender<Result<String, String>>),
    SetTo(String, Sender<Result<String, String>>),
}

struct Shared {
    cfg: Cfg,
    st: Mutex<State>,
    cv: Condvar,
    jobs: Mutex<Sender<Job>>,
    started: Instant,
}

fn bump(st: &mut State) {
    st.gen = st.gen.wrapping_add(1);
}

fn note(st: &mut State, msg: &str) {
    st.log
        .push_back(format!("{} {}", now_secs(), msg));
    while st.log.len() > 200 {
        st.log.pop_front();
    }
}

fn parse_path(line: &str) -> Option<String> {
    let mut it = line.split_whitespace();
    match it.next() {
        Some("SET") | Some("WOULD-SET") => it.next().map(|s| s.to_string()),
        _ => None,
    }
}

fn persist(cfg: &Cfg, st: &State) {
    let hist: Vec<String> = st.history.iter().cloned().collect();
    let _ = fs::write(cfg.state_dir.join("history"), hist.join("\n"));
    let _ = fs::write(cfg.state_dir.join("favorites"), st.favorites.join("\n"));
}

// ---------------------------------------------------------------- worker

fn exec_worker(cfg: &Cfg, set: Option<&str>) -> Result<String, String> {
    let mut c = Command::new(&cfg.rotator);
    c.env("WH_ROTATE_CONFIG", &cfg.rotator_cfg);
    if let Some(p) = set {
        c.arg("-set").arg(p);
    } else if cfg.dry {
        c.arg("-dry-run");
    }
    match c.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
            .unwrap_or("")
            .trim()
            .to_string()),
        Ok(o) => Err(format!(
            "worker exit {:?}: {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("worker spawn: {e}")),
    }
}

fn run_worker(rx: Receiver<Job>, shared: Arc<Shared>) {
    while let Ok(job) = rx.recv() {
        let (target, reply) = match job {
            Job::Rotate(tx) => (None, tx),
            Job::SetTo(p, tx) => (Some(p), tx),
        };
        {
            let mut st = shared.st.lock().unwrap();
            st.rotating = true;
            bump(&mut st);
            shared.cv.notify_all();
        }
        let res = exec_worker(&shared.cfg, target.as_deref());
        {
            let mut st = shared.st.lock().unwrap();
            st.rotating = false;
            match &res {
                Ok(line) => {
                    st.rotations += 1;
                    st.last_line = line.clone();
                    st.last_at = now_secs();
                    if let Some(p) = parse_path(line) {
                        st.last_path = p.clone();
                        st.history.push_back(p);
                        while st.history.len() > 64 {
                            st.history.pop_front();
                        }
                    }
                    st.next_at = now_secs() + shared.cfg.interval;
                    note(&mut st, &format!("rotated {line}"));
                }
                Err(e) => note(&mut st, &format!("error {e}")),
            }
            persist(&shared.cfg, &st);
            bump(&mut st);
        }
        shared.cv.notify_all();
        let _ = reply.send(res);
    }
}

// ---------------------------------------------------------------- scheduler

fn run_scheduler(shared: Arc<Shared>, tx: Sender<Job>) {
    loop {
        // sleep in bounded slices: a machine that slept through its slot
        // rotates once on wake instead of drifting or firing a burst.
        let wait = {
            let st = shared.st.lock().unwrap();
            if st.paused || st.rotating {
                5
            } else {
                st.next_at.saturating_sub(now_secs()).clamp(1, 20)
            }
        };
        thread::sleep(Duration::from_secs(wait));
        let due = {
            let st = shared.st.lock().unwrap();
            !st.paused && !st.rotating && now_secs() >= st.next_at
        };
        if due {
            let (tx2, _rx2) = mpsc::channel();
            if tx.send(Job::Rotate(tx2)).is_err() {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------- commands

fn submit(shared: &Arc<Shared>, w: &mut UnixStream, target: Option<String>) {
    let (tx, rx) = mpsc::channel();
    let job = match target {
        Some(p) => Job::SetTo(p, tx),
        None => Job::Rotate(tx),
    };
    if shared.jobs.lock().unwrap().send(job).is_err() {
        let _ = writeln!(w, "ACK worker unavailable");
        return;
    }
    let _ = writeln!(w, "queued");
    let _ = w.flush();
    match rx.recv_timeout(Duration::from_secs(240)) {
        Ok(Ok(line)) => {
            let _ = writeln!(w, "result: {line}");
            let _ = writeln!(w, "OK");
        }
        Ok(Err(e)) => {
            let _ = writeln!(w, "ACK {e}");
        }
        Err(_) => {
            let _ = writeln!(w, "ACK timeout waiting for worker");
        }
    }
}

fn status(shared: &Arc<Shared>, w: &mut UnixStream) {
    let st = shared.st.lock().unwrap();
    let files = fs::read_dir(&shared.cfg.cache_dir)
        .map(|d| d.filter_map(|e| e.ok()).filter(|e| !e.path().is_dir()).count())
        .unwrap_or(0);
    let rows = [
        ("pid", std::process::id().to_string()),
        ("version", VERSION.to_string()),
        ("rss_mb", format!("{:.1}", rss_mb(std::process::id()))),
        (
            "open_fds",
            fs::read_dir("/dev/fd")
                .map(|d| d.count())
                .unwrap_or(0)
                .to_string(),
        ),
        ("uptime_s", shared.started.elapsed().as_secs().to_string()),
        ("rotations", st.rotations.to_string()),
        ("paused", if st.paused { "1".into() } else { "0".into() }),
        ("rotating", if st.rotating { "1".into() } else { "0".into() }),
        ("interval_s", shared.cfg.interval.to_string()),
        (
            "next_in_s",
            if st.paused {
                "-".into()
            } else {
                st.next_at.saturating_sub(now_secs()).to_string()
            },
        ),
        ("last", st.last_path.clone()),
        ("last_at", st.last_at.to_string()),
        ("history", st.history.len().to_string()),
        ("favorites", st.favorites.len().to_string()),
        ("cache_files", files.to_string()),
        ("gen", st.gen.to_string()),
    ];
    for (k, v) in rows {
        let _ = writeln!(w, "{k}: {v}");
    }
    let _ = writeln!(w, "OK");
}

fn idle(shared: &Arc<Shared>, w: &mut UnixStream) {
    let mut guard = shared.st.lock().unwrap();
    let start = guard.gen;
    let mut changed = false;
    while guard.gen == start {
        let (g, t) = shared
            .cv
            .wait_timeout(guard, Duration::from_secs(60))
            .unwrap();
        guard = g;
        if t.timed_out() {
            break;
        }
        changed = true;
    }
    if changed {
        let _ = writeln!(w, "changed: {}", guard.last_line);
    } else {
        let _ = writeln!(w, "timeout");
    }
    let _ = writeln!(w, "OK");
}

fn handle(stream: UnixStream, shared: Arc<Shared>) {
    let mut w = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut r = BufReader::new(stream);
    let _ = writeln!(w, "OK {VERSION} protocol 1");
    let _ = w.flush();
    loop {
        let mut line = String::new();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let cmd = line.trim().to_string();
        if cmd.is_empty() {
            continue;
        }
        let mut it = cmd.split_whitespace();
        let verb = it.next().unwrap_or("");
        match verb {
            "ping" => {
                let _ = writeln!(w, "OK");
            }
            "close" => {
                let _ = writeln!(w, "OK");
                let _ = w.flush();
                break;
            }
            "status" => status(&shared, &mut w),
            "next" => submit(&shared, &mut w, None),
            "prev" => {
                let target = {
                    let mut st = shared.st.lock().unwrap();
                    if st.history.len() < 2 {
                        None
                    } else {
                        st.history.pop_back();
                        st.history.back().cloned()
                    }
                };
                match target {
                    Some(p) => submit(&shared, &mut w, Some(p)),
                    None => {
                        let _ = writeln!(w, "ACK no earlier wallpaper in history");
                    }
                }
            }
            "favorite" => {
                let mut st = shared.st.lock().unwrap();
                let cur = st.last_path.clone();
                if cur.is_empty() {
                    let _ = writeln!(w, "ACK nothing to favorite yet");
                } else if st.favorites.contains(&cur) {
                    let _ = writeln!(w, "already: {cur}");
                    let _ = writeln!(w, "OK");
                } else {
                    st.favorites.push(cur.clone());
                    let _ = writeln!(w, "favorited: {cur}");
                    persist(&shared.cfg, &st);
                    bump(&mut st);
                    drop(st);
                    shared.cv.notify_all();
                    let _ = writeln!(w, "OK");
                }
            }
            "pause" => {
                let mut st = shared.st.lock().unwrap();
                st.paused = true;
                bump(&mut st);
                drop(st);
                shared.cv.notify_all();
                let _ = writeln!(w, "paused");
                let _ = writeln!(w, "OK");
            }
            "resume" => {
                let mut st = shared.st.lock().unwrap();
                st.paused = false;
                st.next_at = now_secs() + shared.cfg.interval;
                bump(&mut st);
                drop(st);
                shared.cv.notify_all();
                let _ = writeln!(w, "resumed");
                let _ = writeln!(w, "OK");
            }
            "history" => {
                let st = shared.st.lock().unwrap();
                for p in st.history.iter().rev().take(10) {
                    let _ = writeln!(w, "{p}");
                }
                let _ = writeln!(w, "OK");
            }
            "favorites" => {
                let st = shared.st.lock().unwrap();
                for p in st.favorites.iter() {
                    let _ = writeln!(w, "{p}");
                }
                let _ = writeln!(w, "OK");
            }
            "log" => {
                let st = shared.st.lock().unwrap();
                for l in st.log.iter().rev().take(15) {
                    let _ = writeln!(w, "{l}");
                }
                let _ = writeln!(w, "OK");
            }
            "sources" => {
                // the worker owns the config; the daemon only reports it
                match fs::read_to_string(&shared.cfg.rotator_cfg) {
                    Ok(raw) => {
                        let _ = writeln!(w, "config: {}", shared.cfg.rotator_cfg);
                        for chunk in raw.split("\"kind\"").skip(1) {
                            if let Some(v) = chunk.split('"').nth(1) {
                                let _ = writeln!(w, "source: {v}");
                            }
                        }
                        let _ = writeln!(w, "OK");
                    }
                    Err(e) => {
                        let _ = writeln!(w, "ACK cannot read config: {e}");
                    }
                }
            }
            "idle" => idle(&shared, &mut w),
            other => {
                let _ = writeln!(w, "ACK unknown command '{other}'");
            }
        }
        let _ = w.flush();
    }
}

fn main() {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let state_dir = PathBuf::from(
        env::var("WHD_STATE_DIR").unwrap_or_else(|_| format!("{home}/.local/state/wh")),
    );
    let _ = fs::create_dir_all(&state_dir);
    let cfg = Cfg {
        socket: PathBuf::from(
            env::var("WHD_SOCKET").unwrap_or_else(|_| format!("{}/sock", state_dir.display())),
        ),
        state_dir: state_dir.clone(),
        rotator: env::var("WHD_ROTATOR")
            .unwrap_or_else(|_| format!("{home}/.hermes/cache/scratch/wh-rotate/wh-rotate")),
        rotator_cfg: env::var("WH_ROTATE_CONFIG")
            .unwrap_or_else(|_| format!("{home}/.hermes/cache/scratch/wh-rotate/multi-config.json")),
        cache_dir: env::var("WHD_CACHE").unwrap_or_else(|_| {
            format!("{home}/.hermes/cache/scratch/wh-rotate/wh-cache")
        }),
        interval: env::var("WHD_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1800),
        dry: env::var("WHD_DRY").is_ok(),
    };

    let _ = fs::remove_file(&cfg.socket);
    let listener = match UnixListener::bind(&cfg.socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("whd: cannot bind {}: {e}", cfg.socket.display());
            std::process::exit(1);
        }
    };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&cfg.socket, fs::Permissions::from_mode(0o600));
    }

    let history = fs::read_to_string(state_dir.join("history")).unwrap_or_default();
    let favorites = fs::read_to_string(state_dir.join("favorites")).unwrap_or_default();
    let mut st = State {
        paused: false,
        rotating: false,
        rotations: 0,
        last_path: String::new(),
        last_line: String::new(),
        last_at: 0,
        next_at: now_secs() + cfg.interval,
        history: history
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|s| s.to_string())
            .collect(),
        favorites: favorites
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|s| s.to_string())
            .collect(),
        gen: 0,
        log: VecDeque::new(),
    };
    if let Some(l) = st.history.back() {
        st.last_path = l.clone();
    }
    st.log.push_back(format!("{} daemon start", now_secs()));

    let (tx, rx) = mpsc::channel::<Job>();
    let shared = Arc::new(Shared {
        cfg: cfg.clone(),
        st: Mutex::new(st),
        cv: Condvar::new(),
        jobs: Mutex::new(tx.clone()),
        started: Instant::now(),
    });

    {
        let s = shared.clone();
        thread::spawn(move || run_worker(rx, s));
    }
    {
        let s = shared.clone();
        thread::spawn(move || run_scheduler(s, tx));
    }

    println!(
        "whd {} listening on {}  interval={}s  pid={}  rss={:.1} MB",
        VERSION,
        cfg.socket.display(),
        cfg.interval,
        std::process::id(),
        rss_mb(std::process::id())
    );
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let sh = shared.clone();
                thread::spawn(move || handle(s, sh));
            }
            Err(_) => continue,
        }
    }
}
