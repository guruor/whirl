//! The Linux setter and readback: one adapter per desktop, chosen from the
//! session signals docs/architecture.md 1.6 has the daemon pass and
//! docs/research/linux.md's "Detecting the environment" names decisive.
//!
//! **One child process per call, and no crate.** v0.1 allows no third-party
//! dependency at all (docs/development.md section 2), and the CI `guards` job
//! fails if any package other than the four workspace crates appears in
//! `Cargo.lock`, so there is no binding for GSettings, for KDE's session bus or
//! for a compositor's IPC. That is not a loss here: docs/research/linux.md's
//! method is to run the desktop's own tool and read its answer, which is one
//! `std::process::Command` and one exit status per call. Nothing in this file
//! starts a daemon, opens a surface or supervises a process.
//!
//! **The tool per desktop is the research's decision table, not a guess from
//! `PATH`.** docs/research/linux.md is explicit that a binary being present is
//! not a session signal (`gsettings` exists on any GLib desktop and only GNOME
//! installs the schema), so the desktop is chosen from the session variables and
//! the tool follows from the choice:
//!
//! | session | command | one-shot from whirl |
//! |---|---|---|
//! | GNOME | `gsettings set org.gnome.desktop.background picture-uri` | yes, both keys |
//! | KDE Plasma 6 | `plasma-apply-wallpaperimage --fill-mode preserveAspectCrop` | yes |
//! | sway | `swaymsg output '*' bg <path> fill` | yes, sway owns `swaybg` |
//! | bare X11 | `feh --bg-fill`, `xwallpaper --zoom`, `hsetroot -fill` | yes, any one |
//! | Hyprland | `hyprctl hyprpaper wallpaper` | no, refused by name |
//!
//! Two details from the research are load-bearing rather than cosmetic. GNOME's
//! shell chooses between `picture-uri` and `picture-uri-dark` from
//! `org.gnome.desktop.interface color-scheme`, so both keys are written and a
//! dark-mode user is not left looking at the previous picture. And the sway call
//! goes through sway itself rather than starting `swaybg`: the background process
//! belongs to the compositor, and a second owner is what the prototype's
//! `pkill -x swaybg` got wrong.
//!
//! **Hyprland is detected and refused by name.** docs/research/linux.md records
//! it as the one environment a one-shot setter cannot serve: `hyprctl hyprpaper
//! wallpaper` needs `hyprpaper` already running, whirl does not start, own or
//! supervise it, and docs/architecture.md 3.4 defers the platform for exactly
//! that reason. The refusal says which desktop was detected and which tool and
//! helper it would have needed.
//!
//! **Every refusal names itself.** The desktop detected, the signal it came from,
//! the tool, the command, the exit status and what the tool said on the way out
//! are all in the message, because the failure this adapter is most likely to
//! meet is a machine whose desktop nobody anticipated.
//!
//! **Unverified on real hardware.** No Linux desktop session has run these
//! commands: they are the ones docs/research/linux.md records, and the gate image
//! has no GNOME, KDE, sway or X11 session to run them in. What the tests pin is
//! the session-to-tool decision, the argument each command is given, and every
//! refusal path, with stub executables on a temporary `PATH`. The per-environment
//! walk-throughs in docs/research/linux.md are what a real distribution proves,
//! and that run is the owner's, not this file's.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

use super::SetError;
use whirl_core::protocol::ErrorCode;

/// The session signals docs/architecture.md 1.6 has the daemon pass to this
/// worker on Linux. Read once, into one struct, so the choice between the
/// adapters is a function of these five values alone and a test can pose a
/// session the gate image does not have. An empty variable counts as unset, which
/// is what the shell's `VAR=` means.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Signals {
    /// `XDG_CURRENT_DESKTOP`: a colon-separated list, never matched by equality
    /// (Ubuntu ships `ubuntu:GNOME`), and it is the signal that names a desktop.
    current_desktop: Option<String>,
    /// `XDG_SESSION_TYPE`: `wayland` or `x11`. It decides whether the X11 tools
    /// are admissible at all, not which desktop this is.
    session_type: Option<String>,
    /// sway's IPC socket path (`sway-ipc(7)`).
    sway_sock: Option<String>,
    /// i3's IPC socket. docs/architecture.md 1.6 names the two together, because
    /// the research's detection table does.
    i3_sock: Option<String>,
    /// `HYPRLAND_INSTANCE_SIGNATURE`, the variable `hyprctl` itself refuses to
    /// work without.
    hyprland_signature: Option<String>,
}

