// t_62920980 item 4, second probe: add the ingredient the first one missed.
//
// `whirld` sets SO_RCVTIMEO on every connection (IDLE_TIMEOUT=300s, and
// STREAM_POLL=100ms on a subscribed one). A socket read with a timeout is in
// signal(7)'s "never restarted after a signal handler" class, so any signal
// that reaches that thread turns the blocked read into EINTR instead of a
// restart. This probe parks a thread in exactly that read (100ms timeout,
// looping on WouldBlock as `subscribe()` does) while another thread spawns and
// reaps a child, as the daemon does when it runs `whirl-worker`.
//
// Usage: probe2 [window_ms]
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let window: u64 = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(2000);
    let started = Instant::now();
    let uname = Command::new("uname").arg("-m").output().expect("uname");
    print!("arch {} ", String::from_utf8_lossy(&uname.stdout).trim());
    let cpu = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    println!(
        "{}",
        cpu.lines()
            .find(|line| line.starts_with("model name"))
            .unwrap_or("model name: (none)")
    );
    for line in std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("SigCgt"))
    {
        println!("{line}");
    }

    let (a, b) = UnixStream::pair().expect("a socketpair");
    a.set_read_timeout(Some(Duration::from_millis(100)))
        .expect("SO_RCVTIMEO, as STREAM_POLL sets it");
    let interrupted = Arc::new(AtomicU64::new(0));
    let polls = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let reader = {
        let interrupted = Arc::clone(&interrupted);
        let polls = Arc::clone(&polls);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(a);
            while !done.load(Ordering::SeqCst) {
                match reader.fill_buf() {
                    Ok(bytes) if bytes.is_empty() => {
                        println!("reader: EOF at {:?}", started.elapsed());
                        break;
                    }
                    Ok(_) => {
                        println!("reader: data at {:?}", started.elapsed());
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        interrupted.fetch_add(1, Ordering::SeqCst);
                        println!(
                            "reader: fill_buf -> Interrupted (raw_os_error={:?}) at {:?}",
                            error.raw_os_error(),
                            started.elapsed()
                        );
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        polls.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(error) => {
                        println!(
                            "reader: fill_buf -> Err({:?}, raw_os_error={:?}) at {:?}",
                            error.kind(),
                            error.raw_os_error(),
                            started.elapsed()
                        );
                        break;
                    }
                }
            }
        })
    };

    let spawner = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let mut child = Command::new("/bin/sleep")
            .arg("0.05")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a child, as Worker::run spawns whirl-worker");
        let status = child.wait().expect("a wait for the child");
        println!("spawner: child exited {status} at {:?}", started.elapsed());
    });

    std::thread::sleep(Duration::from_millis(window));
    done.store(true, Ordering::SeqCst);
    let mut writer = b;
    let _ = std::io::Write::write_all(&mut writer, b"x");
    let _ = spawner.join();
    let _ = reader.join();
    println!(
        "result: interrupted={} timed_out_polls={}",
        interrupted.load(Ordering::SeqCst),
        polls.load(Ordering::SeqCst)
    );
}
