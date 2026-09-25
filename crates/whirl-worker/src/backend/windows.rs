//! The Windows setter: `SystemParametersInfoW(SPI_SETDESKWALLPAPER, ...)`.
//!
//! Not implemented, and it is the only thing behind the backend boundary
//! (docs/development.md section 7). Until then a `backend: native` rotation
//! fails with `set_failed`, which is the truth: nothing was set.

use super::SetError;
use whirl_core::protocol::ErrorCode;

pub fn set(_path: &str) -> Result<(), SetError> {
    Err(SetError::new(
        ErrorCode::SetFailed,
        "the Windows setter is not implemented yet; run with WHIRL_BACKEND=noop",
    ))
}
