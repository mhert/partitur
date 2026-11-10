//! The new-session sheet: the settings of a session picked before it exists.
//!
//! A `new session` row of the Projects tab opens the sheet for its project and scope, and N
//! opens it for a project session in the project of the highlighted row. The sheet is open
//! exactly while the core prepares a session, so the core holds the only copy of what it
//! shows. The newest session of the project seeds the backend, model, effort and
//! auto-allow, and opening the sheet warms up an agent on that backend in the background.
//! Until the agent answers, the model and effort pickers show the wanted value (or
//! `default`) with a `loading…` hint; then they list what the agent offers, and a setting
//! the agent does not offer says so. Picking another backend warms up an agent there. When
//! the agent cannot start, the sheet says why and the session cannot be created.
//!
//! `Create session` (or Enter) creates the session with the picked settings, selects it,
//! shows the Session tab and puts the focus in its prompt box; when that fails the sheet
//! stays open and says why. `Cancel`, Escape or a click on the backdrop closes the sheet and
//! leaves nothing behind. While the sheet is open no other key reaches the window. The
//! `Branch & worktree` box only shows the defaults; nothing in it can be changed yet.

use iced::widget::{
    Column, button, checkbox, column, container, operation, pick_list, row, space, text,
};
use iced::{Alignment, Border, Element, Task, Theme};
use partitur_core::{
    BackendId, ChoiceView, ConfigValueId, DraftChoice, DraftView, ProjectId, Scope, Warmup,
};

use crate::app::{BareKey, Message, State, report};
use crate::confirm_sheet::{self, GAP, PADDING, TITLE_SIZE, WIDTH};
use crate::session_view::{self, BackendChoice, Choice, with_chevron_inset};
use crate::theme::Tokens;
use crate::{badge, projects_view, style, theme};

/// The ids of the pickers, the checkbox and the buttons.
const MODEL: &str = "new-session-model";
const EFFORT: &str = "new-session-effort";
const BACKEND: &str = "new-session-backend";
const AUTO_ALLOW: &str = "new-session-auto-allow";
const CREATE: &str = "new-session-create";
const CANCEL: &str = "new-session-cancel";

/// The space between the rows of settings.
const SETTINGS_GAP: f32 = 8.0;

/// The width of the column that names the settings.
const LABEL_WIDTH: f32 = 120.0;

/// The width of a picker.
const PICKER_WIDTH: f32 = 240.0;

/// The room a picker keeps around its value: above and below, then left and right.
const PICKER_PADDING: [f32; 2] = [6.0, 10.0];

/// The corner radius of a picker.
const PICKER_RADIUS: f32 = 6.0;

/// The side of the auto-allow checkbox.
const CHECKBOX_SIZE: f32 = 14.0;

/// The space between the auto-allow checkbox and its label.
const CHECKBOX_SPACING: f32 = 8.0;

/// The width of the column that names the rows of the `Branch & worktree` box.
const BOX_LABEL_WIDTH: f32 = 108.0;

/// An interaction with the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewSessionMessage {
    /// The user picked a model.
    Model(ConfigValueId),
    /// The user picked a reasoning effort.
    Effort(ConfigValueId),
    /// The user picked a backend: an agent warms up there.
    Backend(BackendId),
    /// The user switched auto-allow on or off.
    AutoAllow(bool),
    /// The user asked to create the session.
    Create,
    /// The user cancelled: the sheet closes and nothing is left behind.
    Cancel,
}

/// Opens the sheet for a new session of `scope` in `project`. When the core refuses (the
/// project or the repository is gone), the error bar says why and the sheet stays closed.
pub(crate) fn open(state: &mut State, project: ProjectId, scope: Scope) -> Task<Message> {
    if report(&mut state.error, state.app.prepare_session(project, scope)).is_some() {
        state.error = None;
    }
    Task::none()
}

