//! The viewer: something shown to the user over the whole window.
//!
//! What it shows is a [`Shown`]: a picture file, an SVG an agent drew, or an HTML page an
//! agent wrote. The window stays visible under a dimming backdrop and the content is fitted
//! inside a margin of 32 pixels: a larger picture or drawing is scaled down to fit at its own
//! ratio, a smaller one keeps its own size, and the backdrop shows around it. A drawing without
//! a size of its own, whose root lacks a `width` or a `height`, instead fills the space inside
//! the margin at its own ratio (that of its `viewBox`, else square). A drawing sits on a white
//! canvas exactly its size in both themes, since agents draw for a white page.
//!
//! Opening the viewer shows `loading…` while the content is prepared on a thread of its own,
//! so the window never waits for it: a file is decoded, a drawing is parsed. A file that
//! cannot be read or decoded shows `cannot show <file name>: <reason>` in its place, and a
//! drawing that does not parse `cannot show the drawing: <reason>`.
//!
//! A page opens at once in a view of the widget engine of its own, the page view of
//! [`Widgets`], in the desktop's theme and under the default content security policy, and
//! fills the space inside the margin; the view is told that size at the window's scale
//! factor, again whenever the window is resized or rescaled. It shows `loading…` until the
//! engine's first picture of it is allocated. The pointer and the wheel over it go to the
//! page, keys do not. An engine that refuses the page, or fails it later, makes the viewer say
//! `cannot show the drawing: <reason>`. Whatever the viewer shows next, and closing it, closes
//! the page view.
//!
//! Escape or a click beside the content closes the viewer, a click on the content does not,
//! and no other key reaches the window while it is open.

use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::image::Handle;
use iced::widget::{container, image, responsive, space, stack, svg, text};
use iced::{Alignment, Color, ContentFit, Element, Length, Padding, Size, Task, Theme};
use partitur_core::widget_host::ViewId;
use partitur_core::{Drawing, DrawingKind};

use crate::app::{BareKey, Message, State};
use crate::background::off_loop;
use crate::session_view;
use crate::sheet::{self, PADDING, sheet_style};
use crate::style;
use crate::widgets::{self, Widgets};

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
    Svg {
        /// The document.
        markup: Arc<str>,
        /// Its size as parsed, which gives its ratio.
        size: Size,
        /// Whether its root states both a `width` and a `height`.
        sized: bool,
    },
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
    /// A parsed drawing of `size`; one that is not `sized` fills the viewer at its ratio.
    Svg {
        handle: svg::Handle,
        size: Size,
        sized: bool,
    },
    /// An agent's page in the widget engine's view of that name, the page view of
    /// [`Widgets`](crate::widgets::Widgets).
    Page(ViewId),
    /// The content could not be shown; the text says why.
    Failed(String),
}

/// Opens the viewer on `shown`, in place of what it showed, whose page view closes. A page
/// opens its view in the widget engine at once, in the desktop's theme, and needs nothing
/// prepared here; a refusal says why in its place. Anything else shows loading while it is
/// prepared in the background.
pub(crate) fn show(state: &mut State, shown: Shown) -> Task<Message> {
    let host = state.widget_host.as_ref();
    state.widgets.close_page(host);
    if let Shown::Html(html) = &shown {
        let opened = state
            .widgets
            .open_page(host, html, widgets::theme_of(state.mode));
        state.viewer = Some(match opened {
            Ok(view) => Viewer::Page(view),
            Err(error) => Viewer::Failed(format!("cannot show the drawing: {error}")),
        });
        return Task::none();
    }
    state.viewer = Some(Viewer::Loading(shown.clone()));
    load(shown)
}

/// Closes the viewer, and its page view with it.
pub(crate) fn close(state: &mut State) {
    state.viewer = None;
    state.widgets.close_page(state.widget_host.as_ref());
}