impl Signals {
    fn from_env() -> Signals {
        let read = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Signals {
            current_desktop: read("XDG_CURRENT_DESKTOP"),
            session_type: read("XDG_SESSION_TYPE"),
            sway_sock: read("SWAYSOCK"),
            i3_sock: read("I3SOCK"),
            hyprland_signature: read("HYPRLAND_INSTANCE_SIGNATURE"),
        }
    }

    /// Every signal that was read and what it held, spelled out. This is the
    /// half of a refusal that says what it looked for, and it is a sentence a
    /// reader can compare against `env` on the machine that failed.
    fn line(&self) -> String {
        let show = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::to_owned)
                .unwrap_or_else(|| "<unset>".to_string())
        };
        format!(
            "XDG_CURRENT_DESKTOP={} XDG_SESSION_TYPE={} SWAYSOCK={} I3SOCK={} HYPRLAND_INSTANCE_SIGNATURE={}",
            show(&self.current_desktop),
            show(&self.session_type),
            show(&self.sway_sock),
            show(&self.i3_sock),
            show(&self.hyprland_signature),
        )
    }
}

/// A session as this backend sees it: the signals, plus the `PATH` the daemon
/// passed (`docs/architecture.md` 1.6) for the desktop's own tools to be found
/// on. A struct rather than reads of this process's environment, so a test can
/// pose a session the gate image does not have.
#[derive(Debug, Default, Clone)]
struct Session {
    signals: Signals,
    path: Option<String>,
}

impl Session {
    fn from_env() -> Session {
        Session {
            signals: Signals::from_env(),
            path: std::env::var("PATH").ok().filter(|value| !value.is_empty()),
        }
    }

    /// The tool's absolute path, searched in the `PATH` this session carries.
    /// Absolute rather than a bare name so the search is this code's and the
    /// refusal can say which `PATH` was searched. The execute bit is not tested
    /// here: a file that is not executable fails the spawn, and the refusal then
    /// carries the platform's own reason instead of one this code invented.
    fn resolve(&self, tool: &str) -> Option<PathBuf> {
        let path = self.path.as_deref()?;
        path.split(':')
            .filter(|directory| !directory.is_empty())
            .map(|directory| Path::new(directory).join(tool))
            .find(|candidate| candidate.is_file())
    }
}

/// The desktops docs/research/linux.md records a setter for. `X11` is the bare
/// window-manager case (i3, openbox, bspwm), not a desktop shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Desktop {
    Gnome,
    Kde,
    Sway,
    X11,
    Hyprland,
}

impl Desktop {
    fn name(self) -> &'static str {
        match self {
            Desktop::Gnome => "GNOME",
            Desktop::Kde => "KDE Plasma 6",
            Desktop::Sway => "sway",
            Desktop::X11 => "generic X11",
            Desktop::Hyprland => "Hyprland",
        }
    }

    /// The tool the research records for this desktop. It is in every refusal the
    /// desktop can produce, the one where the tool is missing included, because a
    /// message that does not name the command it needed cannot be acted on.
    fn tool(self) -> &'static str {
        match self {
            Desktop::Gnome => "gsettings",
            Desktop::Kde => "plasma-apply-wallpaperimage",
            Desktop::Sway => "swaymsg",
            Desktop::X11 => "feh",
            Desktop::Hyprland => "hyprctl",
        }
    }

    /// The commands docs/research/linux.md records for this desktop, in the order
    /// they are tried. For every desktop but generic X11 the list is the steps of
    /// one set (GNOME writes two keys). For generic X11 it is three alternatives,
    /// not three steps: the research says any one of the three tools is
    /// sufficient, so `set` takes the first that is installed.
    fn calls(self, path: &str) -> Vec<ToolCall> {
        let schema = "org.gnome.desktop.background";
        match self {
            Desktop::Gnome => {
                // One URI, two keys: the shell picks between them from
                // `color-scheme`, so writing one leaves a dark-mode user on the
                // previous picture (docs/research/linux.md, GNOME 1).
                let uri = file_uri(path);
                vec![
                    call(self.tool(), &["set", schema, "picture-uri", &uri]),
                    call(self.tool(), &["set", schema, "picture-uri-dark", &uri]),
                ]
            }
            Desktop::Kde => vec![call(
                self.tool(),
                &["--fill-mode", "preserveAspectCrop", path],
            )],
            Desktop::Sway => vec![call(self.tool(), &["output", "*", "bg", path, "fill"])],
            Desktop::X11 => vec![
                call("feh", &["--bg-fill", path]),
                call("xwallpaper", &["--zoom", path]),
                call("hsetroot", &["-fill", path]),
            ],
            // Never run: `set` refuses Hyprland before this is reached, because a
            // one-shot setter cannot serve it. The call is here so the tool this
            // desktop would need is one the code names rather than a string in a
            // message.
            Desktop::Hyprland => vec![call(
                self.tool(),
                &["hyprpaper", "wallpaper", &format!(", {path}, cover")],
            )],
        }
    }
}

