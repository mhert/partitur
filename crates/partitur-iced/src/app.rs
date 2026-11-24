//! The application: its state, the messages that change it, and the update, view and
//! subscription logic iced drives. The state keeps the current time, which a tick every 30
//! seconds brings up to date, so the tree's activity column tells how long ago each session
//! was active without asking the clock on every frame. At the start, the children that wait
//! for their orchestrator are reported to it; a report that fails shows in the error bar.
//! The state also holds the process environment clicked links are opened in. When Partitur
//! draws the window's frame itself, the state remembers whether the window is maximized, and
//! the window is moved, resized, minimized and closed through its own messages. It holds the
//! widget engine and the MCP App widgets of the shown session (see [`widgets`]), and the
//! window's scale factor, which it asks for at the start and follows as it changes. A
//! session that leaves the tree, deleted or closed with its project, is forgotten by every
//! part of the state that keeps something per session ([`forget_sessions`]).

use std::cell::OnceCell;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::channel::mpsc::UnboundedReceiver;
use iced::theme::Mode;
use iced::widget::{column, image};
use iced::{Element, Event, Subscription, Task, event, keyboard, system, time, window};
use partitur_core::widget_host::{NoWidgetHost, ViewId, WidgetHost, WidgetHostEvent};
use partitur_core::{
    App, AppError, BackendEvent, GitEvent, ProcessEnv, SessionId, StoreEvent, ToolCall, TreeModel,
};

use crate::chrome::{Chrome, Platform, WindowAction, WindowFrame, chrome, resize_edges};
use crate::confirm_sheet::{self, ConfirmMessage, ConfirmSheet};
use crate::env_view::{self, EnvMessage};
use crate::jump::{self, JumpMessage, JumpState};
use crate::new_session_sheet::{self, NewSessionMessage};
use crate::projects_view::{self, ProjectsMessage, ProjectsState};
use crate::session_view::{self, SessionMessage, SessionState, question};
use crate::spec_view::state::{self as flow, FlowMessage, FlowState};
use crate::viewer::{self, Shown, ShownImage, Viewer};
use crate::widgets::{self, WidgetViewMessage, Widgets};
use crate::{bridge, layout, previews, spec_view, theme, toolbar};

/// The views the toolbar switches between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The open projects and their sessions.
    Projects,
    /// The selected session's transcript and prompt; disabled while no session is selected.
    Session,
    /// The design and the plan of the selected session's workflow; disabled for a session
    /// that reports none.
    Spec,
    /// What the selected session runs with; disabled while no session is selected.
    Env,
}

impl Tab {
    /// The first tab after this one in the toolbar's order, after the last coming the first,
    /// that `enabled` lets through. The Projects tab is taken when no other tab is.
    pub(crate) fn next(self, enabled: impl Fn(Tab) -> bool) -> Self {
        let tabs = toolbar::TABS;
        let at = tabs.iter().position(|tab| *tab == self).unwrap_or(0);
        tabs.iter()
            .cycle()
            .skip(at + 1)
            .take(tabs.len())
            .copied()
            .find(|tab| enabled(*tab))
            .unwrap_or(Self::Projects)
    }
}

/// A key pressed without a modifier that the application answers to. Who answers depends on
/// what the window shows, which only [`update`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareKey {
    /// Tab: the tab after the active one.
    Tab,
    /// The down arrow: the row below the highlighted one.
    ArrowDown,
    /// The up arrow: the row above the highlighted one.
    ArrowUp,
    /// Enter: the highlighted row is opened, or a waiting gate or a proposed split is approved.
    Enter,
    /// Escape: the viewer or the jump panel closes, or the open question on the Session tab is cancelled.
    Escape,
    /// N: the new-session sheet opens.
    N,
    /// A: the highlighted session is archived.
    A,
    /// D: the highlighted session is deleted.
    D,
    /// G: the Spec tab of the session that has waited longest at a gate opens.
    G,
    /// F2: the highlighted session's title is edited.
    F2,
    /// A digit from 1 to 6: the option of that number on the open question's card is picked.
    Digit(u8),
}

/// How often the current time the activity column tells from is brought up to date.
const TICK: Duration = Duration::from_secs(30);

/// Everything that changes the [`State`].
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The backend reported something.
    Backend(BackendEvent),
    /// The git port reported the outcome of a request.
    Git(GitEvent),
    /// The store's queue reported a failed write or a load.
    Store(StoreEvent),
    /// An orchestrator session called one of Partitur's tools.
    Tool(ToolCall),
    /// The desktop's preference for light or dark, at start and whenever it changes.
    SystemTheme(Mode),
    /// The user picked a tab.
    SelectTab(Tab),
    /// The user pressed one of the bare keys.
    Key(BareKey),
    /// An interaction on the Projects tab.
    Projects(ProjectsMessage),
    /// The user picked a session, on any tab: the Session tab shows it.
    SelectSession(SessionId),
    /// An interaction on the Session tab.
    Session(SessionMessage),
    /// An interaction on the Env tab.
    Env(EnvMessage),
    /// An interaction with the current session's workflow.
    Flow(FlowMessage),
    /// An interaction with the jump panel, or the request to open it.
    Jump(JumpMessage),
    /// An interaction with the confirmation sheet.
    Confirm(ConfirmMessage),
    /// An interaction with the new-session sheet.
    NewSession(NewSessionMessage),
    /// Something is to be shown to the user in the viewer over the window.
    Show(Shown),
    /// The viewer finished preparing the content asked for, or failed to.
    Shown(Shown, Result<ShownImage, String>),
    /// The preview file of a sent image at the path was read and decoded, or could not be.
    Preview(PathBuf, Option<image::Handle>),
    /// The viewer is closed.
    CloseViewer,
    /// Something failed that the user should know about.
    Error(String),
    /// The user dismissed the error bar.
    DismissError,
    /// Time passed: the current time is read again.
    Tick,
    /// The window drew a frame, which ends a run that lasts until the first one.
    FrameDrawn,
    /// The user asked something of the window the system draws no decorations for.
    Window(WindowAction),
    /// The left mouse button went down on the title bar outside its buttons; the first move
    /// that follows starts moving the window.
    BarPressed,
    /// The mouse moved over the title bar, or left it, while the bar was pressed.
    BarMoved,
    /// The left mouse button came up over the title bar.
    BarReleased,
    /// The window was resized: whether it is maximized is asked again.
    Resized,
    /// Whether the window is maximized, as the system answered.
    Maximized(bool),
    /// The widget engine reported something.
    Widget(WidgetHostEvent),
    /// A widget's view in the transcript reported something.
    WidgetView(ViewId, WidgetViewMessage),
    /// The renderer allocated the picture of a widget's view that was asked for last, or
    /// could not.
    WidgetFrame(ViewId, Result<image::Allocation, String>),
    /// The window's scale factor, at the start and whenever it changes.
    Rescaled(f32),
}

/// The state of the interface: the core [`App`] and what only the interface remembers.
pub struct State {
    pub(crate) app: App,
    /// The desktop's preference for light or dark, which picks the theme.
    pub(crate) mode: Mode,
    pub(crate) tab: Tab,
    /// The error the bar under the toolbar shows, until it is dismissed.
    pub(crate) error: Option<String>,
    /// The error of the new-session sheet's last action, which the sheet shows while it is
    /// open.
    pub(crate) sheet_error: Option<String>,
    pub(crate) projects: ProjectsState,
    pub(crate) session: SessionState,
    /// The documents of the sessions' workflows, read in the background, and which one the
    /// Spec tab shows.
    pub(crate) flow: FlowState,
    /// The jump panel, while it is open over the window.
    pub(crate) jump: Option<JumpState>,
    /// The sheet that confirms archiving or deleting a session, while it is open over the
    /// window.
    pub(crate) confirm: Option<ConfirmSheet>,
    /// The viewer, while it is open over the window.
    pub(crate) viewer: Option<Viewer>,
    /// The current time in seconds since the Unix epoch, as of the start or the last tick.
    pub(crate) now: u64,
    /// The environment a configured link opener runs in; nothing added until [`crate::run`]
    /// hands over the application's.
    pub(crate) process_env: ProcessEnv,
    /// The window's frame: who draws its title bar, set by [`crate::run`].
    pub(crate) chrome: Chrome,
    /// Whether the window is maximized, as of its last resize; a maximized window has no
    /// edges to resize it by.
    pub(crate) maximized: bool,
    /// Whether the left mouse button is down on the title bar and the mouse has not moved
    /// since, which keeps a double click from moving the window.
    pub(crate) bar_pressed: bool,
    /// The engine that renders widgets; one that opens none until [`crate::run`] hands over
    /// the application's.
    pub(crate) widget_host: Box<dyn WidgetHost>,
    /// The widgets of the shown session.
    pub(crate) widgets: Widgets,
}

