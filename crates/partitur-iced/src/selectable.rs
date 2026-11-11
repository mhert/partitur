//! Text the user selects with the mouse and copies.
//!
//! iced draws text without a way to select it, and every paragraph of an answer is a widget
//! of its own. So the selection belongs to an [`area`] around all of them. A press in the
//! area anchors the selection and a drag moves its other end. A double click selects a
//! word and a triple click a line. Ctrl+C (Cmd+C on macOS) copies what lies between the
//! two ends, leaf by leaf in reading order, one leaf per line. A press that starts no
//! selection, or Escape, clears it.
//!
//! The leaves are the pieces of selectable [`Text`] inside the area. Each of them only
//! draws its share of the selection behind its glyphs. Text drawn by iced's own widgets,
//! such as labels, buttons and list bullets, is not part of the selection.

use std::any::Any;

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::graphics::text::cosmic_text::{Buffer, BufferLine, Cursor};
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::click;
use iced::advanced::text::{Paragraph as _, Span};
use iced::advanced::widget::operation::{Operation, Scrollable};
use iced::advanced::widget::{Id, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, clipboard, overlay, renderer};
use iced::keyboard::{self, key};
use iced::mouse;
use iced::widget::text::{self, LineHeight, Shaping, Wrapping};
use iced::{
    Element, Event, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme, Vector,
    alignment,
};

use crate::theme;

/// The text `content` in one span, selectable inside an [`area`].
pub fn text<'a, Message>(content: impl text::IntoFragment<'a>) -> Text<'a, (), Message> {
    rich(vec![Span::new(content)])
}

/// The rich text of `spans`, selectable inside an [`area`].
pub fn rich<'a, Link, Message>(spans: impl AsRef<[Span<'a, Link>]> + 'a) -> Text<'a, Link, Message>
where
    Link: Clone + 'static,
{
    Text {
        spans: Box::new(spans),
        size: None,
        line_height: LineHeight::default(),
        width: Length::Shrink,
        font: None,
        style: None,
        hovered_link: None,
        on_link_click: None,
    }
}

