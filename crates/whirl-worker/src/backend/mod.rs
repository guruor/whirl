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
