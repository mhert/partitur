//! The viewer: something shown to the user over the whole window.
//!
//! What it shows is a [`Shown`]; today that is one picture file, and a new kind of content
//! (an SVG an agent drew) is a new variant. The window stays visible under a dimming
//! backdrop and the content is fitted inside a margin of 32 pixels, never stretched.
//!
//! Opening the viewer shows `loading…` while the file is decoded on a thread of its own, so
//! the window never waits for it; a file that cannot be read or decoded shows
//! `cannot show <file name>: <reason>` in its place. Escape or a click beside the content
//! closes it, a click on the content does not, and no other key reaches the window while it
//! is open.

use std::path::PathBuf;

use iced::widget::image::Handle;
use iced::widget::{container, image, mouse_area, opaque, stack, text};
use iced::{ContentFit, Element, Length, Task, Theme};

use crate::app::{BareKey, Message, State};
use crate::confirm_sheet::{BACKDROP_STRENGTH, PADDING, sheet_style};
use crate::paste::off_loop;
use crate::{style, theme};

/// The room kept between the content and the window's edge, in logical pixels.
const MARGIN: f32 = 32.0;

/// What the viewer shows. New kinds of content are new variants.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// A picture file on disk.
    Image(PathBuf),
}

/// A picture decoded to RGBA, ready to become an image handle.
#[derive(Clone, PartialEq, Eq)]
pub struct ShownImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl std::fmt::Debug for ShownImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShownImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// The viewer while it is open.
#[derive(Debug, Clone)]
pub(crate) enum Viewer {
    /// The content is being prepared in the background.
    Loading(
        #[expect(
            dead_code,
            reason = "it names what is awaited; only its being there is read"
        )]
        Shown,
    ),
    /// A decoded picture.
    Image(Handle),
    /// The content could not be shown; the text says why.
    Failed(String),
}

/// Opens the viewer on `shown`, loading, and starts preparing it in the background.
pub(crate) fn show(state: &mut State, shown: Shown) -> Task<Message> {
    state.viewer = Some(Viewer::Loading(shown.clone()));
    load(shown)
}

/// Prepares `shown` on a thread of its own (`viewer-load`) and resolves with
/// [`Message::Shown`].
pub(crate) fn load(shown: Shown) -> Task<Message> {
    Task::future(off_loop("viewer-load", move || decode(&shown)))
        .map(|done| Message::Shown(done.unwrap_or_else(|| Err("cannot show the file".to_owned()))))
}