/// The initial state on the Projects tab with the tree's cursor on the current session, and
/// the task that learns the desktop's light or dark preference and the window's scale
/// factor, forwards every event `events` yields as a [`Message::Backend`] and every call
/// `tool_calls` yields as a [`Message::Tool`].
///
/// First the app reports the children that wait for their orchestrator
/// ([`App::report_waiting_at_start`]); the error of a report that failed is shown in the
/// error bar, and that child waits for the user.
pub fn boot(
    mut app: App,
    events: UnboundedReceiver<BackendEvent>,
    tool_calls: UnboundedReceiver<ToolCall>,
) -> (State, Task<Message>) {
    let mut error = None;
    report(&mut error, app.report_waiting_at_start());
    let state = State {
        projects: ProjectsState {
            cursor: app.current_session(),
            ..ProjectsState::default()
        },
        app,
        mode: Mode::default(),
        tab: Tab::Projects,
        error,
        sheet_error: None,
        session: SessionState::default(),
        flow: FlowState::default(),
        jump: None,
        confirm: None,
        viewer: None,
        now: unix_seconds(),
        process_env: ProcessEnv::ready(Vec::new()),
        chrome: chrome(WindowFrame::default(), Platform::current()),
        maximized: false,
        bar_pressed: false,
        widget_host: Box::new(NoWidgetHost),
        widgets: Widgets::default(),
    };
    let mode = system::theme().map(Message::SystemTheme);
    let scale = window::latest()
        .and_then(window::scale_factor)
        .map(Message::Rescaled);
    let tasks = [
        mode,
        scale,
        bridge::events(events),
        bridge::tool_calls(tool_calls),
    ];
    (state, Task::batch(tasks))
}

/// Applies `message` to `state` and returns the follow-up work for the runtime.
///
/// A backend or git event or a tool call the [`App`] rejects is logged and shown in the error
/// bar. Every backend event and tool call brings the current session's parsed answers up to
/// date, every git event has the split cards check their children again, and a git event that
/// finishes the new-session sheet's Create shows the new session.
///
/// The split cards also check their children again after any other message that created,
/// archived, restored or deleted a session or selected another one, since a child may not
/// share its checkout with a live session. Backend events are left out: they stream in
/// while an agent answers and never change which sessions there are.
///
/// When the Env tab opens or closes the core is told, so it reads the current session's
/// git state for the tab's Git card while the tab is open.
///
/// After every message the Session tab brings the window of rows its transcript lays out up
/// to date, then the shown session's widgets follow (see [`widgets::sync`]), and the previews
/// of the images its prompts were sent with that were not asked for yet start loading off the
/// loop (see [`previews::sync`]); when the tab is shown again, its transcript stands at its
/// end. A widget's frame, the pointer's look it asks for and pointer input over a widget are
/// left out: they arrive many times a second, and none of them changes which entries there
/// are, how they are laid out or which widgets are read. Widget messages also never
/// change the sessions, so the split cards' re-check does not watch them. A change of tab closes the Projects
/// tab's title field, keeping the old title. A disabled tab is never left showing: the
/// Session tab takes the place of a disabled Spec tab while a session is selected, and the
/// Projects tab takes the place of any other.
pub fn update(state: &mut State, message: Message) -> Task<Message> {
    let watched = !matches!(
        message,
        Message::Backend(_)
            | Message::Git(_)
            | Message::Store(_)
            | Message::Widget(_)
            | Message::WidgetView(..)
            | Message::WidgetFrame(..)
    );
    let streaming = matches!(
        message,
        Message::Widget(WidgetHostEvent::Frame { .. } | WidgetHostEvent::Cursor { .. })
            | Message::WidgetView(_, WidgetViewMessage::Input(_))
            | Message::WidgetFrame(..)
    );
    if let Message::Backend(event) = &message {
        widgets::observe(&mut state.widgets, event);
    }
    let before = watched.then(|| sessions_seen(&state.app));
    let tab = state.tab;
    let task = dispatch(state, message);
    if !tab_enabled(state, state.tab) {
        state.tab = if state.tab == Tab::Spec && tab_enabled(state, Tab::Session) {
            Tab::Session
        } else {
            Tab::Projects
        };
    }
    if before.is_some_and(|before| before != sessions_seen(&state.app)) {
        session_view::refresh_checks(state);
    }
    // A title field left open on the Projects tab would take keys on its return.
    if tab != state.tab {
        state.projects.renaming = None;
    }
    // The Session tab is built anew when it is shown again, its transcript at the end.
    if tab != Tab::Session && state.tab == Tab::Session {
        session_view::shows_the_end(&mut state.session);
    }
    let previews = if streaming {
        Task::none()
    } else {
        follow_up(state)
    };
    env_git(state, tab);
    Task::batch([task, previews])
}

/// What follows every message that is not streamed, as [`update`] describes: the Session
/// tab's window of rows, the widgets and the previews follow the current session, whose view
/// is built once for all three. Returns the work that loads the previews.
pub(crate) fn follow_up(state: &mut State) -> Task<Message> {
    let model = state.app.session_view();
    let model = model.as_ref();
    session_view::keep_window(&mut state.session, state.tab, model);
    widgets::sync(
        &mut state.widgets,
        state.widget_host.as_ref(),
        state.mode,
        state.tab,
        &state.session,
        model,
    );
    previews::sync(&mut state.session.previews, model)
}

/// Tells the core whether the Env tab is open after a message that left `before` showing,
/// when that changed: opening the tab reads the current session from git. Every way to
/// select another session leaves the Env tab, so the session shown there changes only as the
/// tab opens.
fn env_git(state: &mut State, before: Tab) {
    let open = state.tab == Tab::Env;
    if (before == Tab::Env) == open {
        return;
    }
    state.app.set_env_open(open);
}

/// What the split cards' checks depend on among the sessions: the current one and every
/// live one. Cheap enough to take before and after every keystroke.
fn sessions_seen(app: &App) -> (Option<SessionId>, Vec<SessionId>) {
    (app.current_session(), app.live_sessions())
}

/// Applies what git answered, which the split cards and the new-session sheet then take in.
fn git_event(state: &mut State, event: GitEvent) -> Task<Message> {
    let was_creating = state.app.draft().is_some_and(|draft| draft.creating);
    if let Err(error) = state.app.apply_git_event(event) {
        tracing::warn!(%error, "a git event could not be applied");
        state.error = Some(error.to_string());
    }
    // The split cards take in what git answered: a reading, or an approval ended.
    session_view::refresh_checks(state);
    new_session_sheet::after_git(state, was_creating)
}

/// Applies what the store's queue reported: a failed write shows in the error bar, while
/// what it would have saved is shown all the same.
fn store_event(state: &mut State, event: StoreEvent) -> Task<Message> {
    if let Err(error) = state.app.apply_store_event(event) {
        tracing::warn!(%error, "a write to the store failed");
        state.error = Some(error.to_string());
    }
    Task::none()
}

