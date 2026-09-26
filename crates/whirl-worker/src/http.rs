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
//!
//! **The same client fetches the image, and argv is still the same two words.**
//! [`Fetch`] answers a *document*: a listing, which is text and carries the
//! status its parser needs. [`Bytes`] answers the other shape, one body as a
//! stream, and it is here rather than in the pipeline because the argv rule above
//! is a rule about this module: it has one implementation site ([`invocation`])
//! only if both routes go through it.

use std::io::{Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};

/// The 2 minutes docs/architecture.md cites as the worker's HTTP client timeout
/// (`[M 5]`, `prototype/wh-rotate/main.go:267`), and the number the daemon's
/// `schedule.worker_deadline_seconds` of 300 is sized against (1.7.1). One
/// request is one page of a listing or one image, so this is the whole budget a
/// single call may spend.
pub const TIMEOUT_SECONDS: u32 = 120;

/// The user agent every request this client makes carries.
///
/// docs/spec/features.md 2.3 does not name one. A bare programmatic client
/// (`curl/8.x`, or a UA that says `whirl/0.1`) is the fingerprint such a
/// blocklist is built from, so this is an ordinary desktop-browser string with
/// no version of this project's own in it. It lives with the client rather than
/// with the source because it is on every request, listing and image alike.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                             (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

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
        let child = request_started(&configuration(request))?;
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

/// The byte route of the same client: one URL, one body as a stream.
///
/// A second trait rather than a second method on [`Fetch`], because the two
/// answers have different shapes and each shape is right for its one caller. A
/// listing is a document: [`Response`] keeps the status beside the text and the
/// source parses it. The download stage wants none of that — it wants the bytes,
/// it hashes them as they arrive (docs/spec/state-and-cache.md section 3 step 4),
/// and a status is not something it could act on anyway: a 4xx body is not an
/// image, and section 3 step 5 already answers for that with `not_an_image`.
/// Making one method serve both would mean either buffering an image in memory to
/// be able to call it a `String`, or making every listing parse a `Vec<u8>`.
///
/// Behind a trait for the reason [`Fetch`] is: the pipeline's own tests drive the
/// origin route with a recorded body and no socket.
pub trait Bytes {
    /// The body of one request, as a stream that ends when the body does.
    ///
    /// The end of the stream is where `curl`'s verdict arrives: the exit status
    /// is read at EOF and not at spawn, so a connection that died mid-transfer is
    /// an error on the read rather than a short file. A truncated download must
    /// not be able to look like a complete one.
    fn bytes(&self, request: &Request<'_>) -> Result<Box<dyn Read>, String>;
}

impl Bytes for Curl {
    fn bytes(&self, request: &Request<'_>) -> Result<Box<dyn Read>, String> {
        let mut child = request_started(&body_configuration(request))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "cannot read `curl`'s body: its stdout is not a pipe".to_string())?;
        Ok(Box::new(Piped {
            child,
            stdout,
            ended: false,
        }))
    }
}

/// One `curl` whose stdout is the body of the response.
struct Piped {
    child: Child,
    stdout: ChildStdout,
    /// Whether the exit status has been read. `store` stops at the first `Ok(0)`
    /// and never asks again, but a second `Ok(0)` must not read a status that has
    /// already been taken.
    ended: bool,
}

impl Read for Piped {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.stdout.read(buffer)?;
        if read > 0 || self.ended {
            return Ok(read);
        }
        self.ended = true;
        if self.child.wait()?.success() {
            return Ok(0);
        }
        // A one-line complaint: `body_configuration` sets `silent` and
        // `show-error`, so curl's progress meter and its transfer summary are
        // suppressed and only the error itself is written. That is what keeps
        // this read-after-wait safe — the stderr pipe cannot fill while this
        // thread is reading stdout. If a future option makes curl chatty, the
        // drain has to move to a thread or a file before the pipe can.
        let mut complaint = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut complaint);
        }
        Err(std::io::Error::other(format!(
            "`curl` failed: {}",
            first_line(&complaint)
        )))
    }
}

/// The one place `curl`'s argv is decided: `--config -`, and nothing else.
///
/// docs/architecture.md 6.3's key rule is a rule about argv, so it can only hold
/// where there is one argv to hold it for. Both routes — the listing
/// ([`Fetch::get`]) and the body ([`Bytes::bytes`]) — spawn through here.
fn invocation() -> Command {
    let mut command = Command::new("curl");
    command
        .arg("--config")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// A `curl` with its config written to its stdin, or the message a missing
/// program owes the operator (the module doc's `Unavailable` case).
///
/// The stdin handle is taken and dropped here, which is how curl learns that the
/// config ended; a failed write means curl died first.
fn request_started(config: &str) -> Result<Child, String> {
    let mut child = invocation().spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            format!("`curl` is not on PATH, and this build makes HTTP requests with it: {error}")
        }
        _ => format!("cannot run `curl`: {error}"),
    })?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "cannot write `curl`'s config: its stdin is not a pipe".to_string())?;
        stdin
            .write_all(config.as_bytes())
            .map_err(|error| format!("cannot write `curl`'s config: {error}"))?;
    }
    Ok(child)
}

