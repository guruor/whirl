//! The line protocol: the grammar of docs/architecture.md section 2.
//!
//! One implementation, used by the daemon, the worker and the CLI
//! (docs/development.md section 1). This module knows nothing about sockets: it
//! parses a request line into a [`Request`], builds a [`Response`] and encodes it,
//! and parses a response back, which is what makes the round-trip test possible.
//!
//! Framing rules the codecs inherit:
//!
//! - UTF-8 text, one message per line, one `\n`; a `\r` before it is tolerated on
//!   input and never emitted.
//! - a request line is at most [`MAX_REQUEST_LINE`] bytes, excluding the newline.
//! - a NUL byte anywhere in a request line is `bad_framing`.
//! - a `-` value means unset, never an empty string.

use std::fmt;

/// The protocol version this build speaks (docs/architecture.md 2.4).
pub const PROTOCOL_VERSION: u32 = 2;

/// The product name, as it appears exactly once in the greeting.
pub const PRODUCT: &str = "whirl";

/// The build's version, from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The greeting every accepted connection opens with: `OK whirl 0.1.0 protocol 2`.
///
/// The name appears once and the version is bare, so a client can split on spaces
/// and read the version from the third field (docs/architecture.md 2.4).
pub fn greeting() -> String {
    format!("OK {PRODUCT} {VERSION} protocol {PROTOCOL_VERSION}")
}

/// A parsed greeting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Greeting {
    pub product: String,
    pub version: String,
    pub protocol: u32,
}

/// Parse the greeting line. `None` when the line is not one.
pub fn parse_greeting(line: &str) -> Option<Greeting> {
    let mut fields = line.split(' ');
    match fields.next() {
        Some("OK") => {}
        _ => return None,
    }
    let product = fields.next()?.to_string();
    let version = fields.next()?.to_string();
    match fields.next() {
        Some("protocol") => {}
        _ => return None,
    }
    let protocol = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(Greeting {
        product,
        version,
        protocol,
    })
}

/// The longest legal request line, excluding the newline (docs/architecture.md 2.2).
pub const MAX_REQUEST_LINE: usize = 8192;

/// `history` defaults and bounds (docs/architecture.md 2.5).
pub const DEFAULT_HISTORY_COUNT: usize = 10;
pub const MAX_HISTORY_COUNT: usize = 50;

/// How a set happened. A closed vocabulary, so a client never parses free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Via {
    Source,
    Manual,
    Prev,
    Startup,
    Recovered,
}

impl Via {
    pub const ALL: [Via; 5] = [
        Via::Source,
        Via::Manual,
        Via::Prev,
        Via::Startup,
        Via::Recovered,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Via::Source => "source",
            Via::Manual => "manual",
            Via::Prev => "prev",
            Via::Startup => "startup",
            Via::Recovered => "recovered",
        }
    }

    pub fn parse(name: &str) -> Option<Via> {
        Via::ALL.iter().copied().find(|v| v.as_str() == name)
    }
}

/// The origin of an entry, not the mechanism that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Local,
    Wallhaven,
    External,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Local, Kind::Wallhaven, Kind::External];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Local => "local",
            Kind::Wallhaven => "wallhaven",
            Kind::External => "external",
        }
    }

    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.as_str() == name)
    }
}

/// The closed error-code set (docs/architecture.md 2.7). Nothing else in the
/// protocol can be mistaken for a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    UnknownVerb,
    BadArgs,
    TooLong,
    BadFraming,
    BadProtocol,
    Busy,
    NotFound,
    NoPrev,
    NoCandidates,
    Offline,
    TooLarge,
    NotAnImage,
    SetFailed,
    Enospc,
    CacheReadonly,
    CacheUnwritable,
    WorkerFailed,
    Timeout,
    FavoritesDegraded,
    BadConfig,
    Internal,
}

