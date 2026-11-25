//! The new-session sheet: the settings of a session picked before it exists.
//!
//! A `new session` row of the Projects tab opens the sheet for its project and scope, and N
//! opens it for a project session in the project of the highlighted row. The sheet is open
//! exactly while the core prepares a session, so the core holds the only copy of what it
//! shows. Its first field, `Repo`, picks what the session works on: the whole project (the
//! orchestrator when it has several repositories) or one repository, listed with the
//! instruction files it has, and, in a project with a single repository, last
//! `orchestrator · for this repository`. A project that offers nothing besides itself (a
//! folder without repositories) shows the field as a frame, and so does a session being
//! created. Picking another repository, or the orchestrator, restarts the agent there. The subline counts the repositories
//! and says that children of an orchestrator inherit the settings only when the project has
//! several repositories below its folder. The newest session of the project seeds the
//! backend, model, effort and auto-allow, and opening the sheet warms up an agent on that
//! backend in the background. Until the agent answers, the model and effort pickers show the
//! wanted value (or `default`) with a `loading…` hint; then they list what the agent offers,
//! and a setting the agent does not offer says so. Picking another backend warms up an agent
//! there. When the agent cannot start, the sheet says why and the session cannot be created.
//!
//! `Create session` (or Enter) creates the session with the picked settings, selects it,
//! shows the Session tab and puts the focus in its prompt box; when that fails the sheet
//! stays open and says why. Saving the new session runs in the background: a save that fails
//! leaves the session created, and the error bar says so once the store reports it. `Cancel`, Escape or a click on the backdrop closes the sheet and
//! leaves nothing behind. While the sheet is open no other key reaches the window.
//!
//! The `Branch & worktree` box picks how the session uses its repository with two switches:
//! the `folder` (`Main checkout` or `Worktree`) and the `branch` (`As is`, `Existing` or
//! `New`). Every segment is always shown; one the session is not offered is disabled, so `As
//! is` cannot be picked in a worktree, a folder that is no repository offers only the main
//! checkout as it is, and an orchestrator offers no existing branch. Moving to a worktree
//! from `As is` makes a new branch there. A new branch shows its `name`, the `base` it starts
//! from, whether that base is fetched first and, in a worktree, where the worktree goes
//! (`worktree at`). An existing branch shows the `existing` picker over the repository's
//! local branches, those with a worktree first, and, in a worktree, the worktree it uses or
//! adds. `As is` shows the branch the repository has checked out now beside what the core
//! says it does (`works in <repo> as it is now`, and who else works there); an orchestrator
//! says its children work in their repositories as they are. Until git has described the
//! repository, the base, fetch and existing rows say `loading…`; the switches and the name can
//! be changed meanwhile. A dim line says what the choice does and why Create cannot be pressed
//! yet; when a new branch is named after one that exists, a `use existing` link beside that
//! reason switches the box to that existing branch in the same folder. The name field and the
//! picker leave Enter to the sheet, unless the picker's list is open, where Enter picks the
//! first branch left, so Enter in the name field creates the session too.
//!
//! When git has work to do first (a new branch, a branch the main checkout switches to, or a
//! worktree to add), Create lets git do it: the button says `creating…`, every setting is
//! frozen and Enter does nothing, while `Cancel`, Escape and the backdrop still close the
//! sheet at once (the core undoes what git made once it reports back). When git reports
//! success the sheet closes and the new session is shown with the focus in its prompt box;
//! when git fails the sheet says why and Create can be pressed again.

use std::path::Path;

use iced::widget::{
    Column, Id, button, checkbox, column, container, operation, pick_list, row, space, text,
    text_input,
};
use iced::{Alignment, Element, Padding, Task};
use partitur_core::{
    BackendId, BranchKind, ChoiceView, ConfigValueId, DraftChoice, DraftView, Folder, ProjectId,
    Scope, ScopeChoice, Warmup,
};

use crate::app::{BareKey, Message, State, report};
use crate::branch_controls::{
    self, CHECKBOX_SIZE, CHECKBOX_SPACING, ExistingPick, PICKER_PADDING, PICKER_WIDTH, field,
    folder_segments, frame, frame_of_width, framed_picker, kind_segments,
};
use crate::confirm_sheet;
use crate::controls::{BackendChoice, Choice, with_chevron_inset};
use crate::session_view;
use crate::sheet::{self, GAP, PADDING, TITLE_SIZE, WIDTH};
use crate::theme::Tokens;
use crate::{badge, home_path, projects_view, style, theme};

