//! Renders MCP App widgets in windowless CEF browsers, inside the widget engine helper process.
//!
//! The helper (`partitur-widget-engine`) calls [`subprocess`] first, since CEF starts its own
//! helper processes (renderer, GPU, zygote and so on) by running the same executable again with a
//! `--type=` argument, and then [`serve`]. That runs CEF on the helper's main thread with CEF's
//! own message loop, the one model on every platform: a reader thread reads Partitur's commands
//! from standard input and posts each to CEF's UI thread as a task, and a writer thread owns
//! standard output, where the events and the painted frames go, in the `partitur_widgets`
//! protocol. When the input ends, the helper shuts CEF down, and it ends itself when CEF has not
//! done so within five seconds, so it never outlives Partitur.
//!
//! Each widget view is one offscreen browser on a sandbox proxy page, which holds the widget in a
//! frame sandboxed with `allow-scripts allow-same-origin`, as the MCP Apps specification asks. The
//! widget's document has an origin of its own, apart from the proxy's and from every other view's,
//! so it may use storage and cookies but reach neither.
//!
//! On Linux CEF runs in its namespace sandbox unless the helper is told otherwise, and the
//! helper probes the sandbox before it says hello; see the `engine` module. On Windows CEF runs
//! without the OS process sandbox, and says so in its log and its hello: cef-rs 156 sandboxes
//! Windows only through CEF's bootstrap executable, which loads the engine as a DLL, and that is
//! left for later. The widget isolation inside Chromium (origins, the frame sandbox, the request
//! and navigation rules) holds there all the same.
//!
//! The protocol keeps CEF's child processes out: on Linux it moves off file descriptor 1 before
//! CEF starts (see the `serve` module).
//!
//! On Linux and Windows the work is done by the `engine`, `serve`, `view` and `scheme` modules.
//! macOS is not supported yet: CEF there needs an `NSApplication` that speaks CEF's protocol and
//! separate helper app bundles. So on macOS the `engine` module is a stub
//! (`engine_unsupported.rs`): [`serve`] logs why and fails at once without touching CEF, and
//! [`subprocess`] never claims the process.

#![cfg_attr(
    not(target_os = "macos"),
    expect(
        clippy::transmute_ptr_to_ptr,
        reason = "the expansion of the cef crate's wrap_* macros"
    )
)]

#[cfg(not(target_os = "macos"))]
mod engine;
#[cfg(target_os = "macos")]
#[path = "engine_unsupported.rs"]
mod engine;
#[cfg(not(target_os = "macos"))]
mod scheme;
#[cfg(not(target_os = "macos"))]
mod serve;
#[cfg(not(target_os = "macos"))]
mod view;

use thiserror::Error;

/// Why CEF could not be set up.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CefError {
    /// CEF itself did not start.
    #[error("CEF did not start: {0}")]
    Initialize(String),
}

/// Runs this process as one of CEF's helper processes when it was started as one.
///
/// CEF starts its helpers by running the executable again with a `--type=` argument. For such a
/// process this does the work and returns the exit code; for any other process it returns `None`
/// at once.
pub fn subprocess() -> Option<i32> {
    engine::subprocess()
}

/// Runs the widget engine on this thread, which must be the process's main thread, until
/// Partitur shuts it down or its standard input ends, and returns the process's exit code: `0`,
/// `1` when CEF did not start, or `partitur_widgets::protocol::PROBE_FAILED` when the sandbox
/// probe failed. CEF runs in the OS process sandbox when `sandbox` holds, and on Linux the
/// sandbox is probed first when `probe` holds too.
pub fn serve(sandbox: bool, probe: bool) -> i32 {
    engine::serve(sandbox, probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_process_is_no_subprocess() {
        assert_eq!(subprocess(), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_refuses_to_serve() {
        assert_eq!(serve(true, true), 1);
    }
}
