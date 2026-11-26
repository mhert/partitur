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
//! engine's newest picture of a view is kept as its frame for the transcript to draw, and the
//! pointer's look its page last asked for as the look over that image.
//!
//! A picture is shown as soon as its frame event is handled: it becomes the view's frame,
//! which the transcript draws pixel for pixel ([`frame_view`](crate::frame_view)). The
//! renderer takes a new picture into the GPU while it prepares the frame that first draws it,
//! before drawing that frame, so the view never blanks while a picture uploads; it takes only
//! what the frame's damage names, from the pixels the view's slot keeps. A paint whose pixels
//! do not fill its size never reaches the slot, and the view keeps the frame before.
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
//! Besides the transcript's views, [`Widgets`] holds the viewer's page view while the viewer
//! shows an agent's HTML page ([`Widgets::open_page`]), named from the same count. It opens on
//! the page under the page's content security policy ([`PAGE_POLICY`]), a widget's default
//! one with `data:` fonts added, in the desktop's theme, and takes frames, the pointer's look,
//! pointer and wheel input and the links the user follows (through its own [`LinkGate`])
//! exactly like a transcript view. It
//! has no bridge, since a plain page is not an MCP App: what the page sends is dropped, and a
//! change of theme reaches it only through the engine. It is laid out at the size the viewer
//! gives it, and the page can always scroll in it. [`sync`] never hides or closes it; the
//! viewer closes it ([`Widgets::close_page`]). A failure or a refused command closes it and the
//! viewer says why.
//!
//! The engine can stop and restart as a whole ([`WidgetHostEvent::EngineLost`]). Then every
//! view is gone, the viewer's page view included: [`Widgets`] forgets them without closing
//! them, opens none until the engine is back, and each ready widget shows
//! [`ENGINE_RESTARTING`] as its pending line, while a failure recorded before stays. Once the
//! engine is back ([`WidgetHostEvent::EngineBack`]) the next [`sync`] opens every ready widget
//! of the shown session as if it were new, with a fresh bridge from its stored input and
//! result, and the viewer's page opens again on the HTML it showed. An engine that stopped
//! for good ([`WidgetHostEvent::EngineStopped`]) leaves every widget as text with its reason,
//! and the viewer's page says it, for the rest of the run.
//!
//! Every engine command is sent from `update`, never from `view`.

use std::collections::{HashMap, HashSet};

use iced::Task;
use iced::theme::Mode;
use iced::widget::Id;
use partitur_core::widget_bridge::{
    BridgeOut, LinkGate, WidgetBridge, WidgetTheme, content_security_policy, is_web_link,
};
use partitur_core::widget_host::{
    ViewId, WidgetCursor, WidgetHost, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
};
use partitur_core::{
    AgentUpdate, BackendEvent, Entry, PAGE_POLICY, SessionId, SessionViewModel, ToolCallId, Widget,
    WidgetSlot,
};

use crate::app::{self, Message, State, Tab};
use crate::frame_view::Frame;
use crate::link;
use crate::session_view::{self, SessionState};
use crate::viewer::Viewer;

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

/// What stands in for a widget while the widget engine restarts.
pub(crate) const ENGINE_RESTARTING: &str = "Restarting the widget engine…";

/// Whether the widget engine runs, as its engine-wide events tell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Engine {
    /// It runs, and views open.
    #[default]
    Running,
    /// It stopped unexpectedly and restarts; no view opens until it is back.
    Restarting,
    /// It is gone for this run, for the reason given; no view opens again.
    Stopped(String),
}

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
    /// The viewer's page view was laid out this large, in logical pixels, in a window of this
    /// scale factor.
    Filled {
        /// The width in logical pixels.
        width: f32,
        /// The height in logical pixels.
        height: f32,
        /// The window's scale factor.
        scale: f32,
    },
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
    /// The viewer's page view, while the viewer shows an agent's HTML page.
    page: Option<View>,
    /// The HTML of the viewer's page, kept while the viewer shows it, so that it opens again
    /// when the engine is back.
    page_html: Option<String>,
    /// Whether the engine runs.
    engine: Engine,
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
            page: None,
            page_html: None,
            engine: Engine::Running,
            scale: 1.0,
        }
    }
}

