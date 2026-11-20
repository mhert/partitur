//! The MCP App widget under a tool call: the engine's last frame of the widget's view, a row
//! that says it is loading, or a muted line that says why it is not shown.
//!
//! The frame fills the transcript's width at the view's height. Until the engine paints the
//! first frame, a pending row of that height stands in its place. The view reports the width
//! it is laid out at with the window's scale factor, and whether it is in the viewport, each
//! time that changes. Over the frame, the pointer's moves, presses, releases and wheel turns
//! go to the view with the pointer's place in it, and the pointer leaving it as well. A
//! press, its release and a wheel turn are taken there: the wheel scrolls the widget, not the
//! transcript, so the widget stays under the pointer. A wheel's notch is 40 pixels; a
//! touchpad's amounts, which come in physical pixels, are divided by the scale factor and go
//! as precise. Nothing reaches the widget from the keyboard: widgets are read-only. Over the
//! frame the pointer takes the look the page last asked for, such as the hand over a link or
//! the I-beam over text.
//!
//! iced's scrollable keeps the wheel to itself once it has scrolled, until the wheel rests for
//! a moment or the pointer moves on a while later, and gives its content none of it meanwhile.
//! Scrolling the transcript until a widget is under the pointer and turning on would then
//! scroll past the widget. So the transcript sits in [`wheel_to_widgets`], which asks its
//! frames, before each turn of the wheel, whether the pointer is over one of them; a frame
//! knows from the last event it saw, with the transcript's scrolling taken into account. When
//! one is, the scrollable is first handed the release of a mouse button no device has, which
//! ends its hold the way a real release does, and then the turn, which it passes on to the
//! frame. Nothing in the transcript acts on that button. Asking the frames, instead of always
//! ending the hold, keeps it everywhere else: elsewhere the wheel scrolls the transcript on,
//! also over the rows that scroll sideways. While a mouse button is held, as when text is
//! selected or the scrollbar dragged, the hold is left alone. Patching iced's scrollable, the
//! other way, would mean keeping a copy of it.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Shell, Widget, renderer};
use iced::widget::{container, image, sensor, text};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, overlay};
use partitur_core::widget_host::{ViewId, WidgetCursor, WidgetInput};
use partitur_core::{Entry, SessionId};

use crate::app::Message;
use crate::widgets::{LOADING, Look, View, WidgetViewMessage, Widgets, id};
use crate::{style, theme};

/// The space between a tool call's row and its widget.
pub(super) const GAP: f32 = 6.0;

/// How many pixels the wheel scrolls per line.
const LINE_SCROLL: f32 = 40.0;

/// What shows under the tool call `call` of `session` for its widget, if it has one.
pub(super) fn below<'a>(
    widgets: &'a Widgets,
    session: &SessionId,
    call: &'a Entry,
) -> Option<Element<'a, Message>> {
    let Entry::ToolCall {
        id: call,
        widget: Some(slot),
        ..
    } = call
    else {
        return None;
    };
    Some(match widgets.look(session, call, slot) {
        Look::Pending => pending(Length::Shrink),
        Look::Unavailable(error) => container(
            text(format!("Widget unavailable: {error}"))
                .size(theme::SMALL)
                .style(style::muted),
        )
        .padding([0, 12])
        .into(),
        Look::View(view) => shown(view, widgets.scale()),
    })
}

/// The row that says the widget is loading, `height` tall.
fn pending<'a>(height: Length) -> Element<'a, Message> {
    container(text(LOADING).size(theme::SMALL).style(style::muted))
        .padding([0, 12])
        .width(Length::Fill)
        .height(height)
        .center_y(height)
        .into()
}

/// The open `view` in a window of the scale factor `scale`: its last frame, or the pending
/// row until there is one, reporting its width and whether it is in the viewport.
fn shown(view: &View, scale: f32) -> Element<'_, Message> {
    let height = Length::Fixed(f32::from(view.height));
    let at = view.id;
    let content = match &view.frame {
        Some(frame) => Element::from(Surface {
            content: image(frame.clone())
                .width(Length::Fill)
                .height(height)
                .into(),
            view: at,
            cursor: view.cursor,
            scale,
        }),
        None => pending(height),
    };
    let message = move |message| Message::WidgetView(at, message);
    // Without `on_show` a sensor reports every size it is laid out at, wherever it is.
    let sized = sensor(content).key(at).on_resize(move |size: Size| {
        message(WidgetViewMessage::Sized {
            width: size.width,
            scale,
        })
    });
    let visible = sensor(sized)
        .key(at)
        .on_show(move |_| message(WidgetViewMessage::Visible(true)))
        .on_hide(message(WidgetViewMessage::Visible(false)));
    container(visible).id(id(at)).width(Length::Fill).into()
}