/// The ids of the pickers, the checkboxes, the name field and the buttons.
const REPO: &str = "new-session-repo";
const MODEL: &str = "new-session-model";
const EFFORT: &str = "new-session-effort";
const BACKEND: &str = "new-session-backend";
const AUTO_ALLOW: &str = "new-session-auto-allow";
const BRANCH: &str = "new-session-branch";
const EXISTING: &str = "new-session-existing";
const USE_EXISTING: &str = "new-session-use-existing";
const BASE: &str = "new-session-base";
const FETCH: &str = "new-session-fetch";
const CREATE: &str = "new-session-create";
const CANCEL: &str = "new-session-cancel";

/// The space between the rows of settings.
const SETTINGS_GAP: f32 = 8.0;

/// The width of the Repo picker, which holds entries as long as `iqip-frontend · AGENTS.md`.
const REPO_WIDTH: f32 = 320.0;

/// The width of the column that names the settings.
const LABEL_WIDTH: f32 = 120.0;

/// The width of the column that names the rows of the `Branch & worktree` box.
const BOX_LABEL_WIDTH: f32 = 108.0;

/// The space between the rows of the `Branch & worktree` box.
const BOX_GAP: f32 = 8.0;

/// An interaction with the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewSessionMessage {
    /// The user picked what the session works on, and whether it orchestrates.
    Scope(ScopeChoice),
    /// The user picked a model.
    Model(ConfigValueId),
    /// The user picked a reasoning effort.
    Effort(ConfigValueId),
    /// The user picked a backend: an agent warms up there.
    Backend(BackendId),
    /// The user switched auto-allow on or off.
    AutoAllow(bool),
    /// The user picked the folder the session works in.
    Folder(Folder),
    /// The user picked the kind of branch the session works on.
    Kind(BranchKind),
    /// The user picked the existing branch to work on.
    Existing(String),
    /// The user chose to work on the existing branch the typed name names.
    UseExisting,
    /// The user typed in the new branch's name; this is the whole name.
    Branch(String),
    /// The user picked the branch the new branch starts from.
    Base(String),
    /// The user switched fetching the base first on or off.
    Fetch(bool),
    /// The user asked to create the session.
    Create,
    /// The user cancelled: the sheet closes and nothing is left behind.
    Cancel,
}

/// Opens the sheet for a new session of `scope` in `project`. When the core refuses (the
/// project or the repository is gone), the error bar says why and the sheet stays closed.
pub(crate) fn open(state: &mut State, project: ProjectId, scope: Scope) -> Task<Message> {
    if report(&mut state.error, state.app.prepare_session(project, scope)).is_some() {
        state.sheet_error = None;
    }
    Task::none()
}

/// Applies `message` to the open sheet, as the [module](self) describes, and returns the
/// task that focuses the new session's prompt box once it is created. A setting the core
/// refuses shows its error in the sheet; errors from elsewhere stay in the error bar, which
/// the sheet leaves alone. Nothing happens while the sheet is closed.
pub fn update(state: &mut State, message: NewSessionMessage) -> Task<Message> {
    if state.app.draft().is_none() {
        return Task::none();
    }
    let changed = match message {
        NewSessionMessage::Scope(choice) => {
            state.app.draft_scope(choice.scope, choice.orchestrator)
        }
        NewSessionMessage::Model(value) => state.app.draft_model(value),
        NewSessionMessage::Effort(value) => state.app.draft_effort(value),
        NewSessionMessage::Backend(backend) => state.app.draft_backend(backend),
        NewSessionMessage::AutoAllow(enabled) => state.app.draft_auto_allow(enabled),
        NewSessionMessage::Folder(folder) => state.app.draft_folder(folder),
        NewSessionMessage::Kind(kind) => state.app.draft_kind(kind),
        NewSessionMessage::Existing(name) => state.app.draft_existing(name),
        NewSessionMessage::UseExisting => state.app.draft_use_existing(),
        NewSessionMessage::Branch(name) => state.app.draft_branch(name),
        NewSessionMessage::Base(base) => state.app.draft_base(base),
        NewSessionMessage::Fetch(fetch) => state.app.draft_fetch(fetch),
        NewSessionMessage::Create => {
            let created = state.app.create_from_draft();
            if state.app.draft().is_none() {
                // The session exists even when a choice could not be sent; the sheet is
                // closed, so that failure shows in the error bar.
                report(&mut state.error, created);
                return created_shown(state);
            }
            // Either git makes the branch first and the sheet waits for it, or the create
            // failed and the sheet says why.
            state.sheet_error = None;
            created.map(|_| ())
        }
        NewSessionMessage::Cancel => {
            // The sheet closes either way, so a close that cannot be queued shows in the
            // error bar.
            state.sheet_error = None;
            report(&mut state.error, state.app.discard_draft());
            return Task::none();
        }
    };
    report(&mut state.sheet_error, changed);
    Task::none()
}

