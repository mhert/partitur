//! The Env tab: what shaped the selected session.
//!
//! Under the session's breadcrumb row sit cards of labelled values. Two share a row:
//! `Instructions` lists the `AGENTS.md` files that apply to the session (each opens in the
//! desktop's default application when clicked, and tells when it was not sent to the agent)
//! and, for a session an orchestrator started, how the orchestrator's first prompt begins;
//! `Backend` names the backend, its command, the folder it runs in, the model and the
//! reasoning effort. Below them `Context` spans the window: how much of its context the
//! session has used, as counts and as a bar, once the agent has reported it. An archived
//! session has a banner under the breadcrumb that says when it was archived and restores it.

use std::io;
use std::path::{Path, PathBuf};

use iced::widget::{Column, Row, Space, Text, button, column, container, row, scrollable, stack};
use iced::{Alignment, Border, Element, Length, Pixels, Task, Theme};
use partitur_core::{EnvViewModel, InstructionFileView, InstructionScope, SessionId};

use crate::app::Message;
use crate::ellipsis::{keep_end, keep_start};
use crate::session_view::usage_fill;
use crate::theme::Tokens;
use crate::{breadcrumb, home_path, layout, style, theme, tokens_label};

/// The space between the window's edge and the cards, left and right.
const MARGIN_X: f32 = 40.0;

/// The space above the first card and below the last.
const MARGIN_Y: f32 = 24.0;

/// The space between two cards, side by side or one below the other.
const CARD_GAP: f32 = 16.0;

/// The room a card keeps around its content: above and below, then left and right.
const CARD_PADDING: [f32; 2] = [14.0, 16.0];

/// The space between a card's heading and the rows under it.
const HEADING_GAP: f32 = 8.0;

/// The space between the heading, the bar and whatever follows in the `Context` card, whose
/// parts are blocks rather than lines of text.
const CONTEXT_GAP: f32 = 10.0;

/// The space between two rows of a card.
const ROW_GAP: f32 = 4.0;

/// The width of the column the labels sit in, so every value of a card starts at the same
/// place.
const LABEL_WIDTH: f32 = 120.0;

/// The space between a label and its value.
const LABEL_GAP: f32 = 12.0;

/// The height of the bar that shows the context usage, in logical pixels.
const BAR_HEIGHT: f32 = 8.0;

/// The parts the context bar is divided into: the filled share is counted in them, so the
/// bar is exact to a thousandth of its width, whatever that is.
const BAR_PARTS: u16 = 1000;

/// The most characters of the orchestrator's first prompt its row shows: what one line of
/// the card holds in a window of the usual width.
const BRIEF_BUDGET: usize = 48;

/// The most characters of the backend's command and of the session's folder their rows
/// show, which is what fits the value column of a card in a window of the usual width.
const PATH_BUDGET: usize = 44;

/// The id of the context bar, which has no text to find it by.
const USAGE_BAR: &str = "context-usage";

/// An interaction on the Env tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvMessage {
    /// The user clicked an instruction file: it opens in the desktop's default application.
    Open(PathBuf),
}

/// The Env tab for the selected `session`, whose environment is `model`, with the banner
/// that restores it under the breadcrumb when it is archived.
///
/// The element owns what it shows, so it does not borrow `model`, which the caller builds
/// afresh for every frame. A model or effort the backend does not offer reads "default".
pub fn view<'a>(session: &SessionId, model: &EnvViewModel) -> Element<'a, Message> {
    let home = std::env::home_dir();
    let remark = [
        Text::new("·").size(theme::SMALL).style(style::dim).into(),
        Text::new("what shaped this session")
            .size(theme::SMALL)
            .style(style::muted)
            .into(),
    ];
    let cards = column![
        side_by_side(instructions(model), backend(model, home.as_deref())),
        context(model.usage),
    ]
    .spacing(CARD_GAP)
    .padding([MARGIN_Y, MARGIN_X]);
    let mut page = column![breadcrumb::breadcrumb(&model.breadcrumb, remark)];
    if let Some(archived) = model.archived {
        page = page.push(layout::archived_banner(session, archived));
    }
    page.push(scrollable(cards).width(Length::Fill).height(Length::Fill))
        .into()
}