/// Decodes the picture of `shown`, or tells why it cannot be shown.
fn decode(shown: &Shown) -> Result<ShownImage, String> {
    let Shown::Image(path) = shown;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let rgba = ::image::open(path)
        .map_err(|error| format!("cannot show {name}: {error}"))?
        .into_rgba8();
    Ok(ShownImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Takes the outcome of [`load`] while the viewer still waits for it; a result that arrives
/// after the viewer closed is dropped.
pub(crate) fn shown(state: &mut State, result: Result<ShownImage, String>) {
    if !matches!(state.viewer, Some(Viewer::Loading(_))) {
        return;
    }
    state.viewer = Some(match result {
        Ok(picture) => Viewer::Image(Handle::from_rgba(
            picture.width,
            picture.height,
            picture.rgba,
        )),
        Err(reason) => Viewer::Failed(reason),
    });
}

/// What the bare `key` does while the viewer is open: Escape closes it and every other key
/// does nothing, since the window under it takes no keys.
pub(crate) fn press(state: &mut State, key: BareKey) -> Task<Message> {
    if key == BareKey::Escape {
        state.viewer = None;
    }
    Task::none()
}

/// The viewer over `page`: a dimming backdrop that closes it on a click, and the content in
/// a frame (`viewer-frame`) that takes the clicks meant for it.
pub(crate) fn view<'a>(page: Element<'a, Message>, viewer: &Viewer) -> Element<'a, Message> {
    let (content, fills): (Element<'a, Message>, bool) = match viewer {
        Viewer::Image(handle) => (
            image(handle.clone())
                .content_fit(ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            true,
        ),
        Viewer::Loading(_) => (text("loading…").style(style::muted).into(), false),
        Viewer::Failed(reason) => (text(reason.clone()).style(style::muted).into(), false),
    };
    let mut frame = container(content).id("viewer-frame").style(sheet_style);
    frame = if fills {
        frame.width(Length::Fill).height(Length::Fill)
    } else {
        frame.padding(PADDING)
    };
    let backdrop = mouse_area(
        container(opaque(frame))
            .center(Length::Fill)
            .padding(MARGIN)
            .style(|theme: &Theme| container::Style {
                background: Some(
                    theme::tokens(theme)
                        .bg
                        .scale_alpha(BACKDROP_STRENGTH)
                        .into(),
                ),
                ..container::Style::default()
            }),
    )
    .on_press(Message::CloseViewer);
    stack![page, opaque(backdrop)].into()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use iced::Point;
    use iced::widget::Id;
    use iced_test::simulator;

    use super::*;
    use crate::app::{BareKey, Message, State, Tab, update as app_update, view as app_view};
    use crate::test_support::{first_output, plain, temp_dir};

    fn open(state: &mut State, shown: Shown) -> Task<Message> {
        app_update(state, Message::Show(shown))
    }

    #[test]
    fn opening_an_image_shows_loading_until_decoded() {
        let dir = temp_dir();
        let path = dir.path().join("four.png");
        ::image::RgbaImage::new(4, 2).save(&path).unwrap();
        let mut fixture = plain();
        let state = &mut fixture.state;

        let task = open(state, Shown::Image(path));

        assert!(matches!(state.viewer, Some(Viewer::Loading(_))));
        simulator(app_view(state)).find("loading…").unwrap();
        let message = first_output(task);
        let Message::Shown(Ok(image)) = &message else {
            panic!("{message:?}");
        };
        assert_eq!((image.width, image.height), (4, 2));
        let _task = app_update(state, message);
        assert!(matches!(state.viewer, Some(Viewer::Image(_))));
        assert!(simulator(app_view(state)).find("loading…").is_err());
    }

    #[test]
    fn an_unreadable_image_says_so() {
        let dir = temp_dir();
        let mut fixture = plain();
        let state = &mut fixture.state;

        let task = open(state, Shown::Image(dir.path().join("missing.png")));
        let _task = app_update(state, first_output(task));

        assert!(matches!(state.viewer, Some(Viewer::Failed(_))));
        simulator(app_view(state))
            .find(|candidate: iced_test::selector::Candidate<'_>| {
                matches!(candidate, iced_test::selector::Candidate::Text { content, .. }
                    if content.starts_with("cannot show missing.png"))
                .then_some(())
            })
            .unwrap();
    }

    #[test]
    fn a_result_after_the_viewer_closed_is_ignored() {
        let mut fixture = plain();
        let state = &mut fixture.state;

        let _task = app_update(state, Message::Shown(Err("late".to_owned())));

        assert!(state.viewer.is_none());
    }

    #[test]
    fn escape_closes_the_viewer() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        state.viewer = Some(Viewer::Failed("x".to_owned()));

        let _task = app_update(state, Message::Key(BareKey::Escape));

        assert!(state.viewer.is_none());
    }

    #[test]
    fn a_click_on_the_backdrop_closes_the_viewer_and_one_on_the_picture_does_not() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        state.viewer = Some(Viewer::Failed("x".to_owned()));

        let mut ui = simulator(app_view(state));
        ui.click(Id::new("viewer-frame")).unwrap();
        assert_eq!(ui.into_messages().count(), 0);

        let mut ui = simulator(app_view(state));
        ui.point_at(Point::new(4.0, 300.0));
        ui.simulate(simulator::click());
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::CloseViewer]);
        let _task = app_update(state, Message::CloseViewer);
        assert!(state.viewer.is_none());
    }

    #[test]
    fn keys_do_not_reach_the_window_while_the_viewer_is_open() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        state.tab = Tab::Projects;
        state.viewer = Some(Viewer::Failed("x".to_owned()));

        let _task = app_update(state, Message::Key(BareKey::N));
        let _task = app_update(state, Message::SelectTab(Tab::Env));

        assert!(state.app.draft().is_none());
        assert_eq!(state.tab, Tab::Projects);
        assert!(state.viewer.is_some());
    }
}
