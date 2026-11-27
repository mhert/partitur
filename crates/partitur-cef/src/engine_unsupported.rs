//! Stands in for the CEF engine on macOS, where CEF widgets are not supported yet.
//!
//! It offers the same items as the real `engine` module. [`serve`] logs why and fails at once
//! without touching CEF, and [`subprocess`] never claims the process.

/// Never claims the process: no CEF helper runs on macOS.
pub(crate) fn subprocess() -> Option<i32> {
    None
}

/// Fails at once with exit code 1, since CEF widgets are not supported on macOS yet.
pub(crate) fn serve(_sandbox: bool, _probe: bool) -> i32 {
    tracing::error!("CEF widgets are not supported on macOS yet");
    1
}