/// Choose the adapter from the signals, or `None` when none of them names a
/// desktop this backend serves. The second field is the evidence the refusal
/// repeats: which signal was read and what it held.
///
/// The order is the research's, most unambiguous first. A compositor's own socket
/// outranks `XDG_CURRENT_DESKTOP`, which a login manager sets and a user can set
/// by accident; the sockets cannot be set by accident.
fn detect(signals: &Signals) -> Option<(Desktop, String)> {
    if let Some(signature) = &signals.hyprland_signature {
        return Some((
            Desktop::Hyprland,
            format!("HYPRLAND_INSTANCE_SIGNATURE={signature}"),
        ));
    }
    if let Some(socket) = &signals.sway_sock {
        return Some((Desktop::Sway, format!("SWAYSOCK={socket}")));
    }
    if let Some(socket) = &signals.i3_sock {
        return Some((Desktop::Sway, format!("I3SOCK={socket}")));
    }
    if let Some(desktop) = signals.current_desktop.as_deref().and_then(named_desktop) {
        return Some((
            desktop,
            format!(
                "XDG_CURRENT_DESKTOP={}",
                signals.current_desktop.as_deref().unwrap_or_default()
            ),
        ));
    }
    // Bare X11: an X session and no desktop shell in `XDG_CURRENT_DESKTOP`. A
    // named desktop that this build does not serve is deliberately not this case,
    // because on those the desktop shell owns the background and the X root
    // window the three tools paint is covered up (docs/research/linux.md,
    // Generic X11).
    if signals.session_type.as_deref() == Some("x11") && signals.current_desktop.is_none() {
        return Some((
            Desktop::X11,
            "XDG_SESSION_TYPE=x11 with no desktop in XDG_CURRENT_DESKTOP".to_string(),
        ));
    }
    None
}

/// `XDG_CURRENT_DESKTOP` matched as the colon-separated list it is, never by
/// equality: `ubuntu:GNOME` and `KDE` are both real values. The match is
/// case-insensitive because the field is not standardised.
fn named_desktop(value: &str) -> Option<Desktop> {
    for component in value.split(':') {
        let component = component.trim();
        if component.eq_ignore_ascii_case("GNOME") {
            return Some(Desktop::Gnome);
        }
        if component.eq_ignore_ascii_case("KDE") || component.eq_ignore_ascii_case("plasma") {
            return Some(Desktop::Kde);
        }
        if component.eq_ignore_ascii_case("Hyprland") {
            return Some(Desktop::Hyprland);
        }
    }
    None
}

/// One documented command: the desktop's own tool and its arguments, in the
/// order the research gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCall {
    tool: &'static str,
    args: Vec<String>,
}

/// A [`ToolCall`] from literals, so the call sites read like the research's
/// command lines.
fn call(tool: &'static str, args: &[&str]) -> ToolCall {
    ToolCall {
        tool,
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
    }
}