/// Applies `message` to the open sheet, as the [module](self) describes, and returns the
/// task that focuses the new session's prompt box once it is created. A setting the core
/// refuses shows its error in the sheet. Nothing happens while the sheet is closed.
pub fn update(state: &mut State, message: NewSessionMessage) -> Task<Message> {
    if state.app.draft().is_none() {
        return Task::none();
    }
    match message {
        NewSessionMessage::Model(value) => {
            report(&mut state.error, state.app.draft_model(value));
        }
        NewSessionMessage::Effort(value) => {
            report(&mut state.error, state.app.draft_effort(value));
        }
        NewSessionMessage::Backend(backend) => {
            report(&mut state.error, state.app.draft_backend(backend));
        }
        NewSessionMessage::AutoAllow(enabled) => {
            report(&mut state.error, state.app.draft_auto_allow(enabled));
        }
        NewSessionMessage::Create => {
            if report(&mut state.error, state.app.create_from_draft()).is_some() {
                state.error = None;
                projects_view::show_current_session(state);
                return operation::focus(session_view::PROMPT);
            }
        }
        NewSessionMessage::Cancel => {
            // The sheet closes either way, so a close that cannot be queued shows in the
            // error bar.
            state.error = None;
            report(&mut state.error, state.app.discard_draft());
        }
    }
    Task::none()
}

/// What the bare `key` does while the sheet is open: Enter creates the session unless its
/// agent could not start, Escape cancels, and every other key does nothing, since the
/// window under the sheet takes no keys.
pub(crate) fn press(state: &mut State, key: BareKey) -> Task<Message> {
    match key {
        BareKey::Enter => {
            let failed = state
                .app
                .draft()
                .is_some_and(|draft| matches!(draft.warmup, Warmup::Failed(_)));
            if failed {
                Task::none()
            } else {
                update(state, NewSessionMessage::Create)
            }
        }
        BareKey::Escape => update(state, NewSessionMessage::Cancel),
        BareKey::Tab
        | BareKey::ArrowDown
        | BareKey::ArrowUp
        | BareKey::N
        | BareKey::A
        | BareKey::D => Task::none(),
    }
}

/// `page` with the sheet for `draft` over it, and in the sheet why its agent could not
/// start or, else, the `error` of a failed action. The backdrop takes every click meant for
/// the page; a click on it cancels.
pub fn view<'a>(
    page: Element<'a, Message>,
    draft: &DraftView,
    error: Option<&str>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let failure = match &draft.warmup {
        Warmup::Failed(reason) => Some(reason.as_str()),
        Warmup::Loading | Warmup::Ready => error,
    };
    let loading = draft.warmup == Warmup::Loading;
    let mut content = column![
        column![
            text(title(draft)).size(TITLE_SIZE).font(theme::UI_SEMIBOLD),
            text(subline(draft)).size(theme::SMALL).style(style::muted),
        ]
        .spacing(4),
        column![
            setting(
                "Model",
                picker(&draft.model, loading, MODEL, NewSessionMessage::Model),
                Alignment::Center,
            ),
            setting(
                "Effort",
                picker(&draft.effort, loading, EFFORT, NewSessionMessage::Effort),
                Alignment::Center,
            ),
            setting("Backend", backend_picker(draft), Alignment::Center),
            setting(
                "Permissions",
                permissions(draft.auto_allow),
                Alignment::Start
            ),
        ]
        .spacing(SETTINGS_GAP),
        branch_and_worktree(),
    ]
    .spacing(GAP);
    if let Some(failure) = failure {
        content = content.push(
            text(failure.to_owned())
                .size(theme::SMALL)
                .color(tokens.failed.fg),
        );
    }
    content = content.push(actions(!matches!(draft.warmup, Warmup::Failed(_))));
    let card = container(content)
        .width(WIDTH)
        .padding(PADDING)
        .style(confirm_sheet::sheet_style)
        .into();
    confirm_sheet::modal(page, card, Message::NewSession(NewSessionMessage::Cancel))
}

/// `New session in <project>`, and for a repository session `New session in <project> ·
/// <repository>`.
fn title(draft: &DraftView) -> String {
    match &draft.repository {
        Some(repository) => format!("New session in {} · {repository}", draft.project),
        None => format!("New session in {}", draft.project),
    }
}

/// What the session will be: for an orchestrator how many repositories it spans and that
/// its children inherit the settings, for a repository session the repository's name, and
/// else how many repositories the project has.
fn subline(draft: &DraftView) -> String {
    if draft.orchestrator {
        return format!(
            "{} repos · creates an orchestrator session; children inherit these settings",
            draft.repositories
        );
    }
    if let Some(repository) = &draft.repository {
        return repository.clone();
    }
    match draft.repositories {
        1 => "1 repo".to_owned(),
        count => format!("{count} repos"),
    }
}

