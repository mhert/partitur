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
//! view opened is handed to the bridge, and so is every change of the desktop's theme, which
//! also goes to the engine for the page's `prefers-color-scheme`. The
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
//! A view's page can scroll unless it reported a height the view holds: one taller than the
//! greatest height overflows it, and one that never reported may overflow the default height.
//! Only over a view that can scroll does the wheel go to the widget ([`View::scrolls`]).
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
use partitur_core::{
    AgentUpdate, BackendEvent, Entry, SessionId, SessionViewModel, ToolCallId, Widget, WidgetSlot,
};

use crate::app::{self, Message, State, Tab};
use crate::link;
use crate::session_view::{self, SessionState};

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
    /// The height the page last reported, before it was kept within bounds.
    reported: Option<u32>,
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

impl View {
    /// Whether the page may scroll inside the view: unless it reported a height the view
    /// holds, its content may be taller than the view.
    pub(crate) fn scrolls(&self) -> bool {
        self.reported
            .is_none_or(|reported| reported > u32::from(self.height))
    }
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

    /// Forgets the widgets of `sessions`, which left the tree, that a live update reported as
    /// loading. The views of a shown session among them close with the next [`sync`], which
    /// finds the session no longer shown.
    pub(crate) fn forget(&mut self, sessions: &[SessionId]) {
        self.live.retain(|session, _| !sessions.contains(session));
    }

    /// Whether a widget of `session` is remembered as loading in this run.
    #[cfg(test)]
    pub(crate) fn remembers(&self, session: &SessionId) -> bool {
        self.live.contains_key(session)
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
                reported: None,
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
                    view.reported = Some(height);
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
    let (AgentUpdate::ToolCall {
        id,
        widget: Some(WidgetSlot::Loading { .. }),
        ..
    }
    | AgentUpdate::ToolCallUpdate {
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

/// Brings the views of `widgets`, opened on `host`, up to date with the app after a message:
/// the views of a session no longer shown are closed, a widget that was read gets its view
/// (in the theme of `mode`), a result that arrived goes to its view's page, and a view whose
/// row `session` does not lay out, or every view while `tab` is not the Session tab, is
/// hidden. `model` is the current session, `None` while no session is selected.
pub(crate) fn sync(
    widgets: &mut Widgets,
    host: &dyn WidgetHost,
    mode: Mode,
    tab: Tab,
    session: &SessionState,
    model: Option<&SessionViewModel<'_>>,
) {
    let shown = model.map(|model| model.id.clone());
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
    let theme = theme_of(mode);
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
    let laid_out = if tab == Tab::Session {
        session_view::laid_out_entries(model, session)
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

/// Tells every open view's page that the desktop's theme is now the one of `state.mode`: the
/// engine, for the page's `prefers-color-scheme`, and the view's bridge, for the MCP Apps
/// host context.
pub(crate) fn theme_changed(state: &mut State) {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    let theme = theme_of(state.mode);
    let open: Vec<ToolCallId> = widgets.views.keys().cloned().collect();
    for call in &open {
        widgets.command(host, call, |host, view| host.set_theme(view.id, theme));
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
mod tests;