/// Prepares `shown` on a thread of its own (`viewer-load`) and resolves with
/// [`Message::Shown`].
fn load(shown: Shown) -> Task<Message> {
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
/// A page is never prepared: it opens in the widget engine.
fn prepare(shown: &Shown) -> Result<Prepared, String> {
    match shown {
        Shown::Image(path) => decode(path).map(Prepared::Image),
        Shown::Svg(markup) => {
            let tree = usvg::Tree::from_data(markup.as_bytes(), &usvg::Options::default())
                .map_err(|error| format!("cannot show the drawing: {error}"))?;
            Ok(Prepared::Svg {
                markup: Arc::clone(markup),
                size: Size::new(tree.size().width(), tree.size().height()),
                sized: has_size(markup),
            })
        }
        // `show` opens a page in the widget engine and never asks for it here.
        Shown::Html(_) => Err("cannot show the drawing: a page is not prepared".to_owned()),
    }
}

/// Whether the root of the SVG document `markup` states both a `width` and a `height`. A
/// document usvg reads but roxmltree does not counts as sized, so it keeps its own size.
fn has_size(markup: &str) -> bool {
    usvg::roxmltree::Document::parse(markup).map_or(true, |document| {
        let root = document.root_element();
        root.has_attribute("width") && root.has_attribute("height")
    })
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
        Ok(Prepared::Svg {
            markup,
            size,
            sized,
        }) => Viewer::Svg {
            handle: svg::Handle::from_memory(markup.as_bytes().to_vec()),
            size,
            sized,
        },
        Err(reason) => Viewer::Failed(reason),
    });
}

/// What the bare `key` does while the viewer is open: Escape closes it and every other key
/// does nothing, since the window under it takes no keys.
pub(crate) fn press(state: &mut State, key: BareKey) -> Task<Message> {
    if key == BareKey::Escape {
        close(state);
    }
    Task::none()
}

/// The viewer over `page`: a dimming backdrop that closes it on a click, and the content in
/// a frame (`viewer-frame`) that takes the clicks meant for it. The frame is only as large
/// as its content: a picture or a drawing with a size of its own is scaled down to fit inside
/// the margin and never enlarged, so the backdrop shows around it, while a drawing without one
/// fills the space inside the margin at its ratio. A drawing lies on a white canvas
/// (`viewer-canvas`) that is exactly its size. An agent's page fills the space inside the
/// margin with the last frame of the page view of `widgets`, showing `loading…` until there is
/// one.
pub(crate) fn view<'a>(
    page: Element<'a, Message>,
    viewer: &Viewer,
    widgets: &'a Widgets,
) -> Element<'a, Message> {
    let (content, padding): (Element<'a, Message>, Padding) = match viewer {
        Viewer::Page(view) => {
            let loading = || {
                container(text("loading…").style(style::muted))
                    .center(Length::Fill)
                    .into()
            };
            let content = match widgets.page().filter(|page| page.id == *view) {
                Some(page) => session_view::page(page, widgets.scale(), loading()),
                None => loading(),
            };
            (content, Padding::ZERO)
        }
        Viewer::Image(handle) => (
            image(handle.clone())
                .content_fit(ContentFit::ScaleDown)
                .into(),
            Padding::ZERO,
        ),
        Viewer::Svg {
            handle,
            size,
            sized: false,
        } => {
            let (handle, size) = (handle.clone(), *size);
            // The space inside the margin is known only at layout, so the modal is laid out
            // over an empty page there, and the real page stays under it.
            let over = responsive(move |window| {
                let room = Size::new(
                    (window.width - 2.0 * MARGIN).max(0.0),
                    (window.height - 2.0 * MARGIN).max(0.0),
                );
                sheet::modal(
                    space().width(Length::Fill).height(Length::Fill).into(),
                    framed(canvas(handle.clone(), filled(size, room)), Padding::ZERO),
                    Alignment::Center,
                    MARGIN,
                    Message::CloseViewer,
                )
            });
            return stack![page, over].into();
        }
        Viewer::Svg { handle, .. } => (canvas(handle.clone(), None), Padding::ZERO),
        Viewer::Loading(_) => (text("loading…").style(style::muted).into(), PADDING.into()),
        Viewer::Failed(reason) => (
            text(reason.clone()).style(style::muted).into(),
            PADDING.into(),
        ),
    };
    sheet::modal(
        page,
        framed(content, padding),
        Alignment::Center,
        MARGIN,
        Message::CloseViewer,
    )
}