/// One row of settings: the muted `label` in its column, then `control`, both aligned on
/// `align`: a picker's centre line, or the first line of a control with more.
fn setting<'a>(
    label: &'a str,
    control: Element<'a, Message>,
    align: Alignment,
) -> Element<'a, Message> {
    row![text(label).width(LABEL_WIDTH).style(style::muted), control]
        .align_y(align)
        .into()
}

/// The picker of `choice`, found by `id`, that sends `on_select` with the picked value.
///
/// iced's pick list has no disabled state, so a picker that waits for the agent or that the
/// agent does not offer is a frame with text: the wanted value (or `default`) and, while
/// the agent is `loading`, a hint; or the note that the backend does not offer it.
fn picker<'a>(
    choice: &DraftChoice,
    loading: bool,
    id: &'static str,
    on_select: fn(ConfigValueId) -> NewSessionMessage,
) -> Element<'a, Message> {
    let control: Element<'a, Message> = match choice {
        DraftChoice::Waiting(wanted) => {
            let value = wanted
                .as_ref()
                .map_or_else(|| "default".to_owned(), ToString::to_string);
            let mut line = row![text(value).style(style::soft), space::horizontal()]
                .align_y(Alignment::Center);
            if loading {
                line = line.push(text("loading…").size(theme::SMALL).style(style::dim));
            }
            frame(line.into())
        }
        DraftChoice::Offered(choice) => offered(choice, on_select),
        DraftChoice::NotOffered => frame(
            text("not offered by this backend")
                .size(theme::SMALL)
                .style(style::dim)
                .into(),
        ),
    };
    container(control).id(id).into()
}

/// A framed pick list over the values `choice` offers, with its current value selected.
fn offered<'a>(
    choice: &ChoiceView,
    on_select: fn(ConfigValueId) -> NewSessionMessage,
) -> Element<'a, Message> {
    let options: Vec<Choice> = choice
        .choices
        .iter()
        .map(|option| Choice {
            id: option.id.clone(),
            name: option.name.clone(),
        })
        .collect();
    let current = Choice {
        id: choice.current.clone(),
        name: choice.current_name.clone(),
    };
    let picker = pick_list(options, Some(current), move |picked: Choice| {
        Message::NewSession(on_select(picked.id))
    })
    .width(PICKER_WIDTH)
    .padding(PICKER_PADDING)
    .handle(pick_list::Handle::None)
    .style(framed_picker)
    .menu_style(style::menu);
    with_chevron_inset(picker, PICKER_PADDING[1])
}

/// The framed pick list over the configured backends, with the draft's own selected.
fn backend_picker<'a>(draft: &DraftView) -> Element<'a, Message> {
    let options: Vec<BackendChoice> = draft.backends.iter().map(BackendChoice::from).collect();
    let current = options
        .iter()
        .find(|option| option.id == draft.backend)
        .cloned();
    let picker = pick_list(options, current, |picked: BackendChoice| {
        Message::NewSession(NewSessionMessage::Backend(picked.id))
    })
    .width(PICKER_WIDTH)
    .padding(PICKER_PADDING)
    .handle(pick_list::Handle::None)
    .style(framed_picker)
    .menu_style(style::menu);
    container(with_chevron_inset(picker, PICKER_PADDING[1]))
        .id(BACKEND)
        .into()
}

/// `content` in the frame of a picker, for a picker that cannot be opened.
fn frame(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .width(PICKER_WIDTH)
        .padding(PICKER_PADDING)
        .style(|theme: &Theme| {
            let tokens = theme::tokens(theme);
            container::Style {
                background: Some(tokens.bg.into()),
                border: picker_border(tokens.border),
                ..container::Style::default()
            }
        })
        .into()
}

/// A picker with a frame: the window colour inside a one-pixel outline, which takes the
/// input border colour under the pointer and while the picker is open.
fn framed_picker(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let tokens = theme::tokens(theme);
    let outline = match status {
        pick_list::Status::Active => tokens.border,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => tokens.border_input,
    };
    pick_list::Style {
        text_color: tokens.text,
        placeholder_color: tokens.muted,
        handle_color: tokens.muted,
        background: tokens.bg.into(),
        border: picker_border(outline),
    }
}

