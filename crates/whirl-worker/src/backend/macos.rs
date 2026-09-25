//! The macOS setter: `NSWorkspace.setDesktopImageURL:forScreen:options:error:`.
//!
//! Not implemented. This file is the single place it goes: the call is the only
//! thing behind the backend boundary (docs/development.md section 7), so when it
//! lands the noop path and the native path differ here and nowhere else.
//!
//! Until then a `backend: native` rotation fails with `set_failed`, which is the
//! truth: nothing was set.

use super::SetError;
use whirl_core::protocol::ErrorCode;

pub fn set(_path: &str) -> Result<(), SetError> {
    Err(SetError::new(
        ErrorCode::SetFailed,
        "the macOS setter is not implemented yet; run with WHIRL_BACKEND=noop",
    ))
}