impl ToolCall {
    /// The command as it would be typed, for a refusal to carry.
    fn line(&self) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(self.tool.to_string());
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// The `file://` URI `gsettings` wants for a path. The schema accepts only local
/// `file://` URIs, and a path with a space, a `#` or a non-ASCII byte has to be
/// escaped the way `g_filename_to_uri` escapes it: everything outside
/// `A-Z a-z 0-9 - . _ ~ /` becomes `%XX`. The path is absolute by the time this
/// is called.
fn file_uri(path: &str) -> String {
    let mut uri = String::with_capacity(path.len() + 7);
    uri.push_str("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// Why a command did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RunFailure {
    /// The tool is not on the `PATH` this session carries.
    Missing,
    /// The command was run and refused, or could not be spawned for a reason
    /// other than absence. The sentence says which and carries what it said.
    Refused(String),
}

/// Run one command in full, with stdout and stderr captured.
fn run(session: &Session, command: &ToolCall) -> Result<Output, RunFailure> {
    let Some(program) = session.resolve(command.tool) else {
        return Err(RunFailure::Missing);
    };
    let output = Command::new(&program).args(&command.args).output();
    match output {
        Err(error) => Err(RunFailure::Refused(format!(
            "`{}` could not be run: {error}",
            command.line()
        ))),
        Ok(output) if !output.status.success() => {
            let mut reason = format!("`{}` {}", command.line(), exit_status(&output.status));
            let said = said(&output);
            if !said.is_empty() {
                reason.push_str(&format!(": {said}"));
            }
            Err(RunFailure::Refused(reason))
        }
        Ok(output) => Ok(output),
    }
}

/// The refusal fragment one failed command contributes: the tool and the `PATH`
/// searched when it was not there, or the command and what it said when it ran
/// and refused.
fn describe_failure(session: &Session, command: &ToolCall, failure: &RunFailure) -> String {
    match failure {
        RunFailure::Missing => format!(
            "`{}` is not on the worker's PATH ({})",
            command.tool,
            session
                .path
                .as_deref()
                .map(|path| format!("PATH={path}"))
                .unwrap_or_else(|| "PATH is unset".to_string())
        ),
        RunFailure::Refused(reason) => reason.clone(),
    }
}

/// `Exited 1`, or a signal's name rather than a plausible code: `ExitStatus` has
/// no number when the process was killed.
fn exit_status(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exited {code}"),
        None => "was killed by a signal".to_string(),
    }
}

/// What a tool said when it refused: its last non-empty stderr line, or its
/// stdout when stderr is empty, or nothing. One line, because the worker's
/// stderr is the daemon's log line and the `ERR` line after it is one line
/// (docs/architecture.md 2.7).
fn said(output: &Output) -> String {
    let stderr = last_line(&output.stderr);
    if !stderr.is_empty() {
        return stderr;
    }
    last_line(&output.stdout)
}

fn last_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// A refused set: always `set_failed`, because nothing was set
/// (docs/architecture.md 2.7).
fn refused(reason: String) -> SetError {
    SetError::new(ErrorCode::SetFailed, reason)
}

/// A refused readback: `internal`, not `set_failed`. A readback that cannot be
/// taken refuses nothing, and docs/architecture.md 1.7.3 step 3 handles the `Err`
/// as an unverified anchor rather than as a wallpaper.
fn readback_failed(reason: String) -> SetError {
    SetError::new(ErrorCode::Internal, reason)
}

/// Put `path` on the desktop through the tool this session's desktop needs.
///
/// One command per call, or two for GNOME's two keys, and success only when every
/// command exited 0. A set that reached the first key and failed on the second is
/// a failure, not a success, because the caller asked for one image everywhere and
/// must not be told it got it.
pub fn set(path: &str) -> Result<(), SetError> {
    set_in(&Session::from_env(), path)
}

fn set_in(session: &Session, path: &str) -> Result<(), SetError> {
    if !path.starts_with('/') {
        return Err(refused(format!(
            "the Linux setter needs an absolute path and {path:?} is not one: \
             `gsettings` wants a file:// URI and the X11 tools resolve a relative \
             path against this process's working directory"
        )));
    }
    if path.contains('\0') {
        return Err(refused(format!(
            "the Linux setter cannot set {path:?}: the path holds a NUL byte"
        )));
    }

    let Some((desktop, evidence)) = detect(&session.signals) else {
        return Err(refused(format!(
            "the Linux setter detected no desktop it serves: {}. It sets the desktop \
             through gsettings (GNOME), plasma-apply-wallpaperimage (KDE), swaymsg \
             (sway) or feh/xwallpaper/hsetroot (bare X11), so {path} was not set",
            no_desktop(&session.signals)
        )));
    };
    let session_name = format!("a {} session ({evidence})", desktop.name());

    if desktop == Desktop::Hyprland {
        return Err(refused(format!(
            "the Linux setter detected {session_name} and refuses it by name: \
             docs/research/linux.md records Hyprland as the one environment a \
             one-shot setter cannot serve, because `hyprctl hyprpaper wallpaper` \
             needs `hyprpaper` already running and whirl does not start, own or \
             supervise it, so {path} was not set"
        )));
    }

    let calls = desktop.calls(path);
    if desktop == Desktop::X11 {
        return set_generic_x11(session, &session_name, &calls, path);
    }
    for command in &calls {
        if let Err(failure) = run(session, command) {
            return Err(refused(format!(
                "the Linux setter detected {session_name} and {}, so {path} was not set",
                describe_failure(session, command, &failure)
            )));
        }
    }
    Ok(())
}

/// The generic X11 case: three tools, any one of which is sufficient. The first
/// that is installed is the one used, and a tool that exists and refuses is a
/// failure rather than a reason to try the next, because the next paints the same
/// X root window and would refuse for the same reason.
fn set_generic_x11(
    session: &Session,
    session_name: &str,
    calls: &[ToolCall],
    path: &str,
) -> Result<(), SetError> {
    let mut missing = Vec::new();
    for command in calls {
        match run(session, command) {
            Ok(_) => return Ok(()),
            Err(RunFailure::Missing) => missing.push(command),
            Err(failure) => {
                return Err(refused(format!(
                    "the Linux setter detected {session_name} and {}, so {path} was not set",
                    describe_failure(session, command, &failure)
                )));
            }
        }
    }
    let named = missing
        .iter()
        .map(|command| format!("`{}`", command.tool))
        .collect::<Vec<_>>()
        .join(", ");
    Err(refused(format!(
        "the Linux setter detected {session_name} and none of {named} is on the \
         worker's PATH ({}), so {path} was not set",
        session
            .path
            .as_deref()
            .map(|path| format!("PATH={path}"))
            .unwrap_or_else(|| "PATH is unset".to_string())
    )))
}

/// The sentence a session with no recognised desktop gets: every signal that was
/// read, and the one place the research records that an environment cannot be
/// served at all.
fn no_desktop(signals: &Signals) -> String {
    let mut sentence = signals.line();
    if signals.session_type.as_deref() == Some("wayland") {
        sentence.push_str(
            "; the generic X11 tools are refused under Wayland, where they would paint \
             an X server's root window that the compositor does not draw",
        );
    }
    sentence
}

/// What `gsettings get org.gnome.desktop.background picture-uri` answered.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    /// A local `file://` URI, decoded back to the path it names.
    Image(String),
    /// The empty string: the key is set to nothing, which is a verified "no
    /// image" and the one case that answers `Ok(None)`.
    Empty,
    /// Anything else, as it came: not something this readback can call an image
    /// or a verified absence, so it is refused rather than guessed at.
    Other(String),
}

