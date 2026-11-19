//! The port to the engine that renders widget pages.
//!
//! Like [`Backend`](crate::Backend), every method only queues a command and returns at once.
//! What the engine produces later (frames, messages from the page, failures) arrives as
//! [`WidgetHostEvent`]s, so the core never waits on the engine.

use std::sync::Arc;

use thiserror::Error;

use crate::widget_bridge::WidgetTheme;

/// Names one widget view across the host's commands and events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewId(pub u64);

/// The page a widget view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetPage {
    /// The HTML document.
    pub html: String,
    /// The content security policy the page runs under.
    pub csp: String,
    /// The color scheme the page follows.
    pub theme: WidgetTheme,
}

/// A pointer event to forward to a widget view. Widgets are read-only, so there is no keyboard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WidgetInput {
    /// The pointer moved.
    Move {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
    },
    /// A button went down.
    Press {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// The button number.
        button: u8,
    },
    /// A button went up.
    Release {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// The button number.
        button: u8,
    },
    /// The wheel turned.
    Wheel {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// Horizontal scroll amount.
        dx: f32,
        /// Vertical scroll amount.
        dy: f32,
    },
    /// The pointer left the view.
    Leave,
}

/// Something the engine reports about a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetHostEvent {
    /// A new picture of the view.
    Frame {
        /// The view it belongs to.
        view: ViewId,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// The pixels, four bytes each.
        rgba: Arc<Vec<u8>>,
    },
    /// The page sent a message.
    Message {
        /// The view it came from.
        view: ViewId,
        /// The message, as JSON text.
        json: String,
    },
    /// The view could not be shown or broke.
    Failed {
        /// The view that failed.
        view: ViewId,
        /// What went wrong.
        error: String,
    },
}

/// Why a [`WidgetHost`] refused a command.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum WidgetHostError {
    /// There is no engine to run widgets.
    #[error("widget host unavailable: {0}")]
    Unavailable(String),
    /// The command could not be handed to the engine.
    #[error("sending to the widget host failed: {0}")]
    Send(String),
}

/// The command port to the widget engine. Every method queues and returns at once.
pub trait WidgetHost: Send {
    /// Opens `view` on `page`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError>;

    /// Delivers a JSON message to the page of `view`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError>;

    /// Resizes `view` to `width` by `height` pixels at the device pixel ratio `scale`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn resize(
        &self,
        view: ViewId,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<(), WidgetHostError>;

    /// Shows or hides `view`; a hidden view stops producing frames.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError>;

    /// Forwards a pointer event to `view`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError>;

    /// Closes `view` and frees what it held.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn close(&self, view: ViewId) -> Result<(), WidgetHostError>;
}

/// The [`WidgetHost`] used when the engine does not start: every `open` is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoWidgetHost;

impl WidgetHost for NoWidgetHost {
    fn open(&self, _view: ViewId, _page: WidgetPage) -> Result<(), WidgetHostError> {
        Err(WidgetHostError::Unavailable(
            "widgets are not available".into(),
        ))
    }

    fn post(&self, _view: ViewId, _json: String) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn resize(
        &self,
        _view: ViewId,
        _width: u32,
        _height: u32,
        _scale: f32,
    ) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn set_visible(&self, _view: ViewId, _visible: bool) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn input(&self, _view: ViewId, _input: WidgetInput) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn close(&self, _view: ViewId) -> Result<(), WidgetHostError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::testing::{RecordingWidgetHost, WidgetCall};

    fn page() -> WidgetPage {
        WidgetPage {
            html: "<p>hi</p>".into(),
            csp: "default-src 'none'".into(),
            theme: WidgetTheme::Dark,
        }
    }

    #[test]
    fn no_widget_host_refuses_open() {
        let host = NoWidgetHost;
        assert_eq!(
            host.open(ViewId(1), page()),
            Err(WidgetHostError::Unavailable(
                "widgets are not available".into()
            ))
        );
        let view = ViewId(1);
        assert_eq!(host.post(view, "{}".into()), Ok(()));
        assert_eq!(host.resize(view, 1, 2, 1.0), Ok(()));
        assert_eq!(host.set_visible(view, true), Ok(()));
        assert_eq!(host.input(view, WidgetInput::Leave), Ok(()));
        assert_eq!(host.close(view), Ok(()));
    }

    #[test]
    fn recording_widget_host_records_in_order() {
        let host = RecordingWidgetHost::new();
        let view = ViewId(7);
        let input = WidgetInput::Press {
            x: 1.0,
            y: 2.0,
            button: 0,
        };
        host.open(view, page()).unwrap();
        host.post(view, "{\"a\":1}".into()).unwrap();
        host.resize(view, 320, 200, 2.0).unwrap();
        host.set_visible(view, false).unwrap();
        host.input(view, input).unwrap();
        host.close(view).unwrap();
        assert_eq!(
            host.calls(),
            vec![
                WidgetCall::Open { view, page: page() },
                WidgetCall::Post {
                    view,
                    json: "{\"a\":1}".into()
                },
                WidgetCall::Resize {
                    view,
                    width: 320,
                    height: 200,
                    scale: 2.0
                },
                WidgetCall::SetVisible {
                    view,
                    visible: false
                },
                WidgetCall::Input { view, input },
                WidgetCall::Close { view },
            ]
        );
    }

    #[test]
    fn recording_widget_host_fails_on_request() {
        let host = RecordingWidgetHost::new();
        let view = ViewId(1);
        host.fail_next_call();
        assert_eq!(
            host.close(view),
            Err(WidgetHostError::Unavailable("recording widget host".into()))
        );
        assert_eq!(host.close(view), Ok(()));

        host.fail_next_call_with(WidgetHostError::Send("gone".into()));
        assert_eq!(host.close(view), Err(WidgetHostError::Send("gone".into())));
        assert_eq!(host.close(view), Ok(()));

        host.fail_call_after(2);
        assert_eq!(host.close(view), Ok(()));
        assert_eq!(host.close(view), Ok(()));
        assert!(host.close(view).is_err());
        assert_eq!(host.close(view), Ok(()));
        assert_eq!(host.calls().len(), 8);
    }

    #[test]
    fn recording_widget_host_clones_share_the_recording() {
        let host = RecordingWidgetHost::new();
        host.clone().close(ViewId(3)).unwrap();
        assert_eq!(host.calls(), vec![WidgetCall::Close { view: ViewId(3) }]);
    }
}