/// After every [`Message::Git`]: when the sheet was creating and the draft is now gone, shows
/// the new session and focuses its prompt (what Create does at once when git has no work);
/// otherwise nothing. A Create that git failed keeps the draft, which shows why.
pub(crate) fn after_git(state: &mut State, was_creating: bool) -> Task<Message> {
    if was_creating && state.app.draft().is_none() {
        created_shown(state)
    } else {
        Task::none()
    }
}

/// Keeps the existing-branch picker's list in step with the branches the open sheet offers,
/// and empties it once the sheet is closed; it does no work while the branches stay the same.
pub(crate) fn sync(state: &mut State) {
    let options = state
        .app
        .draft()
        .map(|draft| draft.branch.existing_options)
        .unwrap_or_default();
    state.existing_pick.sync(&options, state.home.as_deref());
}

/// Shows the session just created on the Session tab and returns the task that focuses its
/// prompt box; the sheet's error is gone with the sheet.
fn created_shown(state: &mut State) -> Task<Message> {
    state.sheet_error = None;
    projects_view::show_current_session(state);
    operation::focus(session_view::PROMPT)
}

/// What the bare `key` does while the sheet is open: Enter creates the session when Create
/// can be pressed, Escape cancels, and every other key does nothing, since the window under
/// the sheet takes no keys.
pub(crate) fn press(state: &mut State, key: BareKey) -> Task<Message> {
    match key {
        BareKey::Enter => {
            if state.app.draft().is_some_and(|draft| draft.can_create()) {
                update(state, NewSessionMessage::Create)
            } else {
                Task::none()
            }
        }
        BareKey::Escape => update(state, NewSessionMessage::Cancel),
        BareKey::Tab
        | BareKey::ArrowDown
        | BareKey::ArrowUp
        | BareKey::N
        | BareKey::A
        | BareKey::D
        | BareKey::G
        | BareKey::F2
        | BareKey::Digit(_) => Task::none(),
    }
}

/// `page` with the sheet for `draft` over it, its existing branches listed by `pick` and
/// paths under `home` written with `~`, and in the sheet why its agent could not start or,
/// else, why its last Create failed or the `error` of a failed action. The backdrop takes
/// every click meant for the page; a click on it cancels.
pub fn view<'a>(
    page: Element<'a, Message>,
    draft: &DraftView,
    pick: &'a ExistingPick,
    home: Option<&Path>,
    error: Option<&str>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let failure = match &draft.warmup {
        Warmup::Failed(reason) => Some(reason.as_str()),
        Warmup::Loading | Warmup::Ready => draft.failure.as_deref().or(error),
    };
    let loading = draft.warmup == Warmup::Loading;
    let editable = !draft.creating;
    let mut content = column![
        column![
            text(title(draft)).size(TITLE_SIZE).font(theme::UI_SEMIBOLD),
            text(subline(draft)).size(theme::SMALL).style(style::muted),
        ]
        .spacing(4),
        column![
            setting("Repo", repo_picker(draft), Alignment::Center),
            setting(
                "Model",
                picker(
                    &draft.model,
                    loading,
                    editable,
                    MODEL,
                    NewSessionMessage::Model
                ),
                Alignment::Center,
            ),
            setting(
                "Effort",
                picker(
                    &draft.effort,
                    loading,
                    editable,
                    EFFORT,
                    NewSessionMessage::Effort
                ),
                Alignment::Center,
            ),
            setting("Backend", backend_picker(draft), Alignment::Center),
            setting(
                "Permissions",
                permissions(draft.auto_allow, editable),
                Alignment::Start
            ),
        ]
        .spacing(SETTINGS_GAP),
        branch_and_worktree(draft, pick, home),
    ]
    .spacing(GAP);
    if let Some(failure) = failure {
        content = content.push(
            text(failure.to_owned())
                .size(theme::SMALL)
                .color(tokens.failed.fg),
        );
    }
    content = content.push(actions(draft.can_create(), draft.creating));
    let card = container(content)
        .width(WIDTH)
        .padding(PADDING)
        .style(sheet::sheet_style)
        .into();
    sheet::modal(
        page,
        card,
        Alignment::Center,
        Padding::ZERO,
        Message::NewSession(NewSessionMessage::Cancel),
    )
}