/// The image the platform says is on the desktop, on the same boundary as
/// [`set`]: the desktop's own answer, not a parse of its store.
///
/// `Err` is "the platform could not be asked", never `Ok(None)`: `Ok(None)` is a
/// verified "no image", and an unverified anchor is a different thing
/// (docs/architecture.md 1.7.3 step 3). Of the environments the research records,
/// only GNOME has a key a setter can read back; the others answer `Err` with the
/// name of the desktop and the reason.
#[allow(dead_code)]
pub fn current() -> Result<Option<String>, SetError> {
    current_in(&Session::from_env())
}

#[allow(dead_code)]
fn current_in(session: &Session) -> Result<Option<String>, SetError> {
    let Some((desktop, evidence)) = detect(&session.signals) else {
        return Err(readback_failed(format!(
            "the Linux readback detected no desktop it serves: {}, so the platform \
             cannot be asked what is on the desktop",
            no_desktop(&session.signals)
        )));
    };
    let session_name = format!("a {} session ({evidence})", desktop.name());

    if desktop != Desktop::Gnome {
        return Err(readback_failed(format!(
            "the Linux readback detected {session_name}, which docs/research/linux.md \
             records no readable key for, so the platform cannot be asked what is on \
             the desktop"
        )));
    }

    let schema = "org.gnome.desktop.background";
    let command = call("gsettings", &["get", schema, "picture-uri"]);
    let output = run(session, &command).map_err(|failure| {
        readback_failed(format!(
            "the Linux readback detected {session_name} and {}, so the anchor cannot be \
             verified here",
            describe_failure(session, &command, &failure)
        ))
    })?;

    let answer = String::from_utf8_lossy(&output.stdout);
    match read_answer(&answer) {
        Answer::Image(path) => Ok(Some(path)),
        Answer::Empty => Ok(None),
        Answer::Other(value) => Err(readback_failed(format!(
            "the Linux readback detected {session_name} and `{}` answered {value:?}, \
             which is not a file:// URI, so the anchor cannot be verified here",
            command.line()
        ))),
    }
}

/// Read `gsettings get`'s answer: a GVariant string, so a single-quoted value.
#[allow(dead_code)]
fn read_answer(stdout: &str) -> Answer {
    let value = stdout.trim();
    let value = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(value);
    if value.is_empty() {
        return Answer::Empty;
    }
    match value.strip_prefix("file://") {
        Some(path) if path.starts_with('/') => Answer::Image(percent_decode(path)),
        _ => Answer::Other(value.to_string()),
    }
}

