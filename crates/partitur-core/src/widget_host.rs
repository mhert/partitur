//! The port to the engine that renders widget pages.
//!
//! Like [`Backend`](crate::Backend), every method only queues a command and returns at once.
//! What the engine produces later (frames, the pointer's look, messages from the page, links
//! the user followed in it, failures) arrives as [`WidgetHostEvent`]s, so the core never waits
//! on the engine.
//!
//! A page never navigates away from its own document. When the user follows a link in it, or
//! it opens a window on a click, the engine keeps the page where it is and reports the URL as
//! [`WidgetHostEvent::OpenLink`], for the UI to open in the system's browser.
//!
//! A page follows the color scheme it is given: it opens in its [`WidgetPage::theme`], and
//! [`WidgetHost::set_theme`] changes it later. The engine shows the theme to the page as its
//! `prefers-color-scheme` media feature, so a plain page that knows nothing of the MCP Apps
//! bridge follows the desktop all the same.
//!
//! An engine paints a view as often as its page changes, which may be faster than the UI takes
//! the pictures. So a view's pictures pass through one [`LatestFrame`]: the engine puts each
//! new picture there in place of one not taken yet, and sends a [`WidgetHostEvent::Frame`] only
//! when the slot was empty. However fast it paints, at most one frame event per view waits,
//! and taking the picture it announces yields the newest one.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
        /// The button number: 0 for the left button, 1 for the middle one, 2 for the right one.
        button: u8,
    },
    /// A button went up.
    Release {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// The button number: 0 for the left button, 1 for the middle one, 2 for the right one.
        button: u8,
    },
    /// The wheel turned.
    Wheel {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// Horizontal scroll amount in view pixels, positive to the left.
        dx: f32,
        /// Vertical scroll amount in view pixels, positive upwards.
        dy: f32,
        /// Whether the amounts are exact, as a touchpad gives them, rather than whole wheel
        /// notches, which the engine may animate.
        precise: bool,
    },
    /// The pointer left the view.
    Leave,
}

/// The pointer's look a page asks for over a view, as far as the UI can show it. Engines map
/// every look not named here to [`WidgetCursor::Default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WidgetCursor {
    /// The ordinary arrow.
    #[default]
    Default,
    /// The hand over a link or a button.
    Pointer,
    /// The I-beam over text.
    Text,
    /// An open hand over something that can be dragged.
    Grab,
    /// A closed hand while dragging.
    Grabbing,
    /// What may not be done here.
    NotAllowed,
    /// A sideways resize.
    ResizeHorizontal,
    /// An up-and-down resize.
    ResizeVertical,
    /// A crosshair.
    Crosshair,
}

/// One picture of a view, in RGBA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The pixels, four bytes each.
    pub rgba: Vec<u8>,
}

/// The newest picture of a view that the engine painted and nobody took yet; see the module
/// documentation. Clones share the one slot, and two slots are equal when they are the same.
#[derive(Debug, Clone, Default)]
pub struct LatestFrame(Arc<Mutex<Option<Picture>>>);

impl LatestFrame {
    /// Puts `picture` in the slot, in place of one not taken yet. Returns whether the slot was
    /// empty, which is when a [`WidgetHostEvent::Frame`] must announce it.
    pub fn put(&self, picture: Picture) -> bool {
        self.lock().replace(picture).is_none()
    }

    /// Takes the newest picture out of the slot, if one is there.
    pub fn take(&self) -> Option<Picture> {
        self.lock().take()
    }

    /// The slot, also after a thread panicked while holding it: a picture is whole either way.
    fn lock(&self) -> MutexGuard<'_, Option<Picture>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PartialEq for LatestFrame {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for LatestFrame {}

/// Something the engine reports about a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetHostEvent {
    /// The view has a new picture, to be taken from `latest`.
    Frame {
        /// The view it belongs to.
        view: ViewId,
        /// The view's slot, which holds the newest picture.
        latest: LatestFrame,
    },
    /// The page asks for another look of the pointer over the view.
    Cursor {
        /// The view it belongs to.
        view: ViewId,
        /// The look.
        cursor: WidgetCursor,
    },
    /// The page sent a message.
    Message {
        /// The view it came from.
        view: ViewId,
        /// The message, as JSON text.
        json: String,
    },
    /// The user followed a link in the page, or the page opened a window on a click, and the
    /// page stayed where it was: the URL is for the system to open.
    OpenLink {
        /// The view it came from.
        view: ViewId,
        /// The URL, as the page named it.
        url: String,
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

    /// Lets the page of `view` follow `theme` from now on, as its `prefers-color-scheme`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn set_theme(&self, view: ViewId, theme: WidgetTheme) -> Result<(), WidgetHostError>;

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

    fn set_theme(&self, _view: ViewId, _theme: WidgetTheme) -> Result<(), WidgetHostError> {
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

    fn picture(rgba: u8) -> Picture {
        Picture {
            width: 1,
            height: 1,
            rgba: vec![rgba; 4],
        }
    }

    #[test]
    fn latest_frame_keeps_only_the_newest_picture() {
        let latest = LatestFrame::default();
        assert_eq!(latest.take(), None);
        assert!(latest.put(picture(1)));
        assert!(!latest.put(picture(2)));
        assert_eq!(latest.clone().take(), Some(picture(2)));
        assert_eq!(latest.take(), None);
        assert!(latest.put(picture(3)));
    }

    #[test]
    fn latest_frames_are_equal_when_they_are_the_same_slot() {
        let latest = LatestFrame::default();
        assert_eq!(latest, latest.clone());
        assert_ne!(latest, LatestFrame::default());
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
    fn no_widget_host_takes_a_theme() {
        assert_eq!(
            NoWidgetHost.set_theme(ViewId(1), WidgetTheme::Light),
            Ok(())
        );
        assert_eq!(NoWidgetHost.set_theme(ViewId(1), WidgetTheme::Dark), Ok(()));
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
        host.set_theme(view, WidgetTheme::Light).unwrap();
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
                WidgetCall::SetTheme {
                    view,
                    theme: WidgetTheme::Light
                },
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