/// The region whose selectable [`Text`] shares one selection, around `content`.
pub fn area<'a, Message: 'a>(content: impl Into<Element<'a, Message>>) -> Area<'a, Message> {
    Area {
        content: content.into(),
        epoch: 0,
    }
}

/// How a [`Text`] picks its colour from the active theme.
type Style<'a> = Box<dyn Fn(&Theme) -> text::Style + 'a>;

/// Rich text that draws its share of an [`area`]'s selection. Apart from that it is drawn
/// like iced's rich text, starting at its top left corner.
pub struct Text<'a, Link, Message> {
    spans: Box<dyn AsRef<[Span<'a, Link>]> + 'a>,
    size: Option<Pixels>,
    line_height: LineHeight,
    width: Length,
    font: Option<Font>,
    style: Option<Style<'a>>,
    hovered_link: Option<usize>,
    on_link_click: Option<Box<dyn Fn(Link) -> Message + 'a>>,
}

impl<'a, Link, Message> Text<'a, Link, Message> {
    /// Sets the size of the text.
    #[must_use]
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Sets the height of a line of the text.
    #[must_use]
    pub fn line_height(mut self, line_height: impl Into<LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    /// Sets the width of the text's box.
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the font of the spans that name none.
    #[must_use]
    pub fn font(mut self, font: impl Into<Font>) -> Self {
        self.font = Some(font.into());
        self
    }

    /// Sets the style of the text, which colours the spans that name no colour.
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> text::Style + 'a) -> Self {
        self.style = Some(Box::new(style));
        self
    }

    /// Sets the message a click on a link publishes, without a drag in between.
    #[must_use]
    pub fn on_link_click(mut self, on_link_click: impl Fn(Link) -> Message + 'a) -> Self {
        self.on_link_click = Some(Box::new(on_link_click));
        self
    }
}

/// What an [`area`] needs of a leaf: the laid out text and the leaf's share of the
/// selection.
#[derive(Default)]
struct Leaf {
    paragraph: Paragraph,
    share: Option<(Cursor, Cursor)>,
}

/// The tree state of a [`Text`].
struct State<Link> {
    leaf: Leaf,
    /// The spans the paragraph was laid out from.
    spans: Vec<Span<'static, Link>>,
    /// The span of the link the left button went down on.
    link_pressed: Option<usize>,
}

impl<Link, Message> Widget<Message, Theme, Renderer> for Text<'_, Link, Message>
where
    Link: Clone + 'static,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Link>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Link> {
            leaf: Leaf::default(),
            spans: Vec::new(),
            link_pressed: None,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Link>>();
        let spans = (*self.spans).as_ref();
        layout::sized(limits, self.width, Length::Shrink, |limits| {
            let bounds = limits.max();
            let size = self
                .size
                .unwrap_or_else(|| iced::advanced::text::Renderer::default_size(renderer));
            let font = self
                .font
                .unwrap_or_else(|| iced::advanced::text::Renderer::default_font(renderer));
            let shape = iced::advanced::Text {
                content: (),
                bounds,
                size,
                line_height: self.line_height,
                font,
                align_x: text::Alignment::Default,
                align_y: alignment::Vertical::Top,
                shaping: Shaping::Advanced,
                wrapping: Wrapping::default(),
            };
            if state.spans.as_slice() == spans {
                match state.leaf.paragraph.compare(shape) {
                    iced::advanced::text::Difference::None => {}
                    iced::advanced::text::Difference::Bounds => {
                        state.leaf.paragraph.resize(bounds);
                    }
                    iced::advanced::text::Difference::Shape => {
                        state.leaf.paragraph = Paragraph::with_spans(shape.with_content(spans));
                    }
                }
            } else {
                state.leaf.paragraph = Paragraph::with_spans(shape.with_content(spans));
                state.spans = spans.iter().cloned().map(Span::to_static).collect();
            }
            state.leaf.paragraph.min_bounds()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<State<Link>>();
        let style = self
            .style
            .as_ref()
            .map(|style| style(theme))
            .unwrap_or_default();
        let origin = bounds.position() - Point::ORIGIN;
        self.decorate(renderer, state, style, defaults, origin);
        if let Some((start, end)) = state.leaf.share {
            let colour = theme::tokens(theme).text_selection;
            for region in highlights(state.leaf.paragraph.buffer(), start, end) {
                renderer::Renderer::fill_quad(
                    renderer,
                    renderer::Quad {
                        bounds: region + origin,
                        ..renderer::Quad::default()
                    },
                    colour,
                );
            }
        }
        text::draw(
            renderer,
            defaults,
            bounds,
            &state.leaf.paragraph,
            style,
            viewport,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Some(on_link_click) = &self.on_link_click else {
            return;
        };
        let state = tree.state.downcast_mut::<State<Link>>();
        let spans = (*self.spans).as_ref();
        let was_hovered = self.hovered_link.is_some();
        self.hovered_link = cursor
            .position_in(layout.bounds())
            .and_then(|position| state.leaf.paragraph.hit_span(position))
            .filter(|&span| spans.get(span).is_some_and(|span| span.link.is_some()));
        if was_hovered != self.hovered_link.is_some() {
            shell.request_redraw();
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if self.hovered_link.is_some() {
                    state.link_pressed = self.hovered_link;
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let pressed = state.link_pressed.take();
                if pressed.is_some() && pressed == self.hovered_link {
                    let link = pressed
                        .and_then(|span| spans.get(span))
                        .and_then(|span| span.link.clone());
                    if let Some(link) = link {
                        shell.publish(on_link_click(link));
                    }
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.hovered_link.is_some() {
            mouse::Interaction::Pointer
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // Like iced's plain text, the leaf tells what it says, so it can be found by it.
        let said: String = (*self.spans)
            .as_ref()
            .iter()
            .map(|span| span.text.as_ref())
            .collect();
        operation.text(None, layout.bounds(), &said);
        let state = tree.state.downcast_mut::<State<Link>>();
        operation.custom(None, layout.bounds(), &mut state.leaf);
    }
}

impl<Link, Message> Text<'_, Link, Message> {
    /// Draws what the spans add to their glyphs, at `origin`: the backgrounds of
    /// highlighted spans such as inline code, underlines, strikethroughs and the underline
    /// of the hovered link.
    fn decorate(
        &self,
        renderer: &mut Renderer,
        state: &State<Link>,
        style: text::Style,
        defaults: &renderer::Style,
        origin: Vector,
    ) {
        for (index, span) in (*self.spans).as_ref().iter().enumerate() {
            let hovered = self.on_link_click.is_some() && Some(index) == self.hovered_link;
            if span.highlight.is_none() && !span.underline && !span.strikethrough && !hovered {
                continue;
            }
            let regions = state.leaf.paragraph.span_bounds(index);
            if let Some(highlight) = span.highlight {
                for region in &regions {
                    let padded = Rectangle::new(
                        region.position() - Vector::new(span.padding.left, span.padding.top),
                        region.size() + Size::new(span.padding.x(), span.padding.y()),
                    );
                    renderer::Renderer::fill_quad(
                        renderer,
                        renderer::Quad {
                            bounds: padded + origin,
                            border: highlight.border,
                            ..renderer::Quad::default()
                        },
                        highlight.background,
                    );
                }
            }
            let size = span
                .size
                .or(self.size)
                .unwrap_or_else(|| iced::advanced::text::Renderer::default_size(renderer));
            let line_height = span
                .line_height
                .unwrap_or(self.line_height)
                .to_absolute(size);
            let colour = span.color.or(style.color).unwrap_or(defaults.text_color);
            let baseline = origin + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);
            let mut lines = Vec::new();
            if span.underline || hovered {
                lines.push(size.0 * 0.08);
            }
            if span.strikethrough {
                lines.push(size.0 / 2.0);
            }
            for raise in lines {
                for region in &regions {
                    renderer::Renderer::fill_quad(
                        renderer,
                        renderer::Quad {
                            bounds: Rectangle::new(
                                region.position() + baseline - Vector::new(0.0, raise),
                                Size::new(region.width, 1.0),
                            ),
                            ..renderer::Quad::default()
                        },
                        colour,
                    );
                }
            }
        }
    }
}

impl<'a, Link, Message> From<Text<'a, Link, Message>> for Element<'a, Message>
where
    Link: Clone + 'static,
    Message: 'a,
{
    fn from(text: Text<'a, Link, Message>) -> Self {
        Element::new(text)
    }
}

/// The rectangles that show the selection from `start` to `end` in the text laid out in
/// `buffer`, one per visual line it touches, relative to the text's top left corner.
fn highlights(buffer: &Buffer, start: Cursor, end: Cursor) -> Vec<Rectangle> {
    buffer
        .layout_runs()
        .filter_map(|run| {
            let (x, width) = run.highlight(start, end)?;
            Some(Rectangle::new(
                Point::new(x, run.line_top),
                Size::new(width, run.line_height),
            ))
        })
        .collect()
}

/// A region whose selectable [`Text`] shares one selection. See the [module](self).
pub struct Area<'a, Message> {
    content: Element<'a, Message>,
    epoch: usize,
}

impl<Message> Area<'_, Message> {
    /// Sets the epoch of the area's rows, `0` by default. Marks count leaves in reading
    /// order, so when the rows laid out shift, the caller gives the area a new epoch and the
    /// selection is cleared instead of pointing at other text.
    #[must_use]
    pub fn epoch(mut self, epoch: usize) -> Self {
        self.epoch = epoch;
        self
    }
}

/// A place in an [`area`]'s text: the leaf, in reading order, and the line and byte in it.
/// Places compare in reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Mark {
    leaf: usize,
    line: usize,
    index: usize,
}

impl Mark {
    /// The place `cursor` in the leaf `leaf`.
    fn new(leaf: usize, cursor: Cursor) -> Self {
        Self {
            leaf,
            line: cursor.line,
            index: cursor.index,
        }
    }

    /// The place in its leaf, as a cursor of the leaf's text.
    fn cursor(self) -> Cursor {
        Cursor::new(self.line, self.index)
    }
}

/// The tree state of an [`Area`].
#[derive(Default)]
struct Selection {
    /// Where the selection began.
    anchor: Option<Mark>,
    /// Where the selection ends, which follows the pointer while the button is down.
    head: Option<Mark>,
    /// Whether the left button is down after starting the selection.
    dragging: bool,
    /// The last press that started a selection, to tell double and triple clicks.
    last_click: Option<click::Click>,
    /// The epoch of the rows the marks count their leaves in.
    epoch: usize,
}

impl Selection {
    /// The ends of the selection in reading order; `None` when nothing is selected.
    fn range(&self) -> Option<(Mark, Mark)> {
        let (anchor, head) = (self.anchor?, self.head?);
        match anchor.cmp(&head) {
            std::cmp::Ordering::Less => Some((anchor, head)),
            std::cmp::Ordering::Greater => Some((head, anchor)),
            std::cmp::Ordering::Equal => None,
        }
    }
}

impl<Message> Area<'_, Message> {
    /// Every leaf of the content in reading order.
    fn leaves(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer) -> Vec<Found> {
        let mut collect = Collect::default();
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, &mut collect);
        collect.found
    }

    /// Hands every leaf of the content its share of the selection `range`.
    fn assign(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        range: Option<(Mark, Mark)>,
    ) {
        let mut assign = Assign { range, next: 0 };
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, &mut assign);
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Area<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Selection>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Selection::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        let selection = tree.state.downcast_mut::<Selection>();
        if selection.epoch != self.epoch {
            *selection = Selection {
                epoch: self.epoch,
                ..Selection::default()
            };
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let Some(content) = tree.children.first_mut() else {
            return layout::Node::default();
        };
        let node = self
            .content
            .as_widget_mut()
            .layout(content, renderer, limits);
        // Leaves keep their state by position, so changed content may have moved a share
        // to another leaf: hand them out again.
        let range = tree.state.downcast_ref::<Selection>().range();
        if range.is_some()
            && let Some(content) = tree.children.first_mut()
        {
            self.assign(content, Layout::new(&node), renderer, range);
        }
        node
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if let Some(content) = tree.children.first() {
            self.content
                .as_widget()
                .draw(content, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(content) = tree.children.first_mut() {
            self.content
                .as_widget_mut()
                .operate(content, layout, renderer, operation);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let Some(content) = tree.children.first_mut() else {
            return;
        };
        self.content.as_widget_mut().update(
            content, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
        let selection = tree.state.downcast_mut::<Selection>();
        let before = selection.range();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let position = cursor.position_over(layout.bounds());
                let start = match position {
                    Some(position) if !shell.is_event_captured() => {
                        let leaves = self.leaves(content, layout, renderer);
                        mark(&leaves, position).map(|at| (position, at, leaves))
                    }
                    _ => None,
                };
                let Some((position, at, leaves)) = start else {
                    *selection = Selection::default();
                    if before.is_some() {
                        self.assign(content, layout, renderer, None);
                        shell.request_redraw();
                    }
                    return;
                };
                let click = click::Click::new(position, mouse::Button::Left, selection.last_click);
                let (anchor, head) = match click.kind() {
                    click::Kind::Single => (at, at),
                    click::Kind::Double => around(&leaves, at, word),
                    click::Kind::Triple => around(&leaves, at, |line, _| (0, line.len())),
                };
                *selection = Selection {
                    anchor: Some(anchor),
                    head: Some(head),
                    dragging: true,
                    last_click: Some(click),
                    epoch: selection.epoch,
                };
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if selection.dragging => {
                // The scrollable around the area lifts the pointer off its content when it
                // leaves the scrollable; the drag still follows it.
                if let Some(position) = cursor.land().position() {
                    let leaves = self.leaves(content, layout, renderer);
                    if let Some(at) = mark(&leaves, position) {
                        selection.head = Some(at);
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                selection.dragging = false;
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) => {
                if *key == keyboard::Key::Named(key::Named::Escape) {
                    *selection = Selection::default();
                } else if modifiers.command()
                    && key.to_latin(*physical_key) == Some('c')
                    && let Some(range) = before
                {
                    let leaves = self.leaves(content, layout, renderer);
                    clipboard.write(clipboard::Kind::Standard, copied(&leaves, range));
                    shell.capture_event();
                }
            }
            _ => {}
        }
        let selection = tree.state.downcast_ref::<Selection>();
        let after = selection.range();
        if after != before {
            if let Some(content) = tree.children.first_mut() {
                self.assign(content, layout, renderer, after);
            }
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if tree.state.downcast_ref::<Selection>().dragging {
            return mouse::Interaction::Text;
        }
        tree.children
            .first()
            .map_or(mouse::Interaction::None, |content| {
                self.content
                    .as_widget()
                    .mouse_interaction(content, layout, cursor, viewport, renderer)
            })
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let content = tree.children.first_mut()?;
        self.content
            .as_widget_mut()
            .overlay(content, layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a> From<Area<'a, Message>> for Element<'a, Message> {
    fn from(area: Area<'a, Message>) -> Self {
        Element::new(area)
    }
}

/// A leaf as an [`area`] finds it: where it is, in the area's coordinates, and its text.
struct Found {
    bounds: Rectangle,
    paragraph: Paragraph,
}

/// Collects every leaf in reading order. The bounds of a leaf inside a scrollable are moved
/// by the scrollable's offset, so they are where the leaf is seen.
#[derive(Default)]
struct Collect {
    found: Vec<Found>,
    /// How far the scrollables around the current widget have scrolled their content.
    offset: Vector,
    /// The offset of the scrollable whose content is traversed next.
    pending: Vector,
}

impl Operation for Collect {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        let outer = self.offset;
        self.offset += std::mem::replace(&mut self.pending, Vector::ZERO);
        operate(self);
        self.offset = outer;
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        self.pending = translation;
    }

    fn custom(&mut self, _id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
        if let Some(leaf) = state.downcast_ref::<Leaf>() {
            self.found.push(Found {
                bounds: bounds - self.offset,
                paragraph: leaf.paragraph.clone(),
            });
        }
    }
}

/// Hands every leaf, in reading order, its share of the selection `range`.
struct Assign {
    range: Option<(Mark, Mark)>,
    /// The position of the next leaf in reading order.
    next: usize,
}

impl Operation for Assign {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Any) {
        if let Some(leaf) = state.downcast_mut::<Leaf>() {
            let end = end(&lines(leaf.paragraph.buffer()));
            leaf.share = self.range.and_then(|range| share(range, self.next, end));
            self.next = self.next.saturating_add(1);
        }
    }
}

/// The text of every line of `buffer`.
fn lines(buffer: &Buffer) -> Vec<&str> {
    buffer.lines.iter().map(BufferLine::text).collect()
}

/// The place in `leaves` that the point `position` selects up to. See [`pick`].
fn mark(leaves: &[Found], position: Point) -> Option<Mark> {
    let bounds: Vec<Rectangle> = leaves.iter().map(|leaf| leaf.bounds).collect();
    Some(match pick(&bounds, position)? {
        Pick::On(index) => {
            let leaf = leaves.get(index)?;
            let buffer = leaf.paragraph.buffer();
            let local = position - leaf.bounds.position();
            let cursor = buffer
                .hit(local.x, local.y)
                .unwrap_or_else(|| end(&lines(buffer)));
            Mark::new(index, cursor)
        }
        Pick::Before(index) => Mark::new(index, Cursor::new(0, 0)),
        Pick::End(index) => Mark::new(index, end(&lines(leaves.get(index)?.paragraph.buffer()))),
    })
}

/// The selection a double or triple click at `at` makes: the bytes `bounds` gives around
/// the click in its line.
fn around(
    leaves: &[Found],
    at: Mark,
    bounds: impl Fn(&str, usize) -> (usize, usize),
) -> (Mark, Mark) {
    let line = leaves
        .get(at.leaf)
        .and_then(|leaf| leaf.paragraph.buffer().lines.get(at.line))
        .map(BufferLine::text);
    let Some(line) = line else {
        return (at, at);
    };
    let (start, end) = bounds(line, at.index);
    (Mark { index: start, ..at }, Mark { index: end, ..at })
}

/// The text of `leaves` between the ends of `range`, one leaf per line.
fn copied(leaves: &[Found], (from, to): (Mark, Mark)) -> String {
    let mut parts = Vec::new();
    for (index, leaf) in leaves.iter().enumerate() {
        let lines = lines(leaf.paragraph.buffer());
        if let Some((start, end)) = share((from, to), index, end(&lines)) {
            parts.push(excerpt(&lines, start, end));
        }
    }
    parts.join("\n")
}

/// Where a point lies among the leaves of an area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// Beside or on the leaf at the index, at the height of one of its lines.
    On(usize),
    /// Above the leaf at the index, below the one before it.
    Before(usize),
    /// Below the last leaf, which is at the index.
    End(usize),
}

/// Where `point` lies among leaves with the `bounds`, in reading order. A point at the
/// height of leaves picks the one nearest to it across, the first of equally near ones.
/// `None` when there are no leaves.
fn pick(bounds: &[Rectangle], point: Point) -> Option<Pick> {
    let across = |rectangle: &Rectangle| {
        if point.x < rectangle.x {
            rectangle.x - point.x
        } else {
            (point.x - (rectangle.x + rectangle.width)).max(0.0)
        }
    };
    let beside = bounds
        .iter()
        .enumerate()
        .filter(|(_, rectangle)| rectangle.y <= point.y && point.y < rectangle.y + rectangle.height)
        .min_by(|(_, one), (_, other)| across(one).total_cmp(&across(other)));
    if let Some((index, _)) = beside {
        return Some(Pick::On(index));
    }
    if let Some(index) = bounds.iter().position(|rectangle| point.y < rectangle.y) {
        return Some(Pick::Before(index));
    }
    bounds.len().checked_sub(1).map(Pick::End)
}

/// The end of text with the `lines`: after the last byte of its last line.
fn end(lines: &[&str]) -> Cursor {
    match lines.split_last() {
        Some((last, before)) => Cursor::new(before.len(), last.len()),
        None => Cursor::new(0, 0),
    }
}

/// The share of the leaf at `leaf`, whose text ends at `end`, of the selection from `from`
/// to `to`: all of it between the ends, from or up to an end in it. `None` for a leaf
/// outside the selection, or with nothing selected in it.
fn share((from, to): (Mark, Mark), leaf: usize, end: Cursor) -> Option<(Cursor, Cursor)> {
    if leaf < from.leaf || to.leaf < leaf {
        return None;
    }
    let start = if leaf == from.leaf {
        from.cursor()
    } else {
        Cursor::new(0, 0)
    };
    let stop = if leaf == to.leaf { to.cursor() } else { end };
    (start < stop).then_some((start, stop))
}

/// The text with the `lines` from `start` to `end`, its lines parted by line breaks. Ends
/// beyond the text, or inside a character, are moved back to the nearest place before.
fn excerpt(lines: &[&str], start: Cursor, end: Cursor) -> String {
    let mut parts = Vec::new();
    for (number, line) in lines.iter().enumerate() {
        if number < start.line || end.line < number {
            continue;
        }
        let from = if number == start.line {
            floor(line, start.index)
        } else {
            0
        };
        let to = if number == end.line {
            floor(line, end.index)
        } else {
            line.len()
        };
        parts.push(line.get(from..to.max(from)).unwrap_or_default());
    }
    parts.join("\n")
}

/// The last character boundary of `line` at or before the byte `index`.
fn floor(line: &str, index: usize) -> usize {
    (0..=index.min(line.len()))
        .rev()
        .find(|&at| line.is_char_boundary(at))
        .unwrap_or_default()
}

/// The bytes of the word, the run of white space or the single other character at the byte
/// `index` of `line`, as a start and an end. At the end of the line, it is the one before.
fn word(line: &str, index: usize) -> (usize, usize) {
    let index = floor(line, index);
    let at = line
        .get(index..)
        .and_then(|rest| rest.chars().next())
        .map(|character| (index, character))
        .or_else(|| line.char_indices().next_back());
    let Some((at, character)) = at else {
        return (0, 0);
    };
    let kind = class(character);
    if kind == Class::Other {
        return (at, at.saturating_add(character.len_utf8()));
    }
    let before = line.get(..at).unwrap_or_default();
    let start = before
        .char_indices()
        .rev()
        .take_while(|&(_, other)| class(other) == kind)
        .last()
        .map_or(at, |(start, _)| start);
    let after = line.get(at..).unwrap_or_default();
    let end = after
        .char_indices()
        .find(|&(_, other)| class(other) != kind)
        .map_or(line.len(), |(offset, _)| at.saturating_add(offset));
    (start, end)
}

/// What a double click treats a character as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// A letter, digit or underscore: part of a word.
    Word,
    /// White space.
    Space,
    /// Anything else, selected by itself.
    Other,
}

/// What a double click treats `character` as.
fn class(character: char) -> Class {
    if character.is_alphanumeric() || character == '_' {
        Class::Word
    } else if character.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn mark(leaf: usize, line: usize, index: usize) -> Mark {
        Mark { leaf, line, index }
    }

    fn rectangle(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), Size::new(width, height))
    }

    #[test]
    fn excerpt_takes_part_of_one_line() {
        let lines = ["hello world"];
        assert_eq!(
            excerpt(&lines, Cursor::new(0, 6), Cursor::new(0, 11)),
            "world"
        );
    }

    #[test]
    fn excerpt_spans_lines_with_line_breaks() {
        let lines = ["first", "second", "third"];
        assert_eq!(
            excerpt(&lines, Cursor::new(0, 3), Cursor::new(2, 2)),
            "st\nsecond\nth"
        );
    }

    #[test]
    fn excerpt_clamps_ends_beyond_the_text_and_inside_characters() {
        let lines = ["añb"];
        // Byte 2 is inside "ñ", byte 9 beyond the line.
        assert_eq!(excerpt(&lines, Cursor::new(0, 2), Cursor::new(0, 9)), "ñb");
    }

    #[test]
    fn end_is_after_the_last_line() {
        assert_eq!(end(&["ab", "cde"]), Cursor::new(1, 3));
        assert_eq!(end(&[]), Cursor::new(0, 0));
    }

    #[test]
    fn share_is_whole_for_leaves_between_the_ends() {
        let range = (mark(0, 0, 4), mark(2, 0, 3));
        assert_eq!(
            share(range, 1, Cursor::new(1, 5)),
            Some((Cursor::new(0, 0), Cursor::new(1, 5)))
        );
    }

    #[test]
    fn share_runs_from_and_up_to_the_ends() {
        let range = (mark(0, 0, 4), mark(2, 0, 3));
        assert_eq!(
            share(range, 0, Cursor::new(0, 9)),
            Some((Cursor::new(0, 4), Cursor::new(0, 9)))
        );
        assert_eq!(
            share(range, 2, Cursor::new(0, 9)),
            Some((Cursor::new(0, 0), Cursor::new(0, 3)))
        );
    }

    #[test]
    fn share_is_none_outside_the_selection_or_when_empty() {
        let range = (mark(1, 0, 4), mark(2, 0, 0));
        assert_eq!(share(range, 0, Cursor::new(0, 9)), None);
        assert_eq!(share(range, 3, Cursor::new(0, 9)), None);
        assert_eq!(share(range, 2, Cursor::new(0, 9)), None);
    }

    #[test]
    fn marks_compare_in_reading_order() {
        assert!(mark(0, 3, 9) < mark(1, 0, 0));
        assert!(mark(1, 0, 9) < mark(1, 1, 0));
        assert!(mark(1, 1, 2) < mark(1, 1, 3));
    }

    #[test]
    fn range_orders_the_ends_and_is_none_when_empty() {
        let selection = Selection {
            anchor: Some(mark(2, 0, 0)),
            head: Some(mark(0, 0, 5)),
            ..Selection::default()
        };
        assert_eq!(selection.range(), Some((mark(0, 0, 5), mark(2, 0, 0))));
        let empty = Selection {
            anchor: Some(mark(1, 0, 0)),
            head: Some(mark(1, 0, 0)),
            ..Selection::default()
        };
        assert_eq!(empty.range(), None);
    }

    #[test]
    fn pick_finds_the_leaf_at_the_points_height() {
        let bounds = [
            rectangle(0.0, 0.0, 100.0, 20.0),
            rectangle(0.0, 30.0, 100.0, 20.0),
        ];
        assert_eq!(pick(&bounds, Point::new(50.0, 35.0)), Some(Pick::On(1)));
    }

    #[test]
    fn pick_takes_the_nearest_leaf_across() {
        let row = [
            rectangle(0.0, 0.0, 60.0, 20.0),
            rectangle(100.0, 0.0, 200.0, 20.0),
        ];
        assert_eq!(pick(&row, Point::new(70.0, 5.0)), Some(Pick::On(0)));
        assert_eq!(pick(&row, Point::new(95.0, 5.0)), Some(Pick::On(1)));
        assert_eq!(pick(&row, Point::new(400.0, 5.0)), Some(Pick::On(1)));
    }

    #[test]
    fn pick_in_a_gap_is_before_the_next_leaf() {
        let bounds = [
            rectangle(0.0, 10.0, 100.0, 20.0),
            rectangle(0.0, 40.0, 100.0, 20.0),
        ];
        assert_eq!(pick(&bounds, Point::new(5.0, 2.0)), Some(Pick::Before(0)));
        assert_eq!(pick(&bounds, Point::new(5.0, 30.0)), Some(Pick::Before(1)));
    }

    #[test]
    fn pick_below_every_leaf_is_the_end() {
        let bounds = [
            rectangle(0.0, 0.0, 100.0, 20.0),
            rectangle(0.0, 30.0, 100.0, 20.0),
        ];
        assert_eq!(pick(&bounds, Point::new(5.0, 90.0)), Some(Pick::End(1)));
        assert_eq!(pick(&[], Point::new(5.0, 90.0)), None);
    }

    #[test]
    fn word_spans_letters_digits_and_underscores() {
        let line = "call foo_bar2(x)";
        assert_eq!(word(line, 7), (5, 13));
        assert_eq!(word(line, 5), (5, 13));
    }

    #[test]
    fn word_on_white_space_is_the_run_of_it() {
        assert_eq!(word("a   b", 2), (1, 4));
    }

    #[test]
    fn word_on_punctuation_is_that_character() {
        assert_eq!(word("f(x)", 1), (1, 2));
    }

    #[test]
    fn word_at_the_end_of_the_line_is_the_last_one() {
        assert_eq!(word("hello world", 11), (6, 11));
        assert_eq!(word("", 0), (0, 0));
    }

    #[test]
    fn word_handles_characters_wider_than_a_byte() {
        // "über" is five bytes; byte 1 is inside "ü".
        assert_eq!(word("über alles", 1), (0, 5));
    }

    /// A scrollable's state for operations that only read the offset they are handed.
    struct Unscrolled;

    impl Scrollable for Unscrolled {
        fn snap_to(&mut self, _offset: iced::widget::scrollable::RelativeOffset<Option<f32>>) {}

        fn scroll_to(&mut self, _offset: iced::widget::scrollable::AbsoluteOffset<Option<f32>>) {}

        fn scroll_by(
            &mut self,
            _offset: iced::widget::scrollable::AbsoluteOffset,
            _bounds: Rectangle,
            _content_bounds: Rectangle,
        ) {
        }
    }

    #[test]
    fn collect_moves_leaves_inside_a_scrollable_by_its_offset() {
        let mut collect = Collect::default();
        let mut leaf = Leaf::default();
        collect.scrollable(
            None,
            Rectangle::default(),
            Rectangle::default(),
            Vector::new(50.0, 0.0),
            &mut Unscrolled,
        );
        collect.traverse(&mut |operation| {
            operation.custom(None, rectangle(60.0, 10.0, 100.0, 20.0), &mut leaf);
        });
        // A leaf after the scrollable is where it is laid out.
        collect.custom(None, rectangle(60.0, 40.0, 100.0, 20.0), &mut leaf);
        let found: Vec<Rectangle> = collect.found.iter().map(|leaf| leaf.bounds).collect();
        assert_eq!(
            found,
            [
                rectangle(10.0, 10.0, 100.0, 20.0),
                rectangle(60.0, 40.0, 100.0, 20.0)
            ]
        );
    }

    /// The selection as the user makes it: mouse and keyboard events through iced's user
    /// interface, with a clipboard that records what is copied.
    mod interaction {
        use iced::advanced::renderer;
        use iced::advanced::renderer::Headless;
        use iced::widget::{button, column, space};
        use iced_test::runtime::user_interface::{self, UserInterface};

        use super::super::{area, rich, text};
        use super::*;

        /// The height of a line in the fixtures, so the leaves lie at known heights.
        const LINE: f32 = 20.0;

        #[derive(Debug, Clone, PartialEq)]
        enum Probe {
            Link(u8),
            Pressed,
        }

        #[derive(Default)]
        struct Recorded(Vec<String>);

        impl Clipboard for Recorded {
            fn read(&self, _kind: clipboard::Kind) -> Option<String> {
                None
            }

            fn write(&mut self, _kind: clipboard::Kind, contents: String) {
                self.0.push(contents);
            }
        }

        struct Harness<'a> {
            ui: UserInterface<'a, Probe, Theme, Renderer>,
            renderer: Renderer,
            clipboard: Recorded,
            messages: Vec<Probe>,
        }

        impl<'a> Harness<'a> {
            fn new(content: impl Into<Element<'a, Probe>>) -> Self {
                let mut renderer = iced_test::futures::futures::executor::block_on(
                    <Renderer as Headless>::new(Font::DEFAULT, Pixels(16.0), None),
                )
                .expect("a headless renderer");
                let ui = UserInterface::build(
                    content,
                    Size::new(400.0, 300.0),
                    user_interface::Cache::default(),
                    &mut renderer,
                );
                Self {
                    ui,
                    renderer,
                    clipboard: Recorded::default(),
                    messages: Vec::new(),
                }
            }

            fn send(&mut self, at: Point, event: Event) -> user_interface::State {
                let (state, _) = self.ui.update(
                    &[event],
                    mouse::Cursor::Available(at),
                    &mut self.renderer,
                    &mut self.clipboard,
                    &mut self.messages,
                );
                state
            }

            fn press(&mut self, at: Point) {
                self.send(
                    at,
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                );
            }

            fn release(&mut self, at: Point) {
                self.send(
                    at,
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                );
            }

            fn click(&mut self, at: Point) {
                self.press(at);
                self.release(at);
            }

            fn drag(&mut self, from: Point, to: Point) {
                self.press(from);
                self.send(to, Event::Mouse(mouse::Event::CursorMoved { position: to }));
                self.release(to);
            }

            fn key(&mut self, key: keyboard::Key, modifiers: keyboard::Modifiers) {
                self.send(
                    Point::ORIGIN,
                    Event::Keyboard(keyboard::Event::KeyPressed {
                        key: key.clone(),
                        modified_key: key,
                        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                        location: keyboard::Location::Standard,
                        modifiers,
                        repeat: false,
                        text: None,
                    }),
                );
            }

            /// The interface built anew from `content`, keeping the widgets' state the way
            /// iced does between two views.
            fn rebuild(self, content: impl Into<Element<'a, Probe>>) -> Self {
                let Self {
                    ui,
                    mut renderer,
                    clipboard,
                    messages,
                } = self;
                let cache = ui.into_cache();
                let ui =
                    UserInterface::build(content, Size::new(400.0, 300.0), cache, &mut renderer);
                Self {
                    ui,
                    renderer,
                    clipboard,
                    messages,
                }
            }

            /// Presses Ctrl+C (Cmd+C on macOS) and returns what it copied, if anything.
            fn copy(&mut self) -> Option<String> {
                let before = self.clipboard.0.len();
                self.key(
                    keyboard::Key::Character("c".into()),
                    keyboard::Modifiers::COMMAND,
                );
                self.clipboard.0.get(before).cloned()
            }

            /// The mouse cursor the interface shows at `at`.
            fn interaction(&mut self, at: Point) -> mouse::Interaction {
                match self.send(at, Event::Mouse(mouse::Event::CursorMoved { position: at })) {
                    user_interface::State::Updated {
                        mouse_interaction, ..
                    } => mouse_interaction,
                    user_interface::State::Outdated => mouse::Interaction::None,
                }
            }
        }

        fn line(words: &str) -> Element<'_, Probe> {
            text(words).line_height(Pixels(LINE)).into()
        }

        /// A point on the leaf at `row`, from the top, at `x`.
        fn on(row: u8, x: f32) -> Point {
            Point::new(x, LINE * f32::from(row) + LINE / 2.0)
        }

        /// Two leaves in an area as wide as the window, like the transcript's.
        fn two_lines<'a>() -> Element<'a, Probe> {
            area(column![line("hello world"), line("second line")].width(Length::Fill)).into()
        }

        #[test]
        fn a_drag_across_leaves_copies_both_one_per_line() {
            let mut ui = Harness::new(two_lines());
            ui.drag(on(0, 0.5), on(1, 399.0));
            assert_eq!(ui.copy().as_deref(), Some("hello world\nsecond line"));
        }

        #[test]
        fn a_drag_upwards_copies_the_same() {
            let mut ui = Harness::new(two_lines());
            ui.drag(on(1, 399.0), on(0, 0.5));
            assert_eq!(ui.copy().as_deref(), Some("hello world\nsecond line"));
        }

        #[test]
        fn a_drag_to_the_start_of_a_leaf_ends_the_one_before() {
            let mut ui = Harness::new(two_lines());
            ui.drag(on(0, 0.5), on(1, 0.0));
            assert_eq!(ui.copy().as_deref(), Some("hello world"));
        }

        #[test]
        fn nothing_is_copied_without_a_selection() {
            let mut ui = Harness::new(two_lines());
            assert_eq!(ui.copy(), None);
            ui.click(on(0, 30.0));
            assert_eq!(ui.copy(), None);
        }

        #[test]
        fn a_double_click_selects_a_word_and_a_triple_click_the_line() {
            let mut ui = Harness::new(two_lines());
            ui.click(on(1, 1.0));
            ui.click(on(1, 1.0));
            assert_eq!(ui.copy().as_deref(), Some("second"));
            ui.click(on(1, 1.0));
            assert_eq!(ui.copy().as_deref(), Some("second line"));
        }

        #[test]
        fn escape_clears_the_selection() {
            let mut ui = Harness::new(two_lines());
            ui.drag(on(0, 0.5), on(1, 399.0));
            ui.key(
                keyboard::Key::Named(key::Named::Escape),
                keyboard::Modifiers::default(),
            );
            assert_eq!(ui.copy(), None);
        }

        /// `two_lines` in an area of the given epoch.
        fn two_lines_in_epoch<'a>(epoch: usize) -> Element<'a, Probe> {
            area(column![line("hello world"), line("second line")].width(Length::Fill))
                .epoch(epoch)
                .into()
        }

        #[test]
        fn a_new_epoch_clears_the_selection() {
            let mut ui = Harness::new(two_lines_in_epoch(0));
            ui.drag(on(0, 0.5), on(1, 399.0));
            let mut ui = ui.rebuild(two_lines_in_epoch(1));
            assert_eq!(ui.copy(), None);
        }

        #[test]
        fn the_same_epoch_keeps_the_selection() {
            let mut ui = Harness::new(two_lines_in_epoch(0));
            ui.drag(on(0, 0.5), on(1, 399.0));
            let mut ui = ui.rebuild(two_lines_in_epoch(0));
            assert_eq!(ui.copy().as_deref(), Some("hello world\nsecond line"));
        }

        #[test]
        fn c_without_the_command_key_copies_nothing() {
            let mut ui = Harness::new(two_lines());
            ui.drag(on(0, 0.5), on(1, 399.0));
            ui.key(
                keyboard::Key::Character("c".into()),
                keyboard::Modifiers::default(),
            );
            assert!(ui.clipboard.0.is_empty());
        }

        #[test]
        fn a_press_outside_the_area_clears_the_selection() {
            let content = column![two_lines(), space().height(100)];
            let mut ui = Harness::new(content);
            ui.drag(on(0, 0.5), on(1, 399.0));
            ui.click(on(4, 1.0));
            assert_eq!(ui.copy(), None);
        }

        #[test]
        fn a_button_in_the_area_takes_its_click_and_clears_the_selection() {
            let pressable = button(iced::widget::text("press").line_height(Pixels(LINE)))
                .padding(0)
                .on_press(Probe::Pressed);
            let mut ui = Harness::new(area(column![line("hello world"), pressable]));
            ui.drag(on(0, 0.5), on(0, 399.0));
            ui.click(on(1, 5.0));
            assert_eq!(ui.messages, [Probe::Pressed]);
            assert_eq!(ui.copy(), None);
        }

        #[test]
        fn a_click_on_a_link_follows_it_and_a_drag_from_it_does_not() {
            let link = || {
                rich(vec![Span::new("link").link(7_u8)])
                    .line_height(Pixels(LINE))
                    .on_link_click(Probe::Link)
            };
            let mut ui = Harness::new(area(column![link(), line("after")]));
            ui.click(on(0, 2.0));
            assert_eq!(ui.messages, [Probe::Link(7)]);
            ui.drag(on(0, 2.0), on(1, 2.0));
            assert_eq!(ui.messages, [Probe::Link(7)]);
        }

        #[test]
        fn the_cursor_is_a_text_cursor_over_text_and_a_pointer_over_a_link() {
            let link = rich(vec![Span::new("link").link(7_u8)])
                .line_height(Pixels(LINE))
                .on_link_click(Probe::Link);
            let mut ui = Harness::new(area(column![link, line("after")]));
            assert_eq!(ui.interaction(on(0, 2.0)), mouse::Interaction::Pointer);
            assert_eq!(ui.interaction(on(1, 2.0)), mouse::Interaction::Text);
            assert_eq!(ui.interaction(on(1, 390.0)), mouse::Interaction::None);
        }

        /// The colour of the pixel at `at` in a dark window of `ui`, as RGB.
        fn pixel(ui: &mut Harness<'_>, at: Point) -> [u8; 3] {
            let theme = crate::theme::theme(iced::theme::Mode::Dark);
            let defaults = renderer::Style {
                text_color: crate::theme::DARK.text,
            };
            ui.ui.draw(
                &mut ui.renderer,
                &theme,
                &defaults,
                mouse::Cursor::Unavailable,
            );
            let shot = ui
                .renderer
                .screenshot(Size::new(400, 300), 1.0, crate::theme::DARK.bg);
            // Truncation is meant: the pixel's row and column.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let start = ((at.y as usize) * 400 + at.x as usize) * 4;
            [shot[start], shot[start + 1], shot[start + 2]]
        }

        fn rgb(colour: iced::Color) -> [u8; 3] {
            let [red, green, blue, _] = colour.into_rgba8();
            [red, green, blue]
        }

        #[test]
        fn the_selection_is_drawn_behind_the_selected_text_only() {
            let decorated = rich::<(), Probe>(vec![
                Span::new("marked")
                    .underline(true)
                    .strikethrough(true)
                    .background(iced::Color::WHITE),
                Span::new(" plain"),
            ])
            .line_height(Pixels(LINE));
            let mut ui = Harness::new(area(
                column![decorated, line("second line")].width(Length::Fill),
            ));
            let selected = rgb(crate::theme::DARK.text_selection);
            // Above the glyphs, at the top of the selected second line.
            let probe = Point::new(2.0, LINE + 1.0);
            assert_ne!(pixel(&mut ui, probe), selected);
            ui.drag(on(1, 0.5), on(1, 399.0));
            assert_eq!(pixel(&mut ui, probe), selected);
            // The first line is not selected.
            assert_ne!(pixel(&mut ui, Point::new(80.0, 1.0)), selected);
        }

        #[test]
        fn a_leaf_that_grows_inside_the_selection_is_selected_whole() {
            let three = |middle: &'static str| {
                area(column![line("first"), line(middle), line("third line")].width(Length::Fill))
            };
            let mut ui = Harness::new(three("ab"));
            ui.drag(on(0, 0.5), on(2, 20.0));
            let selected = rgb(crate::theme::DARK.text_selection);
            // Beyond "ab", where only the grown text reaches.
            let probe = Point::new(150.0, LINE + 1.0);
            assert_ne!(pixel(&mut ui, probe), selected);

            let mut ui = ui.rebuild(three("ab and much more text after it"));
            assert_eq!(pixel(&mut ui, probe), selected);
        }

        #[test]
        fn the_cursor_stays_a_text_cursor_while_dragging() {
            let mut ui = Harness::new(column![two_lines(), space().height(100)]);
            ui.press(on(0, 0.5));
            assert_eq!(ui.interaction(on(5, 390.0)), mouse::Interaction::Text);
        }
    }
}