/// Applies `message` to the Env tab and returns the follow-up work: a clicked file is
/// handed to the desktop's launcher, which opens it in its default application, and the
/// error bar tells when the launcher cannot be started.
pub fn update(message: EnvMessage) -> Task<Message> {
    match message {
        // The launcher is started, not waited for: it may run as long as the application
        // it opens.
        EnvMessage::Open(path) => match open::that_detached(&path) {
            Ok(()) => Task::none(),
            Err(error) => Task::done(Message::Error(open_failure(&path, &error))),
        },
    }
}

/// What the error bar says when `path` could not be opened because of `error`.
fn open_failure(path: &Path, error: &io::Error) -> String {
    format!("could not open {}: {error}", path.display())
}

/// Two cards in one row, equally wide and both as high as the higher one.
///
/// A row sizes each of its parts by itself, so the cards' surfaces are drawn under the
/// contents instead of around them: the contents give the row its height, and each surface
/// fills its half of that.
fn side_by_side<'a>(left: Column<'a, Message>, right: Column<'a, Message>) -> Element<'a, Message> {
    let surface = || {
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::card)
    };
    let contents = row![left.width(Length::Fill), right.width(Length::Fill)].spacing(CARD_GAP);
    let surfaces = row![surface(), surface()]
        .spacing(CARD_GAP)
        .height(Length::Fill);
    stack![contents]
        .push_under(surfaces)
        .width(Length::Fill)
        .into()
}

/// A card's content: `heading` over `body`, with the room a card keeps around them.
fn card<'a>(
    heading: impl Into<Element<'a, Message>>,
    body: impl Into<Element<'a, Message>>,
) -> Column<'a, Message> {
    column![heading.into(), body.into()]
        .spacing(HEADING_GAP)
        .padding(CARD_PADDING)
}

/// A card's heading.
fn heading(title: &str) -> Text<'_> {
    Text::new(title)
        .size(theme::HEADING)
        .font(theme::UI_SEMIBOLD)
        .line_height(Pixels(theme::LINE_HEIGHT))
}

/// One row of a card: `label` in its column, then `value`.
fn field<'a>(label: &'a str, value: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![
        prose(label).width(LABEL_WIDTH).style(style::muted),
        value.into()
    ]
    .spacing(LABEL_GAP)
    .into()
}

/// A line of text in the interface font, as high as every line of a card.
fn prose<'a>(words: impl iced::widget::text::IntoFragment<'a>) -> Text<'a> {
    Text::new(words).line_height(Pixels(theme::LINE_HEIGHT))
}

/// A path, file name, command or count: monospaced and small, on a line as high as the
/// label beside it.
fn mono<'a>(words: impl iced::widget::text::IntoFragment<'a>) -> Text<'a> {
    prose(words).font(theme::MONO).size(theme::SMALL)
}

/// The `Instructions` card: a row per instruction file, or a note that none applies, and
/// for a session an orchestrator started how the orchestrator's first prompt begins.
fn instructions<'a>(model: &EnvViewModel) -> Column<'a, Message> {
    let mut rows = Column::new().spacing(ROW_GAP);
    if model.instruction_files.is_empty() {
        rows = rows.push(prose("No AGENTS.md applies to this session").style(style::muted));
    }
    for file in &model.instruction_files {
        rows = rows.push(instruction(file));
    }
    if let Some(brief) = &model.orchestrator_brief {
        rows = rows.push(field(
            "from orchestrator",
            prose(brief_beginning(brief)).style(style::soft),
        ));
    }
    card(heading("Instructions"), rows)
}

/// The row of one instruction file: where it applies as the label, then the file as a
/// button that opens it, then its state unless it was simply sent to the agent.
fn instruction<'a>(file: &InstructionFileView) -> Element<'a, Message> {
    let scope = match file.scope {
        InstructionScope::Global => "global",
        InstructionScope::Repository => "repo",
    };
    let open = button(mono(file_label(&file.path)))
        .padding(0)
        .style(style::soft_link_button)
        .on_press(Message::Env(EnvMessage::Open(file.path.clone())));
    let mut value = Row::new().spacing(8).push(open);
    if let Some(reason) = &file.skipped_reason {
        value = value
            .push(state(format!("skipped: {reason}")).style(style::ink(|tokens| tokens.failed.fg)));
    } else if !file.injected {
        value = value.push(state("not injected yet").style(style::muted));
    }
    field(scope, value)
}

