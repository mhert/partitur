//! Hosts MCP App widgets in windowless CEF browsers, behind the core's [`WidgetHost`] port.
//!
//! [`start`] starts CEF and returns the host together with the receiver of its
//! [`WidgetHostEvent`]s; every [`WidgetHost`] method only queues a command for CEF's own thread.
//! Each widget view is one offscreen browser on a sandbox proxy page, which holds the widget in a
//! frame sandboxed with `allow-scripts allow-same-origin`, as the MCP Apps specification asks. The
//! widget's document has an origin of its own, apart from the proxy's and from every other view's,
//! so it may use storage and cookies but reach neither. The binary must call [`subprocess`]
//! before anything else, because CEF starts its helper processes by running Partitur's
//! executable again.
//!
//! On Windows CEF runs without the OS process sandbox, and says so with the warning the Linux
//! fallback gives (`CEF runs without the process sandbox: …`). cef-rs 156 sandboxes Windows only
//! through CEF's bootstrap: the shipped `partitur.exe` would have to be CEF's `bootstrap.exe`,
//! loading Partitur as a DLL through an exported `RunWinMain` that receives the sandbox
//! information. That export needs `#[unsafe(no_mangle)]` and an `unsafe extern "C"` function,
//! and the change from an executable to a DLL reaches the packaging, the MSI, the icon, the
//! signing and the `--job-run` launcher, so it is left for later. The widget isolation inside
//! Chromium (origins, the frame sandbox, the request and navigation rules) holds there all the
//! same.
//!
//! On Linux and Windows the work is done by the `engine`, `view` and `scheme` modules. macOS is
//! not supported yet: CEF there needs the process's main thread, an `NSApplication` that speaks
//! CEF's protocol and separate helper app bundles, and iced owns the main thread. So on macOS
//! the `engine` module is a stub (`engine_unsupported.rs`): [`start`] fails at once without
//! touching CEF, [`subprocess`] never claims the process, and widgets fall back to text.

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
mod view;

use futures::channel::mpsc::UnboundedReceiver;
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{
    ViewId, WidgetHost, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
};
use thiserror::Error;

use crate::engine::Engine;

/// Why [`start`] failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CefError {
    /// CEF itself did not start.
    #[error("CEF did not start: {0}")]
    Initialize(String),
    /// The thread CEF runs on could not be started or stopped early.
    #[error("the CEF thread failed: {0}")]
    Thread(String),
}

/// Runs this process as one of CEF's helper processes when it was started as one.
///
/// CEF starts its helpers (renderer, GPU, zygote and so on) by running the executable again with
/// a `--type=` argument, and [`start`] runs it once as a sandbox probe. For such a process this
/// does the work and returns the exit code; for any other process it returns `None` at once.
pub fn subprocess() -> Option<i32> {
    engine::subprocess()
}

/// Starts CEF and returns the host and the receiver of its events.
///
/// # Errors
///
/// Returns [`CefError::Initialize`] when CEF was started in this process before, does not start,
/// or is not supported here (macOS), and [`CefError::Thread`] when its thread cannot be started.
pub fn start() -> Result<(CefWidgetHost, UnboundedReceiver<WidgetHostEvent>), CefError> {
    engine::start().map(|(engine, events)| (CefWidgetHost { engine }, events))
}

/// The [`WidgetHost`] that renders widgets with CEF. See [`start`].
pub struct CefWidgetHost {
    engine: Engine,
}

impl CefWidgetHost {
    /// Whether CEF runs with the OS process sandbox.
    pub fn sandboxed(&self) -> bool {
        self.engine.sandboxed()
    }
}

impl WidgetHost for CefWidgetHost {
    fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError> {
        self.engine.open(view, page)
    }

    fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError> {
        self.engine.post(view, json)
    }

    fn resize(
        &self,
        view: ViewId,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<(), WidgetHostError> {
        self.engine.resize(view, width, height, scale)
    }

    fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError> {
        self.engine.set_visible(view, visible)
    }

    fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError> {
        self.engine.input(view, input)
    }

    fn set_theme(&self, view: ViewId, theme: WidgetTheme) -> Result<(), WidgetHostError> {
        self.engine.set_theme(view, theme)
    }

    fn close(&self, view: ViewId) -> Result<(), WidgetHostError> {
        self.engine.close(view)
    }
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
    fn macos_refuses_to_start() {
        assert_eq!(
            start().err(),
            Some(CefError::Initialize(
                "CEF widgets are not supported on macOS yet".into()
            ))
        );
    }
}