/// `New session in <project>`.
fn title(draft: &DraftView) -> String {
    format!("New session in {}", draft.project)
}

/// How many repositories the project has and, when it has two or more repositories below
/// the project folder, that the children of an orchestrator inherit the settings.
fn subline(draft: &DraftView) -> String {
    let count = match draft.repositories {
        1 => "1 repo".to_owned(),
        count => format!("{count} repos"),
    };
    let below = draft
        .scopes
        .iter()
        .filter(|choice| matches!(choice.scope, Scope::Repository(_)))
        .count();
    if below >= 2 {
        format!("{count} · children of an orchestrator inherit these settings")
    } else {
        count
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

/// The picker of `choice`, found by `id`, that sends `on_select` with the picked value while
/// the sheet is `editable`.
///
/// iced's pick list has no disabled state, so a picker that waits for the agent, that the
/// agent does not offer or that cannot be changed now is a frame with text: the wanted value
/// (or `default`) and, while the agent is `loading`, a hint; the note that the backend does
/// not offer it; or the value picked.
fn picker<'a>(
    choice: &DraftChoice,
    loading: bool,
    editable: bool,
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
        DraftChoice::Offered(choice) if editable => offered(choice, on_select),
        DraftChoice::Offered(choice) => {
            frame(text(choice.current_name.clone()).style(style::soft).into())
        }
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

/// The framed pick list over the configured backends, with the draft's own selected; while
/// the session is being created, a frame with the backend's name.
fn backend_picker<'a>(draft: &DraftView) -> Element<'a, Message> {
    let options: Vec<BackendChoice> = draft.backends.iter().map(BackendChoice::from).collect();
    let current = options
        .iter()
        .find(|option| option.id == draft.backend)
        .cloned();
    let control = if draft.creating {
        let name = current.map_or_else(|| draft.backend.to_string(), |current| current.to_string());
        frame(text(name).style(style::soft).into())
    } else {
        let picker = pick_list(options, current, |picked: BackendChoice| {
            Message::NewSession(NewSessionMessage::Backend(picked.id))
        })
        .width(PICKER_WIDTH)
        .padding(PICKER_PADDING)
        .handle(pick_list::Handle::None)
        .style(framed_picker)
        .menu_style(style::menu);
        with_chevron_inset(picker, PICKER_PADDING[1])
    };
    container(control).id(BACKEND).into()
}

/// The entry of the Repo field that is the draft's own: the one with its scope and
/// orchestrator flag, since the plain project session and the orchestrator share a scope.
fn picked_scope(draft: &DraftView) -> Option<ScopeChoice> {
    draft
        .scopes
        .iter()
        .find(|choice| choice.scope == draft.scope && choice.orchestrator == draft.orchestrator)
        .cloned()
}

/// The framed pick list over the scopes the project offers, with the draft's own selected.
/// When there is nothing to pick (the project offers no scope
/// besides itself, as a folder without repositories, or the session is being created) it is
/// a frame of the same width that shows the selected scope's label.
fn repo_picker<'a>(draft: &DraftView) -> Element<'a, Message> {
    let current = picked_scope(draft);
    let control = if draft.creating || draft.scopes.len() < 2 {
        let label = current.map_or_else(|| draft.project.clone(), |current| current.to_string());
        frame_of_width(text(label).style(style::soft).into(), REPO_WIDTH)
    } else {
        let picker = pick_list(draft.scopes.clone(), current, |picked: ScopeChoice| {
            Message::NewSession(NewSessionMessage::Scope(picked))
        })
        .width(REPO_WIDTH)
        .padding(PICKER_PADDING)
        .handle(pick_list::Handle::None)
        .style(framed_picker)
        .menu_style(style::menu);
        with_chevron_inset(picker, PICKER_PADDING[1])
    };
    container(control).id(REPO).into()
}

/// The auto-allow checkbox, found by its id, and under its label what it means; it can be
/// switched only while the sheet is `editable`.
fn permissions<'a>(auto_allow: bool, editable: bool) -> Element<'a, Message> {
    let toggle = checkbox(auto_allow)
        .label("Auto-allow tool calls")
        .on_toggle_maybe(
            editable
                .then_some(|enabled| Message::NewSession(NewSessionMessage::AutoAllow(enabled))),
        )
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

/// The id of the segment of the folder switch that picks `folder`.
fn folder_id(folder: Folder) -> Id {
    Id::new(match folder {
        Folder::MainCheckout => "new-session-folder-main",
        Folder::Worktree => "new-session-folder-worktree",
    })
}

/// The id of the segment of the branch switch that picks `kind`.
fn kind_id(kind: BranchKind) -> Id {
    Id::new(match kind {
        BranchKind::AsIs => "new-session-kind-as-is",
        BranchKind::Existing => "new-session-kind-existing",
        BranchKind::New => "new-session-kind-new",
    })
}

/// The `Branch & worktree` box for `draft`, as the [module](self) describes, its existing
/// branches listed by `pick`; a worktree path under `home` is written with `~`.
fn branch_and_worktree<'a>(
    draft: &DraftView,
    pick: &'a ExistingPick,
    home: Option<&Path>,
) -> Element<'a, Message> {
    let branch = &draft.branch;
    let editable = !draft.creating;
    let folder = branch.mode.folder();
    let mut lines: Column<'a, Message> = column![
        text("Branch & worktree")
            .size(theme::SMALL)
            .font(theme::UI_MEDIUM)
            .style(style::muted),
        line(
            "folder",
            branch_controls::switch(
                &folder_segments(&branch.modes),
                folder,
                folder_id,
                editable.then_some(|folder| Message::NewSession(NewSessionMessage::Folder(folder))),
            ),
        ),
        line(
            "branch",
            branch_controls::switch(
                &kind_segments(&branch.modes, folder),
                branch.mode.kind(),
                kind_id,
                editable.then_some(|kind| Message::NewSession(NewSessionMessage::Kind(kind))),
            ),
        ),
    ]
    .spacing(BOX_GAP);
    let mut note = branch.note.as_ref();
    match branch.mode.kind() {
        BranchKind::AsIs => {
            if let Some(checked_out) = checked_out_line(draft) {
                lines = lines.push(checked_out);
                // A repository's note is part of its checked-out line.
                if !draft.orchestrator {
                    note = None;
                }
            }
        }
        BranchKind::New => {
            lines = lines
                .push(line("name", name_field(&branch.branch, editable)))
                .push(line(
                    "base",
                    branch_controls::base_picker(
                        &branch.bases,
                        branch.base.as_deref(),
                        None,
                        branch.loading,
                        Id::new(BASE),
                        editable
                            .then_some(|base| Message::NewSession(NewSessionMessage::Base(base))),
                    ),
                ))
                .push(line("", fetch_row(draft)));
        }
        BranchKind::Existing => {
            let picker = branch_controls::existing_picker(
                pick,
                branch.existing.as_deref(),
                branch.loading,
                editable.then_some(|name| Message::NewSession(NewSessionMessage::Existing(name))),
            );
            lines = lines.push(line("existing", container(picker).id(EXISTING).into()));
        }
    }
    if folder == Folder::Worktree
        && let Some(worktree) = &branch.worktree
    {
        lines = lines.push(line(
            "worktree at",
            text(home_path::abbreviated(worktree, home))
                .size(theme::SMALL)
                .font(theme::MONO)
                .style(style::soft)
                .into(),
        ));
    }
    if let Some(note) = note {
        lines = lines.push(hint(note));
    }
    if let Some(blocker) = &branch.blocker {
        lines = lines.push(blocker_line(draft, blocker));
    }
    confirm_sheet::meaning_box(lines.into())
}

