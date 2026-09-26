//! The one HTTP client this build has, and why it is a program rather than a
//! dependency.
//!
//! **The constraint first.** v0.1 has zero third-party dependencies and the CI
//! `guards` job fails if a fifth package appears in `Cargo.lock`
//! (docs/development.md, "Dependencies: zero"), while the standard library has
//! no TLS at all. A `https://` request therefore cannot be made from Rust here
//! without either vendoring a TLS stack or linking the platform's, and both are
//! the class of dependency this project is a reaction against.
//!
//! So the request is made by `curl`, which is the tool docs/spec/state-and-cache.md
//! already probed this API with (`[L 6]`, `[L 8]`), which is present on the three
//! platforms this build targets, and which uses the platform's own trust store
//! and its own proxy environment. The cost is stated rather than hidden: this
//! build depends on a program being installed. `kill(1)` taught this repository
//! that lesson the expensive way (crates/whirld/src/worker.rs, the `SIGTERM`
//! grace note), so the failure is named: a missing `curl` is
//! [`SourceErrorKind::Unavailable`](whirl_core::source::SourceErrorKind) with
//! "`curl` is not on PATH" in it, never a panic and never an empty answer.
//!
//! **The key is never in argv.** docs/architecture.md 6.3 requires that: "It is
//! never in argv. The worker gets it in its environment (1.6), not as a
//! parameter, so it does not appear in `ps` output for any user on the machine."
//! A `curl -H "X-API-Key: ..."` would be exactly that leak, so the whole request
//! — URL, user agent, timeout and key header — travels to `curl --config -` on
//! its **stdin**. argv holds `--config -` and nothing else, no key reaches a
//! file, and no error message this module produces can contain one.

use std::io::Write;
use std::process::{Command, Stdio};

/// The 2 minutes docs/architecture.md cites as the worker's HTTP client timeout
/// (`[M 5]`, `prototype/wh-rotate/main.go:267`), and the number the daemon's
/// `schedule.worker_deadline_seconds` of 300 is sized against (1.7.1). One
/// request is one page of a listing or one image, so this is the whole budget a
/// single call may spend.
pub const TIMEOUT_SECONDS: u32 = 120;

/// One request: a URL, and the API key when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request<'a> {
    pub url: &'a str,
    /// The value of the `X-API-Key` header (docs/spec/features.md 2.4: "the key
    /// travels in the `X-API-Key` request header, not in the query string").
    /// `None` sends no header at all, which is what a public collection and an
    /// SFW search need.
    pub key: Option<&'a str>,
}

/// One response: the status code and the body, in one piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status, as the wire gave it. `0` means no status existed: the
    /// connection was never made.
    pub status: u16,
    pub body: String,
}

/// Where a request is made.
///
/// Behind a trait for the reason [`crate::pipeline::Transport`] is: a test must
/// never open a socket, and the wallhaven source's own tests drive it with a
/// recorded response instead (docs/spec/features.md 2.6's fixture test).
pub trait Fetch {
    fn get(&self, request: &Request<'_>) -> Result<Response, String>;
}

/// The production client: `curl`, driven entirely from its stdin.
#[derive(Debug, Clone, Copy, Default)]
pub struct Curl;

impl Fetch for Curl {
    fn get(&self, request: &Request<'_>) -> Result<Response, String> {
        let mut child = Command::new("curl")
            .arg("--config")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => {
                    format!(
                        "`curl` is not on PATH, and this build makes HTTP requests with it: {error}"
                    )
                }
                _ => format!("cannot run `curl`: {error}"),
            })?;
        // The pipe is closed by dropping the handle, which is how curl learns
        // that the config ends. A failed write means curl died first.
        {
            let mut stdin = child.stdin.take().ok_or_else(|| {
                "cannot write `curl`'s config: its stdin is not a pipe".to_string()
            })?;
            stdin
                .write_all(configuration(request).as_bytes())
                .map_err(|error| format!("cannot write `curl`'s config: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("cannot wait for `curl`: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "`curl` failed: {}",
                first_line(&String::from_utf8_lossy(&output.stderr))
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let (body, status) = text
            .rsplit_once('\n')
            .ok_or_else(|| "`curl` returned no status line".to_string())?;
        let status: u16 = status
            .trim()
            .parse()
            .map_err(|_| format!("`curl` returned {status:?}, which is not a status code"))?;
        if status == 0 {
            return Err(format!(
                "no response from {}: {}",
                request.url,
                first_line(&String::from_utf8_lossy(&output.stderr))
            ));
        }
        Ok(Response {
            status,
            body: body.to_string(),
        })
    }
}

/// The `curl` config for one request, which is what reaches `curl` on stdin.
///
/// Every option is here and none is in argv, so the key is in no process's
/// argument list. `%{http_code}` is appended after a newline, which is how the
/// status comes back beside a body that may itself contain newlines.
fn configuration(request: &Request<'_>) -> String {
    let mut out = String::new();
    out.push_str("silent\nshow-error\n");
    out.push_str(&format!("max-time = {TIMEOUT_SECONDS}\n"));
    out.push_str(&format!("url = {}\n", quoted(request.url)));
    if let Some(key) = request.key {
        out.push_str(&format!(
            "header = {}\n",
            quoted(&format!("X-API-Key: {key}"))
        ));
    }
    out.push_str("write-out = \"\\n%{http_code}\"\n");
    out
}

/// One value for the config file, quoted and escaped the way curl's own parser
/// reads it: backslash and double quote are the two characters it treats
/// specially inside a quoted value.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // A newline cannot survive a line-oriented config file.
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The first line of a message, so a multi-line `curl` complaint stays one line
/// in a log record that is one line by contract.
fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_travels_as_a_header_in_the_config_and_never_in_argv() {
        let request = Request {
            url: "https://wallhaven.cc/api/v1/search?purity=100",
            key: Some("dummy-key-value"),
        };
        let config = configuration(&request);
        assert!(
            config.contains("header = \"X-API-Key: dummy-key-value\""),
            "{config}"
        );
        assert!(config.contains("url = \"https://wallhaven.cc/api/v1/search?purity=100\""));
        assert!(!request.url.contains("key"));
    }

    #[test]
    fn a_keyless_request_sends_no_header_at_all() {
        let config = configuration(&Request {
            url: "https://wallhaven.cc/api/v1/search",
            key: None,
        });
        assert!(!config.contains("header"), "{config}");
    }

    #[test]
    fn the_status_comes_back_after_the_body() {
        let config = configuration(&Request {
            url: "https://example.invalid/",
            key: None,
        });
        assert!(
            config.ends_with("write-out = \"\\n%{http_code}\"\n"),
            "{config}"
        );
        assert!(config.contains(&format!("max-time = {TIMEOUT_SECONDS}")));
        assert!(config.contains("silent\n"));
    }

    #[test]
    fn a_value_that_would_break_the_config_file_is_escaped() {
        assert_eq!(quoted("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(quoted("two\nlines"), "\"two\\nlines\"");
    }
}
