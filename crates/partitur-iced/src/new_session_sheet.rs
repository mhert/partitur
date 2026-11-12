//! The new-session sheet: the settings of a session picked before it exists.
//!
//! A `new session` row of the Projects tab opens the sheet for its project and scope, and N
//! opens it for a project session in the project of the highlighted row. The sheet is open
//! exactly while the core prepares a session, so the core holds the only copy of what it
//! shows. Its first field, `Repo`, picks what the session works on: the whole project (the
//! orchestrator when it has several repositories) or one repository, listed with the
//! instruction files it has. A project that offers nothing besides itself (its folder is a
//! single repository) shows the field as a frame, and so does a session being created.
//! Picking another repository restarts the agent there. The subline counts the repositories
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
//! stays open and says why. `Cancel`, Escape or a click on the backdrop closes the sheet and
//! leaves nothing behind. While the sheet is open no other key reaches the window.
//!
//! The `Branch & worktree` box picks how the session uses its repository: the mode
//! (`Worktree`, `New branch` or `Checked-out`), and for the first two the new branch's name,
//! the base it starts from, whether that base is fetched first and, in Worktree mode, where
//! the worktree goes. Checked-out mode shows the branch the repository has checked out now.
//! Until git has described the repository, the base and fetch rows say `loading…`; the mode
//! and the name can be changed meanwhile. A dim line says what a mode does and why Create
//! cannot be pressed yet. The name field leaves Enter to the sheet, so Enter in it creates
//! the session too.
//!
//! In Worktree and New branch mode, Create first lets git make the branch: the button says
//! `creating…`, every setting is frozen and Enter does nothing, while `Cancel`, Escape and the
//! backdrop still close the sheet at once (the core undoes what git made once it reports
//! back). When git reports success the sheet closes and the new session is shown with the
//! focus in its prompt box; when git fails the sheet says why and Create can be pressed
//! again.

use std::path::Path;

use iced::widget::{
    Column, Id, button, checkbox, column, container, operation, pick_list, row, space, text,
    text_input,
};
use iced::{Alignment, Element, Task, Theme};
use partitur_core::{
    BackendId, BranchMode, ChoiceView, ConfigValueId, DraftChoice, DraftView, ProjectId, Scope,
    ScopeChoice, Warmup,
};

use crate::app::{BareKey, Message, State, report};
use crate::branch_controls::{
    self, CHECKBOX_SIZE, CHECKBOX_SPACING, PICKER_PADDING, PICKER_WIDTH, frame, frame_of_width,
    framed_picker, picker_border,
};
use crate::confirm_sheet::{self, GAP, PADDING, TITLE_SIZE, WIDTH};
use crate::session_view::{self, BackendChoice, Choice, with_chevron_inset};
use crate::theme::Tokens;
use crate::{badge, home_path, projects_view, style, theme};

/// The ids of the pickers, the checkboxes, the name field and the buttons.
const REPO: &str = "new-session-repo";
const MODEL: &str = "new-session-model";
const EFFORT: &str = "new-session-effort";
const BACKEND: &str = "new-session-backend";
const AUTO_ALLOW: &str = "new-session-auto-allow";
const BRANCH: &str = "new-session-branch";
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
    /// The user picked a branch mode.
    Mode(BranchMode),
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
        NewSessionMessage::Mode(mode) => state.app.draft_mode(mode),
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
        | BareKey::F2 => Task::none(),
    }
}

/// `page` with the sheet for `draft` over it, and in the sheet why its agent could not
/// start or, else, why its last Create failed or the `error` of a failed action. The
/// backdrop takes every click meant for the page; a click on it cancels.
pub fn view<'a>(
    page: Element<'a, Message>,
    draft: &DraftView,
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
        branch_and_worktree(draft, std::env::home_dir().as_deref()),
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
        .style(confirm_sheet::sheet_style)
        .into();
    confirm_sheet::modal(page, card, Message::NewSession(NewSessionMessage::Cancel))
}

/// `New session in <project>`.
fn title(draft: &DraftView) -> String {
    format!("New session in {}", draft.project)
}

