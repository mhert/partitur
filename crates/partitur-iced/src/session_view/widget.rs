//! The MCP App widget under a tool call: the engine's last frame of the widget's view, a row
//! that says it is loading, or a muted line that says why it is not shown.
//!
//! The frame fills the transcript's width at the view's height. Until the engine paints the
//! first frame, a pending row of that height stands in its place. The view reports the width
//! it is laid out at with the window's scale factor, and whether it is in the viewport, each
//! time that changes. Over the frame, the pointer's moves, presses, releases and wheel turns
//! go to the view with the pointer's place in it, and the pointer leaving it as well. A
//! press and its release are taken there, while the wheel also scrolls the transcript.
//! Nothing reaches the widget from the keyboard: widgets are read-only.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, renderer};
use iced::widget::{container, image, sensor, text};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse};
use partitur_core::widget_host::{ViewId, WidgetInput};
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
}

/// Whether the pointer was over the [`Surface`] when it last moved.
#[derive(Default)]
struct Hovered(bool);

impl Widget<Message, Theme, Renderer> for Surface<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Hovered>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Hovered::default())
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
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Event::Mouse(event) = event else {
            return;
        };
        let bounds = layout.bounds();
        let at = cursor.position_in(bounds);
        let hovered = tree.state.downcast_mut::<Hovered>();
        let input = match (*event, at) {
            (mouse::Event::CursorMoved { .. }, Some(at)) => {
                hovered.0 = true;
                WidgetInput::Move { x: at.x, y: at.y }
            }
            (mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft, None) if hovered.0 => {
                hovered.0 = false;
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
                    mouse::ScrollDelta::Pixels { x, y } => Vector::new(x, y),
                };
                WidgetInput::Wheel {
                    x: at.x,
                    y: at.y,
                    dx,
                    dy,
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