/// One open view of a widget.
#[derive(Debug)]
pub(crate) struct View {
    /// Names the view to the engine.
    pub(crate) id: ViewId,
    /// The host side of the conversation with the page; none for the viewer's page view,
    /// whose plain page is not an MCP App.
    bridge: Option<WidgetBridge>,
    /// The tool result the bridge knows.
    result: Option<String>,
    /// The height, in logical pixels.
    pub(crate) height: u16,
    /// The height the page last reported, before it was kept within bounds.
    reported: Option<u32>,
    /// The frame shown: the newest picture of the engine's.
    pub(crate) frame: Option<Frame>,
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
    /// A view named `id` that nothing was painted for yet, talking to its page through
    /// `bridge` if it has one, which knows the tool result `result`.
    fn new(id: ViewId, bridge: Option<WidgetBridge>, result: Option<String>) -> Self {
        Self {
            id,
            bridge,
            result,
            height: DEFAULT_HEIGHT,
            reported: None,
            frame: None,
            wheel_rest: (0.0, 0.0),
            cursor: WidgetCursor::Default,
            links: LinkGate::default(),
            size: None,
            visible: true,
        }
    }

    /// Whether the page may scroll inside the view: unless it reported a height the view
    /// holds, its content may be taller than the view.
    pub(crate) fn scrolls(&self) -> bool {
        self.reported
            .is_none_or(|reported| reported > u32::from(self.height))
    }
}