/// `%XX` escapes decoded, which is how a path with a space or a non-ASCII byte
/// comes back: the schema takes a URI, and `g_filename_to_uri` escapes those.
/// A `%` that is not followed by two hex digits is left alone, because guessing
/// there would turn a literal `%` into a different path.
#[allow(dead_code)]
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[allow(dead_code)]
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// A session with signals and no `PATH`, for the tests that only ask which
    /// tool would be chosen.
    fn session(signals: Signals) -> Session {
        Session {
            signals,
            path: None,
        }
    }

    fn gnome_signals() -> Signals {
        Signals {
            current_desktop: Some("ubuntu:GNOME".to_string()),
            ..Signals::default()
        }
    }

    /// A temporary directory of stub executables, and a session whose `PATH` is
    /// that directory. The stubs are the only way to exercise a spawn without a
    /// desktop, and they live under the system temp directory rather than in the
    /// repository, so nothing a desktop's own tool would answer is in the tree.
    struct Stubs {
        dir: PathBuf,
        signals: Signals,
    }

    impl Stubs {
        fn new(name: &str, signals: Signals) -> Stubs {
            let dir =
                std::env::temp_dir().join(format!("whirl-linux-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("a temp directory for the stubs");
            Stubs { dir, signals }
        }

        /// Install one executable stub: `body` is the whole script, `#!` line
        /// included. A separate statement rather than a builder: the session
        /// borrows this struct, so the stubs have to outlive the call.
        fn put(&self, tool: &str, body: &str) {
            let path = self.dir.join(tool);
            fs::write(&path, body).expect("the stub script");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("the stub is executable");
        }

        fn session(&self) -> Session {
            Session {
                signals: self.signals.clone(),
                path: Some(self.dir.display().to_string()),
            }
        }
    }

    impl Drop for Stubs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    // --- detection: the session signal picks the tool -----------------------

    #[test]
    fn a_gnome_session_selects_gsettings() {
        let signals = gnome_signals();
        let (desktop, evidence) = detect(&signals).expect("a GNOME session is detected");
        assert_eq!(desktop, Desktop::Gnome);
        assert_eq!(desktop.tool(), "gsettings");
        assert!(evidence.contains("ubuntu:GNOME"), "{evidence}");
        assert_eq!(desktop.calls("/tmp/x.jpg")[0].tool, "gsettings");
    }

    #[test]
    fn a_colon_separated_desktop_list_is_matched_component_by_component() {
        for value in ["ubuntu:GNOME", "GNOME", "gnome:GNOME-Flashback"] {
            let signals = Signals {
                current_desktop: Some(value.to_string()),
                ..Signals::default()
            };
            assert_eq!(
                detect(&signals).map(|(desktop, _)| desktop),
                Some(Desktop::Gnome),
                "XDG_CURRENT_DESKTOP={value}"
            );
        }
    }

    #[test]
    fn a_kde_session_selects_plasma_apply_wallpaperimage() {
        let signals = Signals {
            current_desktop: Some("KDE".to_string()),
            ..Signals::default()
        };
        let (desktop, _) = detect(&signals).expect("a KDE session is detected");
        assert_eq!(desktop, Desktop::Kde);
        assert_eq!(desktop.tool(), "plasma-apply-wallpaperimage");
        assert_eq!(
            desktop.calls("/tmp/x.jpg")[0].tool,
            "plasma-apply-wallpaperimage"
        );
    }

    #[test]
    fn a_sway_or_i3_socket_selects_swaymsg() {
        for (name, signals) in [
            (
                "SWAYSOCK",
                Signals {
                    sway_sock: Some("/run/user/1000/sway-ipc.1000.1.sock".to_string()),
                    ..Signals::default()
                },
            ),
            (
                "I3SOCK",
                Signals {
                    i3_sock: Some("/run/user/1000/i3/ipc-socket.1".to_string()),
                    ..Signals::default()
                },
            ),
        ] {
            let (desktop, evidence) = detect(&signals).expect("a sway session is detected");
            assert_eq!(desktop, Desktop::Sway, "{name}");
            assert_eq!(desktop.tool(), "swaymsg");
            assert!(evidence.contains(name), "{evidence}");
            assert_eq!(desktop.calls("/tmp/x.jpg")[0].tool, "swaymsg");
        }
    }

    #[test]
    fn a_bare_x11_session_selects_feh() {
        let signals = Signals {
            session_type: Some("x11".to_string()),
            ..Signals::default()
        };
        let (desktop, evidence) = detect(&signals).expect("a bare X11 session is detected");
        assert_eq!(desktop, Desktop::X11);
        assert_eq!(desktop.tool(), "feh");
        assert!(evidence.contains("x11"), "{evidence}");
        let tools: Vec<&str> = desktop
            .calls("/tmp/x.jpg")
            .iter()
            .map(|command| command.tool)
            .collect();
        assert_eq!(tools, ["feh", "xwallpaper", "hsetroot"]);
    }

    #[test]
    fn a_hyprland_session_selects_hyprctl_and_is_refused_by_name() {
        let signals = Signals {
            hyprland_signature: Some("abc123".to_string()),
            ..Signals::default()
        };
        let (desktop, evidence) = detect(&signals).expect("a Hyprland session is detected");
        assert_eq!(desktop, Desktop::Hyprland);
        assert_eq!(desktop.tool(), "hyprctl");
        assert!(evidence.contains("abc123"), "{evidence}");

        let stubs = Stubs::new("hyprland", signals);
        let error = set_in(&stubs.session(), "/tmp/x.jpg").expect_err("Hyprland is refused");
        assert_eq!(error.code, ErrorCode::SetFailed);
        for needle in ["Hyprland", "hyprctl", "hyprpaper", "/tmp/x.jpg"] {
            assert!(
                error.message.contains(needle),
                "the refusal names {needle:?}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_session_that_matches_no_desktop_refuses_naming_what_it_looked_for() {
        let signals = Signals {
            current_desktop: Some("XFCE".to_string()),
            session_type: Some("x11".to_string()),
            ..Signals::default()
        };
        assert_eq!(detect(&signals), None);

        let error = set_in(&session(signals), "/tmp/x.jpg")
            .expect_err("a desktop this build does not serve is refused");
        assert_eq!(error.code, ErrorCode::SetFailed);
        for needle in [
            "XDG_CURRENT_DESKTOP=XFCE",
            "XDG_SESSION_TYPE=x11",
            "SWAYSOCK=<unset>",
            "I3SOCK=<unset>",
            "HYPRLAND_INSTANCE_SIGNATURE=<unset>",
            "/tmp/x.jpg",
        ] {
            assert!(
                error.message.contains(needle),
                "the refusal names {needle:?}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_wayland_session_with_no_desktop_refuses_the_x11_tools() {
        let signals = Signals {
            session_type: Some("wayland".to_string()),
            ..Signals::default()
        };
        assert_eq!(detect(&signals), None);
        let error = set_in(&session(signals), "/tmp/x.jpg").expect_err("no desktop is served");
        assert!(
            error.message.contains("Wayland"),
            "the refusal says why the X11 tools are not used: {}",
            error.message
        );
    }

    // --- the commands the research records ----------------------------------

    #[test]
    fn the_gnome_adapter_writes_both_the_light_and_dark_keys() {
        let calls = Desktop::Gnome.calls("/tmp/whirl test.jpg");
        assert_eq!(calls.len(), 2, "both keys: {calls:?}");
        assert_eq!(
            calls[0].line(),
            "gsettings set org.gnome.desktop.background picture-uri file:///tmp/whirl%20test.jpg"
        );
        assert_eq!(
            calls[1].line(),
            "gsettings set org.gnome.desktop.background picture-uri-dark file:///tmp/whirl%20test.jpg"
        );
    }

    #[test]
    fn the_sway_and_kde_calls_are_the_ones_the_research_documents() {
        assert_eq!(
            Desktop::Sway.calls("/tmp/x.jpg")[0].line(),
            "swaymsg output * bg /tmp/x.jpg fill"
        );
        assert_eq!(
            Desktop::Kde.calls("/tmp/x.jpg")[0].line(),
            "plasma-apply-wallpaperimage --fill-mode preserveAspectCrop /tmp/x.jpg"
        );
    }

    #[test]
    fn a_file_uri_escapes_what_gsettings_escapes_and_leaves_the_rest() {
        assert_eq!(file_uri("/tmp/x.jpg"), "file:///tmp/x.jpg");
        assert_eq!(file_uri("/a b#c.jpg"), "file:///a%20b%23c.jpg");
        assert_eq!(file_uri("/naïve.jpg"), "file:///na%C3%AFve.jpg");
    }

    // --- refusals: missing, refused, unanswerable ---------------------------

    #[test]
    fn a_missing_binary_is_named() {
        let stubs = Stubs::new("missing-binary", gnome_signals());
        let error =
            set_in(&stubs.session(), "/tmp/x.jpg").expect_err("a missing tool is a refusal");
        assert_eq!(error.code, ErrorCode::SetFailed);
        assert!(
            error.message.contains("gsettings"),
            "the refusal names the tool: {}",
            error.message
        );
        assert!(
            error.message.contains("PATH"),
            "the refusal says where it looked: {}",
            error.message
        );
        assert!(
            error.message.contains("/tmp/x.jpg"),
            "the refusal names the path: {}",
            error.message
        );
    }

    #[test]
    fn a_missing_schema_is_named() {
        let stubs = Stubs::new("missing-schema", gnome_signals());
        stubs.put(
            "gsettings",
            "#!/bin/sh\n\
             echo 'No such schema \"org.gnome.desktop.background\"' >&2\n\
             exit 1\n",
        );
        let error =
            set_in(&stubs.session(), "/tmp/x.jpg").expect_err("a refusing tool is a refusal");
        assert_eq!(error.code, ErrorCode::SetFailed);
        for needle in [
            "gsettings",
            "No such schema",
            "org.gnome.desktop.background",
            "/tmp/x.jpg",
        ] {
            assert!(
                error.message.contains(needle),
                "the refusal carries {needle:?}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_tool_that_exists_and_refuses_is_not_retried_with_the_next_x11_tool() {
        let stubs = Stubs::new(
            "x11-refusal",
            Signals {
                session_type: Some("x11".to_string()),
                ..Signals::default()
            },
        );
        stubs.put("feh", "#!/bin/sh\necho 'feh: no X display' >&2\nexit 1\n");
        stubs.put("xwallpaper", "#!/bin/sh\nexit 0\n");
        let error = set_in(&stubs.session(), "/tmp/x.jpg").expect_err("feh refused");
        assert_eq!(error.code, ErrorCode::SetFailed);
        assert!(
            error.message.contains("feh") && error.message.contains("no X display"),
            "the refusal names feh and what it said: {}",
            error.message
        );
        assert!(
            !error.message.contains("xwallpaper"),
            "the next tool was not tried after one existed and refused: {}",
            error.message
        );
    }

    #[test]
    fn every_x11_tool_missing_is_named() {
        let stubs = Stubs::new(
            "x11-none",
            Signals {
                session_type: Some("x11".to_string()),
                ..Signals::default()
            },
        );
        let error = set_in(&stubs.session(), "/tmp/x.jpg").expect_err("no X11 tool is installed");
        for needle in ["feh", "xwallpaper", "hsetroot", "/tmp/x.jpg"] {
            assert!(
                error.message.contains(needle),
                "the refusal names {needle:?}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_set_that_the_tool_accepts_is_a_success() {
        let stubs = Stubs::new("gnome-set", gnome_signals());
        stubs.put("gsettings", "#!/bin/sh\nexit 0\n");
        set_in(&stubs.session(), "/tmp/x.jpg").expect("the tool accepted the set");
    }

    #[test]
    fn an_unparsable_answer_is_refused_by_name_not_guessed_at() {
        let stubs = Stubs::new("unparsable", gnome_signals());
        stubs.put("gsettings", "#!/bin/sh\necho 'not a uri'\nexit 0\n");
        let error =
            current_in(&stubs.session()).expect_err("an answer that is not a file URI is refused");
        assert_eq!(error.code, ErrorCode::Internal);
        for needle in ["gsettings", "not a uri"] {
            assert!(
                error.message.contains(needle),
                "the refusal carries {needle:?}: {}",
                error.message
            );
        }
    }

    // --- the readback -------------------------------------------------------

    #[test]
    fn a_file_uri_is_answered_as_a_path() {
        let stubs = Stubs::new("readback", gnome_signals());
        stubs.put(
            "gsettings",
            "#!/bin/sh\necho \"'file:///tmp/Whirl%20Test.jpg'\"\n",
        );
        assert_eq!(
            current_in(&stubs.session()).expect("the key answers"),
            Some("/tmp/Whirl Test.jpg".to_string())
        );
    }

    #[test]
    fn the_empty_key_is_a_verified_no_image() {
        let stubs = Stubs::new("empty-key", gnome_signals());
        stubs.put("gsettings", "#!/bin/sh\necho \"''\"\n");
        assert_eq!(current_in(&stubs.session()).expect("the key answers"), None);
    }

    #[test]
    fn a_desktop_with_no_readable_key_is_err_and_not_ok_none() {
        let signals = Signals {
            sway_sock: Some("/run/user/1000/sway-ipc.1000.1.sock".to_string()),
            ..Signals::default()
        };
        let error = current_in(&session(signals))
            .expect_err("a desktop with no readable key cannot be asked");
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(
            error.message.contains("sway"),
            "the refusal names the desktop: {}",
            error.message
        );
    }

    #[test]
    fn a_readback_on_a_missing_tool_is_named_too() {
        let stubs = Stubs::new("readback-missing", gnome_signals());
        let error =
            current_in(&stubs.session()).expect_err("no gsettings means the key cannot be read");
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(
            error.message.contains("gsettings"),
            "the refusal names the tool: {}",
            error.message
        );
    }
}
