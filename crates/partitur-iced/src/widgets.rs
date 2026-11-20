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
//! page, a link it asks for opens with the session's link opener, and a height it reports
//! becomes the view's, kept within 80 to 640 logical pixels; until it reports one the view is
//! 160 pixels tall. A tool result that arrives after the view opened is handed to the bridge,
//! and so is every change of the desktop's theme. The engine's last frame of a view is kept
//! as an image for the transcript to draw.
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
use std::sync::Arc;

use iced::Task;
use iced::theme::Mode;
use iced::widget::Id;
use iced::widget::image::Handle;
use partitur_core::widget_bridge::{BridgeOut, WidgetBridge, WidgetTheme, content_security_policy};
use partitur_core::widget_host::{
    ViewId, WidgetHost, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
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
    /// The last frame the engine painted.
    pub(crate) frame: Option<Handle>,
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
                bridge: WidgetBridge::new(input.clone(), result.clone(), theme),
                result: result.clone(),
                height: DEFAULT_HEIGHT,
                frame: None,
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
        Message::Rescaled(scale) => {
            rescaled(state, scale);
            Task::none()
        }
        _ => Task::none(),
    }
}

/// Applies what the engine reported: a frame becomes the view's picture, a message from the
/// page goes to its bridge, and a failure makes the widget fall back to text. Returns the
/// tasks that open the links the page asked for. Reports of views that are closed are
/// ignored.
fn host_event(state: &mut State, event: WidgetHostEvent) -> Task<Message> {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    match event {
        WidgetHostEvent::Frame {
            view,
            width,
            height,
            rgba,
        } => {
            let shown = widgets
                .calls
                .get(&view)
                .and_then(|call| widgets.views.get_mut(call));
            if let Some(shown) = shown {
                let pixels = Arc::unwrap_or_clone(rgba);
                shown.frame = Some(Handle::from_rgba(width, height, pixels));
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

/// Applies what the view `view` of the transcript reported: its size goes to the engine,
/// as does pointer input, and a change of whether it is in the viewport shows or hides it.
fn view_message(state: &mut State, view: ViewId, message: WidgetViewMessage) {
    let host = state.widget_host.as_ref();
    let widgets = &mut state.widgets;
    let Some(call) = widgets.calls.get(&view).cloned() else {
        return;
    };
    widgets.command(host, &call, |host, shown| match message {
        WidgetViewMessage::Sized { width, scale } => {
            shown.size = Some((width, scale));
            resize(host, shown)
        }
        WidgetViewMessage::Input(input) => host.input(shown.id, input),
        WidgetViewMessage::Visible(visible) if visible != shown.visible => {
            shown.visible = visible;
            host.set_visible(shown.id, visible)
        }
        WidgetViewMessage::Visible(_) => Ok(()),
    });
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
    use std::sync::Arc;

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
    use partitur_core::widget_host::{ViewId, WidgetHostEvent, WidgetInput, WidgetPage};
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
            content_security_policy(META).contains("connect-src https://api.example.com"),
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

        let _task = deliver_widget(
            &mut fixture.state,
            WidgetHostEvent::Frame {
                view: view_id,
                width: 2,
                height: 1,
                rgba: Arc::new(vec![255, 0, 0, 255, 0, 0, 255, 255]),
            },
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
        let task = page_says(
            &mut fixture.state,
            view_id,
            r#"{"jsonrpc":"2.0","id":3,"method":"ui/open-link","params":{"url":"file:///etc/passwd"}}"#,
        );
        assert_eq!(task.units(), 0);
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

    #[test]
    fn pointer_input_reaches_the_view() {
        let (mut fixture, view_id) = with_widget();
        let _task = deliver_widget(
            &mut fixture.state,
            WidgetHostEvent::Frame {
                view: view_id,
                width: 1,
                height: 1,
                rgba: Arc::new(vec![0, 0, 0, 255]),
            },
        );
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
                    dy: -30.0
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
}
