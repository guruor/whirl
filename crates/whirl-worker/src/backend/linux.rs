//! The Linux setter: `gsettings` for GNOME, `swaymsg` for sway, `hyprctl` for
//! Hyprland, chosen by the session signals docs/architecture.md 1.6 names.
//!
//! Not implemented, and it is the only thing behind the backend boundary
//! (docs/development.md section 7); detecting the environment is the part that
//! precedes it and belongs to the same file. Until then a `backend: native`
//! rotation fails with `set_failed`, which is the truth: nothing was set.

use super::SetError;
use whirl_core::protocol::ErrorCode;

pub fn set(_path: &str) -> Result<(), SetError> {
    Err(SetError::new(
        ErrorCode::SetFailed,
        "the Linux setter is not implemented yet; run with WHIRL_BACKEND=noop",
    ))
}

/// Fail loudly, like the setter above, and not with `Ok(None)`: `Ok(None)` would
/// claim the platform answered "no image", which is a verified answer this stub
/// did not get. `Err` is the honest "the platform could not be asked", which
/// docs/architecture.md 1.7.3 step 3 treats as an unverified anchor.
pub fn current() -> Result<Option<String>, SetError> {
    Err(SetError::new(
        ErrorCode::Internal,
        "the Linux readback is not implemented yet, so the anchor cannot be verified here",
    ))
}
