//! The MCP App widgets of the shown session's tool calls, and what passes between them and
//! the widget engine behind the [`WidgetHost`] port.
//!
//! A tool call whose widget was read ([`WidgetSlot::Ready`]) gets a view of the engine once
//! its entry is in the transcript of the session the app shows, the current one. [`Widgets`]
//! names each view with a [`ViewId`] of its own, one per session and tool call, and opens it
//! on the widget's HTML under the content security policy its resource declares, in the
//! desktop's theme (light when the desktop prefers light, dark otherwise). When another
//! session is shown, or none because the shown one was deleted, the views of the session
//! before are closed. Shown again, the session opens them anew from the widget stored with
//! it, with the stored input and result.
//!
//! What a page sends goes to its view's [`WidgetBridge`]. The replies are posted back to the
//! page, a link it asks for opens with the session's link opener, and so does a link the user
//! follows in the page, which the engine reports instead of letting the page navigate; either
//! way only `http`, `https` and `mailto` links open, and only on the user's click: each press
//! on the view lets one link through for a moment after it or its release ([`LinkGate`]), and
//! a link without one is dropped. A height the page reports
//! becomes the view's, kept within 80 to 640 logical pixels. Until it reports one the view is
//! 160 pixels tall, and a reported height of 0 (the bridge drops it) keeps the height the view
//! has. The bridge also learns the width each view is laid out at and the greatest height,
//! which the page sees as its container's dimensions. A tool result that arrives after the
//! view opened is handed to the bridge, and so is every change of the desktop's theme. The
//! engine's newest picture of a view is kept as an image for the transcript to draw, and the
//! pointer's look its page last asked for as the look over that image.
//!
//! A picture is shown only once the renderer has allocated it ([`image::allocate`]): iced
//! uploads a large image on a worker and draws nothing for it until that is done, so drawing a
//! new picture at once would blank the view for a frame or more each time the page paints.
//! Until then the view keeps showing the picture before, whose allocation it holds so that it
//! stays in the renderer's memory. One picture of a view is allocated at a time; pictures the
//! engine paints meanwhile wait in its slot, the newest in place of the older, and the newest
//! goes next. The allocation of a view closed since is dropped.
//!
//! The engine scrolls a page by whole pixels, so the parts of a pixel the wheel turns a view
//! by, which a touchpad gives, are kept with the view and added to its next turn.
//!
//! Only views on screen paint. A view is hidden when its row is not laid out or another tab
//! is shown, and the view reports when it leaves the viewport and comes back
//! ([`WidgetViewMessage::Visible`]).
//!
//! A widget that cannot be shown falls back to its tool call's text with one muted line that
//! says why: when its resource could not be read, the engine refused to open it, failed it
//! later or could not take a command for it. Such a view is not opened again while its
//! session stays shown. A widget that was still loading when the session closed never
//! finishes loading, so only a load a live update reported in this run shows as pending.
//!
//! Every engine command is sent from `update`, never from `view`.

use std::collections::{HashMap, HashSet};

use iced::Task;
use iced::theme::Mode;
use iced::widget::Id;
use iced::widget::image::{self, Allocation, Handle};
use partitur_core::widget_bridge::{
    BridgeOut, LinkGate, WidgetBridge, WidgetTheme, content_security_policy, is_web_link,
};
use partitur_core::widget_host::{
    LatestFrame, ViewId, WidgetCursor, WidgetHost, WidgetHostError, WidgetHostEvent, WidgetInput,
    WidgetPage,
};
use partitur_core::{BackendEvent, Entry, SessionId, ToolCallId, Update, Widget, WidgetSlot};

use crate::app::{self, Message, State, Tab};
use crate::{link, session_view};

/// The least height of a view, in logical pixels.
pub(crate) const MIN_HEIGHT: u16 = 80;

/// The greatest height of a view, in logical pixels.
pub(crate) const MAX_HEIGHT: u16 = 640;

/// The height of a view until its widget reports one, in logical pixels.
pub(crate) const DEFAULT_HEIGHT: u16 = 160;

/// What stands in for a widget that is not painted yet.
pub(crate) const LOADING: &str = "Loading widget…";

/// Why a widget restored as loading is not shown.
pub(crate) const STILL_LOADING: &str = "the widget was still loading when the session closed";

/// What a view of the transcript reports about itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WidgetViewMessage {
    /// The view was laid out this wide, in logical pixels, in a window of this scale factor.
    Sized {
        /// The width in logical pixels.
        width: f32,
        /// The window's scale factor.
        scale: f32,
    },
    /// The pointer did something over the view.
    Input(WidgetInput),
    /// The view came into the viewport (`true`) or left it (`false`).
    Visible(bool),
}

/// The widgets of the shown session: its open views, the widgets that cannot be shown, and
/// the tool calls whose widget a live update reported as loading, in any session.
#[derive(Debug)]
pub struct Widgets {
    /// The number of the next view.
    next: u64,
    /// The session the views and failures belong to.
    shown: Option<SessionId>,
    /// The open views, by tool call.
    views: HashMap<ToolCallId, View>,
    /// The tool call of each open view.
    calls: HashMap<ViewId, ToolCallId>,
    /// Why the widget of a tool call cannot be shown, when the engine gave up on it.
    failed: HashMap<ToolCallId, String>,
    /// The tool calls whose widget a live update of this run reported as loading.
    live: HashMap<SessionId, HashSet<ToolCallId>>,
    /// The window's scale factor.
    scale: f32,
}

impl Default for Widgets {
    fn default() -> Self {
        Self {
            next: 1,
            shown: None,
            views: HashMap::new(),
            calls: HashMap::new(),
            failed: HashMap::new(),
            live: HashMap::new(),
            scale: 1.0,
        }
    }
}

/// One open view of a widget.
#[derive(Debug)]
pub(crate) struct View {
    /// Names the view to the engine.
    pub(crate) id: ViewId,
    /// The host side of the conversation with the page.
    bridge: WidgetBridge,
    /// The tool result the bridge knows.
    result: Option<String>,
    /// The height, in logical pixels.
    pub(crate) height: u16,
    /// The frame shown: the newest picture of the engine's that the renderer has allocated.
    pub(crate) frame: Option<Handle>,
    /// What keeps the shown frame in the renderer's memory, so it draws at once every time.
    allocation: Option<Allocation>,
    /// Whether a picture is being allocated.
    allocating: bool,
    /// The slot of a picture that came while another was being allocated.
    waiting: Option<LatestFrame>,
    /// The parts of a pixel the wheel turned the view by and the engine was not told of yet,
    /// across and down.
    wheel_rest: (f32, f32),
    /// The pointer's look the page last asked for over the view.
    pub(crate) cursor: WidgetCursor,
    /// Whether a link from the view may open: only on the user's click, one per press.
    links: LinkGate,
    /// The width and scale factor the view was last laid out at.
    size: Option<(f32, f32)>,
    /// Whether the engine paints the view.
    visible: bool,
}

/// What the transcript shows for a tool call's widget.
pub(crate) enum Look<'a> {
    /// A row that says the widget is loading.
    Pending,
    /// The open view.
    View(&'a View),
    /// The line that says why the widget is not shown.
    Unavailable(&'a str),
}

impl Widgets {
    /// What the transcript shows for the widget in `slot` of the tool call `call` of
    /// `session`.
    pub(crate) fn look<'a>(
        &'a self,
        session: &SessionId,
        call: &ToolCallId,
        slot: &'a WidgetSlot,
    ) -> Look<'a> {
        match slot {
            WidgetSlot::Loading { .. } => {
                let live = self
                    .live
                    .get(session)
                    .is_some_and(|calls| calls.contains(call));
                if live {
                    Look::Pending
                } else {
                    Look::Unavailable(STILL_LOADING)
                }
            }
            WidgetSlot::Failed { error, .. } => Look::Unavailable(error),
            WidgetSlot::Ready(_) if self.shown.as_ref() != Some(session) => Look::Pending,
            WidgetSlot::Ready(_) => match (self.failed.get(call), self.views.get(call)) {
                (Some(error), _) => Look::Unavailable(error),
                (None, Some(view)) => Look::View(view),
                (None, None) => Look::Pending,
            },
        }
    }

    /// The window's scale factor.
    pub(crate) fn scale(&self) -> f32 {
        self.scale
    }

    /// The open view of the tool call `call` of `session`.
    #[cfg(test)]
    pub(crate) fn view(&self, session: &SessionId, call: &ToolCallId) -> Option<&View> {
        self.views
            .get(call)
            .filter(|_| self.shown.as_ref() == Some(session))
    }

    /// Opens the view of `widget` for the tool call `call`, whose input and result are known
    /// as far as they are; a refusal is remembered as the widget's failure.
    fn open(
        &mut self,
        host: &dyn WidgetHost,
        call: &ToolCallId,
        widget: &Widget,
        (input, result): (&Option<String>, &Option<String>),
        theme: WidgetTheme,
    ) {
        let id = ViewId(self.next);
        self.next = self.next.saturating_add(1);
        let page = WidgetPage {
            html: widget.html.clone(),
            csp: content_security_policy(&widget.meta),
            theme,
        };
        if let Err(error) = host.open(id, page) {
            self.failed.insert(call.clone(), error.to_string());
            return;
        }
        self.calls.insert(id, call.clone());
        self.views.insert(
            call.clone(),
            View {
                id,
                bridge: WidgetBridge::new(
                    input.clone(),
                    result.clone(),
                    theme,
                    u32::from(MAX_HEIGHT),
                ),
                result: result.clone(),
                height: DEFAULT_HEIGHT,
                frame: None,
                allocation: None,
                allocating: false,
                waiting: None,
                wheel_rest: (0.0, 0.0),
                cursor: WidgetCursor::Default,
                links: LinkGate::default(),
                size: None,
                visible: true,
            },
        );
    }

    /// Closes the view of `call`, if it is open.
    fn close(&mut self, host: &dyn WidgetHost, call: &ToolCallId) {
        if let Some(view) = self.views.remove(call) {
            self.calls.remove(&view.id);
            if let Err(error) = host.close(view.id) {
                tracing::warn!(%error, "a widget view could not be closed");
            }
        }
    }

    /// Closes the view of `call` and remembers `error` as the reason it is not shown.
    fn fail(&mut self, host: &dyn WidgetHost, call: &ToolCallId, error: String) {
        tracing::warn!(%error, "a widget cannot be shown");
        self.close(host, call);
        self.failed.insert(call.clone(), error);
    }

    /// Carries out what the bridge of `call` asked for: posts to the page and new heights go
    /// to the engine, and the links come back for the caller to open. A command the engine
    /// refuses fails the view, and nothing after it is carried out.
    fn carry_out(
        &mut self,
        host: &dyn WidgetHost,
        call: &ToolCallId,
        outs: Vec<BridgeOut>,
    ) -> Vec<String> {
        let mut links = Vec::new();
        for out in outs {
            let Some(view) = self.views.get_mut(call) else {
                break;
            };
            let sent = match out {
                BridgeOut::Send(json) => host.post(view.id, json),
                BridgeOut::OpenLink(url) => {
                    links.push(url);
                    Ok(())
                }
                BridgeOut::Resize { height } => {
                    view.height =
                        u16::try_from(height.clamp(u32::from(MIN_HEIGHT), u32::from(MAX_HEIGHT)))
                            .unwrap_or(MAX_HEIGHT);
                    resize(host, view)
                }
            };
            if let Err(error) = sent {
                self.fail(host, call, error.to_string());
                break;
            }
        }
        links
    }

    /// The `links` of the view of `call` that may open now: one per press of the user's on the
    /// view ([`LinkGate`]). The rest open nothing.
    fn admitted(&mut self, call: &ToolCallId, links: Vec<String>) -> Vec<String> {
        let Some(view) = self.views.get_mut(call) else {
            return Vec::new();
        };
        let now = std::time::Instant::now();
        links
            .into_iter()
            .filter(|url| {
                let admitted = view.links.admit(now);
                if !admitted {
                    tracing::debug!(view = view.id.0, %url, "a widget's link without a click was dropped");
                }
                admitted
            })
            .collect()
    }

    /// Runs `command` on the view of `call` with the engine; a refusal fails the view.
    fn command(
        &mut self,
        host: &dyn WidgetHost,
        call: &ToolCallId,
        command: impl FnOnce(&dyn WidgetHost, &mut View) -> Result<(), WidgetHostError>,
    ) {
        let Some(view) = self.views.get_mut(call) else {
            return;
        };
        if let Err(error) = command(host, view) {
            self.fail(host, call, error.to_string());
        }
    }
}