/// The pointer's look the window shows for the look `cursor` a page asks for.
fn interaction(cursor: WidgetCursor) -> mouse::Interaction {
    match cursor {
        WidgetCursor::Default => mouse::Interaction::Idle,
        WidgetCursor::Pointer => mouse::Interaction::Pointer,
        WidgetCursor::Text => mouse::Interaction::Text,
        WidgetCursor::Grab => mouse::Interaction::Grab,
        WidgetCursor::Grabbing => mouse::Interaction::Grabbing,
        WidgetCursor::NotAllowed => mouse::Interaction::NotAllowed,
        WidgetCursor::ResizeHorizontal => mouse::Interaction::ResizingHorizontally,
        WidgetCursor::ResizeVertical => mouse::Interaction::ResizingVertically,
        WidgetCursor::Crosshair => mouse::Interaction::Crosshair,
    }
}

/// The number the engine knows `button` by: 0 for the left button, 1 for the middle one
/// and 2 for the right one. The other buttons do not reach widgets.
fn button_number(button: mouse::Button) -> Option<u8> {
    match button {
        mouse::Button::Left => Some(0),
        mouse::Button::Middle => Some(1),
        mouse::Button::Right => Some(2),
        mouse::Button::Back | mouse::Button::Forward | mouse::Button::Other(_) => None,
    }
}

/// A view's frame that hands the pointer's input over it to the view, as
/// [`WidgetInput`]s at the pointer's place in the frame.
struct Surface<'a> {
    /// The frame.
    content: Element<'a, Message>,
    /// The view the input goes to.
    view: ViewId,
    /// The pointer's look the page asks for.
    cursor: WidgetCursor,
    /// The window's scale factor.
    scale: f32,
}

/// Where the pointer is for a [`Surface`].
#[derive(Default)]
struct Pointer {
    /// Whether the pointer was over the frame when it last moved.
    hovered: bool,
    /// Whether the pointer was over the frame at the last event the frame saw, which may
    /// be one that moved the frame under it, such as a redraw after a scroll.
    over: bool,
}

impl Widget<Message, Theme, Renderer> for Surface<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Pointer>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Pointer::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        match tree.children.first_mut() {
            Some(child) => self.content.as_widget_mut().layout(child, renderer, limits),
            None => layout::Node::new(Size::ZERO),
        }
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
        if let Some(child) = tree.children.first() {
            self.content
                .as_widget()
                .draw(child, renderer, theme, style, layout, cursor, viewport);
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
        if cursor.is_over(layout.bounds()) {
            interaction(self.cursor)
        } else {
            mouse::Interaction::None
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.custom(None, layout.bounds(), tree.state.downcast_mut::<Pointer>());
        if let Some(child) = tree.children.first_mut() {
            self.content
                .as_widget_mut()
                .operate(child, layout, renderer, operation);
        }
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
        let bounds = layout.bounds();
        let at = cursor.position_in(bounds);
        let pointer = tree.state.downcast_mut::<Pointer>();
        pointer.over = at.is_some();
        let Event::Mouse(event) = event else {
            return;
        };
        let input = match (*event, at) {
            (mouse::Event::CursorMoved { .. }, Some(at)) => {
                pointer.hovered = true;
                WidgetInput::Move { x: at.x, y: at.y }
            }
            (mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft, None)
                if pointer.hovered =>
            {
                pointer.hovered = false;
                WidgetInput::Leave
            }
            (mouse::Event::ButtonPressed(button), Some(at)) => {
                let Some(button) = button_number(button) else {
                    return;
                };
                shell.capture_event();
                WidgetInput::Press {
                    x: at.x,
                    y: at.y,
                    button,
                }
            }
            (mouse::Event::ButtonReleased(button), Some(at)) => {
                let Some(button) = button_number(button) else {
                    return;
                };
                shell.capture_event();
                WidgetInput::Release {
                    x: at.x,
                    y: at.y,
                    button,
                }
            }
            (mouse::Event::WheelScrolled { delta }, Some(at)) => {
                let Vector { x: dx, y: dy } = match delta {
                    mouse::ScrollDelta::Lines { x, y } => Vector::new(x, y) * LINE_SCROLL,
                    mouse::ScrollDelta::Pixels { x, y } => Vector::new(x, y) * (1.0 / self.scale),
                };
                shell.capture_event();
                WidgetInput::Wheel {
                    x: at.x,
                    y: at.y,
                    dx,
                    dy,
                    precise: matches!(delta, mouse::ScrollDelta::Pixels { .. }),
                }
            }
            _ => return,
        };
        shell.publish(Message::WidgetView(
            self.view,
            WidgetViewMessage::Input(input),
        ));
    }
}

