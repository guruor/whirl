// t_62920980 item 4, third probe: name the signal.
//
// The emulated guest catches one signal a native guest does not (SigCgt 0x450
// against 0x440: bit 4, signal 5 = SIGTRAP). This runs probe2's scenario with
// SIGTRAP blocked in the parked reader thread, so the emulator's own signal, if
// that is what it is, is delivered to another thread and the read is left alone.
//
// Usage: probe3 [window_ms] [block-trap|none]
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[repr(C)]
struct SigSet {
    bits: [u64; 16],
}

unsafe extern "C" {
    fn pthread_sigmask(how: i32, set: *const SigSet, old: *mut SigSet) -> i32;
}

const SIG_BLOCK: i32 = 0; // Linux
const SIGTRAP: u32 = 5;

fn block_sigtrap() {
    let mut set = SigSet { bits: [0; 16] };
    set.bits[0] |= 1 << (SIGTRAP - 1);
    // SAFETY: the set is a well-formed sigset_t for this ABI, and `how` is
    // SIG_BLOCK. The return value is checked so a failure is visible.
    let rc = unsafe { pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut()) };
    assert_eq!(rc, 0, "pthread_sigmask(SIG_BLOCK, SIGTRAP) failed: {rc}");
}

fn main() {
    let window: u64 = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(1500);
    let mode = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "none".to_string());
    // When the spawner starts, so the interruption can be attributed to it.
    let delay: u64 = std::env::args()
        .nth(3)
        .and_then(|n| n.parse().ok())
        .unwrap_or(300);
    let started = Instant::now();
    let uname = Command::new("uname").arg("-m").output().expect("uname");
    print!(
        "arch {} mode {mode} ",
        String::from_utf8_lossy(&uname.stdout).trim()
    );
    let cpu = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    println!(
        "{}",
        cpu.lines()
            .find(|line| line.starts_with("model name"))
            .unwrap_or("model name: (none)")
    );

    let (a, b) = UnixStream::pair().expect("a socketpair");
    a.set_read_timeout(Some(Duration::from_millis(100)))
        .expect("SO_RCVTIMEO, as STREAM_POLL sets it");
    let interrupted = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let reader = {
        let interrupted = Arc::clone(&interrupted);
        let done = Arc::clone(&done);
        let block = mode == "block-trap";
        std::thread::spawn(move || {
            if block {
                block_sigtrap();
                let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
                for line in status.lines().filter(|l| l.starts_with("SigBlk")) {
                    println!("reader: {line}");
                }
            }
            let mut reader = BufReader::new(a);
            while !done.load(Ordering::SeqCst) {
                match reader.fill_buf() {
                    Ok(bytes) if bytes.is_empty() => break,
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
                        ) => {}
                    Err(error) => {
                        println!(
                            "reader: fill_buf -> Err({:?}, raw_os_error={:?})",
                            error.kind(),
                            error.raw_os_error()
                        );
                        break;
                    }
                }
            }
        })
    };

    let spawner = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay));
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
    println!("result: interrupted={}", interrupted.load(Ordering::SeqCst));
}
