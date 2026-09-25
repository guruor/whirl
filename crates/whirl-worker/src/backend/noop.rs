//! The noop backend: every stage of a rotation except the platform setter
//! (docs/development.md section 7). It is what `cargo test --workspace` and CI
//! run, so a test cannot set a real wallpaper by accident, and what a developer
//! runs all day while a real setter does not exist.

use super::SetError;

/// Succeed without touching the desktop. The rest of the pipeline has already
/// run: the caller reaches this only after it has a candidate and has printed
/// `downloaded:`.
pub fn set(_path: &str) -> Result<(), SetError> {
    Ok(())
}

/// Report nothing at all, and not the machine's real image either. The noop path
/// set nothing, so it has nothing it could truthfully report as an anchor, and
/// reading the live desktop here would make every noop test depend on the desk of
/// whoever ran it.
pub fn current() -> Result<Option<String>, SetError> {
    Ok(None)
}