/// Applies `message` to `state` as [`update`] describes, without the re-check that follows
/// a change of the sessions.
fn dispatch(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Backend(event) => {
            if let Err(error) = state.app.apply_event(event) {
                tracing::warn!(%error, "a backend event could not be applied");
                state.error = Some(error.to_string());
            }
            session_view::sync_markdown(state);
            flow::sync(state)
        }
        Message::Git(event) => git_event(state, event),
        Message::Store(event) => store_event(state, event),
        Message::Tool(call) => {
            if let Err(error) = state.app.call_tool(call) {
                tracing::warn!(%error, "a tool call could not be handled");
                state.error = Some(error.to_string());
            }
            session_view::sync_markdown(state);
            flow::sync(state)
        }
        Message::SystemTheme(mode) => system_theme(state, mode),
        // A sheet covers the window and keeps the user's attention: no shortcut reaches
        // the page under it.
        Message::SelectTab(_) | Message::Jump(JumpMessage::Open)
            if state.confirm.is_some() || state.viewer.is_some() || state.app.draft().is_some() =>
        {
            Task::none()
        }
        Message::SelectTab(tab) if !tab_enabled(state, tab) => Task::none(),
        Message::SelectTab(tab) => {
            state.tab = tab;
            if tab == Tab::Spec {
                return flow::sync(state);
            }
            Task::none()
        }
        Message::Key(key) => press(state, key),
        Message::Projects(message) => projects_view::update(state, message),
        Message::SelectSession(session) => projects_view::select(state, session),
        Message::Session(message) => session_view::update(state, message),
        Message::Env(message) => env_view::update(&state.process_env, message),
        Message::Flow(message) => flow::update(state, message),
        Message::Jump(message) => jump::update(state, message),
        Message::Confirm(message) => confirm_sheet::update(state, message),
        Message::Show(shown) => viewer::show(state, shown),
        Message::Shown(asked, result) => {
            viewer::shown(state, &asked, result);
            Task::none()
        }
        Message::Preview(path, picture) => previews::landed(state, path, picture),
        Message::CloseViewer => {
            state.viewer = None;
            Task::none()
        }
        Message::NewSession(message) => new_session_sheet::update(state, message),
        Message::Error(error) => {
            state.error = Some(error);
            Task::none()
        }
        Message::DismissError => {
            state.error = None;
            Task::none()
        }
        Message::Tick => {
            state.now = unix_seconds();
            Task::none()
        }
        Message::FrameDrawn => iced::exit(),
        Message::Window(action) => {
            state.bar_pressed = false;
            window_task(action)
        }
        Message::BarPressed => {
            state.bar_pressed = true;
            Task::none()
        }
        Message::BarMoved if std::mem::take(&mut state.bar_pressed) => {
            window_task(WindowAction::Drag)
        }
        Message::BarMoved => Task::none(),
        Message::BarReleased => {
            state.bar_pressed = false;
            Task::none()
        }
        Message::Resized => window::latest()
            .and_then(window::is_maximized)
            .map(Message::Maximized),
        Message::Maximized(maximized) => {
            state.maximized = maximized;
            Task::none()
        }
        message @ (Message::Widget(_)
        | Message::WidgetView(..)
        | Message::WidgetFrame(..)
        | Message::Rescaled(_)) => widgets::update(state, message),
    }
}

/// Takes the desktop's preference `mode` for light or dark, and tells the open widgets.
fn system_theme(state: &mut State, mode: Mode) -> Task<Message> {
    state.mode = mode;
    widgets::theme_changed(state);
    Task::none()
}

/// The task that does what `action` asks of the window: closing it ends the application,
/// every other action is done to the latest window, which is the only one.
fn window_task(action: WindowAction) -> Task<Message> {
    if action == WindowAction::Close {
        return iced::exit();
    }
    window::latest().and_then(move |id| match action {
        WindowAction::Drag => window::drag(id),
        WindowAction::ToggleMaximize => window::toggle_maximize(id),
        WindowAction::Minimize => window::minimize(id, true),
        WindowAction::Menu => window::show_system_menu(id),
        WindowAction::Resize(direction) => window::drag_resize(id, direction),
        WindowAction::Close => Task::none(),
    })
}

/// Forgets what the tabs remember about `sessions`, which left the tree, so nothing of a
/// deleted session lingers: the Session tab's drafts, caches and previews, the Spec tab's
/// documents and the widgets reported as loading.
pub(crate) fn forget_sessions(state: &mut State, sessions: &[SessionId]) {
    state.session.forget(sessions);
    state.flow.forget(sessions);
    state.widgets.forget(sessions);
}

/// Whether `tab` is enabled: the Projects tab always, the Session and Env tabs while a
/// session is selected, and the Spec tab while the current session reports a workflow.
/// Asked on every frame, so it builds no view of the transcript.
pub(crate) fn tab_enabled(state: &State, tab: Tab) -> bool {
    match tab {
        Tab::Projects => true,
        Tab::Session | Tab::Env => state.app.current_session().is_some(),
        Tab::Spec => state.app.current_workflow().is_some(),
    }
}

/// The current time in seconds since the Unix epoch; `0` for a clock set before it.
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Hands the bare `key` to whoever answers to it now: the viewer, the new-session sheet, the
/// confirmation sheet or the jump panel while it is open, then the title field of the
/// Projects tab while it is open (Enter commits it, Escape cancels it and every other key is
/// ignored), else the tabs (Tab cycles them, skipping the disabled ones), the Session tab
/// (Enter approves the gate the session waits at, or else a proposed split; Escape cancels
/// the open question and the digits 1 to 6 pick its options), the Spec tab
/// (Enter approves only the gate), G (on every tab: the Spec tab of the oldest waiting gate)
/// and the Projects tab's tree (the arrow keys, Enter, N, A, D and F2).
fn press(state: &mut State, key: BareKey) -> Task<Message> {
    if state.viewer.is_some() {
        return viewer::press(state, key);
    }
    if state.app.draft().is_some() {
        return new_session_sheet::press(state, key);
    }
    if state.confirm.is_some() {
        return confirm_sheet::press(state, key);
    }
    if state.jump.is_some() {
        return jump::press(state, key);
    }
    if state.tab == Tab::Projects && state.projects.renaming.is_some() {
        let rename = match key {
            BareKey::Enter => ProjectsMessage::CommitRename,
            BareKey::Escape => ProjectsMessage::CancelRename,
            _ => return Task::none(),
        };
        return projects_view::update(state, rename);
    }
    let tree_key = match key {
        BareKey::Tab => {
            state.tab = state.tab.next(|tab| tab_enabled(state, tab));
            return Task::none();
        }
        BareKey::Escape if state.tab == Tab::Session && state.app.current_question().is_some() => {
            return question::press_escape(state);
        }
        BareKey::Digit(digit) if state.tab == Tab::Session => {
            return question::press_digit(state, digit);
        }
        BareKey::Escape | BareKey::Digit(_) => return Task::none(),
        BareKey::Enter if state.tab == Tab::Session => return flow::press_enter(state),
        BareKey::Enter if state.tab == Tab::Spec => return flow::press_enter_on_spec(state),
        BareKey::A => ProjectsMessage::ArchiveAtCursor,
        BareKey::D => ProjectsMessage::DeleteAtCursor,
        BareKey::G => return flow::next_gate(state),
        BareKey::ArrowDown => ProjectsMessage::CursorDown,
        BareKey::ArrowUp => ProjectsMessage::CursorUp,
        BareKey::Enter => ProjectsMessage::OpenCursor,
        BareKey::N => ProjectsMessage::NewSessionAtCursor,
        BareKey::F2 => match (&state.projects.cursor, state.tab) {
            (Some(cursor), Tab::Projects) => ProjectsMessage::StartRename(cursor.clone()),
            _ => return Task::none(),
        },
    };
    projects_view::update(state, tree_key)
}

/// The window: the toolbar, the error bar when there is an error, and the active tab; over
/// it the viewer, the new-session sheet, the confirmation sheet or the jump panel while it is
/// open. Under the system's frame that covers the toolbar too. When the toolbar is the title
/// bar it covers only what lies below the toolbar, whose window buttons and dragging keep
/// working while its tabs and jump button do nothing. When the toolbar draws the window's
/// buttons and the window is not maximized, the edges that resize it lie over everything.
pub fn view(state: &State) -> Element<'_, Message> {
    let window = overlaid(state);
    if state.chrome.controls && !state.maximized {
        resize_edges(window)
    } else {
        window
    }
}