/// How many repositories the project has and, when it can have an orchestrator (two or more
/// repositories below the project folder), that the children of one inherit the settings.
fn subline(draft: &DraftView) -> String {
    let count = match draft.repositories {
        1 => "1 repo".to_owned(),
        count => format!("{count} repos"),
    };
    // The scopes are the project, then each repository below its folder.
    if draft.scopes.len() > 2 {
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

/// The framed pick list over the scopes the project offers, with the draft's own selected.
/// When there is nothing to pick (the project offers no scope besides itself, as a project
/// that is a single repository, or the session is being created) it is a frame of the same
/// width that shows the selected scope's label.
fn repo_picker<'a>(draft: &DraftView) -> Element<'a, Message> {
    let current = draft
        .scopes
        .iter()
        .find(|choice| choice.scope == draft.scope)
        .cloned();
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

/// The id of the segment of the mode switch that picks `mode`.
fn mode_id(mode: BranchMode) -> Id {
    Id::new(match mode {
        BranchMode::Worktree => "new-session-mode-worktree",
        BranchMode::NewBranch => "new-session-mode-new-branch",
        BranchMode::CheckedOut => "new-session-mode-checked-out",
    })
}

/// The `Branch & worktree` box for `draft`, as the [module](self) describes; a worktree path
/// under `home` is written with `~`.
fn branch_and_worktree<'a>(draft: &DraftView, home: Option<&Path>) -> Element<'a, Message> {
    let branch = &draft.branch;
    let editable = !draft.creating;
    let mut lines: Column<'a, Message> = column![
        text("Branch & worktree")
            .size(theme::SMALL)
            .font(theme::UI_MEDIUM)
            .style(style::muted),
        line(
            "mode",
            branch_controls::mode_switch(
                &branch.modes,
                branch.mode,
                mode_id,
                editable.then_some(|mode| Message::NewSession(NewSessionMessage::Mode(mode))),
            ),
        ),
    ]
    .spacing(BOX_GAP);
    if branch.mode == BranchMode::CheckedOut {
        if let Some(checked_out) = checked_out_line(draft) {
            lines = lines.push(checked_out);
        }
    } else {
        lines = lines
            .push(line("branch name", name_field(&branch.branch, editable)))
            .push(line(
                "base branch",
                branch_controls::base_picker(
                    &branch.bases,
                    branch.base.as_deref(),
                    branch.loading,
                    Id::new(BASE),
                    editable.then_some(|base| Message::NewSession(NewSessionMessage::Base(base))),
                ),
            ))
            .push(line("", fetch_row(draft)));
        if branch.mode == BranchMode::Worktree
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
    }
    for hint in [&branch.note, &branch.blocker].into_iter().flatten() {
        lines = lines.push(text(hint.clone()).size(theme::SMALL).style(style::dim));
    }
    confirm_sheet::meaning_box(lines.into())
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

/// What Checked-out mode works on: `branch  <name>  works in the repository as it is now`
/// for a repository (`loading…` until git answers, `(detached)` without a branch), and for an
/// orchestrator that its children do the same. `None` where the box offers no other mode,
/// since its note says why.
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
        "branch",
        row![
            text(name)
                .size(theme::SMALL)
                .font(theme::MONO)
                .style(style::soft),
            text("works in the repository as it is now")
                .size(theme::SMALL)
                .style(style::muted),
        ]
        .spacing(8)
        .into(),
    ))
}