impl<'a> From<Surface<'a>> for Element<'a, Message> {
    fn from(surface: Surface<'a>) -> Self {
        Element::new(surface)
    }
}

/// `transcript`, the scrollable of the transcript, with the wheel over a widget's frame handed
/// to the frame at once; see the module documentation.
pub(super) fn wheel_to_widgets(transcript: Element<'_, Message>) -> Element<'_, Message> {
    Element::new(WheelToWidgets {
        content: transcript,
    })
}

/// The wrapper [`wheel_to_widgets`] puts around the transcript.
struct WheelToWidgets<'a> {
    content: Element<'a, Message>,
}

/// The mouse buttons held, as the [`WheelToWidgets`] saw them go down and up.
#[derive(Default)]
struct Held(Vec<mouse::Button>);

/// The release of a button no mouse has, which ends a scrollable's hold on the wheel.
const NO_BUTTON: Event = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Other(u16::MAX)));

/// Asks every [`Surface`] whether the pointer is over it.
struct PointerOverFrame(bool);

impl Operation for PointerOverFrame {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(
        &mut self,
        _id: Option<&iced::widget::Id>,
        _bounds: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if state
            .downcast_ref::<Pointer>()
            .is_some_and(|pointer| pointer.over)
        {
            self.0 = true;
        }
    }

    fn finish(&self) -> operation::Outcome<()> {
        operation::Outcome::None
    }
}

impl Widget<Message, Theme, Renderer> for WheelToWidgets<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Held>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Held::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
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
        match tree.children.first_mut() {
            Some(child) => self.content.as_widget_mut().layout(child, renderer, limits),
            None => layout::Node::new(Size::ZERO),
        }
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
        if let Some(child) = tree.children.first() {
            self.content
                .as_widget()
                .draw(child, renderer, theme, style, layout, cursor, viewport);
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
        tree.children
            .first()
            .map_or(mouse::Interaction::None, |child| {
                self.content
                    .as_widget()
                    .mouse_interaction(child, layout, cursor, viewport, renderer)
            })
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(child) = tree.children.first_mut() {
            self.content
                .as_widget_mut()
                .operate(child, layout, renderer, operation);
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
        let held = &mut tree.state.downcast_mut::<Held>().0;
        let mut wheel_to_frame = false;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) if !held.contains(button) => {
                held.push(*button);
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                held.retain(|other| other != button);
            }
            Event::Mouse(mouse::Event::WheelScrolled { .. }) => wheel_to_frame = held.is_empty(),
            _ => {}
        }
        let Some(child) = tree.children.first_mut() else {
            return;
        };
        if wheel_to_frame {
            let mut over = PointerOverFrame(false);
            self.content
                .as_widget_mut()
                .operate(child, layout, renderer, &mut over);
            if over.0 {
                // Nothing in the transcript acts on this release, so nothing it says is kept.
                let mut said = Vec::new();
                self.content.as_widget_mut().update(
                    child,
                    &NO_BUTTON,
                    layout,
                    cursor,
                    renderer,
                    clipboard,
                    &mut Shell::new(&mut said),
                    viewport,
                );
            }
        }
        self.content.as_widget_mut().update(
            child, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let child = tree.children.first_mut()?;
        self.content
            .as_widget_mut()
            .overlay(child, layout, renderer, viewport, translation)
    }
}