/// Tells the engine the size `view` was laid out at, once it was laid out.
fn resize(host: &dyn WidgetHost, view: &View) -> Result<(), WidgetHostError> {
    let Some((width, scale)) = view.size else {
        return Ok(());
    };
    host.resize(view.id, pixels(width), u32::from(view.height), scale)
}

/// A width in whole logical pixels; a negative one is none.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a laid-out width is far below u32::MAX and clamped at zero"
)]
fn pixels(width: f32) -> u32 {
    width.max(0.0).round() as u32
}

/// The theme widgets are told to follow for the desktop's preference `mode`.
fn theme_of(mode: Mode) -> WidgetTheme {
    match mode {
        Mode::Light => WidgetTheme::Light,
        Mode::Dark | Mode::None => WidgetTheme::Dark,
    }
}

/// The id of the transcript's element that shows `view`.
pub(crate) fn id(view: ViewId) -> Id {
    Id::from(format!("widget-{}", view.0))
}

/// Remembers the tool calls whose widget `event` reports as loading, before the app applies
/// it: those are being read now, unlike a load restored from the store.
pub(crate) fn observe(widgets: &mut Widgets, event: &BackendEvent) {
    let BackendEvent::Update { session, update } = event else {
        return;
    };
    let (Update::ToolCall {
        id,
        widget: Some(WidgetSlot::Loading { .. }),
        ..
    }
    | Update::ToolCallUpdate {
        id,
        widget: Some(WidgetSlot::Loading { .. }),
        ..
    }) = update
    else {
        return;
    };
    widgets
        .live
        .entry(session.clone())
        .or_default()
        .insert(id.clone());
}

/// Brings the views up to date with the app after a message: the views of a session no longer
/// shown are closed, a widget that was read gets its view, a result that arrived goes to its
/// view's page, and a view whose row is not laid out, or every view while another tab is
/// shown, is hidden.
pub(crate) fn sync(state: &mut State) {
    let model = state.app.session_view();
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    let shown = model.as_ref().map(|model| model.id.clone());
    if widgets.shown != shown {
        let open: Vec<ToolCallId> = widgets.views.keys().cloned().collect();
        for call in &open {
            widgets.close(host, call);
        }
        widgets.failed.clear();
        widgets.shown = shown;
    }
    let Some(model) = model else {
        return;
    };
    let theme = theme_of(state.mode);
    let mut ready = HashSet::new();
    for entry in model.entries {
        let Entry::ToolCall {
            id,
            raw_input,
            call_result,
            widget: Some(WidgetSlot::Ready(widget)),
            ..
        } = entry
        else {
            continue;
        };
        ready.insert(id);
        if widgets.failed.contains_key(id) {
            continue;
        }
        let Some(view) = widgets.views.get_mut(id) else {
            widgets.open(host, id, widget, (raw_input, call_result), theme);
            continue;
        };
        if let Some(result) = call_result.as_ref().filter(|_| view.result != *call_result) {
            view.result = Some(result.clone());
            let outs = view.bridge.result_arrived(result);
            // A result only ever posts to the page.
            let _links = widgets.carry_out(host, id, outs);
        }
    }
    let gone: Vec<ToolCallId> = widgets
        .views
        .keys()
        .filter(|call| !ready.contains(call))
        .cloned()
        .collect();
    for call in &gone {
        widgets.close(host, call);
    }
    if widgets.views.is_empty() {
        return;
    }
    let laid_out = if state.tab == Tab::Session {
        session_view::laid_out_entries(&model, &state.session)
    } else {
        0..0
    };
    for (index, entry) in model.entries.iter().enumerate() {
        let Entry::ToolCall { id, .. } = entry else {
            continue;
        };
        if laid_out.contains(&index) {
            continue;
        }
        widgets.command(host, id, |host, view| {
            if !view.visible {
                return Ok(());
            }
            view.visible = false;
            host.set_visible(view.id, false)
        });
    }
}

/// Applies a message of the widgets: what the engine reported, what a view in the
/// transcript reported, or the window's new scale factor. Other messages change nothing.
pub(crate) fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Widget(event) => host_event(state, event),
        Message::WidgetView(view, message) => {
            view_message(state, view, message);
            Task::none()
        }
        Message::WidgetFrame(view, allocation) => frame_allocated(state, view, allocation),
        Message::Rescaled(scale) => {
            rescaled(state, scale);
            Task::none()
        }
        _ => Task::none(),
    }
}

/// Asks the renderer to allocate the picture waiting in `latest` for `view`, if one is there.
fn allocate(view: &mut View, latest: &LatestFrame) -> Task<Message> {
    let Some(picture) = latest.take() else {
        return Task::none();
    };
    view.allocating = true;
    let id = view.id;
    image::allocate(Handle::from_rgba(
        picture.width,
        picture.height,
        picture.rgba,
    ))
    .map(move |allocation| Message::WidgetFrame(id, allocation.map_err(|error| error.to_string())))
}

/// Takes the renderer's answer for the picture of `view` asked for last: an allocated picture
/// becomes the frame shown, in place of the one before, and a picture that came meanwhile goes
/// to be allocated next. A view closed since drops it.
fn frame_allocated(
    state: &mut State,
    view: ViewId,
    allocation: Result<Allocation, String>,
) -> Task<Message> {
    let widgets = &mut state.widgets;
    let Some(shown) = widgets
        .calls
        .get(&view)
        .and_then(|call| widgets.views.get_mut(call))
    else {
        return Task::none();
    };
    shown.allocating = false;
    match allocation {
        Ok(allocation) => {
            shown.frame = Some(allocation.handle().clone());
            shown.allocation = Some(allocation);
        }
        Err(error) => tracing::warn!(%error, "a widget's frame could not be allocated"),
    }
    match shown.waiting.take() {
        Some(latest) => allocate(shown, &latest),
        None => Task::none(),
    }
}

/// Applies what the engine reported: a frame goes to be allocated, the pointer's look
/// becomes the view's, a message from the page goes to its bridge, a link the user followed
/// in the page opens like one the page asks for, and a failure makes the widget fall back to
/// text. Returns the tasks that open the links. Reports of views that are closed are ignored.
fn host_event(state: &mut State, event: WidgetHostEvent) -> Task<Message> {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    match event {
        WidgetHostEvent::Frame { view, latest } => {
            let shown = widgets
                .calls
                .get(&view)
                .and_then(|call| widgets.views.get_mut(call));
            match shown {
                // Left in its slot, where newer pictures take its place, until the renderer
                // has the one before.
                Some(shown) if shown.allocating => {
                    shown.waiting = Some(latest);
                    Task::none()
                }
                Some(shown) => allocate(shown, &latest),
                // Taken, so that the slot of a closed view is emptied.
                None => {
                    let _ = latest.take();
                    Task::none()
                }
            }
        }
        WidgetHostEvent::Cursor { view, cursor } => {
            let shown = widgets
                .calls
                .get(&view)
                .and_then(|call| widgets.views.get_mut(call));
            if let Some(shown) = shown {
                shown.cursor = cursor;
            }
            Task::none()
        }
        WidgetHostEvent::Message { view, json } => {
            let Some(call) = widgets.calls.get(&view).cloned() else {
                return Task::none();
            };
            let outs = widgets
                .views
                .get_mut(&call)
                .map(|shown| shown.bridge.receive(&json))
                .unwrap_or_default();
            let links = widgets.carry_out(host, &call, outs);
            let links = widgets.admitted(&call, links);
            open_links(state, links)
        }
        WidgetHostEvent::OpenLink { view, url } => {
            let Some(call) = widgets.calls.get(&view).cloned() else {
                return Task::none();
            };
            if !is_web_link(&url) {
                return Task::none();
            }
            let links = widgets.admitted(&call, vec![url]);
            open_links(state, links)
        }
        WidgetHostEvent::Failed { view, error } => {
            if let Some(call) = widgets.calls.get(&view).cloned() {
                widgets.fail(host, &call, error);
            }
            Task::none()
        }
    }
}

