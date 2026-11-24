//! The viewer: something shown to the user over the whole window.
//!
//! What it shows is a [`Shown`]: a picture file, an SVG an agent drew, or an HTML page an
//! agent wrote. The window stays visible under a dimming backdrop and the content is fitted
//! inside a margin of 32 pixels: a larger picture or drawing is scaled down to fit at its own
//! ratio, a smaller one keeps its own size, and the backdrop shows around it. A drawing sits
//! on a white canvas in both themes, since agents draw for a white page; one without a size
//! of its own is 100 by 100 pixels.
//!
//! Opening the viewer shows `loading…` while the content is prepared on a thread of its own,
//! so the window never waits for it: a file is decoded, a drawing is parsed. A file that
//! cannot be read or decoded shows `cannot show <file name>: <reason>` in its place, and a
//! drawing that does not parse `cannot show the drawing: <reason>`. A page stays `loading…`,
//! since no page view is opened yet. Escape or a click beside the content closes the viewer,
//! a click on the content does not, and no other key reaches the window while it is open.

use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::image::Handle;
use iced::widget::{container, image, svg, text};
use iced::{Alignment, Color, ContentFit, Element, Length, Padding, Task, Theme};
use partitur_core::{Drawing, DrawingKind};

use crate::app::{BareKey, Message, State};
use crate::background::off_loop;
use crate::sheet::{self, PADDING, sheet_style};
use crate::style;

/// The room kept between the content and the window's edge, in logical pixels.
const MARGIN: f32 = 32.0;

/// What the viewer shows. New kinds of content are new variants.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// A picture file on disk.
    Image(PathBuf),
    /// An SVG document an agent drew.
    Svg(Arc<str>),
    /// An HTML page an agent wrote.
    Html(Arc<str>),
}

/// What the viewer shows for an agent's `drawing`.
pub(crate) fn shown_for(drawing: &Drawing) -> Shown {
    let markup = Arc::clone(&drawing.markup);
    match drawing.kind {
        DrawingKind::Svg => Shown::Svg(markup),
        DrawingKind::Html => Shown::Html(markup),
    }
}

/// Content prepared in the background, ready to be shown.
#[derive(Debug, Clone, PartialEq)]
pub enum Prepared {
    /// A decoded picture.
    Image(ShownImage),
    /// An SVG document that parsed.
    Svg(Arc<str>),
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
    Loading(Shown),
    /// A decoded picture.
    Image(Handle),
    /// A parsed drawing.
    Svg(svg::Handle),
    /// The content could not be shown; the text says why.
    Failed(String),
}

/// Opens the viewer on `shown`, loading, and starts preparing it in the background. A page
/// has nothing to prepare here and stays loading.
pub(crate) fn show(state: &mut State, shown: Shown) -> Task<Message> {
    state.viewer = Some(Viewer::Loading(shown.clone()));
    if matches!(shown, Shown::Html(_)) {
        return Task::none();
    }
    load(shown)
}

/// Prepares `shown` on a thread of its own (`viewer-load`) and resolves with
/// [`Message::Shown`].
pub(crate) fn load(shown: Shown) -> Task<Message> {
    Task::future(off_loop("viewer-load", {
        let shown = shown.clone();
        move || prepare(&shown)
    }))
    .map(move |done| {
        let result = done.unwrap_or_else(|| Err("cannot show the file".to_owned()));
        Message::Shown(shown.clone(), result)
    })
}

/// Prepares `shown`: decodes a picture or parses a drawing, or tells why it cannot be shown.
fn prepare(shown: &Shown) -> Result<Prepared, String> {
    match shown {
        Shown::Image(path) => decode(path).map(Prepared::Image),
        Shown::Svg(markup) => {
            usvg::Tree::from_data(markup.as_bytes(), &usvg::Options::default())
                .map_err(|error| format!("cannot show the drawing: {error}"))?;
            Ok(Prepared::Svg(Arc::clone(markup)))
        }
        Shown::Html(_) => Err("cannot show the page".to_owned()),
    }
}