impl ErrorCode {
    /// Every code, in the order docs/architecture.md 2.7 lists them.
    pub const ALL: [ErrorCode; 21] = [
        ErrorCode::UnknownVerb,
        ErrorCode::BadArgs,
        ErrorCode::TooLong,
        ErrorCode::BadFraming,
        ErrorCode::BadProtocol,
        ErrorCode::Busy,
        ErrorCode::NotFound,
        ErrorCode::NoPrev,
        ErrorCode::NoCandidates,
        ErrorCode::Offline,
        ErrorCode::TooLarge,
        ErrorCode::NotAnImage,
        ErrorCode::SetFailed,
        ErrorCode::Enospc,
        ErrorCode::CacheReadonly,
        ErrorCode::CacheUnwritable,
        ErrorCode::WorkerFailed,
        ErrorCode::Timeout,
        ErrorCode::FavoritesDegraded,
        ErrorCode::BadConfig,
        ErrorCode::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnknownVerb => "unknown_verb",
            ErrorCode::BadArgs => "bad_args",
            ErrorCode::TooLong => "too_long",
            ErrorCode::BadFraming => "bad_framing",
            ErrorCode::BadProtocol => "bad_protocol",
            ErrorCode::Busy => "busy",
            ErrorCode::NotFound => "not_found",
            ErrorCode::NoPrev => "no_prev",
            ErrorCode::NoCandidates => "no_candidates",
            ErrorCode::Offline => "offline",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::NotAnImage => "not_an_image",
            ErrorCode::SetFailed => "set_failed",
            ErrorCode::Enospc => "enospc",
            ErrorCode::CacheReadonly => "cache_readonly",
            ErrorCode::CacheUnwritable => "cache_unwritable",
            ErrorCode::WorkerFailed => "worker_failed",
            ErrorCode::Timeout => "timeout",
            ErrorCode::FavoritesDegraded => "favorites_degraded",
            ErrorCode::BadConfig => "bad_config",
            ErrorCode::Internal => "internal",
        }
    }

    pub fn parse(name: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.iter().copied().find(|c| c.as_str() == name)
    }

    /// Whether the `ERR` ends the connection (docs/architecture.md 2.7).
    pub fn closes(self) -> bool {
        matches!(
            self,
            ErrorCode::TooLong | ErrorCode::BadFraming | ErrorCode::BadProtocol
        ) || self == ErrorCode::Busy
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request line that could not be turned into a [`Request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
        ProtocolError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello {
        version: u32,
        client: Option<String>,
    },
    Ping,
    Version,
    Status,
    Next,
    Prev,
    SetPath(String),
    SetId(String),
    Pause,
    Resume,
    History {
        count: usize,
    },
    Favorites,
    Favorite {
        id: Option<String>,
    },
    Unfavorite(String),
    Sources,
    ConfigPath,
    ConfigCheck,
    Subscribe {
        since: Option<u64>,
    },
    Close,
}

/// Every verb in the grammar, in the order docs/architecture.md 2.5 and 2.9 list
/// them. Nineteen, counting `set path` and `set id`, `config path` and
/// `config check` separately. A verb added here and left unanswered is the defect
/// the coverage test in docs/development.md section 8 looks for.
pub const VERBS: [&str; 19] = [
    "hello",
    "ping",
    "version",
    "status",
    "next",
    "prev",
    "set path",
    "set id",
    "pause",
    "resume",
    "history",
    "favorites",
    "favorite",
    "unfavorite",
    "sources",
    "config path",
    "config check",
    "subscribe",
    "close",
];

impl Request {
    /// The verb, as the grammar names it. `set path` for a [`Request::SetPath`].
    pub fn verb(&self) -> &'static str {
        match self {
            Request::Hello { .. } => "hello",
            Request::Ping => "ping",
            Request::Version => "version",
            Request::Status => "status",
            Request::Next => "next",
            Request::Prev => "prev",
            Request::SetPath(_) => "set path",
            Request::SetId(_) => "set id",
            Request::Pause => "pause",
            Request::Resume => "resume",
            Request::History { .. } => "history",
            Request::Favorites => "favorites",
            Request::Favorite { .. } => "favorite",
            Request::Unfavorite(_) => "unfavorite",
            Request::Sources => "sources",
            Request::ConfigPath => "config path",
            Request::ConfigCheck => "config check",
            Request::Subscribe { .. } => "subscribe",
            Request::Close => "close",
        }
    }

    /// The wire form of this request, without its newline: the inverse of
    /// [`Request::parse`], and tested as one pair, so a verb added to the grammar
    /// cannot be half-added. `history` and `subscribe` encode their argument
    /// only when they have one, which is the shape 2.5 spells out.
    pub fn encode(&self) -> String {
        match self {
            Request::Hello { version, client } => match client {
                Some(client) => format!("hello {version} {client}"),
                None => format!("hello {version}"),
            },
            Request::Ping => "ping".to_string(),
            Request::Version => "version".to_string(),
            Request::Status => "status".to_string(),
            Request::Next => "next".to_string(),
            Request::Prev => "prev".to_string(),
            Request::SetPath(path) => format!("set path {path}"),
            Request::SetId(id) => format!("set id {id}"),
            Request::Pause => "pause".to_string(),
            Request::Resume => "resume".to_string(),
            Request::History { count } => format!("history {count}"),
            Request::Favorites => "favorites".to_string(),
            Request::Favorite { id } => match id {
                Some(id) => format!("favorite {id}"),
                None => "favorite".to_string(),
            },
            Request::Unfavorite(id) => format!("unfavorite {id}"),
            Request::Sources => "sources".to_string(),
            Request::ConfigPath => "config path".to_string(),
            Request::ConfigCheck => "config check".to_string(),
            Request::Subscribe { since } => match since {
                Some(seq) => format!("subscribe {seq}"),
                None => "subscribe".to_string(),
            },
            Request::Close => "close".to_string(),
        }
    }

    /// Parse one request line, without its newline.
    ///
    /// The `\r` of a CRLF client is tolerated here. Framing failures (`too_long`,
    /// `bad_framing`) are refused before anything is parsed.
    pub fn parse(line: &str) -> Result<Request, ProtocolError> {
        if line.len() > MAX_REQUEST_LINE {
            return Err(ProtocolError::new(
                ErrorCode::TooLong,
                format!("request line exceeds {MAX_REQUEST_LINE} bytes"),
            ));
        }
        if line.contains('\0') {
            return Err(ProtocolError::new(
                ErrorCode::BadFraming,
                "NUL in request line",
            ));
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (verb, rest) = match line.split_once(' ') {
            Some((verb, rest)) => (verb, Some(rest)),
            None => (line, None),
        };
        // A bare verb has no argument *list* at all, which is what makes
        // `history` and `favorite` optional-argument verbs (2.5); a trailing
        // space is an empty argument, and the verbs that take none refuse it.
        let args: Vec<&str> = match rest {
            None => Vec::new(),
            Some("") => vec![""],
            Some(rest) => rest.split(' ').collect(),
        };
        match verb {
            "hello" => {
                let version = args.first().copied().unwrap_or("");
                let version: u32 = version.parse().map_err(|_| {
                    ProtocolError::new(
                        ErrorCode::BadProtocol,
                        format!("hello: {version:?} is not a protocol version"),
                    )
                })?;
                let client = match args.get(1) {
                    None => None,
                    Some(name) if args.len() == 2 => Some((*name).to_string()),
                    Some(_) => {
                        return Err(ProtocolError::new(
                            ErrorCode::BadArgs,
                            "hello: at most one client id",
                        ));
                    }
                };
                Ok(Request::Hello { version, client })
            }
            "ping" => no_args(args, Request::Ping),
            "version" => no_args(args, Request::Version),
            "status" => no_args(args, Request::Status),
            "next" => no_args(args, Request::Next),
            "prev" => no_args(args, Request::Prev),
            "pause" => no_args(args, Request::Pause),
            "resume" => no_args(args, Request::Resume),
            "favorites" => no_args(args, Request::Favorites),
            "sources" => no_args(args, Request::Sources),
            "close" => no_args(args, Request::Close),
            "history" => {
                let count = match (args.first().copied(), args.len()) {
                    (None, _) => DEFAULT_HISTORY_COUNT,
                    (Some(n), 1) => {
                        n.parse::<usize>().ok().filter(|n| *n >= 1).ok_or_else(|| {
                            ProtocolError::new(
                                ErrorCode::BadArgs,
                                format!("history: n must be 1..={MAX_HISTORY_COUNT}, got {n}"),
                            )
                        })?
                    }
                    (Some(_), _) => {
                        return Err(ProtocolError::new(
                            ErrorCode::BadArgs,
                            "history: at most one count",
                        ));
                    }
                };
                if count > MAX_HISTORY_COUNT {
                    return Err(ProtocolError::new(
                        ErrorCode::BadArgs,
                        format!("history: n must be 1..={MAX_HISTORY_COUNT}, got {count}"),
                    ));
                }
                Ok(Request::History { count })
            }
            "favorite" => match args.len() {
                0 => Ok(Request::Favorite { id: None }),
                1 => Ok(Request::Favorite {
                    id: Some(non_empty(args[0], "favorite")?.to_string()),
                }),
                _ => Err(ProtocolError::new(
                    ErrorCode::BadArgs,
                    "favorite: at most one id",
                )),
            },
            "unfavorite" => match args.len() {
                1 => Ok(Request::Unfavorite(
                    non_empty(args[0], "unfavorite")?.to_string(),
                )),
                _ => Err(ProtocolError::new(
                    ErrorCode::BadArgs,
                    "unfavorite: exactly one id",
                )),
            },
            "set" => {
                // `set path` and `set id` take the rest of the line, spaces
                // included: that is what lets a path contain spaces (2.2).
                let rest = rest.unwrap_or("");
                if let Some(path) = rest.strip_prefix("path ") {
                    let path = path.trim_end_matches('\r');
                    if path.is_empty() {
                        return Err(ProtocolError::new(
                            ErrorCode::BadArgs,
                            "set path: an absolute path is required",
                        ));
                    }
                    if !is_absolute_path(path) {
                        return Err(ProtocolError::new(
                            ErrorCode::BadArgs,
                            format!("set path: {path:?} is not absolute"),
                        ));
                    }
                    Ok(Request::SetPath(path.to_string()))
                } else if let Some(id) = rest.strip_prefix("id ") {
                    let id = id.trim_end_matches('\r');
                    Ok(Request::SetId(non_empty(id, "set id")?.to_string()))
                } else {
                    Err(ProtocolError::new(
                        ErrorCode::BadArgs,
                        "set: the second token must be `path` or `id`",
                    ))
                }
            }
            "config" => match (args.first().copied(), args.len()) {
                (Some("path"), 1) => Ok(Request::ConfigPath),
                (Some("check"), 1) => Ok(Request::ConfigCheck),
                _ => Err(ProtocolError::new(
                    ErrorCode::BadArgs,
                    "config: the second token must be `path` or `check`",
                )),
            },
            "subscribe" => match args.len() {
                0 => Ok(Request::Subscribe { since: None }),
                1 => {
                    let since: u64 = args[0].parse().map_err(|_| {
                        ProtocolError::new(
                            ErrorCode::BadArgs,
                            format!("subscribe: {} is not a sequence number", args[0]),
                        )
                    })?;
                    Ok(Request::Subscribe { since: Some(since) })
                }
                _ => Err(ProtocolError::new(
                    ErrorCode::BadArgs,
                    "subscribe: at most one sequence number",
                )),
            },
            other => Err(ProtocolError::new(
                ErrorCode::UnknownVerb,
                format!("unknown command {other:?}"),
            )),
        }
    }
}

fn no_args(args: Vec<&str>, request: Request) -> Result<Request, ProtocolError> {
    if args.len() > 1 || (args.len() == 1 && !args[0].is_empty()) {
        return Err(ProtocolError::new(
            ErrorCode::BadArgs,
            format!("{}: takes no arguments", request.verb()),
        ));
    }
    Ok(request)
}

fn non_empty<'a>(value: &'a str, verb: &str) -> Result<&'a str, ProtocolError> {
    if value.is_empty() {
        Err(ProtocolError::new(
            ErrorCode::BadArgs,
            format!("{verb}: an argument is required"),
        ))
    } else {
        Ok(value)
    }
}

/// An absolute path, as the grammar defines it: a leading `/`, a leading `~`, a
/// drive letter, or a UNC `\\` (docs/architecture.md 2.5).
pub fn is_absolute_path(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('~') || path.starts_with("\\\\") {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// The terminator of a response: exactly one, and it is the last line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Ok,
    Err { code: ErrorCode, message: String },
}

impl Terminator {
    pub fn encode(&self) -> String {
        match self {
            Terminator::Ok => "OK".to_string(),
            Terminator::Err { code, message } => {
                if message.is_empty() {
                    format!("ERR {code}")
                } else {
                    format!("ERR {code} {message}")
                }
            }
        }
    }
}

/// A response: zero or more data lines followed by exactly one terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    lines: Vec<String>,
    terminator: Terminator,
}

impl Response {
    pub fn ok() -> Response {
        Response {
            lines: Vec::new(),
            terminator: Terminator::Ok,
        }
    }

    pub fn err(code: ErrorCode, message: impl fmt::Display) -> Response {
        Response {
            lines: Vec::new(),
            terminator: Terminator::Err {
                code,
                message: message.to_string(),
            },
        }
    }

    /// A `key: value` data line, or a positional record line, which is the same
    /// shape with fields after the key, or the bare interim `queued`.
    pub fn line(mut self, line: impl Into<String>) -> Response {
        self.lines.push(line.into());
        self
    }

    pub fn kv(self, key: &str, value: impl fmt::Display) -> Response {
        self.line(format!("{key}: {value}"))
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn terminator(&self) -> &Terminator {
        &self.terminator
    }

    /// Wire encoding: every line, then the terminator, each ending in `\n`.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&self.terminator.encode());
        out.push('\n');
        out
    }
}

/// A parsed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedResponse {
    pub lines: Vec<String>,
    pub terminator: Terminator,
}

/// Parse a whole response. `Err` carries a protocol error only for input that is
/// not a response at all (no terminator); a response that terminates in `ERR` is
/// `Ok` with an `Err` terminator, because that is a legal response.
pub fn parse_response(text: &str) -> Result<ParsedResponse, ProtocolError> {
    let mut lines = Vec::new();
    let mut terminator = None;
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line == "OK" {
            terminator = Some(Terminator::Ok);
            break;
        }
        if let Some(rest) = line.strip_prefix("ERR ") {
            let (code, message) = match rest.split_once(' ') {
                Some((code, message)) => (code, message.to_string()),
                None => (rest, String::new()),
            };
            let code = ErrorCode::parse(code).ok_or_else(|| {
                ProtocolError::new(ErrorCode::Internal, format!("unknown error code {code:?}"))
            })?;
            terminator = Some(Terminator::Err { code, message });
            break;
        }
        lines.push(line.to_string());
    }
    match terminator {
        Some(terminator) => Ok(ParsedResponse { lines, terminator }),
        None => Err(ProtocolError::new(
            ErrorCode::Internal,
            "no OK or ERR terminator",
        )),
    }
}

/// What a line read off the wire is, for a client that prints as it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Greeting,
    /// The only interim line: `queued`.
    Interim,
    /// A `key: value` data line or a positional record line.
    Data,
    /// An `event: <seq> <type> ...` line of a subscription.
    Event,
    Ok,
    Err {
        code: ErrorCode,
        message: String,
    },
    /// A data line with no `key: ` prefix, which the grammar does not allow.
    Malformed,
}

pub fn classify_line(line: &str) -> LineKind {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line == "OK" {
        return LineKind::Ok;
    }
    if let Some(rest) = line.strip_prefix("ERR ") {
        let (code, message) = match rest.split_once(' ') {
            Some((code, message)) => (code, message.to_string()),
            None => (rest, String::new()),
        };
        return match ErrorCode::parse(code) {
            Some(code) => LineKind::Err { code, message },
            None => LineKind::Malformed,
        };
    }
    if parse_greeting(line).is_some() {
        return LineKind::Greeting;
    }
    if line == "queued" {
        return LineKind::Interim;
    }
    if line.starts_with("event: ") {
        return LineKind::Event;
    }
    if is_record_or_data_line(line) {
        return LineKind::Data;
    }
    LineKind::Malformed
}

/// A data line carries a `key: value` prefix, or is one of the four positional
/// record forms (docs/architecture.md 2.6).
fn is_record_or_data_line(line: &str) -> bool {
    for key in ["entry", "source", "set", "plan"] {
        if let Some(rest) = line.strip_prefix(key) {
            if rest.starts_with(": ") {
                return true;
            }
        }
    }
    match line.split_once(':') {
        Some((key, _)) => {
            let mut chars = key.chars();
            match chars.next() {
                Some(first) if first.is_ascii_lowercase() => {}
                _ => return false,
            }
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        }
        None => false,
    }
}

/// A set, as `set:` reports it and as `status` reports the last one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetRecord {
    pub digest: String,
    pub origin_key: String,
    pub via: Via,
    pub path: Option<String>,
}

/// `set: <digest> <origin_key> <via> <path|->` (docs/architecture.md 2.6).
pub fn set_record(digest: &str, origin_key: &str, via: Via, path: Option<&str>) -> String {
    format!(
        "set: {digest} {origin_key} {} {}",
        via.as_str(),
        path.unwrap_or("-")
    )
}

pub fn parse_set_record(line: &str) -> Option<SetRecord> {
    let rest = line.strip_prefix("set: ")?;
    let mut fields = rest.splitn(4, ' ');
    let digest = fields.next()?.to_string();
    let origin_key = fields.next()?.to_string();
    let via = Via::parse(fields.next()?)?;
    let path = fields.next().map(|p| p.trim_end_matches(' ').to_string());
    Some(SetRecord {
        digest,
        origin_key,
        via,
        path: path.and_then(|p| if p == "-" { None } else { Some(p) }),
    })
}

/// A source, as `status`, `sources` and `config check` report it
/// (docs/architecture.md 2.6). The bracketed counter group appears only in a
/// `config check` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub id: String,
    pub kind: String,
    pub weight: u32,
    pub enabled: bool,
    pub last: Option<String>,
    pub counters: Vec<(String, u64)>,
    pub reason: Option<String>,
}

impl SourceRecord {
    pub fn line(&self) -> String {
        let mut out = format!(
            "source: {} {} weight={} enabled={} last={}",
            self.id,
            self.kind,
            self.weight,
            u8::from(self.enabled),
            self.last.as_deref().unwrap_or("-")
        );
        for (name, value) in &self.counters {
            out.push_str(&format!(" {name}={value}"));
        }
        out.push_str(&format!(
            " reason={}",
            self.reason.as_deref().unwrap_or("-")
        ));
        out
    }
}

pub fn parse_source_record(line: &str) -> Option<SourceRecord> {
    let rest = line.strip_prefix("source: ")?;
    // `reason` is the record's last field and is free-form prose, so it takes
    // whatever is left of the line: splitting it on spaces would truncate
    // `no key at keychain:whirl-wallhaven` to `no` (2.6).
    let (head, tail) = match rest.split_once(" reason=") {
        Some((head, reason)) => (head, Some(reason)),
        None => (rest, None),
    };
    let fields: Vec<&str> = head.split(' ').collect();
    let id = fields.first()?.to_string();
    let kind = fields.get(1)?.to_string();
    let mut weight = 1;
    let mut enabled = true;
    let mut last = None;
    let mut counters = Vec::new();
    for field in &fields[2..] {
        if let Some((name, value)) = field.split_once('=') {
            match name {
                "weight" => weight = value.parse().ok()?,
                "enabled" => enabled = value == "1",
                "last" => last = none_if_dash(value),
                // The reason belongs to the tail, never to the middle.
                "reason" => return None,
                other => counters.push((other.to_string(), value.parse().ok()?)),
            }
        } else {
            return None;
        }
    }
    Some(SourceRecord {
        id,
        kind,
        weight,
        enabled,
        last,
        counters,
        reason: tail.and_then(none_if_dash),
    })
}

/// `plan: <config key>=<effective value> ...`, in file order, so that "what did
/// the daemon actually adopt" is answerable without reading the daemon's mind.
pub fn plan_record(pairs: &[(String, String)]) -> String {
    let mut out = String::from("plan: ");
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(key);
        out.push('=');
        out.push_str(value);
    }
    out
}

pub fn parse_plan_record(line: &str) -> Option<Vec<(String, String)>> {
    let rest = line.strip_prefix("plan: ")?;
    let mut pairs = Vec::new();
    for field in rest.split(' ') {
        let (key, value) = field.split_once('=')?;
        pairs.push((key.to_string(), value.to_string()));
    }
    Some(pairs)
}

fn none_if_dash(value: &str) -> Option<String> {
    if value == "-" {
        None
    } else {
        Some(value.to_string())
    }
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

/// The SHA-256 of `data`, as 64 lower-case hex characters: the digest form the
/// protocol carries, so the encoder and the decoder agree on one spelling of it
/// (docs/architecture.md 2.6).
///
/// The algorithm lives in the protocol module for two reasons: a digest is a
/// protocol token, so its encoding belongs with the grammar, and `whirl-core` is
/// dependency-free by rule (docs/development.md section 2), so there is no crate
/// to call. The daemon and the worker both link this crate, and a second hasher
/// would be a second answer to "is this the same image".
pub fn sha256_hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for byte in sha256(data) {
        for nibble in [byte >> 4, byte & 0x0f] {
            out.push(char::from_digit(nibble as u32, 16).unwrap_or('0'));
        }
    }
    out
}

/// Whether `value` is a content digest: 64 lower-case hex characters
/// (docs/architecture.md 2.6). This is what lets `set id` tell a digest from an
/// `origin_key` without guessing (2.5).
pub fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The worker's `set:` line on stdout: `set: <digest> <origin_key> <path>`
/// (docs/architecture.md 1.6). Three fields, because `via` is the daemon's
/// bookkeeping: the worker reports what it set, the daemon decides why, and the
/// four-field record of 2.6 is assembled by the daemon alone.
pub fn parse_worker_set_line(line: &str) -> Option<(String, String, String)> {
    let rest = line.strip_prefix("set: ")?;
    let mut fields = rest.splitn(3, ' ');
    let digest = fields.next()?;
    if !is_digest(digest) {
        return None;
    }
    let origin_key = fields.next()?;
    let path = fields.next()?;
    if origin_key.is_empty() || path.is_empty() {
        return None;
    }
    Some((digest.to_string(), origin_key.to_string(), path.to_string()))
}

/// An RFC 3339 UTC timestamp, e.g. `2026-09-25T07:41:12Z` (docs/architecture.md
/// 2.6: "Timestamps are RFC 3339 UTC"). A timestamp is a protocol token, so it
/// is formatted here beside the grammar and the hasher, and every timestamp in
/// the workspace goes through this one function. The civil-date step is Howard
/// Hinnant's `civil_from_days`, which derives leap years from era arithmetic
/// instead of a table.
pub fn rfc3339_utc(seconds_since_epoch: i64) -> String {
    let days = seconds_since_epoch.div_euclid(86_400);
    let seconds = seconds_since_epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date. Shifts the epoch to
/// 0000-03-01 so a leap day is the last day of the year and the 400-year cycle
/// divides cleanly; `div_euclid` is the floor division the algorithm assumes.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// SHA-256 (FIPS 180-4), one shot: pad, then compress each 64-byte block.
fn sha256(data: &[u8]) -> [u8; 32] {
    // The fractional parts of the cube roots of the first 64 primes.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    // The fractional parts of the square roots of the first 8 primes.
    const H: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = H;
    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let mut v = state;
        for (index, constant) in K.iter().enumerate() {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let choose = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let temp1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(*constant)
                .wrapping_add(w[index]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let majority = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let temp2 = s0.wrapping_add(majority);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(temp1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = temp1.wrapping_add(temp2);
        }
        for index in 0..8 {
            state[index] = state[index].wrapping_add(v[index]);
        }
    }

    let mut digest = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vectors() {
        // FIPS 180-4 and RFC 6234 vectors, so a wrong constant or a wrong padding
        // rule cannot pass. The million-byte case is the padding that spills into a
        // second block.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
        assert_eq!(
            sha256_hex("a".repeat(1_000_000).as_bytes()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        // The digest form the protocol carries: 64 lower-case hex characters.
        let digest = sha256_hex(b"abc");
        assert!(is_digest(&digest));
        assert!(!is_digest(&digest.to_uppercase()));
        assert!(!is_digest(&digest[..63]));
    }

    #[test]
    fn the_greeting_names_the_product_once_and_carries_the_bare_version() {
        let greeting = greeting();
        assert_eq!(
            greeting,
            format!("OK whirl {VERSION} protocol {PROTOCOL_VERSION}")
        );
        assert_eq!(greeting, "OK whirl 0.1.0 protocol 2");
        let parsed = parse_greeting(&greeting).expect("a greeting");
        assert_eq!(parsed.product, "whirl");
        assert_eq!(parsed.version, "0.1.0");
        assert_eq!(parsed.protocol, 2);
        // The two-token form belongs to `daemon_version`, never to the greeting.
        assert!(parse_greeting("OK whirl whirl 0.1.0 protocol 2").is_none());
    }

    #[test]
    fn every_verb_parses_to_a_request_and_round_trips() {
        let cases: [(&str, Request); 19] = [
            (
                "hello 2 whirl-cli/0.1.0",
                Request::Hello {
                    version: 2,
                    client: Some("whirl-cli/0.1.0".to_string()),
                },
            ),
            ("ping", Request::Ping),
            ("version", Request::Version),
            ("status", Request::Status),
            ("next", Request::Next),
            ("prev", Request::Prev),
            (
                "set path /Users/some one/a b.jpg",
                Request::SetPath("/Users/some one/a b.jpg".to_string()),
            ),
            (
                "set id space:ab12cd",
                Request::SetId("space:ab12cd".to_string()),
            ),
            ("pause", Request::Pause),
            ("resume", Request::Resume),
            ("history", Request::History { count: 10 }),
            ("favorites", Request::Favorites),
            (
                "favorite space:ab12cd",
                Request::Favorite {
                    id: Some("space:ab12cd".to_string()),
                },
            ),
            ("unfavorite 0123", Request::Unfavorite("0123".to_string())),
            ("sources", Request::Sources),
            ("config path", Request::ConfigPath),
            ("config check", Request::ConfigCheck),
            ("subscribe 182", Request::Subscribe { since: Some(182) }),
            ("close", Request::Close),
        ];
        let verbs: Vec<&str> = cases.iter().map(|(_, request)| request.verb()).collect();
        assert_eq!(verbs, VERBS.to_vec());
        for (line, expected) in cases {
            let parsed = Request::parse(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(parsed, expected, "parsing {line:?}");
            assert_eq!(parsed.verb(), expected.verb());
        }
        // A CRLF client is tolerated on input.
        assert_eq!(Request::parse("ping\r").unwrap(), Request::Ping);
    }

    #[test]
    fn every_request_encodes_to_a_line_that_parses_back() {
        // The client half of the grammar: `encode` is the inverse of `parse`, so
        // a verb added to one cannot be missing from the other, and the CLI
        // cannot send a line the daemon refuses
        // (docs/development.md section 8).
        let cases = [
            Request::Hello {
                version: 2,
                client: None,
            },
            Request::Hello {
                version: 2,
                client: Some("whirl-cli/0.1.0".to_string()),
            },
            Request::Ping,
            Request::Version,
            Request::Status,
            Request::Next,
            Request::Prev,
            Request::SetPath("/Users/some one/a b.jpg".to_string()),
            Request::SetId("space:ab12cd".to_string()),
            Request::Pause,
            Request::Resume,
            Request::History { count: 1 },
            Request::History { count: 10 },
            Request::History { count: 50 },
            Request::Favorites,
            Request::Favorite { id: None },
            Request::Favorite {
                id: Some("space:ab12cd".to_string()),
            },
            Request::Unfavorite("0123".to_string()),
            Request::Sources,
            Request::ConfigPath,
            Request::ConfigCheck,
            Request::Subscribe { since: None },
            Request::Subscribe { since: Some(182) },
            Request::Close,
        ];
        for request in cases {
            let line = request.encode();
            assert!(!line.contains('\n'), "{line:?}");
            assert!(line.len() <= MAX_REQUEST_LINE, "{line:?}");
            let parsed = Request::parse(&line).unwrap_or_else(|error| panic!("{line:?}: {error}"));
            assert_eq!(parsed, request, "encoding {line:?}");
        }
    }

    #[test]
    fn bad_requests_name_their_code() {
        let cases = [
            ("frobnicate", ErrorCode::UnknownVerb),
            ("status now", ErrorCode::BadArgs),
            ("history 500", ErrorCode::BadArgs),
            ("history 0", ErrorCode::BadArgs),
            ("history abc", ErrorCode::BadArgs),
            ("hello abc", ErrorCode::BadProtocol),
            ("hello -1", ErrorCode::BadProtocol),
            ("set path relative/file.jpg", ErrorCode::BadArgs),
            ("set path", ErrorCode::BadArgs),
            ("set frobnicate x", ErrorCode::BadArgs),
            ("config", ErrorCode::BadArgs),
            ("unfavorite", ErrorCode::BadArgs),
            ("subscribe abc", ErrorCode::BadArgs),
        ];
        for (line, code) in cases {
            let error = Request::parse(line).expect_err(line);
            assert_eq!(error.code, code, "parsing {line:?} gave {error}");
        }
        // The message quotes the offender, as the transcripts do.
        assert_eq!(
            Request::parse("frobnicate").unwrap_err().message,
            "unknown command \"frobnicate\""
        );
        assert_eq!(
            Request::parse("history 500").unwrap_err().message,
            "history: n must be 1..=50, got 500"
        );
        // Framing failures are recognised before anything else.
        assert_eq!(
            Request::parse("status\0").unwrap_err().code,
            ErrorCode::BadFraming
        );
        let long = "x".repeat(MAX_REQUEST_LINE + 1);
        assert_eq!(Request::parse(&long).unwrap_err().code, ErrorCode::TooLong);
        assert_eq!(
            Request::parse(&"x".repeat(MAX_REQUEST_LINE))
                .unwrap_err()
                .code,
            ErrorCode::UnknownVerb
        );
    }

    #[test]
    fn a_response_round_trips() {
        let response = Response::ok()
            .kv("daemon_version", "whirl 0.1.0")
            .kv("protocol", PROTOCOL_VERSION)
            .line("queued")
            .line(set_record(
                "d435840c",
                "pictures:abc",
                Via::Source,
                Some("/tmp/d435840c.jpg"),
            ));
        let encoded = response.encode();
        let parsed = parse_response(&encoded).expect("a response");
        assert_eq!(parsed.lines, response.lines());
        assert_eq!(parsed.terminator, Terminator::Ok);
        assert_eq!(parsed.lines[3], response.lines()[3]);

        let failure = Response::err(ErrorCode::BadArgs, "history: n must be 1..=50, got 500");
        let parsed = parse_response(&failure.encode()).expect("a response");
        assert!(parsed.lines.is_empty());
        assert_eq!(
            parsed.terminator,
            Terminator::Err {
                code: ErrorCode::BadArgs,
                message: "history: n must be 1..=50, got 500".to_string(),
            }
        );
        // The exact terminator text of the transcript.
        assert_eq!(
            failure.encode(),
            "ERR bad_args history: n must be 1..=50, got 500\n"
        );
        // An error with no message is still a legal terminator.
        assert_eq!(Response::err(ErrorCode::Busy, "").encode(), "ERR busy\n");
        // Input with no terminator is not a response.
        assert!(parse_response("count: 0\n").is_err());
    }

    #[test]
    fn the_positional_records_round_trip() {
        let set = set_record(
            "3b29b61764d0a17238f7a51d2585eccf538171d638f210e810f4e8eab970387a",
            "pictures:401df7171a5e52d88b2f7f9e9d201308f9600e7034916d7865c883f33ec64dcd",
            Via::Manual,
            Some("/Users/some one/Pictures/a b.jpg"),
        );
        assert_eq!(
            parse_set_record(&set),
            Some(SetRecord {
                digest: "3b29b61764d0a17238f7a51d2585eccf538171d638f210e810f4e8eab970387a"
                    .to_string(),
                origin_key:
                    "pictures:401df7171a5e52d88b2f7f9e9d201308f9600e7034916d7865c883f33ec64dcd"
                        .to_string(),
                via: Via::Manual,
                path: Some("/Users/some one/Pictures/a b.jpg".to_string()),
            })
        );
        // A reference-mode set has no path.
        let set = set_record("ab", "pictures:ab", Via::Prev, None);
        assert_eq!(parse_set_record(&set).expect("a set record").path, None);

        let source = SourceRecord {
            id: "pictures".to_string(),
            kind: "local".to_string(),
            weight: 1,
            enabled: true,
            last: Some("ok".to_string()),
            counters: vec![
                ("candidates".to_string(), 412),
                ("admitted".to_string(), 97),
                ("rejected_resolution".to_string(), 203),
            ],
            reason: None,
        };
        assert_eq!(parse_source_record(&source.line()), Some(source.clone()));
        assert_eq!(
            source.line(),
            "source: pictures local weight=1 enabled=1 last=ok candidates=412 admitted=97 rejected_resolution=203 reason=-"
        );
        let disabled = SourceRecord {
            id: "space".to_string(),
            kind: "wallhaven".to_string(),
            weight: 3,
            enabled: false,
            last: None,
            counters: Vec::new(),
            reason: Some("no key at keychain:whirl-wallhaven".to_string()),
        };
        assert_eq!(parse_source_record(&disabled.line()), Some(disabled));

        let plan = plan_record(&[
            ("schedule.interval_seconds".to_string(), "1800".to_string()),
            ("backend".to_string(), "noop".to_string()),
            ("sources_enabled".to_string(), "1".to_string()),
        ]);
        assert_eq!(
            plan,
            "plan: schedule.interval_seconds=1800 backend=noop sources_enabled=1"
        );
        assert_eq!(parse_plan_record(&plan).map(|p| p.len()), Some(3));
    }

    #[test]
    fn lines_are_classified_the_way_a_client_reads_them() {
        assert_eq!(classify_line(&greeting()), LineKind::Greeting);
        assert_eq!(classify_line("queued"), LineKind::Interim);
        assert_eq!(classify_line("OK"), LineKind::Ok);
        assert_eq!(classify_line("count: 0"), LineKind::Data);
        assert_eq!(classify_line("last_error: -"), LineKind::Data);
        assert_eq!(
            classify_line("entry: 2026-09-25T07:41:12Z source local x y z"),
            LineKind::Data
        );
        assert_eq!(
            classify_line("event: 184 rotate_start 4211"),
            LineKind::Event
        );
        assert_eq!(
            classify_line("ERR bad_args history: n must be 1..=50, got 500"),
            LineKind::Err {
                code: ErrorCode::BadArgs,
                message: "history: n must be 1..=50, got 500".to_string(),
            }
        );
        assert_eq!(classify_line("whirl status"), LineKind::Malformed);
    }

    #[test]
    fn error_codes_round_trip_and_the_closing_four_are_marked() {
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(ErrorCode::ALL.len(), 21);
        let closing: Vec<&str> = ErrorCode::ALL
            .iter()
            .filter(|c| c.closes())
            .map(|c| c.as_str())
            .collect();
        assert_eq!(
            closing,
            vec!["too_long", "bad_framing", "bad_protocol", "busy"]
        );
    }

    #[test]
    fn absolute_paths_are_recognised_by_the_grammar() {
        assert!(is_absolute_path("/Users/x/a.jpg"));
        assert!(is_absolute_path("~/Pictures/a.jpg"));
        assert!(is_absolute_path("C:\\Users\\x\\a.jpg"));
        assert!(is_absolute_path("\\\\server\\share\\a.jpg"));
        assert!(!is_absolute_path("pictures/a.jpg"));
        assert!(!is_absolute_path("./a.jpg"));
        assert!(!is_absolute_path(""));
    }

    #[test]
    fn via_and_kind_vocabularies_are_closed() {
        for via in Via::ALL {
            assert_eq!(Via::parse(via.as_str()), Some(via));
        }
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(Via::parse("frobnicate"), None);
        assert_eq!(Kind::parse("frobnicate"), None);
    }
}