/// The options every request this client makes carries, whichever route it is on.
///
/// Every option is here and none is in argv, so the key is in no process's
/// argument list. Both routes build on this one function, which is why the key
/// rule has one site rather than two.
fn options(request: &Request<'_>) -> String {
    let mut out = String::new();
    out.push_str("silent\nshow-error\n");
    out.push_str(&format!("max-time = {TIMEOUT_SECONDS}\n"));
    out.push_str(&format!("url = {}\n", quoted(request.url)));
    out.push_str(&format!("user-agent = {}\n", quoted(USER_AGENT)));
    if let Some(key) = request.key {
        out.push_str(&format!(
            "header = {}\n",
            quoted(&format!("X-API-Key: {key}"))
        ));
    }
    out
}

/// The `curl` config for a listing: a body and the status that came with it.
///
/// `%{http_code}` is appended after a newline, which is how the status comes back
/// beside a body that may itself contain newlines. [`Fetch::get`] is the only
/// caller.
fn configuration(request: &Request<'_>) -> String {
    let mut out = options(request);
    out.push_str("write-out = \"\\n%{http_code}\"\n");
    out
}

/// The `curl` config for a body that is not a document: the bytes, and nothing
/// appended to them.
///
/// The `write-out` of a listing would be appended to *stdout*, which on this
/// route is the image itself, and the pipeline already carries what a status
/// would tell it: the digest of the bytes that arrived. A response whose body is
/// an error page is rejected by section 3 step 5's header sniff, which is the
/// authority that section gives for a body that is not an image. So the status is
/// deliberately not collected here rather than collected and ignored.
fn body_configuration(request: &Request<'_>) -> String {
    options(request)
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
    fn a_keyless_request_sends_no_key_header_and_still_names_a_browser() {
        let config = configuration(&Request {
            url: "https://wallhaven.cc/api/v1/search",
            key: None,
        });
        assert!(!config.contains("header"), "{config}");
        assert!(config.contains("user-agent = \"Mozilla/5.0"), "{config}");
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

    /// docs/architecture.md 6.3's rule is a rule about argv, and this is the argv:
    /// two words, on both routes, with the URL, the user agent and the key all on
    /// stdin. A key in an argument is a key in `ps` for every user on the machine.
    ///
    /// Both routes spawn through [`invocation`], so this one test is the rule for
    /// the listing and for the image alike; the live half of it is acceptance 2 of
    /// `t_3ebd4de8`, a `ps` on a real rotate.
    #[test]
    fn argv_is_config_dash_and_nothing_else() {
        let command = invocation();
        let words: Vec<String> = command
            .get_args()
            .map(|word| word.to_string_lossy().into_owned())
            .collect();
        assert_eq!(words, vec!["--config", "-"], "{words:?}");
        assert_eq!(command.get_program().to_string_lossy().as_ref(), "curl");
    }

    /// The image route's config is the listing's with the one line that would be
    /// appended to the body removed: a status written after an image is a
    /// corrupt image, and the digest of the bytes is what the pipeline reports
    /// instead.
    #[test]
    fn a_body_request_appends_nothing_to_the_bytes() {
        let request = Request {
            url: "https://w.wallhaven.cc/full/83/wallhaven-83dp81.png",
            key: None,
        };
        let config = body_configuration(&request);
        assert!(!config.contains("write-out"), "{config}");
        assert!(
            config.contains("url = \"https://w.wallhaven.cc/full/83/wallhaven-83dp81.png\""),
            "{config}"
        );
        assert!(config.contains("user-agent = \"Mozilla/5.0"), "{config}");
        assert!(config.contains("silent\nshow-error\n"), "{config}");
        assert!(
            config.contains(&format!("max-time = {TIMEOUT_SECONDS}")),
            "{config}"
        );
        // The listing's config is the same options plus the status, in one
        // assertion: the two routes cannot drift into two sets of options, and
        // the key rule of 6.3 has one implementation behind both.
        assert_eq!(
            configuration(&request),
            format!("{config}write-out = \"\\n%{{http_code}}\"\n")
        );
    }
}