/// `content` in the viewer's frame (`viewer-frame`), inside `padding`.
fn framed(content: Element<'_, Message>, padding: Padding) -> Element<'_, Message> {
    container(content)
        .id("viewer-frame")
        .padding(padding)
        .style(sheet_style)
        .into()
}

/// A drawing on its white canvas (`viewer-canvas`): `fill` sized, the drawing fitted into it,
/// or without one the drawing at its own size, scaled down to fit.
fn canvas<'a>(handle: svg::Handle, fill: Option<Size>) -> Element<'a, Message> {
    let drawing = svg(handle);
    let canvas = match fill {
        Some(fill) => container(
            drawing
                .content_fit(ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(fill.width)
        .height(fill.height),
        None => container(
            drawing
                .content_fit(ContentFit::ScaleDown)
                .width(Length::Shrink)
                .height(Length::Shrink),
        ),
    };
    canvas.id("viewer-canvas").style(canvas_style).into()
}

/// The largest size at the ratio of `size` that fits `room`; none for a drawing without an
/// area, which then keeps its own size.
fn filled(size: Size, room: Size) -> Option<Size> {
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    let scale = (room.width / size.width).min(room.height / size.height);
    Some(Size::new(size.width * scale, size.height * scale))
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
    use partitur_core::testing::{RecordingWidgetHost, WidgetCall};
    use partitur_core::widget_host::{ViewId, WidgetHostError};

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
            Message::Shown(
                Shown::Svg(markup.into()),
                Ok(Prepared::Svg {
                    markup: markup.into(),
                    size: Size::new(4.0, 2.0),
                    sized: true,
                })
            )
        );
        let _task = app_update(state, message);
        assert!(matches!(
            state.viewer,
            Some(Viewer::Svg { sized: true, .. })
        ));
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

    /// The bounds of the canvas of a drawing of `markup` in a window whose frame is the
    /// system's, so the viewer covers the whole 1024×768 window.
    fn canvas_of(markup: &str) -> iced::Rectangle {
        let mut fixture = plain();
        let state = &mut fixture.state;
        state.chrome = chrome(WindowFrame::System, Platform::Other);
        viewer_with(state, Shown::Svg(markup.into()));
        let mut ui = simulator(app_view(state));
        ui.find(Id::new("viewer-canvas")).unwrap().bounds()
    }

    #[test]
    fn an_svg_without_a_size_fills_the_viewer_at_its_ratio() {
        // Inside the margin there is 960×704; a 2:1 drawing fills its width.
        let wide = canvas_of("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 4 2'/>");
        assert!((wide.width - 960.0).abs() < 1.0, "{wide:?}");
        assert!((wide.height - 480.0).abs() < 1.0, "{wide:?}");
        assert!((wide.x - MARGIN).abs() < 1.0, "{wide:?}");
        assert!((wide.center_y() - 384.0).abs() < 1.0, "{wide:?}");

        // A 1:2 drawing fills its height.
        let tall = canvas_of("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 2 4'/>");
        assert!((tall.height - 704.0).abs() < 1.0, "{tall:?}");
        assert!((tall.width - 352.0).abs() < 1.0, "{tall:?}");
        assert!((tall.center_x() - 512.0).abs() < 1.0, "{tall:?}");

        // A width alone is no size of its own either.
        let half =
            canvas_of("<svg xmlns='http://www.w3.org/2000/svg' width='4' viewBox='0 0 4 2'/>");
        assert!((half.width - 960.0).abs() < 1.0, "{half:?}");

        // One with both keeps its size.
        let sized = canvas_of("<svg xmlns='http://www.w3.org/2000/svg' width='4' height='2'/>");
        assert!(
            (sized.width - 4.0).abs() < 0.5 && (sized.height - 2.0).abs() < 0.5,
            "{sized:?}"
        );
    }

    #[test]
    fn a_click_beside_a_filled_drawing_closes_the_viewer_and_one_on_it_does_not() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        state.chrome = chrome(WindowFrame::System, Platform::Other);
        viewer_with(
            state,
            Shown::Svg("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 4 2'/>".into()),
        );

        let mut ui = simulator(app_view(state));
        let canvas = ui.find(Id::new("viewer-canvas")).unwrap().bounds();
        ui.point_at(canvas.center());
        ui.simulate(simulator::click());
        assert_eq!(ui.into_messages().count(), 0);

        let mut ui = simulator(app_view(state));
        ui.point_at(Point::new(canvas.center_x(), canvas.y - 40.0));
        ui.simulate(simulator::click());
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::CloseViewer]);
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

    /// The view the last `open` of the widget engine named.
    fn opened(host: &RecordingWidgetHost) -> ViewId {
        host.calls()
            .iter()
            .rev()
            .find_map(|call| match call {
                WidgetCall::Open { view, .. } => Some(*view),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn closing_the_viewer_closes_the_page() {
        let mut fixture = plain();
        let state = &mut fixture.state;

        let _task = open(state, Shown::Html("<p>a</p>".into()));
        let view = opened(&fixture.host);
        let _task = app_update(state, Message::Key(BareKey::Escape));
        assert!(state.viewer.is_none());
        assert_eq!(
            fixture.host.calls().last(),
            Some(&WidgetCall::Close { view })
        );
        assert!(state.widgets.page().is_none());

        let _task = open(state, Shown::Html("<p>b</p>".into()));
        let view = opened(&fixture.host);
        let mut ui = simulator(app_view(state));
        ui.point_at(Point::new(4.0, 300.0));
        ui.simulate(simulator::click());
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::CloseViewer]);
        let _task = app_update(state, Message::CloseViewer);
        assert_eq!(
            fixture.host.calls().last(),
            Some(&WidgetCall::Close { view })
        );
        assert!(state.widgets.page().is_none());
    }

    #[test]
    fn a_new_drawing_closes_the_page_view_before() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        let _task = open(state, Shown::Html("<p>a</p>".into()));
        let a = opened(&fixture.host);

        let _task = open(
            state,
            Shown::Svg("<svg xmlns='http://www.w3.org/2000/svg'/>".into()),
        );

        assert_eq!(
            fixture.host.calls().last(),
            Some(&WidgetCall::Close { view: a })
        );
        assert!(state.widgets.page().is_none());

        // Another page closes the one before, then opens its own.
        let _task = open(state, Shown::Html("<p>b</p>".into()));
        let b = opened(&fixture.host);
        let _task = open(state, Shown::Html("<p>c</p>".into()));
        let c = opened(&fixture.host);
        let calls = fixture.host.calls();
        assert_eq!(calls[calls.len() - 2], WidgetCall::Close { view: b });
        assert_ne!(b, c);
        assert!(matches!(state.viewer, Some(Viewer::Page(view)) if view == c));
        assert_eq!(state.widgets.page().map(|page| page.id), Some(c));
    }

    #[test]
    fn a_refused_page_says_why() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        let error = WidgetHostError::Unavailable("no engine".into());
        fixture.host.fail_next_call_with(error.clone());

        let _task = open(state, Shown::Html("<p>hi</p>".into()));

        let reason = format!("cannot show the drawing: {error}");
        assert!(matches!(&state.viewer, Some(Viewer::Failed(shown)) if *shown == reason));
        assert!(state.widgets.page().is_none());
        simulator(app_view(state)).find(reason.as_str()).unwrap();
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