/// What is to tell about an instruction file beside its name: small, and wrapping in the
/// room the name leaves, since the reason a file was skipped can be long.
fn state<'a>(words: impl iced::widget::text::IntoFragment<'a>) -> Text<'a> {
    prose(words).size(theme::SMALL).width(Length::Fill)
}

/// How the interface names the file at `path`: its folder and its name, which tells the
/// project's file from a repository's without the whole path.
fn file_label(path: &Path) -> String {
    let folder = path.parent().and_then(Path::file_name);
    folder
        .into_iter()
        .chain(path.file_name())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// How the orchestrator's first prompt `brief` begins, on one line: the row is a reminder
/// of the prompt, which the session's transcript holds in full.
fn brief_beginning(brief: &str) -> String {
    let line = brief.split_whitespace().collect::<Vec<_>>().join(" ");
    keep_start(&line, BRIEF_BUDGET)
}

/// The `Backend` card: the backend's id and command, the folder the session runs in with
/// the `home` directory shortened, the model and the reasoning effort.
fn backend<'a>(model: &EnvViewModel, home: Option<&Path>) -> Column<'a, Message> {
    let command: Element<'a, Message> = match &model.command {
        Some(command) => mono(keep_start(command, PATH_BUDGET))
            .style(style::soft)
            .into(),
        None => prose("not configured").style(style::muted).into(),
    };
    let folder = keep_end(&home_path::abbreviated(&model.cwd, home), PATH_BUDGET);
    let rows = column![
        field("backend", prose(model.backend.clone()).style(style::soft)),
        field("command", command),
        field("folder", mono(folder).style(style::soft)),
        field(
            "model",
            prose(or_default(model.model.as_ref())).style(style::soft)
        ),
        field(
            "effort",
            prose(or_default(model.effort.as_ref())).style(style::soft)
        ),
    ]
    .spacing(ROW_GAP);
    card(heading("Backend"), rows)
}

/// The current choice's name, or "default" when the backend offers no such option.
fn or_default(name: Option<&String>) -> String {
    name.map_or_else(|| "default".to_owned(), Clone::clone)
}

/// The `Context` card across the window: the heading with the used and the available
/// tokens of `usage` beside it, over a bar filled with the used share. Before the agent
/// reported a usage there is no bar, and the heading says so.
fn context<'a>(usage: Option<(u64, u64)>) -> Element<'a, Message> {
    let title = |remark: Text<'a>| {
        row![heading("Context"), remark.style(style::muted)]
            .spacing(12)
            .align_y(Alignment::Center)
    };
    let content = match usage {
        Some((used, size)) => column![
            title(mono(tokens_label::usage(used, size))),
            usage_bar(used, size)
        ],
        None => column![title(prose("not reported yet").size(theme::SMALL))],
    };
    container(content.spacing(CONTEXT_GAP).padding(CARD_PADDING))
        .width(Length::Fill)
        .style(style::card)
        .into()
}