/// The rounded one-pixel outline of a picker in `color`.
fn picker_border(color: iced::Color) -> Border {
    Border::default()
        .rounded(PICKER_RADIUS)
        .width(1)
        .color(color)
}

/// The auto-allow checkbox, found by its id, and under its label what it means.
fn permissions<'a>(auto_allow: bool) -> Element<'a, Message> {
    let toggle = checkbox(auto_allow)
        .label("Auto-allow tool calls")
        .on_toggle(|enabled| Message::NewSession(NewSessionMessage::AutoAllow(enabled)))
        .size(CHECKBOX_SIZE)
        .spacing(CHECKBOX_SPACING);
    column![
        container(toggle).id(AUTO_ALLOW),
        container(
            text("Every tool call runs without asking.")
                .size(theme::SMALL)
                .style(style::muted)
        )
        .padding(iced::Padding::default().left(CHECKBOX_SIZE + CHECKBOX_SPACING)),
    ]
    .spacing(4)
    .into()
}

/// The box with the branch and worktree a session will get, which cannot be changed yet.
fn branch_and_worktree<'a>() -> Element<'a, Message> {
    let small = |label: &'a str| text(label).size(theme::SMALL);
    let label = |label: &'a str| small(label).width(BOX_LABEL_WIDTH).style(style::dim);
    let value = |label: &'a str| small(label).font(theme::MONO).style(style::muted);
    let lines: Column<'a, Message> = column![
        row![
            small("Branch & worktree")
                .font(theme::UI_MEDIUM)
                .style(style::muted),
            small("defaults · editable in a later version").style(style::dim),
        ]
        .spacing(8),
        row![label("base branch"), value("main")],
        row![
            label("worktree"),
            row![
                value("wt/<title>"),
                small("per repo, created on first prompt").style(style::dim),
            ]
            .spacing(8),
        ],
    ]
    .spacing(6);
    confirm_sheet::meaning_box(lines.into())
}