/// Decodes the picture at `path`, or tells why it cannot be shown.
fn decode(path: &std::path::Path) -> Result<ShownImage, String> {
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

/// Takes the outcome of [`load`] for `asked` while the viewer still waits for exactly that; a
/// result that arrives after the viewer closed or moved on to other content is dropped.
pub(crate) fn shown(state: &mut State, asked: &Shown, result: Result<Prepared, String>) {
    if !matches!(&state.viewer, Some(Viewer::Loading(waiting)) if waiting == asked) {
        return;
    }
    state.viewer = Some(match result {
        Ok(Prepared::Image(picture)) => Viewer::Image(Handle::from_rgba(
            picture.width,
            picture.height,
            picture.rgba,
        )),
        Ok(Prepared::Svg(markup)) => {
            Viewer::Svg(svg::Handle::from_memory(markup.as_bytes().to_vec()))
        }
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
/// a frame (`viewer-frame`) that takes the clicks meant for it. The frame is only as large
/// as its content: a picture or a drawing is scaled down to fit inside the margin and never
/// enlarged, so the backdrop shows around it. A drawing lies on a white canvas
/// (`viewer-canvas`) that is exactly its size.
pub(crate) fn view<'a>(page: Element<'a, Message>, viewer: &Viewer) -> Element<'a, Message> {
    let (content, padding): (Element<'a, Message>, Padding) = match viewer {
        Viewer::Image(handle) => (
            image(handle.clone())
                .content_fit(ContentFit::ScaleDown)
                .into(),
            Padding::ZERO,
        ),
        Viewer::Svg(handle) => (
            container(
                svg(handle.clone())
                    .content_fit(ContentFit::ScaleDown)
                    .width(Length::Shrink)
                    .height(Length::Shrink),
            )
            .id("viewer-canvas")
            .style(canvas_style)
            .into(),
            Padding::ZERO,
        ),
        Viewer::Loading(_) => (text("loading…").style(style::muted).into(), PADDING.into()),
        Viewer::Failed(reason) => (
            text(reason.clone()).style(style::muted).into(),
            PADDING.into(),
        ),
    };
    let frame = container(content)
        .id("viewer-frame")
        .padding(padding)
        .style(sheet_style);
    sheet::modal(
        page,
        frame.into(),
        Alignment::Center,
        MARGIN,
        Message::CloseViewer,
    )
}

/// The canvas a drawing lies on: white in both themes, as the page agents draw for.
fn canvas_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::WHITE.into()),
        ..container::Style::default()
    }
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
    use crate::chrome::{Platform, WindowFrame, chrome};
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
        let Message::Shown(_, Ok(Prepared::Image(image))) = &message else {
            panic!("{message:?}");
        };
        assert_eq!((image.width, image.height), (4, 2));
        let _task = app_update(state, message);
        assert!(matches!(state.viewer, Some(Viewer::Image(_))));
        assert!(simulator(app_view(state)).find("loading…").is_err());
    }

    #[test]
    fn an_svg_is_shown_on_a_white_canvas() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        let markup = "<svg xmlns='http://www.w3.org/2000/svg' width='4' height='2'/>";

        let task = open(state, Shown::Svg(markup.into()));

        assert!(matches!(state.viewer, Some(Viewer::Loading(_))));
        simulator(app_view(state)).find("loading…").unwrap();
        let message = first_output(task);
        assert_eq!(
            message,
            Message::Shown(Shown::Svg(markup.into()), Ok(Prepared::Svg(markup.into())))
        );
        let _task = app_update(state, message);
        assert!(matches!(state.viewer, Some(Viewer::Svg(_))));
        let mut ui = simulator(app_view(state));
        assert!(ui.find("loading…").is_err());
        let frame = ui.find(Id::new("viewer-frame")).unwrap().bounds();
        let canvas = ui.find(Id::new("viewer-canvas")).unwrap().bounds();
        // The canvas holds the drawing at its own size, inside the frame and its border.
        assert!(
            (canvas.width - 4.0).abs() < 0.5 && (canvas.height - 2.0).abs() < 0.5,
            "{canvas:?}"
        );
        assert!(frame.contains(canvas.center()), "{frame:?} {canvas:?}");
    }

    #[test]
    fn the_canvas_is_white_in_both_themes() {
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let style = canvas_style(&theme);
            assert_eq!(style.background, Some(Color::WHITE.into()));
        }
    }

    #[test]
    fn a_large_svg_is_scaled_down_inside_the_margin() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        state.chrome = chrome(WindowFrame::System, Platform::Other);
        let markup = "<svg xmlns='http://www.w3.org/2000/svg' width='4000' height='3000'/>";
        viewer_with(state, Shown::Svg(markup.into()));

        let mut ui = simulator(app_view(state));
        let canvas = ui.find(Id::new("viewer-canvas")).unwrap().bounds();
        assert!((canvas.height - 704.0).abs() < 4.0, "{canvas:?}");
        assert!((canvas.width - 704.0 * 4.0 / 3.0).abs() < 4.0, "{canvas:?}");
    }

    /// Opens the viewer on `shown` and feeds it the outcome of preparing it.
    fn viewer_with(state: &mut State, shown: Shown) {
        let task = open(state, shown);
        let _task = app_update(state, first_output(task));
    }

    #[test]
    fn a_broken_svg_says_why() {
        let mut fixture = plain();
        let state = &mut fixture.state;

        viewer_with(state, Shown::Svg("<svg".into()));

        let Some(Viewer::Failed(reason)) = &state.viewer else {
            panic!("{:?}", state.viewer);
        };
        assert!(reason.starts_with("cannot show the drawing: "), "{reason}");
        let reason = reason.clone();
        simulator(app_view(state)).find(reason.as_str()).unwrap();
    }

    #[test]
    fn a_page_waits_in_the_viewer() {
        let mut fixture = plain();
        let state = &mut fixture.state;

        let _task = open(state, Shown::Html("<p>hi</p>".into()));

        assert!(matches!(
            state.viewer,
            Some(Viewer::Loading(Shown::Html(_)))
        ));
        simulator(app_view(state)).find("loading…").unwrap();
    }

    #[test]
    fn a_drawing_is_shown_by_its_kind() {
        let drawing = |kind, markup: &str| Drawing {
            kind,
            markup: markup.into(),
        };
        assert_eq!(
            shown_for(&drawing(DrawingKind::Svg, "<svg/>")),
            Shown::Svg("<svg/>".into())
        );
        assert_eq!(
            shown_for(&drawing(DrawingKind::Html, "<p/>")),
            Shown::Html("<p/>".into())
        );
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

    /// Two decodable pictures of different sizes in `dir`.
    fn two_pictures(dir: &std::path::Path) -> (Shown, Shown) {
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        ::image::RgbaImage::new(4, 2).save(&a).unwrap();
        ::image::RgbaImage::new(6, 3).save(&b).unwrap();
        (Shown::Image(a), Shown::Image(b))
    }

    fn waits_for(state: &State, shown: &Shown) -> bool {
        matches!(&state.viewer, Some(Viewer::Loading(waiting)) if waiting == shown)
    }

    #[test]
    fn a_result_for_an_earlier_picture_is_ignored() {
        let dir = temp_dir();
        let (a, b) = two_pictures(dir.path());
        let mut fixture = plain();
        let state = &mut fixture.state;
        let for_a = first_output(open(state, a));
        let _task = app_update(state, Message::CloseViewer);
        let for_b = first_output(open(state, b.clone()));

        let _task = app_update(state, for_a);
        assert!(waits_for(state, &b));

        let _task = app_update(state, for_b);
        assert!(matches!(state.viewer, Some(Viewer::Image(_))));
    }

    #[test]
    fn a_result_for_an_earlier_picture_is_ignored_when_another_is_opened_meanwhile() {
        let dir = temp_dir();
        let (a, b) = two_pictures(dir.path());
        let mut fixture = plain();
        let state = &mut fixture.state;
        let for_a = first_output(open(state, a));
        let for_b = first_output(open(state, b.clone()));

        let _task = app_update(state, for_a);
        assert!(waits_for(state, &b));

        let _task = app_update(state, for_b);
        assert!(matches!(state.viewer, Some(Viewer::Image(_))));
    }

    #[test]
    fn a_result_after_the_viewer_closed_is_ignored() {
        let mut fixture = plain();
        let state = &mut fixture.state;

        let _task = app_update(
            state,
            Message::Shown(
                Shown::Image(PathBuf::from("late.png")),
                Err("late".to_owned()),
            ),
        );

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
    fn a_small_picture_keeps_its_size_and_a_click_beside_it_closes_the_viewer() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        state.viewer = Some(Viewer::Image(Handle::from_rgba(4, 2, vec![255; 4 * 2 * 4])));

        let mut ui = simulator(app_view(state));
        let frame = ui.find(Id::new("viewer-frame")).unwrap().bounds();
        // The frame holds the picture at its own size, with room for its border only.
        assert!(frame.width < 12.0 && frame.height < 10.0, "{frame:?}");
        let middle = frame.center();
        ui.point_at(middle);
        ui.simulate(simulator::click());
        assert_eq!(ui.into_messages().count(), 0);

        let mut ui = simulator(app_view(state));
        ui.point_at(Point::new(middle.x + 100.0, middle.y));
        ui.simulate(simulator::click());
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::CloseViewer]);
    }

    #[test]
    fn a_large_picture_is_scaled_down_inside_the_margin() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        // Under the system's frame the viewer covers the whole window, toolbar and all.
        state.chrome = chrome(WindowFrame::System, Platform::Other);
        let (width, height) = (4000, 3000);
        state.viewer = Some(Viewer::Image(Handle::from_rgba(
            width,
            height,
            vec![255; 4000 * 3000 * 4],
        )));

        let mut ui = simulator(app_view(state));
        let frame = ui.find(Id::new("viewer-frame")).unwrap().bounds();
        // The simulated window is 1024×768, so the picture fits in 960×704 at its own ratio.
        assert!((frame.height - 704.0).abs() < 4.0, "{frame:?}");
        assert!((frame.width - 704.0 * 4.0 / 3.0).abs() < 4.0, "{frame:?}");
        assert!(frame.x >= MARGIN && frame.y >= MARGIN, "{frame:?}");
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