/// The tasks that open `links` with the shown session's link opener; when it cannot be read,
/// the error bar says why.
fn open_links(state: &mut State, links: Vec<String>) -> Task<Message> {
    let Some(session) = state.widgets.shown.clone().filter(|_| !links.is_empty()) else {
        return Task::none();
    };
    let Some(opener) = app::report(&mut state.error, state.app.link_opener(&session)) else {
        return Task::none();
    };
    let env = state.process_env.clone();
    Task::batch(
        links
            .into_iter()
            .map(|url| link::open(opener.clone(), url, env.clone())),
    )
}

/// Applies what the view `view` of the transcript reported: its size goes to the engine and
/// its width to the page's bridge, pointer input goes to the engine, and a change of whether
/// it is in the viewport shows or hides it.
fn view_message(state: &mut State, view: ViewId, message: WidgetViewMessage) {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    let Some(call) = widgets.calls.get(&view).cloned() else {
        return;
    };
    if let WidgetViewMessage::Sized { width, .. } = message {
        let outs = widgets
            .views
            .get_mut(&call)
            .map(|shown| shown.bridge.width_changed(pixels(width)))
            .unwrap_or_default();
        // A width only ever posts to the page.
        let _links = widgets.carry_out(host, &call, outs);
    }
    widgets.command(host, &call, |host, shown| match message {
        WidgetViewMessage::Sized { width, scale } => {
            shown.size = Some((width, scale));
            resize(host, shown)
        }
        WidgetViewMessage::Input(input) => {
            match input {
                WidgetInput::Press { .. } => shown.links.pressed(std::time::Instant::now()),
                WidgetInput::Release { .. } => shown.links.released(std::time::Instant::now()),
                _ => {}
            }
            match whole_wheel(shown, input) {
                Some(input) => host.input(shown.id, input),
                None => Ok(()),
            }
        }
        WidgetViewMessage::Visible(visible) if visible != shown.visible => {
            shown.visible = visible;
            host.set_visible(shown.id, visible)
        }
        WidgetViewMessage::Visible(_) => Ok(()),
    });
}

/// `input` for the engine, which scrolls by whole pixels: a wheel turn goes by the whole
/// pixels it and the parts `view` kept from the turns before add up to, keeping the parts
/// left, and none when that is nothing. Other input goes as it is.
fn whole_wheel(view: &mut View, input: WidgetInput) -> Option<WidgetInput> {
    let WidgetInput::Wheel {
        x,
        y,
        dx,
        dy,
        precise,
    } = input
    else {
        return Some(input);
    };
    let (across, down) = (view.wheel_rest.0 + dx, view.wheel_rest.1 + dy);
    let (dx, dy) = (across.trunc(), down.trunc());
    view.wheel_rest = (across - dx, down - dy);
    (dx != 0.0 || dy != 0.0).then_some(WidgetInput::Wheel {
        x,
        y,
        dx,
        dy,
        precise,
    })
}

/// Tells every open view's page that the desktop's theme is now the one of `state.mode`.
pub(crate) fn theme_changed(state: &mut State) {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    let theme = theme_of(state.mode);
    let open: Vec<ToolCallId> = widgets.views.keys().cloned().collect();
    for call in &open {
        let outs = widgets
            .views
            .get_mut(call)
            .map(|view| view.bridge.theme_changed(theme))
            .unwrap_or_default();
        // A theme change only ever posts to the page.
        let _links = widgets.carry_out(host, call, outs);
    }
}