/// The buttons, found by their ids, and on the right the hint where the title comes from.
/// Create can be pressed only when it `can_create`.
fn actions<'a>(can_create: bool) -> Element<'a, Message> {
    let create = button(
        row![
            text("Create session").font(theme::UI_SEMIBOLD),
            badge::key_cap_on_primary("Enter")
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 14])
    .style(style::primary_button)
    .on_press_maybe(can_create.then_some(Message::NewSession(NewSessionMessage::Create)));
    let cancel = button(
        row![text("Cancel"), badge::key_cap("Esc")]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([6, 14])
    .style(style::outlined_button)
    .on_press(Message::NewSession(NewSessionMessage::Cancel));
    row![
        container(create).id(CREATE),
        container(cancel).id(CANCEL),
        space::horizontal(),
        text("title comes from your first prompt")
            .size(theme::SMALL)
            .style(style::dim),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;

    use futures::channel::mpsc;
    use iced::Point;
    use iced::widget::Id;
    use iced_test::simulator;
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, BackendId, BackendInfo, BackendSessionId, ConfigCategory, ConfigChoice,
        ConfigId, ConfigOption, ConfigValueId, ProjectId, RepositoryId, RequestId, Scope,
        SessionId,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{BareKey, Message, State, Tab, boot, update as app_update, view as app_view};
    use crate::projects_view::ProjectsMessage;
    use crate::test_support::{orchestrated, shown, two_backends};

    /// A state over a recording backend and an in-memory store with one project open, whose
    /// only repository is `api`.
    struct Fixture {
        state: State,
        backend: RecordingBackend,
        store: MemoryStore,
        project: ProjectId,
        name: String,
        _dir: TempDir,
    }

    fn fixture_with(backends: (Vec<BackendInfo>, BackendId)) -> Fixture {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        let backend = RecordingBackend::new();
        let store = MemoryStore::new();
        let (backends, default) = backends;
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(store.clone()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let name = state.app.tree().projects[0].name.clone();
        Fixture {
            state,
            backend,
            store,
            project,
            name,
            _dir: dir,
        }
    }

    fn fixture() -> Fixture {
        fixture_with(single_claude())
    }

    fn send(state: &mut State, message: Message) {
        let _task = app_update(state, message);
    }

    fn sheet(message: NewSessionMessage) -> Message {
        Message::NewSession(message)
    }

    fn open_project_sheet(fixture: &mut Fixture) {
        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::NewSession(
                fixture.project.clone(),
                Scope::Project,
            )),
        );
    }

    /// The request and the id of the latest warm session the backend was asked to start.
    fn warm_up(backend: &RecordingBackend) -> (RequestId, SessionId) {
        backend
            .calls()
            .iter()
            .filter_map(Call::as_new_session)
            .map(|(request, session, _)| (request, session.clone()))
            .next_back()
            .unwrap()
    }

    fn warm_session(backend: &RecordingBackend) -> SessionId {
        warm_up(backend).1
    }

    fn new_session_calls(backend: &RecordingBackend) -> usize {
        backend
            .calls()
            .iter()
            .filter(|call| call.as_new_session().is_some())
            .count()
    }

    /// An option of `category` with the values `ids`, the first one current.
    fn option(category: ConfigCategory, ids: &[&str]) -> ConfigOption {
        ConfigOption {
            id: ConfigId::new(format!("{category:?}").to_lowercase()),
            name: format!("{category:?}"),
            category,
            current: ConfigValueId::new(ids[0]),
            choices: ids
                .iter()
                .map(|id| ConfigChoice {
                    id: ConfigValueId::new(*id),
                    name: (*id).to_owned(),
                })
                .collect(),
        }
    }

    /// Answers the latest warm-up with `config`.
    fn answer(state: &mut State, backend: &RecordingBackend, config: Vec<ConfigOption>) {
        let (request, session) = warm_up(backend);
        let _task = app_update(
            state,
            Message::Backend(BackendEvent::SessionCreated {
                request,
                session,
                backend_session: BackendSessionId::new("b-warm"),
                config,
            }),
        );
    }

    fn sessions_in_tree(state: &State) -> usize {
        state.app.tree().projects[0].sessions.len()
    }

    #[test]
    fn clicking_new_session_opens_the_sheet_and_creates_nothing() {
        let mut fixture = fixture();
        let messages: Vec<Message> = {
            let mut ui = simulator(app_view(&fixture.state));
            ui.click(Id::from(format!("new-session-{}", fixture.project)))
                .unwrap();
            ui.into_messages().collect()
        };
        for message in messages {
            send(&mut fixture.state, message);
        }

        let mut ui = simulator(app_view(&fixture.state));
        ui.find(format!("New session in {}", fixture.name).as_str())
            .unwrap();
        ui.find("1 repo").unwrap();
        ui.find("loading…").unwrap();
        assert_eq!(sessions_in_tree(&fixture.state), 0);
        assert!(matches!(
            fixture.backend.calls().last(),
            Some(Call::NewSession { .. })
        ));
        assert_eq!(fixture.state.tab, Tab::Projects);
    }

    #[test]
    fn n_opens_the_sheet_for_the_cursors_project() {
        let mut fixture = fixture();

        send(&mut fixture.state, Message::Key(BareKey::N));

        assert!(fixture.state.app.draft().is_some());
        assert_eq!(sessions_in_tree(&fixture.state), 0);
    }

    #[test]
    fn a_repository_sheet_names_the_repository() {
        let mut fixture = fixture();
        let api = RepositoryId::try_from("api").unwrap();

        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::NewSession(
                fixture.project.clone(),
                Scope::Repository(api),
            )),
        );

        let mut ui = simulator(app_view(&fixture.state));
        ui.find(format!("New session in {} · api", fixture.name).as_str())
            .unwrap();
        ui.find("api").unwrap();
    }

    #[test]
    fn an_orchestrator_sheet_says_children_inherit() {
        let mut fixture = orchestrated();
        let project = fixture.state.app.tree().projects[0].id.clone();
        fixture.state.tab = Tab::Projects;

        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::NewSession(project, Scope::Project)),
        );

        let mut ui = simulator(app_view(&fixture.state));
        ui.find("2 repos · creates an orchestrator session; children inherit these settings")
            .unwrap();
    }

    #[test]
    fn the_agents_answer_fills_the_pickers() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);

        answer(
            &mut fixture.state,
            &fixture.backend,
            vec![option(ConfigCategory::Model, &["Fast", "opus"])],
        );

        let draft = fixture.state.app.draft().unwrap();
        assert!(matches!(
            &draft.model,
            DraftChoice::Offered(choice) if choice.current_name == "Fast"
        ));
        let mut ui = simulator(app_view(&fixture.state));
        assert!(ui.find("loading…").is_err());
        ui.find("not offered by this backend").unwrap();

        // A pick list keeps its text from selectors, so the menu is opened and its second
        // entry, one picker height below the first, is picked.
        let picker = ui.find(Id::new("new-session-model")).unwrap().bounds();
        ui.point_at(picker.center());
        let _ = ui.simulate(iced_test::simulator::click());
        let second = Point::new(picker.x + 12.0, picker.y + picker.height * 2.5);
        ui.point_at(second);
        let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: second });
        let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [sheet(NewSessionMessage::Model(ConfigValueId::new("opus")))]
        );
    }

    #[test]
    fn the_sheet_shows_the_placeholder_texts() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);

        let mut ui = simulator(app_view(&fixture.state));
        for label in [
            "Model",
            "Effort",
            "Backend",
            "Permissions",
            "Auto-allow tool calls",
            "Every tool call runs without asking.",
            "Branch & worktree",
            "defaults · editable in a later version",
            "base branch",
            "main",
            "worktree",
            "wt/<title>",
            "per repo, created on first prompt",
            "Create session",
            "Enter",
            "Cancel",
            "Esc",
            "title comes from your first prompt",
            "default",
        ] {
            assert!(shown(&mut ui, label) >= 1, "{label} is not shown");
        }
    }

    #[test]
    fn picking_values_reaches_the_core() {
        let mut fixture = fixture_with(two_backends());
        open_project_sheet(&mut fixture);
        answer(
            &mut fixture.state,
            &fixture.backend,
            vec![
                option(ConfigCategory::Model, &["sonnet", "opus"]),
                option(ConfigCategory::Effort, &["low", "high"]),
            ],
        );

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Model(ConfigValueId::new("opus"))),
        );
        assert!(matches!(
            fixture.backend.calls().last(),
            Some(Call::SetConfig { value, .. }) if value == &ConfigValueId::new("opus")
        ));

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Effort(ConfigValueId::new("high"))),
        );
        let draft = fixture.state.app.draft().unwrap();
        assert!(matches!(
            draft.effort,
            DraftChoice::Offered(choice) if choice.current == ConfigValueId::new("high")
        ));

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::AutoAllow(true)),
        );
        assert!(fixture.state.app.draft().unwrap().auto_allow);

        let warm = warm_session(&fixture.backend);
        let work = BackendId::try_from("claude-work").unwrap();
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Backend(work.clone())),
        );
        let calls = fixture.backend.calls();
        let n = calls.len();
        assert_eq!(calls[n - 2], Call::CloseSession { session: warm });
        assert!(matches!(
            &calls[n - 1],
            Call::NewSession { backend, .. } if backend == &work
        ));
        assert_eq!(fixture.state.app.draft().unwrap().backend, work);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_failed_warm_up_shows_the_error_and_disables_create() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);
        let (request, session) = warm_up(&fixture.backend);
        send(
            &mut fixture.state,
            Message::Backend(BackendEvent::RequestFailed {
                request,
                session,
                error: "no login".into(),
            }),
        );

        {
            let mut ui = simulator(app_view(&fixture.state));
            ui.find("no login").unwrap();
            ui.click(Id::new("new-session-create")).unwrap();
            assert_eq!(ui.into_messages().count(), 0);
        }

        send(&mut fixture.state, Message::Key(BareKey::Enter));
        assert!(fixture.state.app.draft().is_some());
        assert_eq!(sessions_in_tree(&fixture.state), 0);
    }

    #[test]
    fn enter_creates_selects_and_focuses_the_composer() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);
        let warm = warm_session(&fixture.backend);
        answer(&mut fixture.state, &fixture.backend, Vec::new());

        let task = app_update(&mut fixture.state, Message::Key(BareKey::Enter));

        assert!(task.units() > 0);
        assert_eq!(fixture.state.tab, Tab::Session);
        assert_eq!(fixture.state.app.current_session(), Some(warm.clone()));
        assert_eq!(fixture.state.projects.cursor, Some(warm));
        assert_eq!(fixture.state.app.draft(), None);
        assert_eq!(fixture.state.error, None);
        assert_eq!(sessions_in_tree(&fixture.state), 1);
    }

    #[test]
    fn escape_cancels_and_leaves_nothing() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);
        let warm = warm_session(&fixture.backend);

        send(&mut fixture.state, Message::Key(BareKey::Escape));

        assert_eq!(
            fixture.backend.calls().last(),
            Some(&Call::CloseSession { session: warm })
        );
        assert_eq!(fixture.state.app.draft(), None);
        assert_eq!(sessions_in_tree(&fixture.state), 0);
        assert_eq!(fixture.store.session_saves(&fixture.project), 0);
        assert_eq!(fixture.state.tab, Tab::Projects);
    }

    #[test]
    fn clicking_the_backdrop_cancels() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);
        let mut ui = simulator(app_view(&fixture.state));

        ui.point_at(Point::new(20.0, 600.0));
        ui.simulate(iced_test::simulator::click());

        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [sheet(NewSessionMessage::Cancel)]);
    }

    #[test]
    fn the_buttons_create_and_cancel() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);

        for (button, expected) in [
            ("new-session-create", NewSessionMessage::Create),
            ("new-session-cancel", NewSessionMessage::Cancel),
        ] {
            let mut ui = simulator(app_view(&fixture.state));
            ui.click(Id::new(button)).unwrap();
            let messages: Vec<Message> = ui.into_messages().collect();
            assert_eq!(messages, [sheet(expected)]);
        }
    }

    #[test]
    fn the_pickers_and_the_checkbox_are_found_by_their_ids() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);
        {
            let mut ui = simulator(app_view(&fixture.state));
            for id in [
                "new-session-model",
                "new-session-effort",
                "new-session-backend",
                "new-session-auto-allow",
            ] {
                ui.find(Id::new(id)).unwrap();
            }
        }

        let mut ui = simulator(app_view(&fixture.state));
        ui.click(Id::new("new-session-auto-allow")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [sheet(NewSessionMessage::AutoAllow(true))]);
    }

    #[test]
    fn keys_other_than_enter_and_escape_do_nothing_while_the_sheet_is_open() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);
        let draft = fixture.state.app.draft();
        let starts = new_session_calls(&fixture.backend);

        for key in [
            BareKey::Tab,
            BareKey::ArrowDown,
            BareKey::ArrowUp,
            BareKey::N,
            BareKey::A,
            BareKey::D,
        ] {
            send(&mut fixture.state, Message::Key(key));
        }
        send(
            &mut fixture.state,
            Message::Jump(crate::jump::JumpMessage::Open),
        );
        send(&mut fixture.state, Message::SelectTab(Tab::Env));

        assert_eq!(fixture.state.tab, Tab::Projects);
        assert_eq!(fixture.state.jump, None);
        assert_eq!(fixture.state.app.draft(), draft);
        assert_eq!(new_session_calls(&fixture.backend), starts);
    }

    #[test]
    fn a_failed_create_keeps_the_sheet_open_with_the_error() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);

        fixture.store.fail_next_session_save();
        send(&mut fixture.state, Message::Key(BareKey::Enter));

        assert!(fixture.state.app.draft().is_some());
        let error = fixture.state.error.clone().unwrap();
        {
            let mut ui = simulator(app_view(&fixture.state));
            assert!(shown(&mut ui, &error) >= 1);
        }
        assert_eq!(sessions_in_tree(&fixture.state), 0);

        send(&mut fixture.state, Message::Key(BareKey::Escape));
        assert_eq!(fixture.state.app.draft(), None);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_failed_open_shows_the_error_and_no_sheet() {
        let mut fixture = fixture();

        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::NewSession(
                ProjectId::new(),
                Scope::Project,
            )),
        );

        assert!(fixture.state.error.is_some());
        assert_eq!(fixture.state.app.draft(), None);
    }

    #[test]
    fn a_closed_sheet_ignores_its_messages() {
        let mut fixture = fixture();

        for message in [
            NewSessionMessage::Create,
            NewSessionMessage::Cancel,
            NewSessionMessage::AutoAllow(true),
        ] {
            send(&mut fixture.state, sheet(message));
        }

        assert_eq!(sessions_in_tree(&fixture.state), 0);
        assert_eq!(fixture.state.error, None);
        assert!(fixture.backend.calls().is_empty());
    }
}
