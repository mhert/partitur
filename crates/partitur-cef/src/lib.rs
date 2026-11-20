//! Hosts MCP App widgets in windowless CEF browsers, behind the core's [`WidgetHost`] port.
//!
//! [`start`] starts CEF and returns the host together with the receiver of its
//! [`WidgetHostEvent`]s; every [`WidgetHost`] method only queues a command for CEF's own thread.
//! Each widget view is one offscreen browser on a sandbox proxy page, which holds the widget in a
//! sandboxed frame of its own origin. The binary must call [`subprocess`] before anything else,
//! because CEF starts its helper processes by running Partitur's executable again.
//!
//! On Linux and Windows the work is done by the `engine`, `view` and `scheme` modules. macOS is
//! not supported yet: CEF there needs the process's main thread, an `NSApplication` that speaks
//! CEF's protocol and separate helper app bundles, and iced owns the main thread. So on macOS
//! [`start`] fails at once without touching CEF, [`subprocess`] never claims the process, and
//! widgets fall back to text.

#![cfg_attr(
    not(target_os = "macos"),
    expect(
        clippy::transmute_ptr_to_ptr,
        reason = "the expansion of the cef crate's wrap_* macros"
    )
)]

#[cfg(not(target_os = "macos"))]
mod engine;
#[cfg(not(target_os = "macos"))]
mod scheme;
#[cfg(not(target_os = "macos"))]
mod view;

use futures::channel::mpsc::UnboundedReceiver;
use partitur_core::widget_host::{
    ViewId, WidgetHost, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
};
use thiserror::Error;

#[cfg(not(target_os = "macos"))]
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
    #[cfg(not(target_os = "macos"))]
    return engine::subprocess();
    #[cfg(target_os = "macos")]
    return None;
}

/// Starts CEF and returns the host and the receiver of its events.
///
/// # Errors
///
/// Returns [`CefError::Initialize`] when CEF was started in this process before, does not start,
/// or is not supported here (macOS), and [`CefError::Thread`] when its thread cannot be started.
pub fn start() -> Result<(CefWidgetHost, UnboundedReceiver<WidgetHostEvent>), CefError> {
    #[cfg(not(target_os = "macos"))]
    return engine::start().map(|(engine, events)| (CefWidgetHost { engine }, events));
    #[cfg(target_os = "macos")]
    return Err(CefError::Initialize(
        "CEF widgets are not supported on macOS yet".into(),
    ));
}

/// The [`WidgetHost`] that renders widgets with CEF. See [`start`].
pub struct CefWidgetHost {
    #[cfg(not(target_os = "macos"))]
    engine: Engine,
    #[cfg(target_os = "macos")]
    engine: std::convert::Infallible,
}

#[cfg(not(target_os = "macos"))]
impl CefWidgetHost {
    /// Whether CEF runs with the OS process sandbox.
    pub fn sandboxed(&self) -> bool {
        self.engine.sandboxed()
    }
}

#[cfg(not(target_os = "macos"))]
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

    fn close(&self, view: ViewId) -> Result<(), WidgetHostError> {
        self.engine.close(view)
    }
}

/// No host exists on macOS, where [`start`] always fails; this only keeps the type whole.
#[cfg(target_os = "macos")]
impl CefWidgetHost {
    /// Whether CEF runs with the OS process sandbox.
    pub fn sandboxed(&self) -> bool {
        match self.engine {}
    }
}

#[cfg(target_os = "macos")]
impl WidgetHost for CefWidgetHost {
    fn open(&self, _view: ViewId, _page: WidgetPage) -> Result<(), WidgetHostError> {
        match self.engine {}
    }

    fn post(&self, _view: ViewId, _json: String) -> Result<(), WidgetHostError> {
        match self.engine {}
    }

    fn resize(
        &self,
        _view: ViewId,
        _width: u32,
        _height: u32,
        _scale: f32,
    ) -> Result<(), WidgetHostError> {
        match self.engine {}
    }

    fn set_visible(&self, _view: ViewId, _visible: bool) -> Result<(), WidgetHostError> {
        match self.engine {}
    }

    fn input(&self, _view: ViewId, _input: WidgetInput) -> Result<(), WidgetHostError> {
        match self.engine {}
    }

    fn close(&self, _view: ViewId) -> Result<(), WidgetHostError> {
        match self.engine {}
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