/// Takes the window's new scale factor `scale`, and resizes every view laid out at another.
fn rescaled(state: &mut State, scale: f32) {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    widgets.scale = scale;
    let open: Vec<ToolCallId> = widgets.views.keys().cloned().collect();
    for call in &open {
        widgets.command(host, call, |host, view| match view.size {
            Some((width, before)) if before.to_bits() != scale.to_bits() => {
                view.size = Some((width, scale));
                resize(host, view)
            }
            _ => Ok(()),
        });
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

    use std::collections::HashSet;

    use futures::channel::mpsc;
    use iced::advanced::renderer::Headless;
    use iced::theme::Mode;
    use iced::{Point, Size, mouse};
    use iced_test::runtime::user_interface::{self, UserInterface};
    use iced_test::{Simulator, simulator};
    use partitur_core::testing::{
        MemoryGit, MemoryStore, RecordingBackend, RecordingWidgetHost, WidgetCall, single_claude,
    };
    use partitur_core::widget_bridge::{WidgetTheme, content_security_policy};
    use partitur_core::widget_host::{
        LatestFrame, Picture, ViewId, WidgetCursor, WidgetHostEvent, WidgetInput, WidgetPage,
    };
    use partitur_core::{
        App, BackendEvent, GlobalSettings, Level, ProjectSettings, SessionId, SettingsLayer,
        ToolCallId, ToolContent, ToolKind, ToolStatus, Update, Widget, WidgetSlot,
    };
    use serde_json::Value;

    use super::*;
    use crate::app::{Message, State, Tab, boot, update, view};
    use crate::session_view::{SessionMessage, Visible};
    use crate::test_support::{Plain, deliver, deliver_widget, first_output, plain, shown};

    const META: &str = r#"{"ui":{"csp":{"connectDomains":["https://api.example.com"]}}}"#;
    const INPUT: &str = r#"{"issueKey":"PROJ-1"}"#;
    const RESULT: &str = r#"{"content":[{"type":"text","text":"PROJ-1 is open"}]}"#;

    fn widget() -> Widget {
        Widget {
            server: "atlassian".into(),
            uri: "ui://jira/issue".into(),
            html: "<p>PROJ-1</p>".into(),
            meta: META.into(),
        }
    }

    fn call_id() -> ToolCallId {
        ToolCallId::new("w1")
    }

    /// The tool call `w1` with its widget in `slot`.
    fn call(slot: WidgetSlot) -> Update {
        Update::ToolCall {
            id: call_id(),
            title: "getJiraIssue".into(),
            kind: ToolKind::Other,
            status: ToolStatus::Completed,
            content: vec![ToolContent::Text("PROJ-1 is open".into())],
            locations: Vec::new(),
            raw_input: Some(INPUT.into()),
            call_result: None,
            widget: Some(slot),
        }
    }

    /// The update that brings the result of the call `w1`.
    fn result_arrives() -> Update {
        Update::ToolCallUpdate {
            id: call_id(),
            title: None,
            kind: None,
            status: None,
            content: None,
            raw_output: None,
            raw_input: None,
            call_result: Some(RESULT.into()),
            widget: None,
        }
    }

    fn loading() -> WidgetSlot {
        WidgetSlot::Loading {
            server: "atlassian".into(),
            uri: "ui://jira/issue".into(),
        }
    }

    /// The frame event that announces a `width` by `height` picture of `view`.
    fn frame(view: ViewId, width: u32, height: u32, rgba: Vec<u8>) -> WidgetHostEvent {
        let latest = LatestFrame::default();
        latest.put(Picture {
            width,
            height,
            rgba,
        });
        WidgetHostEvent::Frame { view, latest }
    }

    /// Has the engine paint a `width` by `height` picture of `view` and the renderer allocate
    /// it, so that it is the frame shown.
    fn paint(state: &mut State, view: ViewId, width: u32, height: u32, rgba: Vec<u8>) {
        let handle = Handle::from_rgba(width, height, rgba.clone());
        let _task = deliver_widget(state, frame(view, width, height, rgba));
        let allocation = iced::advanced::image::Renderer::load_image(&renderer(), &handle)
            .map_err(|error| error.to_string());
        let _task = update(state, Message::WidgetFrame(view, allocation));
    }

    fn report(state: &mut State, session: &SessionId, update: Update) {
        deliver(
            state,
            BackendEvent::Update {
                session: session.clone(),
                update,
            },
        );
    }

    /// A plain session on the Session tab.
    fn on_screen() -> Plain {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;
        fixture
    }

    /// A plain session on the Session tab whose call `w1` reported its widget as read.
    fn with_widget() -> (Plain, ViewId) {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        report(
            &mut fixture.state,
            &session,
            call(WidgetSlot::Ready(widget())),
        );
        let view = opened(&fixture.host).unwrap();
        (fixture, view)
    }

    /// The view the last `open` named.
    fn opened(host: &RecordingWidgetHost) -> Option<ViewId> {
        host.calls().iter().rev().find_map(|call| match call {
            WidgetCall::Open { view, .. } => Some(*view),
            _ => None,
        })
    }

    /// The JSON texts posted to `view`, parsed.
    fn posted(host: &RecordingWidgetHost, view: ViewId) -> Vec<Value> {
        host.calls()
            .iter()
            .filter_map(|call| match call {
                WidgetCall::Post { view: to, json } if *to == view => {
                    Some(serde_json::from_str(json).unwrap())
                }
                _ => None,
            })
            .collect()
    }

    fn page_says(state: &mut State, view: ViewId, json: &str) -> iced::Task<Message> {
        deliver_widget(
            state,
            WidgetHostEvent::Message {
                view,
                json: json.into(),
            },
        )
    }

    fn view_says(state: &mut State, view: ViewId, message: WidgetViewMessage) {
        let _task = update(state, Message::WidgetView(view, message));
    }

    /// The user clicks the view `view`: a press and its release.
    fn click(state: &mut State, view: ViewId) {
        for input in [
            WidgetInput::Press {
                x: 5.0,
                y: 5.0,
                button: 0,
            },
            WidgetInput::Release {
                x: 5.0,
                y: 5.0,
                button: 0,
            },
        ] {
            view_says(state, view, WidgetViewMessage::Input(input));
        }
    }

    fn size_changed(height: u32) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","method":"ui/notifications/size-changed","params":{{"height":{height}}}}}"#
        )
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ui/initialize","params":{}}"#;
    const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"ui/notifications/initialized"}"#;

    fn unavailable(error: &str) -> String {
        format!("Widget unavailable: {error}")
    }

    /// The messages the window of `state` publishes in one frame drawn with the widget
    /// state of `cache`, and the cache to draw the next frame with.
    fn one_frame(
        state: &State,
        renderer: &mut iced::Renderer,
        cache: user_interface::Cache,
    ) -> (Vec<Message>, user_interface::Cache) {
        let mut ui = UserInterface::build(view(state), Size::new(1024.0, 768.0), cache, renderer);
        let mut messages = Vec::new();
        let _ = ui.update(
            &[iced::Event::Window(iced::window::Event::RedrawRequested(
                iced::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        (messages, ui.into_cache())
    }

    fn renderer() -> iced::Renderer {
        futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap()
    }

    /// The application booted again on `store`, with the session `session` selected and
    /// the Session tab shown, and the widget engine it talks to.
    fn reopened(store: &MemoryStore, session: &SessionId) -> (State, RecordingWidgetHost) {
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store.clone()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
        let host = RecordingWidgetHost::new();
        state.widget_host = Box::new(host.clone());
        state.app.select_session(session.clone()).unwrap();
        let _task = update(&mut state, Message::SelectTab(Tab::Session));
        (state, host)
    }

    #[test]
    fn ready_widget_opens_a_view_with_csp_and_theme() {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        let _task = update(&mut fixture.state, Message::SystemTheme(Mode::Light));
        report(
            &mut fixture.state,
            &session,
            call(WidgetSlot::Ready(widget())),
        );
        let _task = update(&mut fixture.state, Message::Tick);

        let opens: Vec<WidgetCall> = fixture
            .host
            .calls()
            .into_iter()
            .filter(|call| matches!(call, WidgetCall::Open { .. }))
            .collect();
        let view = opened(&fixture.host).unwrap();
        assert_eq!(
            opens,
            [WidgetCall::Open {
                view,
                page: WidgetPage {
                    html: "<p>PROJ-1</p>".into(),
                    csp: content_security_policy(META),
                    theme: WidgetTheme::Light,
                },
            }]
        );
        assert!(
            content_security_policy(META).contains("connect-src 'self' https://api.example.com;"),
            "the page runs under the declared policy"
        );
    }

    #[test]
    fn loading_widget_shows_pending_row() {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        report(&mut fixture.state, &session, call(loading()));

        assert_eq!(opened(&fixture.host), None);
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, LOADING), 1);
    }

    #[test]
    fn frame_replaces_pending_row() {
        let (mut fixture, view_id) = with_widget();
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, LOADING), 1);
        drop(ui);

        paint(
            &mut fixture.state,
            view_id,
            2,
            1,
            vec![255, 0, 0, 255, 0, 0, 255, 255],
        );

        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, LOADING), 0);
        let shown_view = fixture
            .state
            .widgets
            .view(&fixture.session, &call_id())
            .unwrap();
        assert!(shown_view.frame.is_some());
        // The picture fills the transcript's width at the view's height.
        let bounds = ui.find(id(view_id)).unwrap().bounds();
        assert!((bounds.height - 160.0).abs() < 0.5, "{bounds:?}");
        assert!(bounds.width > 600.0, "{bounds:?}");
    }

    /// How far below the bottom of the `tools` row's label the text or view `what` starts.
    fn below_the_row(ui: &mut Simulator<'_, Message>, what: iced::Rectangle) -> f32 {
        let label = ui.find("tools").unwrap().bounds();
        what.y - (label.y + label.height)
    }

    // A widget sits in a frame like the tool call's row above it: the same border and
    // padding, the view inside, at the width it is told. The loading row is framed alike; the
    // line that says a widget is unavailable is not.
    #[test]
    fn a_widget_is_framed_like_the_tool_rows() {
        // The row's padding below its label, the gap, and the frame's padding above the view.
        let framed = 8.0 + 6.0 + 8.0;
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        report(&mut fixture.state, &session, call(loading()));
        let mut ui = simulator(view(&fixture.state));
        let reading = ui.find(LOADING).unwrap().bounds();
        assert!(
            (below_the_row(&mut ui, reading) - framed).abs() < 0.5,
            "{reading:?}"
        );
        drop(ui);

        // An open view's loading row is as tall as the view, the words in its middle.
        let (mut fixture, view_id) = with_widget();
        let mut ui = simulator(view(&fixture.state));
        let opening = ui.find(LOADING).unwrap().bounds();
        let middle = (160.0 - opening.height) / 2.0;
        assert!(
            (below_the_row(&mut ui, opening) - framed - middle).abs() < 0.5,
            "{opening:?}"
        );
        drop(ui);

        paint(&mut fixture.state, view_id, 1, 1, vec![0, 0, 0, 255]);
        let mut ui = simulator(view(&fixture.state));
        let shown = ui.find(id(view_id)).unwrap().bounds();
        assert!(
            (below_the_row(&mut ui, shown) - framed).abs() < 0.5,
            "{shown:?}"
        );
        // Inside the frame the view lines up with the row's label and its control.
        let label = ui.find("tools").unwrap().bounds();
        let control = ui.find("expand").unwrap().bounds();
        assert!((shown.x - label.x).abs() < 0.5, "{shown:?} {label:?}");
        assert!(
            (shown.x + shown.width - (control.x + control.width)).abs() < 0.5,
            "{shown:?} {control:?}"
        );
        drop(ui);
        // The width the view is told is the width inside the frame.
        let (messages, _) = one_frame(
            &fixture.state,
            &mut renderer(),
            user_interface::Cache::default(),
        );
        let told: Vec<f32> = messages
            .iter()
            .filter_map(|message| match message {
                Message::WidgetView(of, WidgetViewMessage::Sized { width, .. })
                    if *of == view_id =>
                {
                    Some(*width)
                }
                _ => None,
            })
            .collect();
        assert_eq!(told, [shown.width]);

        let mut fixture = on_screen();
        let session = fixture.session.clone();
        report(
            &mut fixture.state,
            &session,
            call(WidgetSlot::Failed {
                server: "atlassian".into(),
                uri: "ui://jira/issue".into(),
                error: "resource not found".into(),
            }),
        );
        let mut ui = simulator(view(&fixture.state));
        let note = ui
            .find(unavailable("resource not found").as_str())
            .unwrap()
            .bounds();
        assert!(
            (below_the_row(&mut ui, note) - (8.0 + 6.0)).abs() < 0.5,
            "{note:?}"
        );
    }

    // iced uploads a large picture on a worker and draws nothing for it until that is done, so
    // a view keeps showing its frame until the next one is allocated, with one in flight.
    #[test]
    fn a_frame_is_shown_only_once_it_is_allocated() {
        let (mut fixture, view_id) = with_widget();
        let renderer = renderer();
        let shown_frame = |state: &State| {
            state
                .widgets
                .view(&fixture.session, &call_id())
                .unwrap()
                .frame
                .clone()
        };
        let allocated = |rgba: [u8; 4]| {
            let handle = Handle::from_rgba(1, 1, rgba.to_vec());
            let allocation = iced::advanced::image::Renderer::load_image(&renderer, &handle);
            (handle, allocation.map_err(|error| error.to_string()))
        };
        let ready = |state: &mut State, view, allocation| {
            update(state, Message::WidgetFrame(view, allocation))
        };

        let task = deliver_widget(
            &mut fixture.state,
            frame(view_id, 1, 1, vec![255, 0, 0, 255]),
        );
        assert_eq!(task.units(), 1, "the picture is allocated");
        assert_eq!(shown_frame(&fixture.state), None);
        let (first, allocation) = allocated([255, 0, 0, 255]);
        let task = ready(&mut fixture.state, view_id, allocation);
        assert_eq!(task.units(), 0);
        assert_eq!(shown_frame(&fixture.state), Some(first.clone()));

        // The next picture is allocated while the first stays shown, and one that comes in the
        // meantime waits in its slot.
        let task = deliver_widget(
            &mut fixture.state,
            frame(view_id, 1, 1, vec![0, 0, 255, 255]),
        );
        assert_eq!(task.units(), 1);
        assert_eq!(shown_frame(&fixture.state), Some(first.clone()));
        let third = LatestFrame::default();
        third.put(Picture {
            width: 1,
            height: 1,
            rgba: vec![0, 255, 0, 255],
        });
        let task = deliver_widget(
            &mut fixture.state,
            WidgetHostEvent::Frame {
                view: view_id,
                latest: third.clone(),
            },
        );
        assert_eq!(task.units(), 0, "one allocation at a time");
        assert_eq!(shown_frame(&fixture.state), Some(first.clone()));

        // The second arrives and is shown, and the waiting picture goes to be allocated.
        let (second, allocation) = allocated([0, 0, 255, 255]);
        let task = ready(&mut fixture.state, view_id, allocation);
        assert_eq!(shown_frame(&fixture.state), Some(second.clone()));
        assert_eq!(task.units(), 1);
        assert_eq!(third.take(), None, "the waiting picture was taken");

        // A failed allocation keeps the frame shown and lets the next picture through.
        let task = ready(&mut fixture.state, view_id, Err("no memory".into()));
        assert_eq!(task.units(), 0);
        assert_eq!(shown_frame(&fixture.state), Some(second));
        let task = deliver_widget(&mut fixture.state, frame(view_id, 1, 1, vec![9, 9, 9, 255]));
        assert_eq!(task.units(), 1);

        // What arrives for a closed view changes nothing.
        let (_, allocation) = allocated([1, 1, 1, 255]);
        assert_eq!(
            ready(&mut fixture.state, ViewId(999), allocation).units(),
            0
        );
        let closed = frame(ViewId(999), 1, 1, vec![1, 1, 1, 255]);
        let WidgetHostEvent::Frame { latest, .. } = &closed else {
            unreachable!()
        };
        let latest = latest.clone();
        assert_eq!(deliver_widget(&mut fixture.state, closed).units(), 0);
        assert_eq!(latest.take(), None, "a closed view's slot is emptied");
    }

    #[test]
    fn initialize_from_view_is_answered_through_post() {
        let (mut fixture, view_id) = with_widget();

        let _task = page_says(&mut fixture.state, view_id, INITIALIZE);
        let _task = page_says(&mut fixture.state, view_id, INITIALIZED);

        let posts = posted(&fixture.host, view_id);
        assert_eq!(posts[0]["id"], 1);
        assert_eq!(posts[0]["result"]["protocolVersion"], "2026-01-26");
        assert_eq!(posts[0]["result"]["hostContext"]["theme"], "dark");
        assert_eq!(posts[1]["method"], "ui/notifications/tool-input");
        assert_eq!(posts[1]["params"]["arguments"]["issueKey"], "PROJ-1");
        assert_eq!(posts.len(), 2);

        // A result that arrives later goes to the page at once.
        let session = fixture.session.clone();
        report(&mut fixture.state, &session, result_arrives());
        let posts = posted(&fixture.host, view_id);
        assert_eq!(posts[2]["method"], "ui/notifications/tool-result");
        assert_eq!(posts[2]["params"]["content"][0]["text"], "PROJ-1 is open");
        // Only once.
        let _task = update(&mut fixture.state, Message::Tick);
        assert_eq!(posted(&fixture.host, view_id).len(), 3);
    }

    #[test]
    fn open_link_from_view_uses_link_opener() {
        let (mut fixture, view_id) = with_widget();
        let project = fixture.state.app.tree().projects[0].id.clone();
        let mut layer = SettingsLayer::default();
        layer
            .set(Level::Project, "link_opener", "partitur-no-such-opener -x")
            .unwrap();
        fixture.store.set_settings(
            project,
            ProjectSettings {
                project: layer,
                ..ProjectSettings::default()
            },
        );

        click(&mut fixture.state, view_id);
        let task = page_says(
            &mut fixture.state,
            view_id,
            r#"{"jsonrpc":"2.0","id":2,"method":"ui/open-link","params":{"url":"https://example.com/PROJ-1"}}"#,
        );

        let Message::Error(error) = first_output(task) else {
            panic!("the opener cannot start");
        };
        assert!(
            error.starts_with(
                "cannot open https://example.com/PROJ-1 with partitur-no-such-opener -x: "
            ),
            "{error}"
        );
        assert_eq!(posted(&fixture.host, view_id)[0]["id"], 2);

        // A scheme the bridge refuses opens nothing.
        click(&mut fixture.state, view_id);
        let task = page_says(
            &mut fixture.state,
            view_id,
            r#"{"jsonrpc":"2.0","id":3,"method":"ui/open-link","params":{"url":"file:///etc/passwd"}}"#,
        );
        assert_eq!(task.units(), 0);
    }

    // A widget opens links only on the user's click, one per click, whichever way it asks.
    #[test]
    fn a_widget_opens_one_link_per_click_and_none_on_its_own() {
        let (mut fixture, view_id) = with_widget();
        let asks = |state: &mut State, id: u32| {
            page_says(
                state,
                view_id,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"method":"ui/open-link","params":{{"url":"https://example.com/{id}"}}}}"#
                ),
            )
            .units()
        };
        let follows = |state: &mut State| {
            deliver_widget(
                state,
                WidgetHostEvent::OpenLink {
                    view: view_id,
                    url: "https://example.com/followed".into(),
                },
            )
            .units()
        };
        // No click: neither way opens anything, however often the widget asks.
        for id in 1..=5 {
            assert_eq!(asks(&mut fixture.state, id), 0, "{id}");
        }
        assert_eq!(follows(&mut fixture.state), 0);
        // The page is still answered.
        assert_eq!(posted(&fixture.host, view_id).len(), 5);

        // One click opens one link.
        click(&mut fixture.state, view_id);
        assert_eq!(asks(&mut fixture.state, 6), 1);
        assert_eq!(asks(&mut fixture.state, 7), 0);
        assert_eq!(follows(&mut fixture.state), 0);
        click(&mut fixture.state, view_id);
        assert_eq!(follows(&mut fixture.state), 1);
        assert_eq!(asks(&mut fixture.state, 8), 0);

        // A click on another view gives this one nothing, nor does moving the pointer.
        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Input(WidgetInput::Move { x: 1.0, y: 1.0 }),
        );
        assert_eq!(asks(&mut fixture.state, 9), 0);
    }

    // A link the user follows in the page is kept from navigating it, and reported instead.
    #[test]
    fn a_followed_link_opens_with_the_link_opener_within_the_allowed_schemes() {
        let (mut fixture, view_id) = with_widget();
        let project = fixture.state.app.tree().projects[0].id.clone();
        let mut layer = SettingsLayer::default();
        layer
            .set(Level::Project, "link_opener", "partitur-no-such-opener -x")
            .unwrap();
        fixture.store.set_settings(
            project,
            ProjectSettings {
                project: layer,
                ..ProjectSettings::default()
            },
        );
        let followed = |state: &mut State, view, url: &str| {
            deliver_widget(
                state,
                WidgetHostEvent::OpenLink {
                    view,
                    url: url.into(),
                },
            )
        };

        click(&mut fixture.state, view_id);
        let task = followed(&mut fixture.state, view_id, "https://example.com/x");
        let Message::Error(error) = first_output(task) else {
            panic!("the opener cannot start");
        };
        assert!(
            error
                .starts_with("cannot open https://example.com/x with partitur-no-such-opener -x: "),
            "{error}"
        );
        click(&mut fixture.state, view_id);
        assert_eq!(
            followed(&mut fixture.state, view_id, "mailto:a@example.com").units(),
            1
        );
        // A scheme the bridge refuses opens nothing, and nor does a closed view's link.
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "partitur-app://view-1/",
        ] {
            click(&mut fixture.state, view_id);
            assert_eq!(
                followed(&mut fixture.state, view_id, url).units(),
                0,
                "{url}"
            );
        }
        assert_eq!(
            followed(&mut fixture.state, ViewId(999), "https://example.com/x").units(),
            0
        );
    }

    #[test]
    fn resize_from_view_changes_height_within_bounds() {
        let (mut fixture, view_id) = with_widget();
        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Sized {
                width: 700.0,
                scale: 2.0,
            },
        );
        let resize = |height| WidgetCall::Resize {
            view: view_id,
            width: 700,
            height,
            scale: 2.0,
        };
        assert_eq!(fixture.host.calls().last(), Some(&resize(160)));

        let _task = page_says(&mut fixture.state, view_id, &size_changed(2000));
        assert_eq!(fixture.host.calls().last(), Some(&resize(640)));
        let height = |state: &State| {
            state
                .widgets
                .view(&fixture.session, &call_id())
                .unwrap()
                .height
        };
        assert_eq!(height(&fixture.state), 640);
        let mut ui = simulator(view(&fixture.state));
        let bounds = ui.find(id(view_id)).unwrap().bounds();
        assert!((bounds.height - 640.0).abs() < 0.5, "{bounds:?}");
        drop(ui);

        let _task = page_says(&mut fixture.state, view_id, &size_changed(10));
        assert_eq!(fixture.host.calls().last(), Some(&resize(80)));
        assert_eq!(height(&fixture.state), 80);

        let _task = page_says(&mut fixture.state, view_id, &size_changed(300));
        assert_eq!(fixture.host.calls().last(), Some(&resize(300)));
    }

    #[test]
    fn the_page_learns_its_width_and_greatest_height() {
        let (mut fixture, view_id) = with_widget();
        let sized = |width| WidgetViewMessage::Sized { width, scale: 1.0 };
        view_says(&mut fixture.state, view_id, sized(700.4));
        let _task = page_says(&mut fixture.state, view_id, INITIALIZE);
        let posts = posted(&fixture.host, view_id);
        assert_eq!(
            posts[0]["result"]["hostContext"]["containerDimensions"],
            serde_json::json!({"width": 700, "maxHeight": MAX_HEIGHT})
        );

        let _task = page_says(&mut fixture.state, view_id, INITIALIZED);
        let before = posted(&fixture.host, view_id).len();
        view_says(&mut fixture.state, view_id, sized(512.0));
        // The same width again tells the page nothing new.
        view_says(&mut fixture.state, view_id, sized(512.0));
        let posts = posted(&fixture.host, view_id);
        assert_eq!(posts.len(), before + 1);
        assert_eq!(
            posts[before]["method"],
            "ui/notifications/host-context-changed"
        );
        assert_eq!(
            posts[before]["params"],
            serde_json::json!({"containerDimensions": {"width": 512, "maxHeight": MAX_HEIGHT}})
        );
    }

    // A page reports 0 before it has laid itself out; taking it would shrink the view to the
    // least height and grow it again a moment later.
    #[test]
    fn a_reported_height_of_zero_keeps_the_height() {
        let (mut fixture, view_id) = with_widget();
        let calls = fixture.host.calls().len();
        let _task = page_says(&mut fixture.state, view_id, &size_changed(0));
        assert_eq!(fixture.host.calls().len(), calls);
        let shown_view = fixture
            .state
            .widgets
            .view(&fixture.session, &call_id())
            .unwrap();
        assert_eq!(shown_view.height, DEFAULT_HEIGHT);
    }

    #[test]
    fn a_view_reports_its_width_and_a_new_scale_resizes_it() {
        let (mut fixture, view_id) = with_widget();
        let _task = update(&mut fixture.state, Message::Rescaled(1.5));
        let mut renderer = renderer();
        let (messages, _cache) = one_frame(
            &fixture.state,
            &mut renderer,
            user_interface::Cache::default(),
        );
        let sized = messages.iter().find_map(|message| match message {
            Message::WidgetView(view, WidgetViewMessage::Sized { width, scale }) => {
                Some((*view, *width, *scale))
            }
            _ => None,
        });
        let (view, width, scale) = sized.unwrap();
        assert_eq!((view, scale), (view_id, 1.5));
        assert!(width > 600.0, "{width}");

        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Sized { width, scale },
        );
        let _task = update(&mut fixture.state, Message::Rescaled(2.0));
        let Some(WidgetCall::Resize { scale, height, .. }) = fixture.host.calls().last().cloned()
        else {
            panic!("the view is resized at the new scale");
        };
        assert_eq!((scale, height), (2.0, 160));
    }

    #[test]
    fn theme_change_reaches_open_widgets() {
        let (mut fixture, view_id) = with_widget();
        let _task = page_says(&mut fixture.state, view_id, INITIALIZED);
        let before = posted(&fixture.host, view_id).len();

        let _task = update(&mut fixture.state, Message::SystemTheme(Mode::Light));
        let _task = update(&mut fixture.state, Message::SystemTheme(Mode::Dark));
        let _task = update(&mut fixture.state, Message::SystemTheme(Mode::None));

        let posts = posted(&fixture.host, view_id);
        let themes: Vec<&Value> = posts
            .iter()
            .skip(before)
            .map(|post| {
                assert_eq!(post["method"], "ui/notifications/host-context-changed");
                &post["params"]["theme"]
            })
            .collect();
        assert_eq!(themes, ["light", "dark", "dark"]);
    }

    #[test]
    fn failed_widget_falls_back_to_text() {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        report(
            &mut fixture.state,
            &session,
            call(WidgetSlot::Failed {
                server: "atlassian".into(),
                uri: "ui://jira/issue".into(),
                error: "resource not found".into(),
            }),
        );
        assert_eq!(opened(&fixture.host), None);
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, &unavailable("resource not found")), 1);
        // The call shows its text as it does without a widget.
        ui.click("expand").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        let mut ui = simulator(view(&fixture.state));
        ui.click("getJiraIssue").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, "PROJ-1 is open"), 1);
        assert_eq!(shown(&mut ui, &unavailable("resource not found")), 1);
    }

    #[test]
    fn a_view_the_engine_refuses_or_breaks_falls_back_to_text() {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        fixture.host.fail_next_call();
        report(
            &mut fixture.state,
            &session,
            call(WidgetSlot::Ready(widget())),
        );
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(
            shown(
                &mut ui,
                &unavailable("widget host unavailable: recording widget host")
            ),
            1
        );
        drop(ui);
        // It is not opened again while the session stays shown.
        let _task = update(&mut fixture.state, Message::Tick);
        assert_eq!(fixture.host.calls().len(), 1);

        let (mut fixture, view_id) = with_widget();
        let _task = deliver_widget(
            &mut fixture.state,
            WidgetHostEvent::Failed {
                view: view_id,
                error: "the renderer crashed".into(),
            },
        );
        assert_eq!(
            fixture.host.calls().last(),
            Some(&WidgetCall::Close { view: view_id })
        );
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, &unavailable("the renderer crashed")), 1);
        assert_eq!(shown(&mut ui, LOADING), 0);
        drop(ui);

        // A view whose command cannot be handed over fails the same way.
        let (mut fixture, view_id) = with_widget();
        fixture
            .host
            .fail_next_call_with(partitur_core::widget_host::WidgetHostError::Send(
                "gone".into(),
            ));
        let _task = page_says(&mut fixture.state, view_id, INITIALIZE);
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(
            shown(
                &mut ui,
                &unavailable("sending to the widget host failed: gone")
            ),
            1
        );
    }

    #[test]
    fn offscreen_widget_is_hidden_and_shown_again() {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        for n in 0..400 {
            report(
                &mut fixture.state,
                &session,
                Update::UserText(format!("prompt {n}")),
            );
        }
        report(
            &mut fixture.state,
            &session,
            call(WidgetSlot::Ready(widget())),
        );
        let view_id = opened(&fixture.host).unwrap();
        let visibility = |host: &RecordingWidgetHost| -> Vec<bool> {
            host.calls()
                .iter()
                .filter_map(|call| match call {
                    WidgetCall::SetVisible { visible, .. } => Some(*visible),
                    _ => None,
                })
                .collect()
        };

        // At the end of the transcript the view is in sight and reports itself so.
        let mut renderer = renderer();
        let (messages, _cache) = one_frame(
            &fixture.state,
            &mut renderer,
            user_interface::Cache::default(),
        );
        assert!(
            messages.contains(&Message::WidgetView(
                view_id,
                WidgetViewMessage::Visible(true)
            )),
            "{messages:?}"
        );
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        assert_eq!(visibility(&fixture.host), Vec::<bool>::new());

        // Scrolled to the top, its row is no longer laid out: the view stops painting.
        let at = |bottom| Visible {
            bottom,
            height: 768.0,
            width: 800.0,
        };
        let _task = update(
            &mut fixture.state,
            Message::Session(SessionMessage::Scrolled(at(1.0e6))),
        );
        assert_eq!(visibility(&fixture.host), [false]);
        let _task = update(&mut fixture.state, Message::Tick);
        assert_eq!(visibility(&fixture.host), [false]);

        // Back at the end, its row is laid out again and the view reports itself in sight.
        let _task = update(
            &mut fixture.state,
            Message::Session(SessionMessage::Scrolled(at(0.0))),
        );
        let (messages, _cache) = one_frame(
            &fixture.state,
            &mut renderer,
            user_interface::Cache::default(),
        );
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        assert_eq!(visibility(&fixture.host), [false, true]);

        // A view that scrolls out of the viewport within the rows laid out says so.
        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Visible(false),
        );
        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Visible(false),
        );
        assert_eq!(visibility(&fixture.host), [false, true, false]);
        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Visible(true),
        );
        assert_eq!(visibility(&fixture.host), [false, true, false, true]);

        // Another tab hides it.
        let _task = update(&mut fixture.state, Message::SelectTab(Tab::Projects));
        assert_eq!(visibility(&fixture.host), [false, true, false, true, false]);
    }

    // iced's sensor counts a view as out of sight once both its top-left and its bottom-right
    // corners are, which a view taller than the transcript's viewport is while its middle shows.
    #[test]
    fn a_tall_view_sticking_out_at_both_ends_stays_visible() {
        let (mut fixture, view_id) = painted_below_a_long_transcript();
        let _task = page_says(&mut fixture.state, view_id, &size_changed(2000));
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            view(&fixture.state),
            Size::new(1024.0, 520.0),
            user_interface::Cache::default(),
            &mut renderer,
        );
        let seen = |ui: &mut UserInterface<'_, Message, iced::Theme, iced::Renderer>,
                    renderer: &mut iced::Renderer| {
            let mut messages = Vec::new();
            let _ = ui.update(
                &[iced::Event::Window(iced::window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            messages
                .into_iter()
                .filter_map(|message| match message {
                    Message::WidgetView(view, WidgetViewMessage::Visible(visible))
                        if view == view_id =>
                    {
                        Some(visible)
                    }
                    _ => None,
                })
                .collect::<Vec<bool>>()
        };
        // The transcript is anchored at its end, so its offset counts from there.
        let scroll_up = |ui: &mut UserInterface<'_, Message, iced::Theme, iced::Renderer>,
                         renderer: &iced::Renderer,
                         by: f32| {
            ui.operate(
                renderer,
                &mut iced::advanced::widget::operation::scrollable::scroll_by(
                    fixture.state.session.scroll.clone(),
                    iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: by },
                ),
            );
        };
        assert_eq!(seen(&mut ui, &mut renderer), [true]);

        // Scrolled up until both corners of the 640 pixels tall view are out of the shorter
        // viewport, its middle filling it, it stays in sight.
        scroll_up(&mut ui, &renderer, 200.0);
        assert_eq!(seen(&mut ui, &mut renderer), Vec::<bool>::new());

        // Scrolled on until none of it is left in sight, it is out of sight.
        scroll_up(&mut ui, &renderer, 1500.0);
        assert_eq!(seen(&mut ui, &mut renderer), [false]);
    }

    #[test]
    fn pointer_input_reaches_the_view() {
        let (mut fixture, view_id) = with_widget();
        paint(&mut fixture.state, view_id, 1, 1, vec![0, 0, 0, 255]);
        let mut ui: Simulator<'_, Message> = simulator(view(&fixture.state));
        let bounds = ui.find(id(view_id)).unwrap().bounds();
        let at = Point::new(bounds.x + 10.0, bounds.y + 20.0);
        ui.point_at(at);
        let _ = ui.simulate([
            iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right)),
            iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -30.0 },
            }),
        ]);
        let outside = Point::new(bounds.x + 10.0, bounds.y - 30.0);
        ui.point_at(outside);
        let _ = ui.simulate([iced::Event::Mouse(mouse::Event::CursorMoved {
            position: outside,
        })]);
        let inputs: Vec<WidgetInput> = ui
            .into_messages()
            .filter_map(|message| match message {
                Message::WidgetView(view, WidgetViewMessage::Input(input)) if view == view_id => {
                    Some(input)
                }
                _ => None,
            })
            .collect();
        let (x, y) = (10.0, 20.0);
        assert_eq!(
            inputs,
            [
                WidgetInput::Move { x, y },
                WidgetInput::Press { x, y, button: 2 },
                WidgetInput::Release { x, y, button: 2 },
                WidgetInput::Wheel {
                    x,
                    y,
                    dx: 0.0,
                    dy: -30.0,
                    precise: true,
                },
                WidgetInput::Leave,
            ]
        );

        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Input(WidgetInput::Leave),
        );
        assert_eq!(
            fixture.host.calls().last(),
            Some(&WidgetCall::Input {
                view: view_id,
                input: WidgetInput::Leave
            })
        );
    }

    /// The pointer's look the window of `state` shows with the pointer at `at`.
    fn interaction_at(state: &State, at: Point) -> mouse::Interaction {
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            view(state),
            Size::new(1024.0, 768.0),
            user_interface::Cache::default(),
            &mut renderer,
        );
        let (outcome, _) = ui.update(
            &[iced::Event::Mouse(mouse::Event::CursorMoved {
                position: at,
            })],
            mouse::Cursor::Available(at),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Vec::new(),
        );
        match outcome {
            user_interface::State::Updated {
                mouse_interaction, ..
            } => mouse_interaction,
            user_interface::State::Outdated => panic!("the window is outdated"),
        }
    }

    #[test]
    fn the_page_sets_the_pointers_look_over_its_view() {
        let (mut fixture, view_id) = with_widget();
        paint(&mut fixture.state, view_id, 1, 1, vec![0, 0, 0, 255]);
        let bounds = simulator(view(&fixture.state))
            .find(id(view_id))
            .unwrap()
            .bounds();
        let over = Point::new(bounds.x + 10.0, bounds.y + 20.0);
        let outside = Point::new(bounds.x + 10.0, bounds.y - 30.0);
        assert_eq!(
            interaction_at(&fixture.state, over),
            mouse::Interaction::Idle
        );

        let looks = [
            (WidgetCursor::Pointer, mouse::Interaction::Pointer),
            (WidgetCursor::Text, mouse::Interaction::Text),
            (WidgetCursor::Grab, mouse::Interaction::Grab),
            (WidgetCursor::Grabbing, mouse::Interaction::Grabbing),
            (WidgetCursor::NotAllowed, mouse::Interaction::NotAllowed),
            (
                WidgetCursor::ResizeHorizontal,
                mouse::Interaction::ResizingHorizontally,
            ),
            (
                WidgetCursor::ResizeVertical,
                mouse::Interaction::ResizingVertically,
            ),
            (WidgetCursor::Crosshair, mouse::Interaction::Crosshair),
            (WidgetCursor::Default, mouse::Interaction::Idle),
        ];
        for (cursor, look) in looks {
            let _task = deliver_widget(
                &mut fixture.state,
                WidgetHostEvent::Cursor {
                    view: view_id,
                    cursor,
                },
            );
            assert_eq!(interaction_at(&fixture.state, over), look, "{cursor:?}");
        }

        // Only over the view.
        let _task = deliver_widget(
            &mut fixture.state,
            WidgetHostEvent::Cursor {
                view: view_id,
                cursor: WidgetCursor::Pointer,
            },
        );
        assert_ne!(
            interaction_at(&fixture.state, outside),
            mouse::Interaction::Pointer
        );
        // A closed view's look changes nothing.
        let _task = deliver_widget(
            &mut fixture.state,
            WidgetHostEvent::Cursor {
                view: ViewId(999),
                cursor: WidgetCursor::Text,
            },
        );
        assert_eq!(
            interaction_at(&fixture.state, over),
            mouse::Interaction::Pointer
        );
    }

    // The wheel over a widget scrolls the widget alone: were the transcript to scroll as well,
    // the widget would move away from under the pointer.
    #[test]
    fn the_wheel_over_a_view_is_kept_from_the_transcript() {
        let (mut fixture, view_id) = with_widget();
        paint(&mut fixture.state, view_id, 1, 1, vec![0, 0, 0, 255]);
        let mut ui: Simulator<'_, Message> = simulator(view(&fixture.state));
        let bounds = ui.find(id(view_id)).unwrap().bounds();
        let wheel = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
        });
        let over = Point::new(bounds.x + 10.0, bounds.y + 20.0);
        ui.point_at(over);
        assert_eq!(
            ui.simulate([wheel.clone()]),
            [iced::event::Status::Captured]
        );
        let outside = Point::new(bounds.x + 10.0, bounds.y - 30.0);
        ui.point_at(outside);
        let _ = ui.simulate([wheel]);
        let wheels = ui
            .into_messages()
            .filter(|message| {
                matches!(
                    message,
                    Message::WidgetView(_, WidgetViewMessage::Input(WidgetInput::Wheel { .. }))
                )
            })
            .count();
        assert_eq!(wheels, 1, "only the wheel over the view reaches it");
    }

    /// A session on the Session tab long enough to scroll, ending in the call `w1` whose
    /// widget has painted, and the view.
    fn painted_below_a_long_transcript() -> (Plain, ViewId) {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        for n in 0..60 {
            report(
                &mut fixture.state,
                &session,
                Update::UserText(format!("prompt {n}")),
            );
        }
        report(
            &mut fixture.state,
            &session,
            call(WidgetSlot::Ready(widget())),
        );
        let view_id = opened(&fixture.host).unwrap();
        paint(&mut fixture.state, view_id, 1, 1, vec![0, 0, 0, 255]);
        (fixture, view_id)
    }

    /// Where `view` is on the screen, all of it in sight.
    fn on_screen_bounds(ui: &mut Simulator<'_, Message>, view: ViewId) -> iced::Rectangle {
        let target = ui.find(id(view)).unwrap();
        let bounds = target.visible_bounds().unwrap();
        assert_eq!(
            bounds.size(),
            target.bounds().size(),
            "all of it is in sight"
        );
        bounds
    }

    fn wheel(y: f32) -> iced::Event {
        iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y },
        })
    }

    /// The wheel turns that reached `view` among `messages`, and whether the transcript
    /// scrolled.
    fn wheels_and_scrolls(messages: impl Iterator<Item = Message>, view: ViewId) -> (usize, bool) {
        let (mut wheels, mut scrolled) = (0, false);
        for message in messages {
            match message {
                Message::WidgetView(of, WidgetViewMessage::Input(WidgetInput::Wheel { .. }))
                    if of == view =>
                {
                    wheels += 1;
                }
                Message::Session(SessionMessage::Scrolled(_)) => scrolled = true,
                _ => {}
            }
        }
        (wheels, scrolled)
    }

    // iced's scrollable keeps the wheel to itself for a while after it scrolled, so a wheel
    // that goes on turning once the pointer is over a widget would scroll the transcript past
    // it. Over a widget the wheel is the widget's, at once.
    #[test]
    fn the_wheel_reaches_a_view_right_after_the_transcript_scrolled() {
        let (fixture, view_id) = painted_below_a_long_transcript();
        let mut ui: Simulator<'_, Message> = simulator(view(&fixture.state));
        let bounds = on_screen_bounds(&mut ui, view_id);
        // Scrolling up by a line moves the view down by less than this, so the point stays on it.
        let over = Point::new(bounds.x + 10.0, bounds.y + 100.0);
        let above = Point::new(bounds.x + 10.0, bounds.y - 30.0);

        // The pointer comes onto the view right after the transcript scrolled.
        ui.point_at(above);
        let _ = ui.simulate([
            iced::Event::Mouse(mouse::Event::CursorMoved { position: above }),
            wheel(1.0),
        ]);
        ui.point_at(over);
        let _ = ui.simulate([
            iced::Event::Mouse(mouse::Event::CursorMoved { position: over }),
            wheel(-1.0),
            wheel(-1.0),
        ]);
        let (wheels, scrolled) = wheels_and_scrolls(ui.into_messages(), view_id);
        assert!(scrolled, "the first turn scrolls the transcript");
        assert_eq!(wheels, 2, "the turns over the view reach it");

        // The transcript scrolls the view under a pointer that stays where it is.
        let mut ui: Simulator<'_, Message> = simulator(view(&fixture.state));
        ui.point_at(above);
        let _ = ui.simulate([
            iced::Event::Mouse(mouse::Event::CursorMoved { position: above }),
            wheel(1.0),
        ]);
        // Seen from the transcript's rows the pointer is on the view now, as it is after the
        // next frame is drawn.
        ui.point_at(over);
        let _ = ui.simulate([
            iced::Event::Window(iced::window::Event::RedrawRequested(
                iced::time::Instant::now(),
            )),
            wheel(-1.0),
        ]);
        let (wheels, _) = wheels_and_scrolls(ui.into_messages(), view_id);
        assert_eq!(wheels, 1);
    }

    /// The window of `state` at the widget state of `cache`, given `events` with the pointer
    /// resting at `at`, and then a redraw when `redraw`: how many wheel turns reached `view`,
    /// whether the transcript scrolled, and the cache for the next batch. The messages are
    /// applied to `state`, as the application does before it draws again.
    fn batch(
        state: &mut State,
        renderer: &mut iced::Renderer,
        cache: user_interface::Cache,
        (at, view_id): (Point, ViewId),
        events: &[iced::Event],
        redraw: bool,
    ) -> (usize, bool, user_interface::Cache) {
        let mut ui = UserInterface::build(view(state), Size::new(1024.0, 768.0), cache, renderer);
        let mut messages = Vec::new();
        let cursor = mouse::Cursor::Available(at);
        let _ = ui.update(
            events,
            cursor,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        if redraw {
            let _ = ui.update(
                &[iced::Event::Window(iced::window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                cursor,
                renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
        }
        let cache = ui.into_cache();
        let (wheels, scrolled) = wheels_and_scrolls(messages.clone().into_iter(), view_id);
        for message in messages {
            let _task = update(state, message);
        }
        (wheels, scrolled, cache)
    }

    // The pointer rests over the transcript while the wheel scrolls a widget under it. The
    // frame sees no event that tells it the pointer is over it now: the pointer does not
    // move, and the scrollable keeps the turns to itself. Still the next turn is the widget's,
    // whether the window drew a frame between the turns or not.
    #[test]
    fn the_wheel_reaches_a_view_it_scrolled_under_a_resting_pointer() {
        for redraw in [false, true] {
            let (mut fixture, view_id) = painted_below_a_long_transcript();
            let bounds = on_screen_bounds(&mut simulator(view(&fixture.state)), view_id);
            let mut renderer = renderer();
            // Scrolled up by 200 pixels, the view's top is 50 pixels below the pointer.
            let at = Point::new(bounds.x + 10.0, bounds.y + 150.0);
            let mut ui = UserInterface::build(
                view(&fixture.state),
                Size::new(1024.0, 768.0),
                user_interface::Cache::default(),
                &mut renderer,
            );
            ui.operate(
                &renderer,
                &mut iced::advanced::widget::operation::scrollable::scroll_by(
                    fixture.state.session.scroll.clone(),
                    iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 200.0 },
                ),
            );
            let cache = ui.into_cache();
            let moved = iced::Event::Mouse(mouse::Event::CursorMoved { position: at });
            let (wheels, _, cache) = batch(
                &mut fixture.state,
                &mut renderer,
                cache,
                (at, view_id),
                &[moved],
                true,
            );
            assert_eq!(wheels, 0);

            // A turn down scrolls the transcript by 60 pixels: the view is under the pointer.
            let (wheels, scrolled, cache) = batch(
                &mut fixture.state,
                &mut renderer,
                cache,
                (at, view_id),
                &[wheel(-1.0)],
                redraw,
            );
            assert!(scrolled, "the first turn scrolls the transcript ({redraw})");
            assert_eq!(wheels, 0, "the first turn is the transcript's ({redraw})");

            let (wheels, scrolled, _) = batch(
                &mut fixture.state,
                &mut renderer,
                cache,
                (at, view_id),
                &[wheel(-1.0)],
                redraw,
            );
            assert_eq!(wheels, 1, "the next turn reaches the view ({redraw})");
            assert!(!scrolled, "the transcript stays ({redraw})");
        }
    }

    // While a button is held, as when text is selected by dragging or the scrollbar is
    // dragged, the wheel is left to the transcript as iced has it.
    #[test]
    fn the_wheel_is_left_to_the_transcript_while_a_button_is_held() {
        let (fixture, view_id) = painted_below_a_long_transcript();
        let mut ui: Simulator<'_, Message> = simulator(view(&fixture.state));
        let bounds = on_screen_bounds(&mut ui, view_id);
        let over = Point::new(bounds.x + 10.0, bounds.y + 100.0);
        let above = Point::new(bounds.x + 10.0, bounds.y - 30.0);
        ui.point_at(above);
        let _ = ui.simulate([
            iced::Event::Mouse(mouse::Event::CursorMoved { position: above }),
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            wheel(1.0),
        ]);
        ui.point_at(over);
        let _ = ui.simulate([
            iced::Event::Mouse(mouse::Event::CursorMoved { position: over }),
            wheel(-1.0),
        ]);
        let (wheels, _) = wheels_and_scrolls(ui.into_messages(), view_id);
        assert_eq!(wheels, 0);
    }

    // A touchpad reports physical pixels; the page scrolls by logical ones, and small moves
    // add up instead of each rounding to nothing.
    #[test]
    fn touchpad_scrolling_is_scaled_and_keeps_its_remainder() {
        let (mut fixture, view_id) = with_widget();
        let _task = update(&mut fixture.state, Message::Rescaled(2.0));
        paint(&mut fixture.state, view_id, 1, 1, vec![0, 0, 0, 255]);
        let mut ui: Simulator<'_, Message> = simulator(view(&fixture.state));
        let bounds = ui.find(id(view_id)).unwrap().bounds();
        let at = Point::new(bounds.x + 10.0, bounds.y + 20.0);
        ui.point_at(at);
        let pixels = |x, y| {
            iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x, y },
            })
        };
        let _ = ui.simulate([pixels(0.0, -3.0), pixels(1.0, -0.6), wheel(1.0)]);
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        let wheels: Vec<WidgetInput> = fixture
            .host
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                WidgetCall::Input {
                    input: input @ WidgetInput::Wheel { .. },
                    ..
                } => Some(input),
                _ => None,
            })
            .collect();
        let (x, y) = (10.0, 20.0);
        assert_eq!(
            wheels,
            [
                // -3 physical pixels are -1.5 logical ones: -1 goes, -0.5 is kept.
                WidgetInput::Wheel {
                    x,
                    y,
                    dx: 0.0,
                    dy: -1.0,
                    precise: true,
                },
                // 0.5 and -0.3 more make 0.5 and -0.8: nothing whole yet.
                // A notch is 40 pixels, with the -0.8 kept before it.
                WidgetInput::Wheel {
                    x,
                    y,
                    dx: 0.0,
                    dy: 39.0,
                    precise: false,
                },
            ]
        );
    }

    #[test]
    fn reopened_session_opens_view_from_persisted_page() {
        let (mut fixture, _view) = with_widget();
        let session = fixture.session.clone();
        report(&mut fixture.state, &session, result_arrives());
        let store = fixture.store.clone();
        drop(fixture);

        let (mut state, host) = reopened(&store, &session);

        let view_id = opened(&host).unwrap();
        assert_eq!(
            host.calls()[0],
            WidgetCall::Open {
                view: view_id,
                page: WidgetPage {
                    html: "<p>PROJ-1</p>".into(),
                    csp: content_security_policy(META),
                    theme: WidgetTheme::Dark,
                },
            }
        );
        // The stored input and result reach the page.
        let _task = page_says(&mut state, view_id, INITIALIZED);
        let methods: Vec<Value> = posted(&host, view_id)
            .into_iter()
            .map(|post| post["method"].clone())
            .collect();
        assert_eq!(
            methods,
            [
                "ui/notifications/tool-input",
                "ui/notifications/tool-result"
            ]
        );
    }

    #[test]
    fn restored_loading_widget_falls_back_to_text() {
        let mut fixture = on_screen();
        let session = fixture.session.clone();
        report(&mut fixture.state, &session, call(loading()));
        let store = fixture.store.clone();
        drop(fixture);

        let (state, host) = reopened(&store, &session);

        assert_eq!(opened(&host), None);
        let mut ui = simulator(view(&state));
        assert_eq!(shown(&mut ui, LOADING), 0);
        assert_eq!(shown(&mut ui, &unavailable(STILL_LOADING)), 1);
    }

    #[test]
    fn closing_session_closes_its_views() {
        let (mut fixture, first) = with_widget();
        let closes = |host: &RecordingWidgetHost| -> HashSet<ViewId> {
            host.calls()
                .iter()
                .filter_map(|call| match call {
                    WidgetCall::Close { view } => Some(*view),
                    _ => None,
                })
                .collect()
        };

        // Another session shown closes the views of the one before.
        let project = fixture.state.app.tree().projects[0].id.clone();
        let other = fixture
            .state
            .app
            .new_session(project, partitur_core::Scope::Project)
            .unwrap();
        let _task = update(&mut fixture.state, Message::Tick);
        assert_eq!(fixture.state.app.current_session(), Some(other));
        assert_eq!(closes(&fixture.host), HashSet::from([first]));

        // Shown again, it opens a view of its own, which deleting the session closes.
        let session = fixture.session.clone();
        fixture.state.app.select_session(session.clone()).unwrap();
        let _task = update(&mut fixture.state, Message::Tick);
        let second = opened(&fixture.host).unwrap();
        assert_ne!(second, first);
        fixture.state.app.delete(session, &[]).unwrap();
        let _task = update(&mut fixture.state, Message::Tick);
        assert_eq!(closes(&fixture.host), HashSet::from([first, second]));
        // Events for a closed view change nothing.
        let calls = fixture.host.calls().len();
        let _task = page_says(&mut fixture.state, second, INITIALIZE);
        view_says(
            &mut fixture.state,
            second,
            WidgetViewMessage::Visible(false),
        );
        assert_eq!(fixture.host.calls().len(), calls);
    }

    // A frame, the pointer's look and pointer input arrive many times a second. None of them
    // changes which widgets are read or laid out, so none brings the views or the transcript's
    // window up to date: here the views would be hidden, and the window kept anew, if they did.
    #[test]
    fn frames_and_pointer_input_leave_views_and_window_alone() {
        let (mut fixture, view_id) = with_widget();
        fixture.state.tab = Tab::Projects;
        fixture.state.session.shown = None;
        let calls = fixture.host.calls().len();

        paint(&mut fixture.state, view_id, 1, 1, vec![0, 0, 0, 255]);
        let _task = deliver_widget(
            &mut fixture.state,
            WidgetHostEvent::Cursor {
                view: view_id,
                cursor: WidgetCursor::Text,
            },
        );
        view_says(
            &mut fixture.state,
            view_id,
            WidgetViewMessage::Input(WidgetInput::Leave),
        );

        assert_eq!(
            fixture.host.calls()[calls..],
            [WidgetCall::Input {
                view: view_id,
                input: WidgetInput::Leave,
            }]
        );
        assert!(fixture.state.session.shown.is_none());
        assert!(
            fixture
                .state
                .widgets
                .view(&fixture.session, &call_id())
                .unwrap()
                .frame
                .is_some()
        );

        // Any other message does both.
        let _task = update(&mut fixture.state, Message::Tick);
        assert_eq!(
            fixture.host.calls().last(),
            Some(&WidgetCall::SetVisible {
                view: view_id,
                visible: false,
            })
        );
        assert!(fixture.state.session.shown.is_some());
    }
}