/// What the transcript shows for a tool call's widget.
pub(crate) enum Look<'a> {
    /// A row with this line, which says the widget is loading or the engine restarts.
    Pending(&'static str),
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
                    Look::Pending(LOADING)
                } else {
                    Look::Unavailable(STILL_LOADING)
                }
            }
            WidgetSlot::Failed { error, .. } => Look::Unavailable(error),
            WidgetSlot::Ready(_) if self.shown.as_ref() != Some(session) => Look::Pending(LOADING),
            WidgetSlot::Ready(_) => {
                match (&self.engine, self.failed.get(call), self.views.get(call)) {
                    (Engine::Stopped(error), _, _) | (_, Some(error), _) => {
                        Look::Unavailable(error)
                    }
                    (_, None, Some(view)) => Look::View(view),
                    (Engine::Restarting, None, None) => Look::Pending(ENGINE_RESTARTING),
                    (Engine::Running, None, None) => Look::Pending(LOADING),
                }
            }
        }
    }

    /// The window's scale factor.
    pub(crate) fn scale(&self) -> f32 {
        self.scale
    }

    /// Whether the engine restarts, so that no view is open.
    pub(crate) fn restarting(&self) -> bool {
        self.engine == Engine::Restarting
    }

    /// Forgets every view, which the engine lost, without closing it; the HTML of the
    /// viewer's page is kept to open it again.
    fn lose(&mut self) {
        self.views.clear();
        self.calls.clear();
        self.page = None;
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
        let bridge = WidgetBridge::new(input.clone(), result.clone(), theme, u32::from(MAX_HEIGHT));
        self.views
            .insert(call.clone(), View::new(id, Some(bridge), result.clone()));
    }

    /// Opens the viewer's page view on the agent's page `html`, in `theme`, under the page's
    /// content security policy ([`PAGE_POLICY`]), after closing the one before. Returns the
    /// view, or why the engine refused to open it or is gone. While the engine restarts the
    /// view is named but opens only when the engine is back.
    pub(crate) fn open_page(
        &mut self,
        host: &dyn WidgetHost,
        html: &str,
        theme: WidgetTheme,
    ) -> Result<ViewId, String> {
        self.close_page(host);
        if let Engine::Stopped(error) = &self.engine {
            return Err(error.clone());
        }
        let id = ViewId(self.next);
        self.next = self.next.saturating_add(1);
        self.page_html = Some(html.to_owned());
        if self.engine == Engine::Restarting {
            return Ok(id);
        }
        let page = WidgetPage {
            html: html.to_owned(),
            csp: PAGE_POLICY.to_owned(),
            theme,
        };
        if let Err(error) = host.open(id, page) {
            self.page_html = None;
            return Err(error.to_string());
        }
        self.page = Some(View::new(id, None, None));
        Ok(id)
    }

    /// Closes the viewer's page view, if it is open, and forgets its HTML.
    pub(crate) fn close_page(&mut self, host: &dyn WidgetHost) {
        self.page_html = None;
        if let Some(page) = self.page.take()
            && let Err(error) = host.close(page.id)
        {
            tracing::warn!(%error, "the viewer's page view could not be closed");
        }
    }

    /// The viewer's page view, while it is open.
    pub(crate) fn page(&self) -> Option<&View> {
        self.page.as_ref()
    }

    /// The open view named `id`: one of the transcript's, or the viewer's page view.
    fn view_mut(&mut self, id: ViewId) -> Option<&mut View> {
        if let Some(page) = self.page.as_mut().filter(|page| page.id == id) {
            return Some(page);
        }
        let call = self.calls.get(&id)?;
        self.views.get_mut(call)
    }

    /// Whether `id` names the viewer's page view.
    fn is_page(&self, id: ViewId) -> bool {
        self.page.as_ref().is_some_and(|page| page.id == id)
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
        match self.views.get_mut(call) {
            Some(view) => admit(view, links),
            None => Vec::new(),
        }
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

/// The `links` of `view` that may open now: one per press of the user's on the view
/// ([`LinkGate`]). The rest open nothing.
fn admit(view: &mut View, links: Vec<String>) -> Vec<String> {
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

/// Closes the viewer's page view, which the engine failed or refused a command for with
/// `error`, and has the viewer say why in its place.
fn fail_page(state: &mut State, error: &str) {
    tracing::warn!(%error, "the viewer's page cannot be shown");
    state.widgets.close_page(state.widget_host.as_ref());
    if matches!(state.viewer, Some(Viewer::Page(_))) {
        state.viewer = Some(Viewer::Failed(format!("cannot show the drawing: {error}")));
    }
}

/// Runs `command` on the viewer's page view with the engine, if it is open; a refusal fails
/// the page.
fn page_command(
    state: &mut State,
    command: impl FnOnce(&dyn WidgetHost, &mut View) -> Result<(), WidgetHostError>,
) {
    let host = state.widget_host.as_ref();
    let Some(page) = state.widgets.page.as_mut() else {
        return;
    };
    if let Err(error) = command(host, page) {
        fail_page(state, &error.to_string());
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
pub(crate) fn theme_of(mode: Mode) -> WidgetTheme {
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
    let Some(model) = model.filter(|_| widgets.engine == Engine::Running) else {
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
            let outs = view
                .bridge
                .as_mut()
                .map(|bridge| bridge.result_arrived(result))
                .unwrap_or_default();
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
        Message::Rescaled(scale) => {
            rescaled(state, scale);
            Task::none()
        }
        _ => Task::none(),
    }
}

/// Applies what the engine reported: a frame becomes the one its view shows, the pointer's look
/// becomes the view's, a message from the page goes to its bridge, a link the user followed
/// in the page opens like one the page asks for, and a failure makes the widget fall back to
/// text. A lost engine forgets every view, one back has the viewer's page open again (the
/// transcript's views open with the next [`sync`]), and a stopped one leaves every widget and
/// the viewer's page as text. Returns the tasks that open the links. Reports of views that are
/// closed are ignored.
fn host_event(state: &mut State, event: WidgetHostEvent) -> Task<Message> {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    match event {
        WidgetHostEvent::Frame { view, latest } => {
            // Taken even for a closed view, so that its slot is emptied.
            let damage = latest.take();
            if let (Some(shown), Some(damage)) = (widgets.view_mut(view), damage) {
                let generation = shown
                    .frame
                    .as_ref()
                    .map_or(1, |frame| frame.generation() + 1);
                shown.frame = Some(Frame::new(view, generation, latest, damage));
            }
            Task::none()
        }
        WidgetHostEvent::Cursor { view, cursor } => {
            if let Some(shown) = widgets.view_mut(view) {
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
                .and_then(|shown| shown.bridge.as_mut())
                .map(|bridge| bridge.receive(&json))
                .unwrap_or_default();
            let links = widgets.carry_out(host, &call, outs);
            let links = widgets.admitted(&call, links);
            open_links(state, links)
        }
        WidgetHostEvent::OpenLink { view, url } => {
            if !is_web_link(&url) {
                return Task::none();
            }
            if let Some(page) = widgets.page.as_mut().filter(|page| page.id == view) {
                let links = admit(page, vec![url]);
                return open_links(state, links);
            }
            let Some(call) = widgets.calls.get(&view).cloned() else {
                return Task::none();
            };
            let links = widgets.admitted(&call, vec![url]);
            open_links(state, links)
        }
        WidgetHostEvent::Failed { view, error } => {
            if widgets.is_page(view) {
                fail_page(state, &error);
            } else if let Some(call) = widgets.calls.get(&view).cloned() {
                widgets.fail(host, &call, error);
            }
            Task::none()
        }
        WidgetHostEvent::EngineLost => {
            tracing::warn!("the widget engine stopped; it restarts");
            widgets.lose();
            widgets.engine = Engine::Restarting;
            Task::none()
        }
        WidgetHostEvent::EngineBack => {
            tracing::info!("the widget engine runs again");
            widgets.engine = Engine::Running;
            reopen_page(state);
            Task::none()
        }
        WidgetHostEvent::EngineStopped { error } => {
            widgets.lose();
            widgets.engine = Engine::Stopped(error.clone());
            fail_page(state, &error);
            Task::none()
        }
    }
}

/// Opens the viewer's page view again on the HTML it showed, once the engine is back; a
/// refusal says why in its place.
fn reopen_page(state: &mut State) {
    let Some(html) = state.widgets.page_html.clone() else {
        return;
    };
    if state.widgets.page.is_some() || !matches!(state.viewer, Some(Viewer::Page(_))) {
        return;
    }
    let opened = state
        .widgets
        .open_page(state.widget_host.as_ref(), &html, theme_of(state.mode));
    state.viewer = Some(match opened {
        Ok(view) => Viewer::Page(view),
        Err(error) => Viewer::Failed(format!("cannot show the drawing: {error}")),
    });
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
    if state.widgets.is_page(view) {
        page_message(state, message);
        return;
    }
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    let Some(call) = widgets.calls.get(&view).cloned() else {
        return;
    };
    if let WidgetViewMessage::Sized { width, .. } = message {
        let outs = widgets
            .views
            .get_mut(&call)
            .and_then(|shown| shown.bridge.as_mut())
            .map(|bridge| bridge.width_changed(pixels(width)))
            .unwrap_or_default();
        // A width only ever posts to the page.
        let _links = widgets.carry_out(host, &call, outs);
    }
    widgets.command(host, &call, |host, shown| match message {
        WidgetViewMessage::Sized { width, scale } => {
            shown.size = Some((width, scale));
            resize(host, shown)
        }
        WidgetViewMessage::Input(input) => pointer(host, shown, input),
        WidgetViewMessage::Visible(visible) if visible != shown.visible => {
            shown.visible = visible;
            host.set_visible(shown.id, visible)
        }
        WidgetViewMessage::Visible(_) | WidgetViewMessage::Filled { .. } => Ok(()),
    });
}

/// Applies what the viewer's page view reported: the size it fills goes to the engine, and
/// pointer input as well. It is never hidden while the viewer shows it, so whether it is in
/// the viewport changes nothing, and it has no width of a transcript's row.
fn page_message(state: &mut State, message: WidgetViewMessage) {
    page_command(state, |host, page| match message {
        WidgetViewMessage::Filled {
            width,
            height,
            scale,
        } => {
            page.size = Some((width, scale));
            page.height = u16::try_from(pixels(height)).unwrap_or(u16::MAX);
            resize(host, page)
        }
        WidgetViewMessage::Input(input) => pointer(host, page, input),
        WidgetViewMessage::Sized { .. } | WidgetViewMessage::Visible(_) => Ok(()),
    });
}

/// Hands the pointer's `input` over `view` to the engine: a press or a release lets a link
/// through ([`LinkGate`]), and a wheel turn goes by whole pixels ([`whole_wheel`]).
fn pointer(
    host: &dyn WidgetHost,
    view: &mut View,
    input: WidgetInput,
) -> Result<(), WidgetHostError> {
    match input {
        WidgetInput::Press { .. } => view.links.pressed(std::time::Instant::now()),
        WidgetInput::Release { .. } => view.links.released(std::time::Instant::now()),
        _ => {}
    }
    match whole_wheel(view, input) {
        Some(input) => host.input(view.id, input),
        None => Ok(()),
    }
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
            .and_then(|view| view.bridge.as_mut())
            .map(|bridge| bridge.theme_changed(theme))
            .unwrap_or_default();
        // A theme change only ever posts to the page.
        let _links = widgets.carry_out(host, call, outs);
    }
    page_command(state, |host, page| host.set_theme(page.id, theme));
}

/// Takes the window's new scale factor `scale`, and resizes every view laid out at another.
fn rescaled(state: &mut State, scale: f32) {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    widgets.scale = scale;
    let open: Vec<ToolCallId> = widgets.views.keys().cloned().collect();
    for call in &open {
        widgets.command(host, call, |host, view| rescale(host, view, scale));
    }
    page_command(state, |host, page| rescale(host, page, scale));
}

/// Resizes `view` at the scale factor `scale` when it was laid out at another.
fn rescale(host: &dyn WidgetHost, view: &mut View, scale: f32) -> Result<(), WidgetHostError> {
    match view.size {
        Some((width, before)) if before.to_bits() != scale.to_bits() => {
            view.size = Some((width, scale));
            resize(host, view)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