/// The toolbar and the page with the viewer, the new-session sheet, the confirmation sheet or
/// the jump panel over it while it is open, as [`view`] describes. The tree is built only for
/// the Projects tab and the jump panel, once for both.
fn overlaid(state: &State) -> Element<'_, Message> {
    let no_session = || layout::empty("No session selected");
    let tree = OnceCell::new();
    let tree = || tree.get_or_init(|| state.app.tree());
    let approvable = state.app.approvable_gate().is_some();
    let body = match state.tab {
        Tab::Projects => projects_view::view(tree(), &state.projects, state.now),
        Tab::Session => state.app.session_view().map_or_else(no_session, |model| {
            session_view::view(
                &model,
                &state.session,
                &state.widgets,
                approvable,
                theme::tokens_for(state.mode),
            )
        }),
        Tab::Spec => state
            .app
            .session_view()
            .filter(|model| model.workflow.is_some())
            .map_or_else(no_session, |model| {
                spec_view::view(
                    &model,
                    &state.flow,
                    approvable,
                    theme::tokens_for(state.mode),
                )
            }),
        Tab::Env => state
            .app
            .current_session()
            .zip(state.app.env_view())
            .map_or_else(no_session, |(session, model)| {
                env_view::view(&session, &model, approvable, theme::tokens_for(state.mode))
            }),
    };

    let bar = toolbar::toolbar(
        state.tab,
        |tab| tab_enabled(state, tab),
        modal_open(state),
        state.chrome,
        state.maximized,
        state.bar_pressed,
    );
    // As the title bar the toolbar stays out from under the sheets, so the window can still
    // be moved and closed.
    let (title_bar, mut page) = if state.chrome.drags {
        (Some(bar), column![])
    } else {
        (None, column![bar])
    };
    if let Some(error) = &state.error {
        page = page.push(layout::error_bar(error));
    }
    let page = covered(state, page.push(body).into(), tree);
    match title_bar {
        Some(bar) => column![bar, page].into(),
        None => page,
    }
}

/// Whether the viewer, the new-session sheet, the confirmation sheet or the jump panel is
/// open.
fn modal_open(state: &State) -> bool {
    state.viewer.is_some()
        || state.app.draft().is_some()
        || state.confirm.is_some()
        || state.jump.is_some()
}

/// `page` with the viewer, the new-session sheet, the confirmation sheet or the jump panel
/// over it while it is open, the first of them that is open; `tree` gives what the jump panel
/// lists, and is asked only while it is open.
fn covered<'a, 't>(
    state: &'a State,
    page: Element<'a, Message>,
    tree: impl FnOnce() -> &'t TreeModel,
) -> Element<'a, Message> {
    if let Some(viewer) = &state.viewer {
        return viewer::view(page, viewer);
    }
    if let Some(draft) = state.app.draft() {
        return new_session_sheet::view(
            page,
            &draft,
            state.sheet_error.as_deref(),
            theme::tokens_for(state.mode),
        );
    }
    if let Some(sheet) = &state.confirm {
        return confirm_sheet::view(
            page,
            sheet,
            &confirm_sheet::lines(&state.app, sheet),
            state.error.as_deref(),
            theme::tokens_for(state.mode),
        );
    }
    match &state.jump {
        Some(panel) => jump::view(page, tree(), panel),
        None => page,
    }
}

/// The keyboard shortcuts (Ctrl+K opens the jump panel; Ctrl+1 to Ctrl+4 select a tab; the
/// bare keys Tab, the arrows, Enter, Escape, N, A, D, G, F2 and 1 to 6 go to whoever answers
/// to them),
/// the desktop's changes between light and dark, the window's changes of its scale factor,
/// and a tick every 30 seconds. When the toolbar draws the window's buttons, every resize of
/// the window asks again whether it is maximized.
pub fn subscription(state: &State) -> Subscription<Message> {
    let mut subscriptions = vec![
        event::listen_with(key_message),
        event::listen_with(rescaled),
        system::theme_changes().map(Message::SystemTheme),
        time::every(TICK).map(|_| Message::Tick),
    ];
    if state.chrome.controls {
        subscriptions.push(window::resize_events().map(|_| Message::Resized));
    }
    Subscription::batch(subscriptions)
}

/// What the keyboard `event` asks for, given whether a widget took it (`status`).
///
/// A shortcut counts only when no widget took its key, with one exception: a focused text
/// field takes Escape to give up the focus, and the jump panel's field has the focus while
/// the panel is open, yet Escape closes the panel all the same.
pub(crate) fn key_message(
    event: Event,
    status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    let Event::Keyboard(event) = event else {
        return None;
    };
    let message = shortcut(event)?;
    (status == event::Status::Ignored || message == Message::Key(BareKey::Escape))
        .then_some(message)
}

/// The window's new scale factor, if `event` reports one.
#[expect(
    clippy::needless_pass_by_value,
    reason = "`event::listen_with` hands the event over by value"
)]
fn rescaled(event: Event, _status: event::Status, _window: window::Id) -> Option<Message> {
    match event {
        Event::Window(window::Event::Rescaled(scale)) => Some(Message::Rescaled(scale)),
        _ => None,
    }
}

/// The value of an `App` call, or `None` after logging its error and showing it in the
/// error bar.
///
/// Takes only the error bar, so the caller can borrow `state.app` for the call.
pub(crate) fn report<T>(error_bar: &mut Option<String>, result: Result<T, AppError>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, "a command could not be applied");
            *error_bar = Some(error.to_string());
            None
        }
    }
}

/// What a key press asks for, if it is one of the shortcuts.
fn shortcut(event: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed {
        key,
        physical_key,
        modifiers,
        repeat,
        ..
    } = event
    else {
        return None;
    };
    // Only the bare keys count: with Alt or Ctrl held, Tab belongs to the desktop, and a
    // letter with a modifier is not the letter's shortcut.
    if modifiers.is_empty() {
        // A held Enter counts once: its first press can open an orchestrator's session, and
        // the presses that follow would approve the split proposed there.
        return bare_key(&key)
            .filter(|bare| !(repeat && *bare == BareKey::Enter))
            .map(Message::Key);
    }
    if jump_shortcut(&key, physical_key, modifiers) {
        return Some(Message::Jump(JumpMessage::Open));
    }
    tab_shortcut(&key, physical_key, modifiers).map(Message::SelectTab)
}

/// The bare key that `key`, pressed without a modifier, is, if the application answers to
/// it.
///
/// The subscription cannot see what the window shows, so the keys are reported as they are
/// and [`update`] decides who answers.
pub(crate) fn bare_key(key: &keyboard::Key) -> Option<BareKey> {
    use keyboard::key::Named;

    match key.as_ref() {
        keyboard::Key::Named(Named::Tab) => Some(BareKey::Tab),
        keyboard::Key::Named(Named::ArrowDown) => Some(BareKey::ArrowDown),
        keyboard::Key::Named(Named::ArrowUp) => Some(BareKey::ArrowUp),
        keyboard::Key::Named(Named::Enter) => Some(BareKey::Enter),
        keyboard::Key::Named(Named::Escape) => Some(BareKey::Escape),
        keyboard::Key::Named(Named::F2) => Some(BareKey::F2),
        // Caps Lock types the capital without a modifier; it is still the bare key.
        keyboard::Key::Character(character) if character.eq_ignore_ascii_case("n") => {
            Some(BareKey::N)
        }
        keyboard::Key::Character(character) if character.eq_ignore_ascii_case("a") => {
            Some(BareKey::A)
        }
        keyboard::Key::Character(character) if character.eq_ignore_ascii_case("d") => {
            Some(BareKey::D)
        }
        keyboard::Key::Character(character) if character.eq_ignore_ascii_case("g") => {
            Some(BareKey::G)
        }
        keyboard::Key::Character(character) => match character {
            "1" => Some(BareKey::Digit(1)),
            "2" => Some(BareKey::Digit(2)),
            "3" => Some(BareKey::Digit(3)),
            "4" => Some(BareKey::Digit(4)),
            "5" => Some(BareKey::Digit(5)),
            "6" => Some(BareKey::Digit(6)),
            _ => None,
        },
        _ => None,
    }
}