/// The name field: the window colour inside a picker's outline, which takes the input border
/// colour under the pointer and while the field has the focus.
fn field(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let tokens = theme::tokens(theme);
    let outline = match status {
        text_input::Status::Focused { .. } | text_input::Status::Hovered => tokens.border_input,
        text_input::Status::Active | text_input::Status::Disabled => tokens.border,
    };
    text_input::Style {
        background: tokens.bg.into(),
        border: picker_border(outline),
        icon: tokens.muted,
        placeholder: tokens.dim,
        value: if status == text_input::Status::Disabled {
            tokens.text_soft
        } else {
            tokens.text
        },
        selection: tokens.text_selection,
    }
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
    use std::path::PathBuf;

    use iced::event;
    use iced::keyboard::key;
    use partitur_core::testing::{
        Call, GitCall, GitOp, MemoryGit, MemoryStore, RecordingBackend, repo_info, single_claude,
    };
    use partitur_core::{
        App, BackendEvent, BackendId, BackendInfo, BackendSessionId, BranchMode, ConfigCategory,
        ConfigChoice, ConfigId, ConfigOption, ConfigValueId, GlobalSettings, ProjectId,
        RepositoryId, RequestId, Scope, SessionId,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{BareKey, Message, State, Tab, boot, update as app_update, view as app_view};
    use crate::projects_view::ProjectsMessage;
    use crate::test_support::{orchestrated, settle_git, shown, two_backends};

    /// A state over a recording backend, an in-memory store and an in-memory git with one
    /// project open. Each of its repositories (only `api` by default) has `main` checked out,
    /// a branch `dev` besides, and `origin/main` on the remote.
    struct Fixture {
        state: State,
        backend: RecordingBackend,
        store: MemoryStore,
        git: MemoryGit,
        project: ProjectId,
        name: String,
        dir: TempDir,
    }

    fn fixture_with(backends: (Vec<BackendInfo>, BackendId)) -> Fixture {
        fixture_in(backends, &["api"])
    }

    fn fixture_in(backends: (Vec<BackendInfo>, BackendId), repositories: &[&str]) -> Fixture {
        let dir = TempDir::new().unwrap();
        let backend = RecordingBackend::new();
        let store = MemoryStore::new();
        let git = MemoryGit::new();
        let root = fs::canonicalize(dir.path()).unwrap();
        for name in repositories {
            let path = root.join(name);
            fs::create_dir_all(path.join(".git")).unwrap();
            git.add_repository(
                path.clone(),
                repo_info(&path, "main", &["dev", "main"], &["origin/main"]),
            );
        }
        let (backends, default) = backends;
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(store.clone()),
            Box::new(git.clone()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let name = state.app.tree().projects[0].name.clone();
        Fixture {
            state,
            backend,
            store,
            git,
            project,
            name,
            dir,
        }
    }

    fn fixture() -> Fixture {
        fixture_with(single_claude())
    }

    /// A fixture whose project folder is itself a repository, so it has no other.
    fn rooted_fixture() -> Fixture {
        let mut fixture = fixture_in(single_claude(), &[]);
        let root = fs::canonicalize(fixture.dir.path()).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fixture
            .git
            .add_repository(root.clone(), repo_info(&root, "main", &["main"], &[]));
        fixture.project = fixture.state.app.open_project(&root).unwrap();
        fixture
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

    /// The folder of the repository `api`, as the project records it.
    fn api(fixture: &Fixture) -> PathBuf {
        fs::canonicalize(fixture.dir.path()).unwrap().join("api")
    }

    /// Opens the sheet for a session in the repository `api`; git has not answered yet.
    fn open_api_sheet(fixture: &mut Fixture) {
        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::NewSession(
                fixture.project.clone(),
                Scope::Repository(RepositoryId::try_from("api").unwrap()),
            )),
        );
    }

    /// Opens the sheet for `api`, lets its agent and git answer, and picks Worktree mode
    /// with the branch `feat/x`.
    fn worktree_sheet() -> Fixture {
        let mut fixture = fixture();
        open_api_sheet(&mut fixture);
        answer(&mut fixture.state, &fixture.backend, Vec::new());
        let _task = settle_git(&mut fixture.state, &fixture.git);
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Mode(BranchMode::Worktree)),
        );
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Branch("feat/x".into())),
        );
        fixture
    }

    /// The messages a click on the control `id` of the window sends.
    fn click(state: &State, id: &'static str) -> Vec<Message> {
        let mut ui = simulator(app_view(state));
        ui.click(Id::new(id)).unwrap();
        ui.into_messages().collect()
    }

    /// The messages produced by opening the Repo field and clicking its second entry, one
    /// picker height below the first. A frame produces none; an enabled pick list sends
    /// `Scope`.
    fn pick_second_repo(state: &State) -> Vec<Message> {
        let mut ui = simulator(app_view(state));
        let picker = ui.find(Id::new("new-session-repo")).unwrap().bounds();
        ui.point_at(picker.center());
        let _ = ui.simulate(iced_test::simulator::click());
        let second = Point::new(picker.x + 12.0, picker.y + picker.height * 2.5);
        ui.point_at(second);
        let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: second });
        let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));
        ui.into_messages().collect()
    }

    /// How many texts of the window read `label`.
    fn count(state: &State, label: &str) -> usize {
        let mut ui = simulator(app_view(state));
        shown(&mut ui, label)
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
    fn the_title_names_only_the_project() {
        let mut fixture = fixture();

        open_api_sheet(&mut fixture);

        let mut ui = simulator(app_view(&fixture.state));
        ui.find(format!("New session in {}", fixture.name).as_str())
            .unwrap();
        assert!(
            ui.find(format!("New session in {} · api", fixture.name).as_str())
                .is_err()
        );
    }

    #[test]
    fn the_repo_field_comes_first_and_lists_orchestrator_then_repos() {
        let mut fixture = fixture_in(single_claude(), &["api", "web"]);
        open_project_sheet(&mut fixture);

        let mut ui = simulator(app_view(&fixture.state));
        let repo = ui.find(Id::new("new-session-repo")).unwrap().bounds();
        let model = ui.find(Id::new("new-session-model")).unwrap().bounds();
        assert!(repo.y < model.y);
        let labels: Vec<String> = fixture
            .state
            .app
            .draft()
            .unwrap()
            .scopes
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(labels, ["orchestrator · all 2 repos", "api", "web"]);
    }

    #[test]
    fn picking_a_repo_makes_a_repository_session() {
        let mut fixture = fixture_in(single_claude(), &["api", "web"]);
        open_project_sheet(&mut fixture);
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Scope(api_choice())),
        );
        answer(&mut fixture.state, &fixture.backend, Vec::new());
        send(&mut fixture.state, sheet(NewSessionMessage::Create));

        let calls = fixture.backend.calls();
        let (_, session, cwd) = calls.last().and_then(Call::as_new_session).unwrap();
        assert_eq!(cwd, api(&fixture));
        assert_eq!(fixture.state.app.current_session().as_ref(), Some(session));
        assert_eq!(
            fixture.state.app.tree().projects[0].sessions[0].scope_label,
            "api"
        );
    }

    /// The picker's entry for the repository `api`, which has no instruction files.
    fn api_choice() -> ScopeChoice {
        ScopeChoice {
            scope: Scope::Repository(RepositoryId::try_from("api").unwrap()),
            orchestrator: false,
            label: "api".to_owned(),
        }
    }

    #[test]
    fn a_pick_in_the_repo_field_sends_the_scope() {
        let mut fixture = fixture_in(single_claude(), &["api", "web"]);
        open_project_sheet(&mut fixture);
        let mut ui = simulator(app_view(&fixture.state));

        // A pick list keeps its text from selectors, so the menu is opened and its second
        // entry, one picker height below the first, is picked.
        let picker = ui.find(Id::new("new-session-repo")).unwrap().bounds();
        ui.point_at(picker.center());
        let _ = ui.simulate(iced_test::simulator::click());
        let second = Point::new(picker.x + 12.0, picker.y + picker.height * 2.5);
        ui.point_at(second);
        let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: second });
        let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));

        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            [sheet(NewSessionMessage::Scope(api_choice()))]
        );
    }

    #[test]
    fn the_repo_entries_name_their_instruction_files() {
        let mut fixture = fixture_in(single_claude(), &["api", "web"]);
        fs::write(fixture.dir.path().join("api").join("CLAUDE.md"), "x").unwrap();
        // The project reads its instruction files when it is opened, so it is opened again
        // to see the one just written.
        let _ = fixture.state.app.close_project(fixture.project.clone());
        let project = fixture.state.app.open_project(fixture.dir.path()).unwrap();

        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::NewSession(project, Scope::Project)),
        );

        let labels: Vec<String> = fixture
            .state
            .app
            .draft()
            .unwrap()
            .scopes
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            labels,
            ["orchestrator · all 2 repos", "api · CLAUDE.md", "web"]
        );
    }

    #[test]
    fn a_root_repo_sheet_can_pick_the_orchestrator() {
        let mut fixture = rooted_fixture();
        open_project_sheet(&mut fixture);
        assert_eq!(fixture.state.app.draft().unwrap().scopes.len(), 2);

        let mut ui = simulator(app_view(&fixture.state));
        ui.find(Id::new("new-session-repo")).unwrap();
        ui.find(fixture.name.as_str()).unwrap();
        let picked = pick_second_repo(&fixture.state);
        let picks_orchestrator = matches!(
            picked.as_slice(),
            [Message::NewSession(NewSessionMessage::Scope(choice))]
                if choice.scope == Scope::Project && choice.orchestrator
        );
        assert!(picks_orchestrator, "{picked:?}");
    }

    #[test]
    fn a_folder_around_one_repo_can_pick_it() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);

        let picked = pick_second_repo(&fixture.state);
        let wanted = Scope::Repository(RepositoryId::try_from("api").unwrap());
        let picks_api = matches!(
            picked.as_slice(),
            [Message::NewSession(NewSessionMessage::Scope(choice))]
                if choice.scope == wanted && !choice.orchestrator
        );
        assert!(picks_api, "{picked:?}");
        for message in picked {
            send(&mut fixture.state, message);
        }
        answer(&mut fixture.state, &fixture.backend, Vec::new());
        send(&mut fixture.state, sheet(NewSessionMessage::Create));

        let calls = fixture.backend.calls();
        let (_, _, cwd) = calls.last().and_then(Call::as_new_session).unwrap();
        assert_eq!(cwd, api(&fixture));
        assert_eq!(
            fixture.state.app.tree().projects[0].sessions[0].scope_label,
            "api"
        );
    }

    #[test]
    fn the_repo_field_is_frozen_while_creating() {
        let mut fixture = fixture_in(single_claude(), &["api", "web"]);
        open_project_sheet(&mut fixture);
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Scope(api_choice())),
        );
        answer(&mut fixture.state, &fixture.backend, Vec::new());
        let _task = settle_git(&mut fixture.state, &fixture.git);
        for message in [
            NewSessionMessage::Mode(BranchMode::Worktree),
            NewSessionMessage::Branch("feat/x".into()),
            NewSessionMessage::Create,
        ] {
            send(&mut fixture.state, sheet(message));
        }
        assert!(fixture.state.app.draft().unwrap().creating);

        assert_eq!(pick_second_repo(&fixture.state), []);

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Scope(api_choice())),
        );
        assert!(fixture.state.sheet_error.is_some());
    }

    #[test]
    fn the_subline_says_children_inherit() {
        let mut fixture = fixture_in(single_claude(), &["api", "web"]);
        open_project_sheet(&mut fixture);
        let mut ui = simulator(app_view(&fixture.state));
        ui.find("2 repos · children of an orchestrator inherit these settings")
            .unwrap();

        let mut single = fixture_in(single_claude(), &["api"]);
        open_project_sheet(&mut single);
        let mut ui = simulator(app_view(&single.state));
        ui.find("1 repo").unwrap();
    }

    #[test]
    fn the_subline_promises_no_orchestrator_where_there_can_be_none() {
        let mut empty = fixture_in(single_claude(), &[]);
        let behind = count(&empty.state, "0 repos");
        open_project_sheet(&mut empty);
        assert_eq!(count(&empty.state, "0 repos"), behind + 1);
        assert_eq!(
            count(
                &empty.state,
                "0 repos · children of an orchestrator inherit these settings"
            ),
            0
        );

        let mut rooted = rooted_fixture();
        open_project_sheet(&mut rooted);
        let mut ui = simulator(app_view(&rooted.state));
        ui.find("1 repo").unwrap();
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
        ui.find("2 repos · children of an orchestrator inherit these settings")
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
    fn the_sheet_no_longer_shows_placeholders() {
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
            "mode",
            "Checked-out",
            "the project folder is not a repository",
            "Create session",
            "Enter",
            "Cancel",
            "Esc",
            "title comes from your first prompt",
            "default",
        ] {
            assert!(shown(&mut ui, label) >= 1, "{label} is not shown");
        }
        for gone in [
            "defaults · editable in a later version",
            "wt/<title>",
            "per repo, created on first prompt",
            // A project folder that is not a repository offers no other mode.
            "Worktree",
            "New branch",
            "works in the repository as it is now",
        ] {
            assert_eq!(shown(&mut ui, gone), 0, "{gone} is shown");
        }
    }

    #[test]
    fn the_box_shows_the_checked_out_branch_by_default() {
        let mut fixture = fixture();
        open_api_sheet(&mut fixture);
        let _task = settle_git(&mut fixture.state, &fixture.git);

        let mut ui = simulator(app_view(&fixture.state));
        assert_eq!(shown(&mut ui, "works in the repository as it is now"), 1);
        assert_eq!(shown(&mut ui, "main"), 1);
        for mode in ["Worktree", "New branch", "Checked-out"] {
            assert_eq!(shown(&mut ui, mode), 1, "{mode}");
        }
        for row in ["branch name", "base branch", "worktree at"] {
            assert_eq!(shown(&mut ui, row), 0, "{row}");
        }
    }

    #[test]
    fn worktree_mode_shows_name_base_fetch_and_the_resolved_path() {
        let mut fixture = fixture();
        open_api_sheet(&mut fixture);
        let _task = settle_git(&mut fixture.state, &fixture.git);

        let messages = click(&fixture.state, "new-session-mode-worktree");
        assert_eq!(
            messages,
            [sheet(NewSessionMessage::Mode(BranchMode::Worktree))]
        );
        for message in messages {
            send(&mut fixture.state, message);
        }
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Branch("feat/x".into())),
        );

        let worktree = api(&fixture).parent().unwrap().join("api-wt/feat/x");
        let path = home_path::abbreviated(&worktree, std::env::home_dir().as_deref());
        let mut ui = simulator(app_view(&fixture.state));
        for label in [
            "branch name",
            "base branch",
            "Fetch origin and start from origin/main",
            "worktree at",
            path.as_str(),
        ] {
            assert_eq!(shown(&mut ui, label), 1, "{label}");
        }
        assert_eq!(shown(&mut ui, "works in the repository as it is now"), 0);
        assert_eq!(fixture.state.sheet_error, None);
    }

    #[test]
    fn a_base_without_a_remote_counterpart_cannot_be_fetched() {
        let mut fixture = worktree_sheet();

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Base("dev".into())),
        );

        assert_eq!(count(&fixture.state, "dev has no remote counterpart"), 1);
        assert!(click(&fixture.state, "new-session-fetch").is_empty());
        assert!(!fixture.state.app.draft().unwrap().branch.fetch);
    }

    #[test]
    fn create_is_disabled_and_says_why_until_the_name_is_valid() {
        let mut fixture = worktree_sheet();
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Branch(String::new())),
        );
        let calls = fixture.git.calls().len();

        assert_eq!(count(&fixture.state, "enter a branch name"), 1);
        assert!(click(&fixture.state, "new-session-create").is_empty());
        send(&mut fixture.state, Message::Key(BareKey::Enter));
        assert!(!fixture.state.app.draft().unwrap().creating);
        assert_eq!(fixture.git.calls().len(), calls);

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Branch("feat/x".into())),
        );
        assert_eq!(count(&fixture.state, "enter a branch name"), 0);
        assert_eq!(
            click(&fixture.state, "new-session-create"),
            [sheet(NewSessionMessage::Create)]
        );
    }

    #[test]
    fn typing_n_in_the_name_field_does_not_reach_the_window() {
        let mut fixture = worktree_sheet();
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Branch(String::new())),
        );

        let mut ui = simulator(app_view(&fixture.state));
        ui.click(Id::new("new-session-branch")).unwrap();
        // A key the field takes is no shortcut of the window, where N opens a sheet.
        assert_eq!(ui.typewrite("n"), event::Status::Captured);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [sheet(NewSessionMessage::Branch("n".into()))]);
        for message in messages {
            send(&mut fixture.state, message);
        }

        assert_eq!(fixture.state.app.draft().unwrap().branch.branch, "n");
        assert_eq!(fixture.state.sheet_error, None);
    }

    #[test]
    fn enter_in_the_name_field_creates_the_session() {
        let mut fixture = worktree_sheet();

        let mut ui = simulator(app_view(&fixture.state));
        ui.click(Id::new("new-session-branch")).unwrap();
        // The field lets Enter through, so the window's shortcut creates the session.
        assert_eq!(ui.tap_key(key::Named::Enter), event::Status::Ignored);
        assert_eq!(ui.into_messages().count(), 0);
        send(&mut fixture.state, Message::Key(BareKey::Enter));

        assert!(fixture.state.app.draft().unwrap().creating);
        let _task = settle_git(&mut fixture.state, &fixture.git);
        assert_eq!(fixture.state.app.draft(), None);
        assert_eq!(sessions_in_tree(&fixture.state), 1);
    }

    #[test]
    fn new_branch_mode_hints_that_the_repository_switches() {
        let mut fixture = worktree_sheet();

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Mode(BranchMode::NewBranch)),
        );

        let mut ui = simulator(app_view(&fixture.state));
        assert_eq!(shown(&mut ui, "switches api itself to the new branch"), 1);
        assert_eq!(shown(&mut ui, "branch name"), 1);
        assert_eq!(shown(&mut ui, "worktree at"), 0);
    }

    #[test]
    fn the_branch_rows_say_loading_until_git_answers() {
        let mut fixture = fixture();
        open_api_sheet(&mut fixture);
        answer(&mut fixture.state, &fixture.backend, Vec::new());

        // Checked-out mode waits for the branch checked out now.
        {
            let mut ui = simulator(app_view(&fixture.state));
            assert_eq!(shown(&mut ui, "loading…"), 1);
            assert_eq!(shown(&mut ui, "works in the repository as it is now"), 1);
        }

        // The mode and the name can be changed meanwhile; base and fetch wait.
        let messages = click(&fixture.state, "new-session-mode-worktree");
        for message in messages {
            send(&mut fixture.state, message);
        }
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Branch("feat/x".into())),
        );
        {
            let mut ui = simulator(app_view(&fixture.state));
            assert_eq!(shown(&mut ui, "loading…"), 2);
            assert_eq!(shown(&mut ui, "reading the branches of api…"), 1);
        }
        assert!(click(&fixture.state, "new-session-fetch").is_empty());
        assert_eq!(fixture.state.app.draft().unwrap().branch.branch, "feat/x");

        let _task = settle_git(&mut fixture.state, &fixture.git);
        let mut ui = simulator(app_view(&fixture.state));
        assert_eq!(shown(&mut ui, "loading…"), 0);
        assert_eq!(shown(&mut ui, "Fetch origin and start from origin/main"), 1);
        let branch = fixture.state.app.draft().unwrap().branch;
        assert_eq!(branch.base.as_deref(), Some("main"));
    }

    #[test]
    fn create_shows_creating_and_waits_for_git() {
        let mut fixture = worktree_sheet();

        let messages = click(&fixture.state, "new-session-create");
        assert_eq!(messages, [sheet(NewSessionMessage::Create)]);
        for message in messages {
            send(&mut fixture.state, message);
        }

        assert_eq!(count(&fixture.state, "creating…"), 1);
        assert_eq!(count(&fixture.state, "Create session"), 0);
        assert!(fixture.state.app.draft().unwrap().creating);
        let calls = fixture.git.calls().len();
        assert!(click(&fixture.state, "new-session-create").is_empty());
        send(&mut fixture.state, Message::Key(BareKey::Enter));
        assert_eq!(fixture.git.calls().len(), calls);
        assert_eq!(sessions_in_tree(&fixture.state), 0);

        let task = settle_git(&mut fixture.state, &fixture.git);

        assert!(task.units() > 0);
        assert_eq!(fixture.state.app.draft(), None);
        assert_eq!(fixture.state.tab, Tab::Session);
        let session = fixture.state.app.current_session().unwrap();
        assert_eq!(fixture.state.projects.cursor, Some(session));
        assert_eq!(sessions_in_tree(&fixture.state), 1);
        assert_eq!(fixture.state.error, None);
        assert_eq!(fixture.state.sheet_error, None);
    }

    #[test]
    fn the_sheet_stays_disabled_while_creating() {
        let mut fixture = fixture_with(two_backends());
        open_api_sheet(&mut fixture);
        answer(
            &mut fixture.state,
            &fixture.backend,
            vec![option(ConfigCategory::Model, &["sonnet", "opus"])],
        );
        let _task = settle_git(&mut fixture.state, &fixture.git);
        for message in [
            NewSessionMessage::Mode(BranchMode::Worktree),
            NewSessionMessage::Branch("feat/x".into()),
            NewSessionMessage::Create,
        ] {
            send(&mut fixture.state, sheet(message));
        }
        let draft = fixture.state.app.draft();

        let mut ui = simulator(app_view(&fixture.state));
        for id in [
            "new-session-mode-checked-out",
            "new-session-mode-new-branch",
            "new-session-fetch",
            "new-session-auto-allow",
            "new-session-base",
            "new-session-model",
            "new-session-backend",
            "new-session-branch",
        ] {
            ui.click(Id::new(id)).unwrap();
        }
        let _ = ui.typewrite("y");
        assert_eq!(ui.into_messages().count(), 0);
        // The pickers are frames that show the values picked.
        assert_eq!(count(&fixture.state, "sonnet"), 1);
        assert_eq!(count(&fixture.state, "claude-main"), 1);

        // A change that gets through anyway is refused by the core, in the sheet.
        send(&mut fixture.state, sheet(NewSessionMessage::Fetch(false)));
        assert!(fixture.state.sheet_error.is_some());
        assert_eq!(fixture.state.app.draft(), draft);
    }

    #[test]
    fn a_git_failure_on_create_shows_in_the_sheet() {
        let mut fixture = worktree_sheet();
        fixture.git.fail_next(GitOp::AddWorktree, &api(&fixture));

        send(&mut fixture.state, sheet(NewSessionMessage::Create));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        let draft = fixture.state.app.draft().unwrap();
        let failure = draft.failure.clone().unwrap();
        assert!(!draft.creating);
        assert_eq!(count(&fixture.state, &failure), 1);
        assert_eq!(count(&fixture.state, "Create session"), 1);
        assert_eq!(
            click(&fixture.state, "new-session-create"),
            [sheet(NewSessionMessage::Create)]
        );
        assert_eq!(sessions_in_tree(&fixture.state), 0);
        assert_eq!(fixture.state.tab, Tab::Projects);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn cancel_while_creating_closes_the_sheet_at_once() {
        let mut fixture = worktree_sheet();
        send(&mut fixture.state, sheet(NewSessionMessage::Create));
        // Git reads the repository again, and the worktree is queued.
        for event in fixture.git.answer() {
            send(&mut fixture.state, Message::Git(event));
        }
        assert!(fixture.state.app.draft().unwrap().creating);

        let messages = click(&fixture.state, "new-session-cancel");
        assert_eq!(messages, [sheet(NewSessionMessage::Cancel)]);
        for message in messages {
            send(&mut fixture.state, message);
        }
        assert_eq!(fixture.state.app.draft(), None);

        let _task = settle_git(&mut fixture.state, &fixture.git);
        assert_eq!(sessions_in_tree(&fixture.state), 0);
        assert_eq!(fixture.state.tab, Tab::Projects);
        assert_eq!(fixture.state.error, None);
        let calls = fixture.git.calls();
        assert!(
            matches!(
                calls.as_slice(),
                [
                    ..,
                    GitCall::AddWorktree { .. },
                    GitCall::RemoveWorktree { .. },
                    GitCall::DeleteBranch { .. }
                ]
            ),
            "{calls:?}"
        );
    }

    #[test]
    fn escape_while_git_reads_again_closes_the_sheet_and_leaves_nothing() {
        let mut fixture = worktree_sheet();
        send(&mut fixture.state, sheet(NewSessionMessage::Create));

        send(&mut fixture.state, Message::Key(BareKey::Escape));
        assert_eq!(fixture.state.app.draft(), None);
        let _task = settle_git(&mut fixture.state, &fixture.git);

        assert_eq!(sessions_in_tree(&fixture.state), 0);
        assert_eq!(fixture.state.error, None);
        assert!(
            fixture
                .git
                .calls()
                .iter()
                .all(|call| matches!(call, GitCall::Inspect(_))),
        );
    }

    #[test]
    fn the_new_controls_are_found_by_their_ids() {
        let mut fixture = worktree_sheet();
        {
            let mut ui = simulator(app_view(&fixture.state));
            for id in [
                "new-session-mode-worktree",
                "new-session-mode-new-branch",
                "new-session-mode-checked-out",
                "new-session-branch",
                "new-session-base",
                "new-session-fetch",
            ] {
                ui.find(Id::new(id)).unwrap();
            }
        }
        for (id, expected) in [
            (
                "new-session-mode-new-branch",
                NewSessionMessage::Mode(BranchMode::NewBranch),
            ),
            (
                "new-session-mode-checked-out",
                NewSessionMessage::Mode(BranchMode::CheckedOut),
            ),
            ("new-session-fetch", NewSessionMessage::Fetch(false)),
        ] {
            assert_eq!(click(&fixture.state, id), [sheet(expected)]);
        }

        // The base picker opens its menu above it, since the picker sits low in the window,
        // and the menu's first entry, one and a half picker heights up, is picked.
        let mut ui = simulator(app_view(&fixture.state));
        let picker = ui.find(Id::new("new-session-base")).unwrap().bounds();
        ui.point_at(picker.center());
        let _ = ui.simulate(iced_test::simulator::click());
        let first = Point::new(picker.x + 12.0, picker.y - picker.height * 1.5);
        ui.point_at(first);
        let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: first });
        let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [sheet(NewSessionMessage::Base("dev".into()))]);
        for message in messages {
            send(&mut fixture.state, message);
        }
        assert_eq!(
            fixture.state.app.draft().unwrap().branch.base.as_deref(),
            Some("dev")
        );
    }

    #[test]
    fn an_orchestrator_sheet_picks_the_childrens_branch_settings() {
        let mut fixture = fixture_in(single_claude(), &["api", "web"]);
        open_project_sheet(&mut fixture);
        let _task = settle_git(&mut fixture.state, &fixture.git);
        assert_eq!(
            count(
                &fixture.state,
                "children work in their repositories as they are now"
            ),
            1
        );

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Mode(BranchMode::Worktree)),
        );

        let mut ui = simulator(app_view(&fixture.state));
        for label in [
            "branch name",
            "Fetch first and start from each repository's remote base",
        ] {
            assert_eq!(shown(&mut ui, label), 1, "{label}");
        }
        assert_eq!(shown(&mut ui, "worktree at"), 0);
        let messages: Vec<Message> = {
            ui.click(Id::new("new-session-fetch")).unwrap();
            ui.into_messages().collect()
        };
        assert_eq!(messages, [sheet(NewSessionMessage::Fetch(false))]);
    }

    #[test]
    fn a_branch_change_the_core_refuses_shows_in_the_sheet() {
        let mut fixture = worktree_sheet();

        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Base("gone".into())),
        );

        let error = fixture.state.sheet_error.clone().unwrap();
        assert_eq!(count(&fixture.state, &error), 1);
        assert_eq!(fixture.state.error, None);
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
    fn a_create_that_waits_for_git_keeps_the_sheet_open() {
        let mut fixture = worktree_sheet();
        fixture.state.sheet_error = Some("an earlier error".into());

        send(&mut fixture.state, sheet(NewSessionMessage::Create));

        assert!(fixture.state.app.draft().unwrap().creating);
        assert_eq!(fixture.state.sheet_error, None);
        assert_eq!(fixture.state.error, None);
        assert_eq!(sessions_in_tree(&fixture.state), 0);
    }

    #[test]
    fn a_git_answer_for_another_request_leaves_the_sheet_alone() {
        let mut fixture = worktree_sheet();
        let task = app_update(
            &mut fixture.state,
            Message::Git(partitur_core::GitEvent::Answered {
                request: partitur_core::GitRequestId(999),
                answers: Vec::new(),
            }),
        );

        assert_eq!(task.units(), 0);
        assert!(fixture.state.app.draft().is_some());
        assert_eq!(fixture.state.tab, Tab::Projects);
    }

    #[test]
    fn a_failed_create_keeps_the_sheet_open_with_the_error() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);

        fixture.store.fail_next_session_save();
        send(&mut fixture.state, Message::Key(BareKey::Enter));

        assert!(fixture.state.app.draft().is_some());
        let error = fixture.state.sheet_error.clone().unwrap();
        {
            let mut ui = simulator(app_view(&fixture.state));
            assert_eq!(shown(&mut ui, &error), 1);
        }
        assert_eq!(fixture.state.error, None);
        assert_eq!(sessions_in_tree(&fixture.state), 0);

        send(&mut fixture.state, Message::Key(BareKey::Escape));
        assert_eq!(fixture.state.app.draft(), None);
        assert_eq!(fixture.state.sheet_error, None);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn an_error_from_elsewhere_stays_in_the_error_bar() {
        let mut fixture = fixture();
        let elsewhere = "another session failed";
        open_project_sheet(&mut fixture);

        send(&mut fixture.state, Message::Error(elsewhere.into()));

        {
            let mut ui = simulator(app_view(&fixture.state));
            assert_eq!(shown(&mut ui, elsewhere), 1);
        }
        send(&mut fixture.state, Message::Key(BareKey::Escape));
        assert_eq!(fixture.state.error.as_deref(), Some(elsewhere));

        open_project_sheet(&mut fixture);
        answer(&mut fixture.state, &fixture.backend, Vec::new());
        send(&mut fixture.state, Message::Key(BareKey::Enter));

        assert_eq!(sessions_in_tree(&fixture.state), 1);
        assert_eq!(fixture.state.error.as_deref(), Some(elsewhere));
    }

    #[test]
    fn a_create_whose_choices_cannot_be_sent_shows_the_session_and_the_error() {
        let mut fixture = fixture();
        open_project_sheet(&mut fixture);
        let warm = warm_session(&fixture.backend);
        answer(
            &mut fixture.state,
            &fixture.backend,
            vec![option(ConfigCategory::Effort, &["low", "high"])],
        );
        send(
            &mut fixture.state,
            sheet(NewSessionMessage::Effort(ConfigValueId::new("high"))),
        );

        fixture.backend.fail_next_call();
        let task = app_update(&mut fixture.state, Message::Key(BareKey::Enter));

        assert!(task.units() > 0);
        assert_eq!(fixture.state.app.draft(), None);
        assert_eq!(fixture.state.tab, Tab::Session);
        assert_eq!(fixture.state.app.current_session(), Some(warm));
        assert!(fixture.state.error.is_some());
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
