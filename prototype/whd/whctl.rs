// whctl - the mpc to whd's mpd. Thin client, no state, no logic.
// Speaks the line protocol over a unix socket. Exit codes: 0 ok, 1 command failed, 2 unreachable.
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::exit;
use std::time::Duration;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let sock = env::var("WHD_SOCKET").unwrap_or_else(|_| {
        let home = env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{home}/.local/state/wh/sock")
    });
    let watch = args.first().map(|s| s.as_str()) == Some("watch");
    let cmd = if args.is_empty() {
        "status".to_string()
    } else if watch {
        "idle".to_string()
    } else {
        args.join(" ")
    };

    let stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("whctl: cannot reach whd at {sock}: {e}");
            eprintln!("whctl: start it with: whd");
            exit(2);
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
    let mut w = stream.try_clone().expect("clone socket");
    let mut r = BufReader::new(stream);

    // greeting line, same shape as mpd's "OK MPD <version>"
    let mut greet = String::new();
    if r.read_line(&mut greet).unwrap_or(0) == 0 {
        eprintln!("whctl: whd closed the connection");
        exit(2);
    }
    if !greet.starts_with("OK ") {
        eprintln!("whctl: unexpected greeting: {}", greet.trim());
        exit(2);
    }

    let mut sent = 0usize;
    loop {
        if writeln!(w, "{cmd}").is_err() {
            eprintln!("whctl: write failed");
            exit(2);
        }
        sent += 1;
        loop {
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {}
                Err(e) => {
                    eprintln!("whctl: read failed: {e}");
                    exit(2);
                }
            }
            let t = line.trim_end();
            if t == "OK" {
                break;
            }
            if let Some(msg) = t.strip_prefix("ACK ") {
                eprintln!("whctl: {msg}");
                exit(1);
            }
            println!("{t}");
        }
        if !watch || sent >= 200 {
            break;
        }
    }
}
