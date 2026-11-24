//! Stands in for the CEF engine on macOS, where CEF widgets are not supported yet.
//!
//! It offers the same items as the real `engine` module, so the crate keeps one
//! [`WidgetHost`](partitur_core::widget_host::WidgetHost) implementation for every platform.
//! [`start`] always fails without touching CEF, and [`subprocess`] never claims the process.
//! [`Engine`] has no values, so none of its methods can ever be called.

use futures::channel::mpsc::UnboundedReceiver;
use partitur_core::widget_host::{
    ViewId, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
};

use crate::CefError;

/// Never claims the process: no CEF helper runs on macOS.
pub(crate) fn subprocess() -> Option<i32> {
    None
}

/// Fails at once, since CEF widgets are not supported on macOS yet.
pub(crate) fn start() -> Result<(Engine, UnboundedReceiver<WidgetHostEvent>), CefError> {
    Err(CefError::Initialize(
        "CEF widgets are not supported on macOS yet".into(),
    ))
}

/// The engine that cannot exist on macOS: an enum without variants, so it has no values.
pub(crate) enum Engine {}

impl Engine {
    pub(crate) fn sandboxed(&self) -> bool {
        match *self {}
    }

    pub(crate) fn open(&self, _view: ViewId, _page: WidgetPage) -> Result<(), WidgetHostError> {
        match *self {}
    }

    pub(crate) fn post(&self, _view: ViewId, _json: String) -> Result<(), WidgetHostError> {
        match *self {}
    }

    pub(crate) fn resize(
        &self,
        _view: ViewId,
        _width: u32,
        _height: u32,
        _scale: f32,
    ) -> Result<(), WidgetHostError> {
        match *self {}
    }

    pub(crate) fn set_visible(&self, _view: ViewId, _visible: bool) -> Result<(), WidgetHostError> {
        match *self {}
    }

    pub(crate) fn input(&self, _view: ViewId, _input: WidgetInput) -> Result<(), WidgetHostError> {
        match *self {}
    }

    pub(crate) fn close(&self, _view: ViewId) -> Result<(), WidgetHostError> {
        match *self {}
    }
}