/// A dim line of the box that says what the choice does or why Create cannot be pressed.
fn hint<'a>(hint: &str) -> Element<'a, Message> {
    text(hint.to_owned())
        .size(theme::SMALL)
        .style(style::dim)
        .into()
}

/// Why Create cannot be pressed, and beside it the `use existing` link when core offers it,
/// which it does only while the reason is that the new branch's name names one that exists
/// already.
fn blocker_line<'a>(draft: &DraftView, blocker: &str) -> Element<'a, Message> {
    if draft.branch.use_existing.is_none() {
        return hint(blocker);
    }
    let link = branch_controls::use_existing_link(
        (!draft.creating).then_some(Message::NewSession(NewSessionMessage::UseExisting)),
    );
    row![hint(blocker), container(link).id(USE_EXISTING)]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// One row of the box: the dim `label` in its column, then `control` on its centre line.
fn line<'a>(label: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    row![
        text(label)
            .size(theme::SMALL)
            .width(BOX_LABEL_WIDTH)
            .style(style::muted),
        control
    ]
    .align_y(Alignment::Center)
    .into()
}

/// The field the new branch's name is typed in, found by its id; it can be typed in only
/// while the sheet is `editable`. It has no submit, so Enter goes on to the sheet, which
/// creates the session.
fn name_field<'a>(name: &str, editable: bool) -> Element<'a, Message> {
    text_input("name of the new branch", name)
        .id(BRANCH)
        .on_input_maybe(
            editable.then_some(|name| Message::NewSession(NewSessionMessage::Branch(name))),
        )
        .width(PICKER_WIDTH)
        .padding(PICKER_PADDING)
        .size(theme::SMALL)
        .font(theme::MONO)
        .style(field)
        .into()
}

