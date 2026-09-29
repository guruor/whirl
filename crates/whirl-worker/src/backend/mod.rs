//! The backend boundary: setting the platform's image is the only thing behind
//! it, so the noop path and the native path differ in one function
//! (docs/development.md section 7). Everything else in a rotation is
//! platform-free, which is what makes `WHIRL_BACKEND=noop` a real test path:
//! the whole pipeline runs, and exactly one stage is skipped.

use whirl_core::config::Backend;
use whirl_core::protocol::ErrorCode;

pub mod noop;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// A refused set: the code the daemon reports and the message it carries.
#[derive(Debug, Clone)]
pub struct SetError {
    pub code: ErrorCode,
    pub message: String,
}

impl SetError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> SetError {
        SetError {
            code,
            message: message.into(),
        }
    }
}

/// The one call the backend owns: put `path` on the platform's desktop.
pub fn set(backend: Backend, path: &str) -> Result<(), SetError> {
    match backend {
        Backend::Noop => noop::set(path),
        Backend::Native => native(path),
    }
}

/// The other call the backend owns, and readback rather than write: what the
/// platform says is on the desktop right now (docs/architecture.md 3.2 and
/// 1.7.3). It is a separate function and not a change to `set`, so `set` still
/// means exactly what it meant. `Ok(None)` is "the platform reports no image";
/// an `Err` is "the platform could not be asked", which 1.7.3 step 3 handles as
/// an unverified anchor rather than as a wallpaper.
///
/// Nothing in the worker calls this yet, hence `dead_code`: it exists now because
/// the by-hand proof that a real wallpaper changes needs it, and because
/// `status`'s `anchor_path` is the card after this one.
#[allow(dead_code)]
pub fn current(backend: Backend) -> Result<Option<String>, SetError> {
    match backend {
        Backend::Noop => noop::current(),
        Backend::Native => native_current(),
    }
}

#[cfg(target_os = "macos")]
fn native(path: &str) -> Result<(), SetError> {
    macos::set(path)
}

#[cfg(target_os = "linux")]
fn native(path: &str) -> Result<(), SetError> {
    linux::set(path)
}

#[cfg(target_os = "windows")]
fn native(path: &str) -> Result<(), SetError> {
    windows::set(path)
}

#[cfg(target_os = "macos")]
fn native_current() -> Result<Option<String>, SetError> {
    macos::current()
}

#[cfg(target_os = "linux")]
fn native_current() -> Result<Option<String>, SetError> {
    linux::current()
}

#[cfg(target_os = "windows")]
fn native_current() -> Result<Option<String>, SetError> {
    windows::current()
}

/// No platform setter exists yet, and there is nowhere else to put this: a
/// rotation with `backend: native` must fail loudly rather than report a set
/// that never happened.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn native(path: &str) -> Result<(), SetError> {
    let _ = path;
    Err(SetError::new(
        ErrorCode::SetFailed,
        "this platform has no whirl backend yet; run with WHIRL_BACKEND=noop",
    ))
}

/// The same for the readback: no platform is asked, so nothing is reported.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn native_current() -> Result<Option<String>, SetError> {
    Err(SetError::new(
        ErrorCode::Internal,
        "this platform has no whirl readback yet; the anchor cannot be verified here",
    ))
}