/// The bar across the `Context` card, filled from the left as far as `used` of `size`
/// tokens reach.
fn usage_bar<'a>(used: u64, size: u64) -> Element<'a, Message> {
    let part = |parts: u16, pick: fn(&Tokens) -> iced::Color| {
        container(Space::new())
            .width(Length::FillPortion(parts))
            .height(BAR_HEIGHT)
            .style(move |theme: &Theme| container::Style {
                background: Some(pick(theme::tokens(theme)).into()),
                border: Border::default().rounded(BAR_HEIGHT / 2.0),
                ..container::Style::default()
            })
    };
    let filled = usage_fill(used, size, BAR_PARTS);
    // The track lies under the whole bar, so its rounded ends show whatever is filled.
    let bar = stack![
        part(BAR_PARTS, |tokens| tokens.track),
        row![
            part(filled, |tokens| tokens.muted_strong),
            Space::new().width(Length::FillPortion(BAR_PARTS.saturating_sub(filled)))
        ]
    ]
    .width(Length::Fill);
    container(bar).id(USAGE_BAR).into()
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
    use iced::widget::Id;
    use iced_test::simulator;
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, BackendSessionId, ConfigCategory, ConfigChoice, ConfigId, ConfigOption,
        ConfigValueId, Scope, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{State, Tab, boot, update, view};
    use crate::projects_view::ProjectsMessage;
    use crate::test_support::{deliver, orchestrated, state_on_removed_backend, two_backends};

    /// A project folder with an `AGENTS.md` (holding `global`) and the repository `api` with
    /// its own `AGENTS.md`.
    fn project_dir(global: &[u8]) -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("AGENTS.md"), global).unwrap();
        fs::write(dir.path().join("api").join("AGENTS.md"), "api only").unwrap();
        dir
    }

    /// A state on the Env tab whose current session runs in the repository `api` of `dir`
    /// and was created by the backend.
    fn repository_session(dir: &TempDir) -> State {
        let backend = RecordingBackend::new();
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(MemoryStore::new()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let repository = state.app.tree().projects[0].repositories[0].id.clone();
        let session = state
            .app
            .new_session(project, Scope::Repository(repository))
            .unwrap();
        let request = backend
            .calls()
            .iter()
            .find_map(Call::as_new_session)
            .map(|(request, ..)| request)
            .unwrap();
        let created = BackendEvent::SessionCreated {
            request,
            session,
            backend_session: BackendSessionId::new("b-1"),
            config: Vec::new(),
        };
        let _task = update(&mut state, Message::Backend(created));
        state.tab = Tab::Env;
        assert_eq!(state.error, None);
        state
    }

    /// Reports `update` for the current session of `state`, as its backend would.
    fn report(state: &mut State, update: Update) {
        let session = state.app.current_session().unwrap();
        deliver(state, BackendEvent::Update { session, update });
    }

    /// The name of the folder `dir`, as the label of a file in it starts.
    fn folder_name(dir: &TempDir) -> String {
        dir.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// An environment with nothing to tell: no instruction files, the backend `claude`
    /// without options, the folder `/work`, no usage and no orchestrator.
    fn bare_model() -> EnvViewModel {
        EnvViewModel {
            breadcrumb: vec!["work".into(), "New session".into()],
            instruction_files: Vec::new(),
            backend: "claude".into(),
            command: Some("claude-agent-acp".into()),
            model: None,
            effort: None,
            cwd: PathBuf::from("/work"),
            usage: None,
            orchestrator_brief: None,
            archived: None,
        }
    }

    #[test]
    fn the_breadcrumb_row_says_what_the_tab_is_about() {
        let dir = project_dir(b"global rules");
        let state = repository_session(&dir);

        let mut ui = simulator(view(&state));

        ui.find(folder_name(&dir).as_str()).unwrap();
        ui.find("New session").unwrap();
        ui.find("what shaped this session").unwrap();
        for card in ["Instructions", "Backend", "Context"] {
            ui.find(card).unwrap();
        }
    }

    #[test]
    fn the_env_view_lists_instruction_files_with_their_state() {
        let dir = project_dir(b"global rules");
        let mut state = repository_session(&dir);
        let global = format!("{}/AGENTS.md", folder_name(&dir));

        let mut ui = simulator(view(&state));
        ui.find("global").unwrap();
        ui.find(global.as_str()).unwrap();
        ui.find("repo").unwrap();
        ui.find("api/AGENTS.md").unwrap();
        ui.find("not injected yet").unwrap();
        assert!(ui.find("No AGENTS.md applies to this session").is_err());
        drop(ui);

        state.app.submit_prompt("Hello", Vec::new()).unwrap();

        // A file that was sent to the agent needs no remark.
        let mut ui = simulator(view(&state));
        ui.find(global.as_str()).unwrap();
        ui.find("api/AGENTS.md").unwrap();
        assert!(ui.find("not injected yet").is_err());
    }

    #[test]
    fn a_file_is_named_by_its_folder_and_its_name() {
        assert_eq!(
            file_label(Path::new("/home/me/shop-app/backend/AGENTS.md")),
            "backend/AGENTS.md"
        );
        assert_eq!(file_label(Path::new("/AGENTS.md")), "AGENTS.md");
        assert_eq!(file_label(Path::new("AGENTS.md")), "AGENTS.md");
    }

    #[test]
    fn an_unreadable_instruction_file_shows_why_it_was_skipped() {
        let dir = project_dir(&[0xff, 0xfe, 0xfd]);
        let state = repository_session(&dir);
        let model = state.app.env_view().unwrap();
        let skipped = model
            .instruction_files
            .iter()
            .find_map(|file| file.skipped_reason.clone())
            .unwrap();

        let mut ui = simulator(view(&state));

        ui.find(format!("skipped: {skipped}").as_str()).unwrap();
        ui.find(format!("{}/AGENTS.md", folder_name(&dir)).as_str())
            .unwrap();
        // The readable file of the repository still waits for the first prompt.
        ui.find("api/AGENTS.md").unwrap();
        ui.find("not injected yet").unwrap();
    }

    #[test]
    fn clicking_an_instruction_file_asks_to_open_it() {
        let dir = project_dir(b"global rules");
        let state = repository_session(&dir);
        let path = dir.path().join("api").join("AGENTS.md");

        let mut ui = simulator(view(&state));
        ui.click("api/AGENTS.md").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::Env(EnvMessage::Open(path))]);
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_named_with_the_reason() {
        let error = io::Error::new(io::ErrorKind::NotFound, "no launcher");

        assert_eq!(
            open_failure(Path::new("/work/AGENTS.md"), &error),
            "could not open /work/AGENTS.md: no launcher"
        );
    }

    #[test]
    fn without_instruction_files_the_env_view_says_so() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        let state = repository_session(&dir);

        let mut ui = simulator(view(&state));

        ui.find("No AGENTS.md applies to this session").unwrap();
        assert!(ui.find("global").is_err());
        assert!(ui.find("repo").is_err());
        assert!(ui.find("from orchestrator").is_err());
    }

    #[test]
    fn a_session_an_orchestrator_started_shows_how_its_brief_begins() {
        let mut fixture = orchestrated();
        let state = &mut fixture.state;
        state.tab = Tab::Env;

        // The orchestrator itself was prompted by nobody but the user.
        let mut ui = simulator(view(state));
        assert!(ui.find("from orchestrator").is_err());
        drop(ui);

        let select = ProjectsMessage::Select(fixture.child.clone());
        let _task = update(state, Message::Projects(select));
        state.tab = Tab::Env;

        let mut ui = simulator(view(state));
        ui.find("from orchestrator").unwrap();
        ui.find("Add the endpoint").unwrap();
    }

    #[test]
    fn a_long_brief_is_cut_to_one_line() {
        let brief = format!("Add the\n\nendpoint.  {}", "é".repeat(BRIEF_BUDGET));

        let beginning = brief_beginning(&brief);

        // On one line the words before the filler are "Add the endpoint. ": 18 characters.
        let kept = "é".repeat(BRIEF_BUDGET - 18);
        assert_eq!(beginning, format!("Add the endpoint. {kept}…"));
        assert_eq!(brief_beginning("Add the\nendpoint"), "Add the endpoint");
    }

    #[test]
    fn the_backend_card_names_the_backend_its_command_and_the_folder() {
        let dir = project_dir(b"global rules");
        let state = repository_session(&dir);
        let home = std::env::home_dir();
        let folder = home_path::abbreviated(&dir.path().join("api"), home.as_deref());

        let mut ui = simulator(view(&state));

        for label in ["backend", "command", "folder", "model", "effort"] {
            ui.find(label).unwrap();
        }
        ui.find("claude").unwrap();
        ui.find("claude-agent-acp").unwrap();
        ui.find(keep_end(&folder, PATH_BUDGET).as_str()).unwrap();
        // Without options from the backend, model and effort are its defaults.
        ui.find("default").unwrap();
    }

    #[test]
    fn the_backend_card_names_the_model_and_the_effort_in_effect() {
        let dir = project_dir(b"global rules");
        let mut state = repository_session(&dir);
        let picker = |id: &str, category, current: &str| ConfigOption {
            id: ConfigId::new(id),
            name: id.into(),
            category,
            current: ConfigValueId::new(current),
            choices: vec![ConfigChoice {
                id: ConfigValueId::new(current),
                name: current.to_uppercase(),
            }],
        };
        report(
            &mut state,
            Update::ConfigOptions(vec![
                picker("model", ConfigCategory::Model, "opus"),
                picker("effort", ConfigCategory::Effort, "high"),
            ]),
        );

        let mut ui = simulator(view(&state));

        ui.find("OPUS").unwrap();
        ui.find("HIGH").unwrap();
        assert!(ui.find("default").is_err());
    }

    #[test]
    fn the_folder_is_written_from_the_home_directory_and_keeps_its_end() {
        let mut model = bare_model();
        model.cwd = PathBuf::from("/home/me/shop");
        let near: Element<'_, Message> = backend(&model, Some(Path::new("/home/me"))).into();
        let mut ui = simulator(near);
        ui.find("~/shop").unwrap();
        drop(ui);

        model.cwd = PathBuf::from(format!("/home/me/{}/shop", "d".repeat(PATH_BUDGET)));
        model.command = Some(format!("agent {}", "x".repeat(PATH_BUDGET)));
        let far: Element<'_, Message> = backend(&model, Some(Path::new("/home/me"))).into();
        let mut ui = simulator(far);
        let kept = "d".repeat(PATH_BUDGET - "/shop".len());
        ui.find(format!("…{kept}/shop").as_str()).unwrap();
        // A command is read from its start, so that is what a long one keeps.
        let kept = "x".repeat(PATH_BUDGET - "agent ".len());
        ui.find(format!("agent {kept}…").as_str()).unwrap();
    }

    #[test]
    fn the_env_tab_shows_the_backend_command_or_not_configured() {
        let dir = TempDir::new().unwrap();
        let (backends, default) = two_backends();
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(MemoryStore::new()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        state.app.new_session(project, Scope::Project).unwrap();
        state.tab = Tab::Env;

        let mut ui = simulator(view(&state));
        ui.find("claude-main").unwrap();
        ui.find("claude-main-acp").unwrap();
        assert!(ui.find("not configured").is_err());
        drop(ui);

        let mut state = state_on_removed_backend(&dir);
        state.tab = Tab::Env;
        let mut ui = simulator(view(&state));
        ui.find("gone").unwrap();
        ui.find("not configured").unwrap();
        assert!(ui.find("gone-acp").is_err());
    }

    #[test]
    fn the_context_card_shows_the_usage_once_the_agent_reported_it() {
        let dir = project_dir(b"global rules");
        let mut state = repository_session(&dir);

        let mut ui = simulator(view(&state));
        ui.find("Context").unwrap();
        ui.find("not reported yet").unwrap();
        assert!(ui.find(Id::new(USAGE_BAR)).is_err());
        drop(ui);

        report(
            &mut state,
            Update::Usage {
                used: 41_260,
                size: 200_000,
            },
        );

        let mut ui = simulator(view(&state));
        ui.find("Context").unwrap();
        ui.find("41k / 200k").unwrap();
        ui.find(Id::new(USAGE_BAR)).unwrap();
        assert!(ui.find("not reported yet").is_err());
    }

    #[test]
    fn a_usage_without_a_context_size_leaves_the_bar_empty() {
        let mut model = bare_model();
        model.usage = Some((1200, 0));

        let mut ui = simulator(context(model.usage));

        ui.find("1k / 0").unwrap();
        ui.find(Id::new(USAGE_BAR)).unwrap();
    }

    #[test]
    fn the_env_tab_shows_the_banner_for_an_archived_session() {
        let dir = project_dir(b"global rules");
        let mut state = repository_session(&dir);
        let session = state.app.current_session().unwrap();
        {
            let mut ui = simulator(view(&state));
            assert!(ui.find(Id::new("banner-restore")).is_err());
        }
        state.app.archive(session.clone()).unwrap();
        let mark = state.app.env_view().unwrap().archived.unwrap();

        let mut ui = simulator(view(&state));
        ui.find(format!("Archived {}", partitur_core::day_month(mark.at)).as_str())
            .unwrap();
        ui.click(Id::new("banner-restore")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Projects(ProjectsMessage::Restore(session))]
        );
    }
}