/// The checkbox that fetches the base first. An orchestrator's children each fetch their
/// own repository's remote, so its label says that instead of naming one remote.
fn fetch_row<'a>(draft: &DraftView) -> Element<'a, Message> {
    let branch = &draft.branch;
    let on_toggle =
        (!draft.creating).then_some(|fetch| Message::NewSession(NewSessionMessage::Fetch(fetch)));
    if !draft.orchestrator {
        return branch_controls::fetch_toggle(
            branch.fetch,
            branch.fetch_remote.as_deref(),
            branch.base.as_deref(),
            branch.loading,
            Id::new(FETCH),
            on_toggle,
        );
    }
    let label = if branch.loading {
        "loading…"
    } else {
        "Fetch first and start from each repository's remote base"
    };
    let possible = !branch.loading && branch.fetch_remote.is_some();
    branch_controls::fetch_checkbox(
        label.to_owned(),
        branch.fetch,
        Id::new(FETCH),
        on_toggle.filter(|_| possible),
    )
}

/// What working as it is does: for a repository the `checked out` row with the branch it has
/// checked out now (`loading…` until git answers, `(detached)` without a branch) beside the
/// core's note, and for an orchestrator that its children do the same. `None` where the box
/// offers no other choice, since its note says why.
fn checked_out_line<'a>(draft: &DraftView) -> Option<Element<'a, Message>> {
    let branch = &draft.branch;
    if draft.orchestrator {
        return Some(
            text("children work in their repositories as they are now")
                .size(theme::SMALL)
                .style(style::muted)
                .into(),
        );
    }
    if branch.modes.len() < 2 {
        return None;
    }
    let name = if branch.loading {
        "loading…".to_owned()
    } else {
        branch
            .checked_out
            .clone()
            .unwrap_or_else(|| "(detached)".to_owned())
    };
    Some(line(
        "checked out",
        row![
            text(name)
                .size(theme::SMALL)
                .font(theme::MONO)
                .style(style::soft),
            text(branch.note.clone().unwrap_or_default())
                .size(theme::SMALL)
                .style(style::muted),
        ]
        .spacing(8)
        .into(),
    ))
}

/// The buttons, found by their ids, and on the right the hint where the title comes from.
/// Create can be pressed only when it `can_create`, and reads `creating…` while `creating`.
fn actions<'a>(can_create: bool, creating: bool) -> Element<'a, Message> {
    let label = if creating {
        "creating…"
    } else {
        "Create session"
    };
    let create = button(
        row![
            text(label).font(theme::UI_SEMIBOLD),
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
mod tests;