/// Whether the key press is Ctrl and K, which opens the jump panel.
///
/// The letter is the one the key types on a Latin layout, so the shortcut sits on the same
/// key whatever script the layout types.
pub(crate) fn jump_shortcut(
    key: &keyboard::Key,
    physical_key: keyboard::key::Physical,
    modifiers: keyboard::Modifiers,
) -> bool {
    modifiers.command()
        && key
            .to_latin(physical_key)
            .is_some_and(|letter| letter.eq_ignore_ascii_case(&'k'))
}

/// The tab that Ctrl and a digit select: 1 to 4 in the toolbar's order.
///
/// The digit is read from the key's character. Only when the key types no digit (the digit
/// row of AZERTY types symbols) does the physical key in the digit's position count.
pub(crate) fn tab_shortcut(
    key: &keyboard::Key,
    physical_key: keyboard::key::Physical,
    modifiers: keyboard::Modifiers,
) -> Option<Tab> {
    if !modifiers.command() {
        return None;
    }
    match key.as_ref() {
        keyboard::Key::Character(character) if character.chars().all(|c| c.is_ascii_digit()) => {
            match character {
                "1" => Some(Tab::Projects),
                "2" => Some(Tab::Session),
                "3" => Some(Tab::Spec),
                "4" => Some(Tab::Env),
                _ => None,
            }
        }
        _ => match physical_key {
            keyboard::key::Physical::Code(keyboard::key::Code::Digit1) => Some(Tab::Projects),
            keyboard::key::Physical::Code(keyboard::key::Code::Digit2) => Some(Tab::Session),
            keyboard::key::Physical::Code(keyboard::key::Code::Digit3) => Some(Tab::Spec),
            keyboard::key::Physical::Code(keyboard::key::Code::Digit4) => Some(Tab::Env),
            _ => None,
        },
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

    use futures::channel::mpsc;
    use iced::Point;
    use iced::keyboard::key;
    use iced::keyboard::{self, Key, Modifiers};
    use iced::widget::Id;
    use iced_test::simulator;
    use partitur_core::testing::{
        MemoryGit, MemoryStore, RecordingBackend, RecordingReply, single_claude,
    };
    use partitur_core::{
        AgentUpdate, App, BackendEvent, GitRequestId, GlobalSettings, OrchestratorTool,
        PROPOSAL_RECORDED, ProposedChild, QuestionId, RepositoryId, SessionId, TranscriptRecord,
    };

    use partitur_core::Store as _;

    use super::*;
    use crate::test_support::{orchestrated, plain};

    fn state() -> State {
        let (backends, default) = single_claude();
        let memory = MemoryStore::new();
        let app = App::load(
            Box::new(RecordingBackend::default()),
            Box::new(memory.clone()),
            Box::new(memory),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
            Box::new(partitur_core::testing::FixedClock::default()),
        )
        .unwrap();
        boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0
    }

    /// Feeds every message through `update`, dropping the tasks: the simulator cannot run them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = Message>) {
        for message in messages {
            let _task = update(state, message);
        }
    }

    /// An orchestrated project whose child asked a question while its orchestrator was busy,
    /// loaded again from the same store, with every history failing to load when `corrupt`,
    /// and booted: the booted state and the orchestrator.
    fn booted_with_a_waiting_child(corrupt: bool) -> (State, SessionId, MemoryStore) {
        let mut orchestrated = orchestrated();
        let child = orchestrated.child.clone();
        let question = partitur_core::Question {
            text: "Merge or PR?".into(),
            options: ["Merge", "PR"]
                .into_iter()
                .map(|label| partitur_core::QuestionOption {
                    label: label.into(),
                    description: None,
                })
                .collect(),
        };
        crate::test_support::deliver_tool(
            &mut orchestrated.state,
            child.clone(),
            partitur_core::PartiturTool::AskUser(question),
        );
        crate::test_support::end_turn(&mut orchestrated.state, &child);
        assert_eq!(orchestrated.state.error, None);
        let store = orchestrated.store.clone();
        let orchestrator = orchestrated.orchestrator.clone();
        drop(orchestrated);
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(RecordingBackend::default()),
            Box::new(store.clone()),
            Box::new(store.clone()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
            Box::new(partitur_core::testing::FixedClock::default()),
        )
        .unwrap();
        store.set_corrupt_updates(corrupt);
        let state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
        store.set_corrupt_updates(false);
        (state, orchestrator, store)
    }

    #[test]
    fn boot_reports_a_waiting_child_to_its_orchestrator() {
        let (state, orchestrator, store) = booted_with_a_waiting_child(false);

        assert_eq!(state.error, None);
        assert!(
            store
                .load_records(store.project_of(&orchestrator).unwrap(), orchestrator)
                .unwrap()
                .iter()
                .any(|update| matches!(update, TranscriptRecord::ChildResult { .. }))
        );
    }

    #[test]
    fn a_report_that_fails_at_boot_shows_in_the_error_bar() {
        let (state, orchestrator, store) = booted_with_a_waiting_child(true);

        let error = state.error.unwrap();
        assert!(error.contains("simulated corruption"), "{error}");
        assert_eq!(state.app.tree().projects.len(), 1);
        assert!(
            !store
                .load_records(store.project_of(&orchestrator).unwrap(), orchestrator)
                .unwrap()
                .iter()
                .any(|update| matches!(update, TranscriptRecord::ChildResult { .. }))
        );
    }

    #[test]
    fn a_drawn_frame_exits_the_application() {
        let mut state = state();
        assert_eq!(update(&mut state, Message::FrameDrawn).units(), 1);
        assert_eq!(update(&mut state, Message::Tick).units(), 0);
    }

    #[test]
    fn a_tick_reads_the_time_again() {
        let mut state = state();
        let booted = state.now;
        assert!(booted > 0);
        state.now = 0;

        apply(&mut state, [Message::Tick]);

        assert!(state.now >= booted);
    }

    #[test]
    fn the_title_field_takes_only_enter_and_escape_on_the_projects_tab() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        fixture.state.tab = Tab::Projects;
        apply(
            &mut fixture.state,
            [Message::Projects(ProjectsMessage::StartRename(
                orchestrator.clone(),
            ))],
        );

        // Tab and the tree's keys wait until the field is closed.
        apply(
            &mut fixture.state,
            [
                Message::Key(BareKey::Tab),
                Message::Key(BareKey::ArrowDown),
                Message::Key(BareKey::A),
            ],
        );
        assert_eq!(fixture.state.tab, Tab::Projects);
        assert!(fixture.state.confirm.is_none());
        assert!(fixture.state.projects.renaming.is_some());

        // On another tab the keys do what they always do, and a change of tab closes the
        // field.
        fixture.state.tab = Tab::Session;
        apply(&mut fixture.state, [Message::Key(BareKey::Tab)]);
        assert_eq!(fixture.state.tab, Tab::Env);
        assert_eq!(fixture.state.projects.renaming, None);
    }

    #[test]
    fn the_toolbar_shows_four_tabs_and_projects_is_active_at_start() {
        let state = state();
        let mut ui = simulator(view(&state));

        for tab in ["Projects", "Session", "Spec", "Env"] {
            ui.find(tab).unwrap();
        }
        ui.find("No projects open").unwrap();
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn clicking_session_switches_the_active_tab() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Projects;
        let mut ui = simulator(view(&fixture.state));

        ui.click("Session").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::SelectTab(Tab::Session)]);
        apply(&mut fixture.state, messages);
        assert_eq!(fixture.state.tab, Tab::Session);
    }

    #[test]
    fn session_and_env_are_disabled_without_a_session() {
        let mut state = state();
        let mut ui = simulator(view(&state));

        ui.click("Session").unwrap();
        ui.click("Env").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        apply(
            &mut state,
            [
                Message::SelectTab(Tab::Session),
                Message::SelectTab(Tab::Env),
            ],
        );
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn the_tab_key_stays_on_projects_without_a_session() {
        let mut state = state();

        apply(&mut state, [Message::Key(BareKey::Tab)]);

        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn deleting_the_open_session_falls_back_to_projects() {
        let mut fixture = plain();
        apply(&mut fixture.state, [Message::SelectTab(Tab::Env)]);
        assert_eq!(fixture.state.tab, Tab::Env);

        let session = fixture.session.clone();
        fixture.state.app.delete(session, &[]).unwrap();
        apply(&mut fixture.state, [Message::Tick]);

        assert_eq!(fixture.state.app.current_session(), None);
        assert_eq!(fixture.state.tab, Tab::Projects);
    }

    /// Has the running turn of `session`, shown on the Session tab, leave something with
    /// every part of the state that keeps something per session: a widget reported as
    /// loading, a typed answer to a question, a picked design document and an image whose
    /// preview is asked for. Returns the question and the preview's file.
    fn remember_everything(state: &mut State, session: &SessionId) -> (QuestionId, PathBuf) {
        use partitur_core::{
            ModuleEvent, PartiturTool, Question, QuestionOption, SentAttachment,
            SuperpowersV7Event, ToolCallId, ToolKind, ToolStatus, WidgetSlot,
        };

        use crate::test_support::{deliver_tool, end_turn, feed};

        state.tab = Tab::Session;
        let report = |state: &mut State, record: TranscriptRecord| feed(state, session, record);
        // A widget the running turn reports as loading.
        report(
            state,
            TranscriptRecord::ToolCall {
                id: ToolCallId::new("w1"),
                title: "getJiraIssue".into(),
                kind: ToolKind::Other,
                status: ToolStatus::InProgress,
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                call_result: None,
                widget: Some(WidgetSlot::Loading {
                    server: "atlassian".into(),
                    uri: "ui://jira/issue".into(),
                }),
            },
        );
        // A question with an answer typed into its `Other` field.
        deliver_tool(
            state,
            session.clone(),
            PartiturTool::AskUser(Question {
                text: "Which one?".into(),
                options: vec![
                    QuestionOption {
                        label: "This".into(),
                        description: None,
                    },
                    QuestionOption {
                        label: "That".into(),
                        description: None,
                    },
                ],
            }),
        );
        assert_eq!(state.error, None);
        let question = state.app.current_question().cloned().unwrap();
        apply(
            state,
            [Message::Session(SessionMessage::Question(
                question.clone(),
                question::QuestionMessage::EditOther("half".into()),
            ))],
        );
        // A design document the Spec tab reads and shows.
        for tool in [
            SuperpowersV7Event::StartSession {
                topic: "Endpoint".into(),
            },
            SuperpowersV7Event::StartBrainstorming,
            SuperpowersV7Event::RequestDesignApproval {
                path: "docs/design.md".into(),
            },
        ] {
            deliver_tool(state, session.clone(), ModuleEvent::from(tool));
        }
        apply(state, [Message::Flow(FlowMessage::Select("design".into()))]);
        end_turn(state, session);
        // An image the next prompt was sent with, whose preview is asked for.
        report(state, TranscriptRecord::UserText(String::new()));
        report(
            state,
            TranscriptRecord::UserAttachments(vec![SentAttachment::Image {
                name: "s.png".into(),
                copy: PathBuf::from("1-s.png"),
                preview: Some(PathBuf::from("1-s.png.preview.png")),
            }]),
        );
        let preview = state
            .app
            .attachment_dir(session)
            .unwrap()
            .join("1-s.png.preview.png");
        (question, preview)
    }

    #[test]
    fn deleting_a_session_clears_its_question_draft_and_spec_documents() {
        let root = tempfile::tempdir().unwrap();
        let mut fixture = plain();
        fixture.store.set_attachment_root(root.path());
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        let (question, preview) = remember_everything(state, &session);

        assert!(
            state
                .session
                .question_drafts
                .contains_key(&(session.clone(), question.clone()))
        );
        assert!(state.flow.remembers(&session));
        assert!(state.session.previews.asked_for(&preview));
        assert!(state.widgets.remembers(&session));

        apply(
            state,
            [
                Message::Projects(ProjectsMessage::Delete(session.clone())),
                Message::Confirm(ConfirmMessage::Confirm),
            ],
        );
        assert_eq!(state.error, None);
        assert!(state.app.tree().projects[0].sessions.is_empty());

        assert!(state.session.question_drafts.is_empty());
        assert!(!state.flow.remembers(&session));
        assert!(!state.session.previews.asked_for(&preview));
        assert!(!state.widgets.remembers(&session));
        // A preview that finishes loading after its session went is not kept.
        apply(state, [Message::Preview(preview.clone(), None)]);
        assert!(!state.session.previews.asked_for(&preview));
    }

    #[test]
    fn the_toolbar_shows_the_wordmark() {
        let state = state();
        let mut ui = simulator(view(&state));

        ui.find("Partitur").unwrap();
    }

    fn framed(frame: WindowFrame) -> State {
        let mut state = state();
        state.chrome = chrome(frame, Platform::Other);
        state
    }

    #[test]
    fn the_window_buttons_minimize_maximize_and_close() {
        for (id, action) in [
            ("window-minimize", WindowAction::Minimize),
            ("window-maximize", WindowAction::ToggleMaximize),
            ("window-close", WindowAction::Close),
        ] {
            let state = framed(WindowFrame::Partitur);
            let mut ui = simulator(view(&state));
            ui.click(Id::new(id)).unwrap();
            assert_eq!(
                ui.into_messages().collect::<Vec<_>>(),
                [Message::Window(action)],
                "{id}"
            );
        }
    }

    #[test]
    fn pressing_the_wordmark_presses_the_bar_without_dragging() {
        let state = framed(WindowFrame::Partitur);
        let mut ui = simulator(view(&state));
        ui.click("Partitur").unwrap();
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            [Message::BarPressed]
        );
    }

    #[test]
    fn the_pressed_bar_reports_a_move_and_the_release() {
        let mut state = framed(WindowFrame::Partitur);
        apply(&mut state, [Message::BarPressed]);
        let mut ui = simulator(view(&state));
        let wordmark = ui.find("Partitur").unwrap().bounds().center();
        let moved = Point::new(wordmark.x + 5.0, wordmark.y);
        ui.point_at(moved);
        let _ = ui.simulate([Event::Mouse(iced::mouse::Event::CursorMoved {
            position: moved,
        })]);
        let _ = ui.simulate(simulator::click());
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            [Message::BarMoved, Message::BarPressed, Message::BarReleased]
        );
    }

    #[test]
    fn the_first_move_while_the_bar_is_pressed_drags_the_window() {
        let mut state = framed(WindowFrame::Partitur);
        assert_eq!(update(&mut state, Message::BarMoved).units(), 0);
        assert_eq!(update(&mut state, Message::BarPressed).units(), 0);
        assert!(state.bar_pressed);
        assert_eq!(update(&mut state, Message::BarMoved).units(), 1);
        assert!(!state.bar_pressed);
        assert_eq!(update(&mut state, Message::BarMoved).units(), 0);
    }

    #[test]
    fn a_press_released_without_a_move_does_not_drag() {
        let mut state = framed(WindowFrame::Partitur);
        apply(&mut state, [Message::BarPressed, Message::BarReleased]);
        assert!(!state.bar_pressed);
        assert_eq!(update(&mut state, Message::BarMoved).units(), 0);
    }

    #[test]
    fn a_double_click_on_the_wordmark_maximizes_without_dragging() {
        let mut state = framed(WindowFrame::Partitur);
        let mut ui = simulator(view(&state));
        ui.click("Partitur").unwrap();
        ui.click("Partitur").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.contains(&Message::Window(WindowAction::ToggleMaximize)),
            "{messages:?}"
        );
        assert!(
            !messages.iter().any(|message| matches!(
                message,
                Message::BarMoved | Message::Window(WindowAction::Drag)
            )),
            "{messages:?}"
        );

        // A double click whose release the bar never saw leaves it unpressed all the same.
        apply(
            &mut state,
            [
                Message::BarPressed,
                Message::Window(WindowAction::ToggleMaximize),
            ],
        );
        assert!(!state.bar_pressed);
    }

    #[test]
    fn pressing_a_tab_selects_it_without_dragging() {
        let mut fixture = plain();
        fixture.state.chrome = chrome(WindowFrame::Partitur, Platform::Other);
        let mut ui = simulator(view(&fixture.state));
        ui.click("Session").unwrap();
        ui.click("Session").unwrap(); // a double click
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(!messages.is_empty());
        assert!(
            messages
                .iter()
                .all(|m| *m == Message::SelectTab(Tab::Session)),
            "{messages:?}"
        );
    }

    #[test]
    fn the_window_buttons_work_while_the_jump_panel_or_a_sheet_is_open() {
        let mut fixture = plain();
        fixture.state.chrome = chrome(WindowFrame::Partitur, Platform::Other);
        fixture.state.jump = Some(JumpState::default());
        let mut ui = simulator(view(&fixture.state));
        ui.click(Id::new("window-close")).unwrap();
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            [Message::Window(WindowAction::Close)]
        );

        fixture.state.jump = None;
        let project = fixture.state.app.tree().projects[0].id.clone();
        fixture
            .state
            .app
            .prepare_session(project, partitur_core::Scope::Project)
            .unwrap();
        assert!(fixture.state.app.draft().is_some());
        let mut ui = simulator(view(&fixture.state));
        ui.click(Id::new("window-close")).unwrap();
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            [Message::Window(WindowAction::Close)]
        );
    }

    #[test]
    fn the_tabs_and_the_jump_button_do_nothing_while_the_jump_panel_is_open() {
        let mut fixture = plain();
        fixture.state.chrome = chrome(WindowFrame::Partitur, Platform::Other);
        fixture.state.tab = Tab::Projects;
        fixture.state.jump = Some(JumpState::default());
        let mut ui = simulator(view(&fixture.state));
        ui.click("Session").unwrap();
        ui.click("Jump to session").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, []);
    }

    #[test]
    fn the_system_frame_centers_the_tabs_in_the_window() {
        let state = framed(WindowFrame::System);
        let mut ui = simulator(view(&state));
        let first = ui.find("Projects").unwrap().bounds();
        let last = ui.find("Env").unwrap().bounds();
        let middle = f32::midpoint(first.x, last.x + last.width);
        // The simulated window is 1024 wide.
        assert!((middle - 512.0).abs() < 1.0, "{middle}");
    }

    #[test]
    fn the_system_frame_draws_no_window_buttons_and_does_not_drag() {
        let state = framed(WindowFrame::System);
        let mut ui = simulator(view(&state));
        assert!(ui.find(Id::new("window-close")).is_err());
        ui.click("Partitur").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
    }

    #[test]
    fn the_tab_key_cycles_through_the_tabs() {
        // The fixture keeps the project folder alive while its state is used.
        let fixture = plain();
        let mut state = fixture.state;
        state.tab = Tab::Projects;
        let tab = |modifiers| keyboard::Event::KeyPressed {
            key: Key::Named(key::Named::Tab),
            modified_key: Key::Named(key::Named::Tab),
            physical_key: key::Physical::Code(key::Code::Tab),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };

        assert_eq!(
            shortcut(tab(Modifiers::empty())),
            Some(Message::Key(BareKey::Tab))
        );
        assert_eq!(shortcut(tab(Modifiers::ALT)), None);
        assert_eq!(shortcut(tab(Modifiers::COMMAND)), None);

        let mut visited = Vec::new();
        for _ in 0..3 {
            apply(&mut state, [Message::Key(BareKey::Tab)]);
            visited.push(state.tab);
        }
        assert_eq!(visited, [Tab::Session, Tab::Env, Tab::Projects]);
    }

    #[test]
    fn the_bare_keys_are_reported_as_they_are() {
        let named = |named: key::Named, modifiers| keyboard::Event::KeyPressed {
            key: Key::Named(named),
            modified_key: Key::Named(named),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };
        let letter = |character: &str, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };
        let bare = |key| Some(Message::Key(key));

        assert_eq!(
            shortcut(named(key::Named::ArrowDown, Modifiers::empty())),
            bare(BareKey::ArrowDown)
        );
        assert_eq!(
            shortcut(named(key::Named::ArrowUp, Modifiers::empty())),
            bare(BareKey::ArrowUp)
        );
        assert_eq!(
            shortcut(named(key::Named::Enter, Modifiers::empty())),
            bare(BareKey::Enter)
        );
        assert_eq!(
            shortcut(named(key::Named::Escape, Modifiers::empty())),
            bare(BareKey::Escape)
        );
        assert_eq!(
            shortcut(named(key::Named::F2, Modifiers::empty())),
            bare(BareKey::F2)
        );
        assert_eq!(shortcut(letter("n", Modifiers::empty())), bare(BareKey::N));
        // Caps Lock types the capital without a modifier.
        assert_eq!(shortcut(letter("N", Modifiers::empty())), bare(BareKey::N));
        assert_eq!(shortcut(letter("a", Modifiers::empty())), bare(BareKey::A));
        assert_eq!(shortcut(letter("A", Modifiers::empty())), bare(BareKey::A));
        assert_eq!(shortcut(letter("a", Modifiers::COMMAND)), None);
        assert_eq!(shortcut(letter("d", Modifiers::empty())), bare(BareKey::D));
        assert_eq!(shortcut(letter("D", Modifiers::empty())), bare(BareKey::D));
        assert_eq!(shortcut(letter("d", Modifiers::COMMAND)), None);
        assert_eq!(shortcut(letter("D", Modifiers::SHIFT)), None);

        assert_eq!(
            shortcut(named(key::Named::ArrowDown, Modifiers::SHIFT)),
            None
        );
        assert_eq!(shortcut(named(key::Named::Enter, Modifiers::COMMAND)), None);
        assert_eq!(shortcut(letter("N", Modifiers::SHIFT)), None);
        assert_eq!(shortcut(letter("n", Modifiers::COMMAND)), None);
        assert_eq!(shortcut(letter("m", Modifiers::empty())), None);
        assert_eq!(
            shortcut(named(key::Named::ArrowLeft, Modifiers::empty())),
            None
        );
        assert_eq!(shortcut(named(key::Named::Escape, Modifiers::SHIFT)), None);
    }

    #[test]
    fn a_held_enter_counts_once_and_a_held_arrow_keeps_moving() {
        let held = |named: key::Named| keyboard::Event::KeyPressed {
            key: Key::Named(named),
            modified_key: Key::Named(named),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: true,
        };

        assert_eq!(shortcut(held(key::Named::Enter)), None);
        assert_eq!(
            shortcut(held(key::Named::ArrowDown)),
            Some(Message::Key(BareKey::ArrowDown))
        );
        assert_eq!(
            shortcut(held(key::Named::Tab)),
            Some(Message::Key(BareKey::Tab))
        );
    }

    #[test]
    fn g_is_ignored_while_a_text_input_has_it() {
        let g = Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Character("g".into()),
            modified_key: Key::Character("g".into()),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        });
        let window = window::Id::unique();

        assert_eq!(
            key_message(g.clone(), event::Status::Captured, window),
            None
        );
        assert_eq!(
            key_message(g, event::Status::Ignored, window),
            Some(Message::Key(BareKey::G))
        );
    }

    #[test]
    fn ctrl_and_k_ask_for_the_jump_panel() {
        let press = |character: &str, code, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };
        let open = Some(Message::Jump(JumpMessage::Open));

        assert_eq!(
            shortcut(press("k", key::Code::KeyK, Modifiers::COMMAND)),
            open
        );
        assert_eq!(
            shortcut(press("K", key::Code::KeyK, Modifiers::COMMAND)),
            open
        );
        // A layout of another script types its own letter on the key.
        assert_eq!(
            shortcut(press("л", key::Code::KeyK, Modifiers::COMMAND)),
            open
        );
        assert_eq!(
            shortcut(press("k", key::Code::KeyK, Modifiers::empty())),
            None
        );
        assert_eq!(shortcut(press("k", key::Code::KeyK, Modifiers::ALT)), None);
        assert_eq!(
            shortcut(press("j", key::Code::KeyJ, Modifiers::COMMAND)),
            None
        );
    }

    #[test]
    fn a_key_a_widget_took_is_no_shortcut_except_escape() {
        let named = |named: key::Named| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(named),
                modified_key: Key::Named(named),
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: keyboard::Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let window = window::Id::unique();
        let (ignored, captured) = (event::Status::Ignored, event::Status::Captured);

        assert_eq!(
            key_message(named(key::Named::Enter), ignored, window),
            Some(Message::Key(BareKey::Enter))
        );
        assert_eq!(
            key_message(named(key::Named::Enter), captured, window),
            None
        );
        // A focused text field takes Escape for itself, and the jump panel still closes.
        assert_eq!(
            key_message(named(key::Named::Escape), captured, window),
            Some(Message::Key(BareKey::Escape))
        );
        assert_eq!(
            key_message(named(key::Named::Escape), ignored, window),
            Some(Message::Key(BareKey::Escape))
        );
        assert_eq!(
            key_message(named(key::Named::ArrowLeft), ignored, window),
            None
        );
        let moved = Event::Mouse(iced::mouse::Event::CursorLeft);
        assert_eq!(key_message(moved, ignored, window), None);
    }

    #[test]
    fn escape_without_the_jump_panel_changes_nothing() {
        let mut state = state();

        apply(&mut state, [Message::Key(BareKey::Escape)]);

        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.jump, None);
        assert_eq!(state.error, None);
    }

    #[test]
    fn the_desktop_preference_picks_the_theme() {
        let mut state = state();
        assert_eq!(state.mode, Mode::None);

        apply(&mut state, [Message::SystemTheme(Mode::Light)]);

        assert_eq!(state.mode, Mode::Light);
    }

    #[test]
    fn a_git_answer_nothing_waits_for_shows_no_error() {
        let mut state = state();
        let answer = GitEvent::Ran {
            request: GitRequestId(9),
            done: 0,
            result: Ok(()),
        };

        apply(&mut state, [Message::Git(answer)]);

        assert_eq!(state.error, None);
    }

    #[test]
    fn a_failed_backend_event_shows_a_dismissible_error_bar() {
        let mut state = state();
        let unknown = BackendEvent::Update {
            session: SessionId::new(),
            update: AgentUpdate::Notice("hello".into()),
        };

        apply(&mut state, [Message::Backend(unknown)]);
        let error = state.error.clone().unwrap();
        let mut ui = simulator(view(&state));
        ui.find(error.as_str()).unwrap();
        ui.click("dismiss").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::DismissError]);
        apply(&mut state, messages);
        let mut ui = simulator(view(&state));
        assert!(ui.find(error.as_str()).is_err());
    }

    #[test]
    fn a_tool_call_message_reaches_the_core() {
        let mut fixture = orchestrated();
        let reply = RecordingReply::new();
        let call = ToolCall {
            session: fixture.orchestrator.clone(),
            tool: OrchestratorTool::ProposeChildren(vec![ProposedChild {
                repository: RepositoryId::try_from("web").unwrap(),
                prompt: "Add the page".into(),
                title: "Web page".into(),
                title_source: partitur_core::TitleSource::Given,
                base: None,
            }])
            .into(),
            reply: reply.clone(),
        };

        apply(&mut fixture.state, [Message::Tool(call)]);

        assert_eq!(reply.answers(), [Ok(PROPOSAL_RECORDED.to_owned())]);
        assert_eq!(fixture.state.error, None);
        assert!(
            fixture
                .state
                .session
                .proposals
                .keys()
                .any(|(session, _)| *session == fixture.orchestrator)
        );
    }

    #[test]
    fn an_error_message_is_shown_in_the_error_bar() {
        let mut state = state();

        apply(&mut state, [Message::Error("the folder vanished".into())]);

        let mut ui = simulator(view(&state));
        ui.find("the folder vanished").unwrap();
    }

    #[test]
    fn ctrl_and_a_digit_select_the_tabs() {
        let press = |character: &str, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };

        assert_eq!(
            shortcut(press("1", Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Projects))
        );
        assert_eq!(
            shortcut(press("2", Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Session))
        );
        assert_eq!(
            shortcut(press("3", Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Spec))
        );
        assert_eq!(
            shortcut(press("4", Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Env))
        );
        // Without Ctrl a digit is no tab: it picks an option of an open question.
        assert_eq!(
            shortcut(press("2", Modifiers::empty())),
            Some(Message::Key(BareKey::Digit(2)))
        );
        assert_eq!(shortcut(press("5", Modifiers::COMMAND)), None);
    }

    #[test]
    fn the_physical_digit_keys_select_the_tabs_on_other_layouts() {
        let press = |code, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character("&".into()),
            modified_key: Key::Character("&".into()),
            physical_key: key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };

        assert_eq!(
            shortcut(press(key::Code::Digit1, Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Projects))
        );
        assert_eq!(
            shortcut(press(key::Code::Digit2, Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Session))
        );
        assert_eq!(
            shortcut(press(key::Code::Digit3, Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Spec))
        );
        assert_eq!(
            shortcut(press(key::Code::Digit4, Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Env))
        );
        assert_eq!(shortcut(press(key::Code::Digit5, Modifiers::COMMAND)), None);
        assert_eq!(shortcut(press(key::Code::Digit1, Modifiers::empty())), None);
    }

    #[test]
    fn a_digit_typed_by_another_key_wins_over_the_physical_position() {
        let event = keyboard::Event::KeyPressed {
            key: Key::Character("7".into()),
            modified_key: Key::Character("7".into()),
            physical_key: key::Physical::Code(key::Code::Digit1),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::COMMAND,
            text: None,
            repeat: false,
        };

        assert_eq!(shortcut(event), None);
    }

    #[test]
    fn every_window_action_asks_for_a_window_task() {
        for action in [
            WindowAction::Drag,
            WindowAction::ToggleMaximize,
            WindowAction::Minimize,
            WindowAction::Close,
            WindowAction::Menu,
            WindowAction::Resize(window::Direction::SouthEast),
        ] {
            let mut state = state();
            assert_eq!(
                update(&mut state, Message::Window(action)).units(),
                1,
                "{action:?}"
            );
        }
    }

    #[test]
    fn pressing_the_bottom_right_corner_resizes_the_window() {
        let mut state = state();
        state.chrome = chrome(WindowFrame::Partitur, Platform::Other);
        let mut ui = simulator(view(&state));
        ui.click(Id::new("resize-south-east")).unwrap();
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            [Message::Window(WindowAction::Resize(
                window::Direction::SouthEast
            ))]
        );
    }

    #[test]
    fn a_maximized_window_has_no_resize_edges() {
        let mut state = state();
        state.chrome = chrome(WindowFrame::Partitur, Platform::Other);
        apply(&mut state, [Message::Maximized(true)]);
        assert!(
            simulator(view(&state))
                .find(Id::new("resize-north"))
                .is_err()
        );
    }

    #[test]
    fn the_system_frame_has_no_resize_edges() {
        let mut state = state();
        state.chrome = chrome(WindowFrame::System, Platform::Other);
        assert!(
            simulator(view(&state))
                .find(Id::new("resize-north"))
                .is_err()
        );
    }

    #[test]
    fn a_resize_asks_whether_the_window_is_maximized() {
        let mut state = state();
        assert_eq!(update(&mut state, Message::Resized).units(), 1);
        apply(&mut state, [Message::Maximized(true)]);
        assert!(state.maximized);
        apply(&mut state, [Message::Maximized(false)]);
        assert!(!state.maximized);
    }
}
