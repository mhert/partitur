//! The card of a question the agent asks through the `ask_user` tool.
//!
//! An open question is a card in the transcript where it was asked: the header `Question`,
//! the question's text, then one row per option with the key that picks it (`1` to `6`), its
//! label and, when it has one, its description. Under the options sits the `Other` field for
//! an answer in the user's own words, with `Send` beside it, and `Cancel`. Clicking an option,
//! or its digit with the cursor in no field, sends the option's label as the user's next
//! prompt; `Send`, or Enter in the field, sends the trimmed text of the field, and a blank
//! one sends nothing. `Cancel`, or Escape on the Session tab whatever has the focus, closes
//! the question as cancelled, sends nothing and puts the cursor into the prompt box. While
//! the session is in its turn the options and `Send` take no click and the digits do
//! nothing, since the core takes no prompt meanwhile; `Cancel` stays available.
//!
//! A closed question stays in the transcript as a dimmed card with its text and how it was
//! closed: `answered: <text>`, `answered by orchestrator: <text>` when the session's
//! orchestrator answered for the user, `cancelled` or `replaced by a newer question`.

use iced::widget::{Column, button, column, container, operation, row, space, text, text_input};
use iced::{Alignment, Border, Element, Length, Task, Theme};
use partitur_core::{
    Decider, Entry, Question, QuestionId, QuestionOutcome, SessionId, SessionViewModel,
};

use super::entries::LINE;
use super::proposal::field;
use super::{PROMPT, SessionMessage, to_the_end};
use crate::app::{self, Message, State};
use crate::{badge, style, theme};

/// The keys that pick the options, in order.
const KEYS: [&str; 6] = ["1", "2", "3", "4", "5", "6"];

/// The placeholder of the field for an answer of the user's own.
const OTHER: &str = "type your own answer…";

/// What the card's header says about its options.
const HINT: &str = "pick an option, or answer in your own words";

/// How a closed card says that the user answered, before the answer when there is text.
const ANSWERED: &str = "answered";

/// What a closed card puts after [`ANSWERED`] when the orchestrator answered.
const BY_ORCHESTRATOR: &str = "by orchestrator";

/// How a closed card says that the user dismissed the question.
const CANCELLED: &str = "cancelled";

/// How a closed card says that a newer question took its place.
const SUPERSEDED: &str = "replaced by a newer question";

/// The space inside the card's sections.
const PADDING: [u16; 2] = [10, 14];

/// An interaction on the card of an open question.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestionMessage {
    /// The user picked the option at this position.
    Pick(usize),
    /// The user edited the answer in the `Other` field.
    EditOther(String),
    /// The user sent the answer in the `Other` field.
    SendOther,
    /// The user dismissed the question.
    Cancel,
}

/// Applies `message` to the card of the current session's question `id` and returns the
/// follow-up work; a message for a question that is not the current session's open one is
/// ignored.
///
/// An answer goes to the core [`App`] as the session's next prompt and scrolls the transcript
/// to it; Cancel closes the question there and, once it has, puts the cursor into the prompt
/// box. When the app refuses, the error bar shows why, the card stays as it is and the focus
/// stays where it was.
pub(crate) fn update(state: &mut State, id: QuestionId, message: QuestionMessage) -> Task<Message> {
    let Some(session) = state.app.current_session() else {
        return Task::none();
    };
    if state.app.current_question() != Some(&id) {
        return Task::none();
    }
    let key = (session.clone(), id);
    match message {
        QuestionMessage::Pick(index) => {
            let label = state.app.session_view().and_then(|model| {
                open_question(&model, &key.1)
                    .and_then(|question| question.options.get(index))
                    .map(|option| option.label.clone())
            });
            match label {
                Some(label) => answer(state, &key, &label),
                None => Task::none(),
            }
        }
        QuestionMessage::EditOther(other) => {
            state.session.question_drafts.insert(key, other);
            Task::none()
        }
        QuestionMessage::SendOther => {
            let other = state
                .session
                .question_drafts
                .get(&key)
                .map(|other| other.trim().to_owned())
                .unwrap_or_default();
            if other.is_empty() {
                return Task::none();
            }
            answer(state, &key, &other)
        }
        QuestionMessage::Cancel => {
            let cancelled = state.app.cancel_question(session);
            if app::report(&mut state.error, cancelled).is_none() {
                return Task::none();
            }
            state.session.question_drafts.remove(&key);
            operation::focus(PROMPT)
        }
    }
}

/// Answers the question `key` with `answer`; once the app took it, the card's draft goes and
/// the transcript scrolls to the prompt the answer became.
fn answer(state: &mut State, key: &(SessionId, QuestionId), answer: &str) -> Task<Message> {
    let answered = state.app.answer_question(key.0.clone(), answer);
    if app::report(&mut state.error, answered).is_none() {
        return Task::none();
    }
    state.session.question_drafts.remove(key);
    to_the_end(&mut state.session)
}

/// The question `id` of the session `model` while it is open.
fn open_question<'m>(model: &'m SessionViewModel<'_>, id: &QuestionId) -> Option<&'m Question> {
    model.entries.iter().find_map(|entry| match entry {
        Entry::Question {
            id: asked,
            question,
            outcome: None,
            ..
        } if asked == id => Some(question),
        _ => None,
    })
}

/// What a digit pressed with the cursor in no field does on the Session tab: it picks the
/// option of that number on the current session's open question, as a click on it does. It
/// does nothing without an open question, without such an option and while the session is
/// in its turn.
pub(crate) fn press_digit(state: &mut State, digit: u8) -> Task<Message> {
    let Some(id) = state.app.current_question().cloned() else {
        return Task::none();
    };
    let Some(index) = usize::from(digit).checked_sub(1) else {
        return Task::none();
    };
    let offered = state.app.session_view().is_some_and(|model| {
        !model.running()
            && open_question(&model, &id).is_some_and(|question| index < question.options.len())
    });
    if !offered {
        return Task::none();
    }
    update(state, id, QuestionMessage::Pick(index))
}

/// What Escape does on the Session tab while the current session has a question open: it
/// cancels the question and puts the cursor into the prompt box, as `Cancel` does.
pub(crate) fn press_escape(state: &mut State) -> Task<Message> {
    match state.app.current_question().cloned() {
        Some(id) => update(state, id, QuestionMessage::Cancel),
        None => Task::none(),
    }
}

/// The card of the open question `id` of the session `model`, asking `question`, with
/// `other` in its `Other` field.
pub(crate) fn card<'a>(
    model: &SessionViewModel<'_>,
    id: &QuestionId,
    question: &'a Question,
    other: &str,
) -> Element<'a, Message> {
    let running = model.running();
    let header = row![
        text("Question")
            .size(theme::HEADING)
            .font(theme::UI_SEMIBOLD),
        space::horizontal(),
        text(HINT).size(theme::SMALL).style(style::muted),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let mut card = column![
        container(header).padding(PADDING),
        style::hairline(|tokens| tokens.border),
        container(text(question.text.as_str()).line_height(LINE)).padding(PADDING),
    ];
    for (index, option) in question.options.iter().enumerate() {
        let pick = (!running).then(|| message(id, QuestionMessage::Pick(index)));
        card = card
            .push(style::hairline(|tokens| tokens.border_soft))
            .push(
                container(option_row(
                    index,
                    &option.label,
                    option.description.as_deref(),
                    pick,
                ))
                .id(widget(id, &format!("option-{index}"))),
            );
    }
    card = card
        .push(style::hairline(|tokens| tokens.border_soft))
        .push(container(answer_row(id, other, running)).padding(PADDING));
    container(card)
        .padding(1)
        .width(Length::Fill)
        .clip(true)
        .style(|theme: &Theme| {
            let tokens = theme::tokens(theme);
            container::Style {
                background: Some(tokens.panel.into()),
                border: Border::default()
                    .rounded(style::CARD_RADIUS)
                    .width(1)
                    .color(tokens.border_split),
                ..container::Style::default()
            }
        })
        .into()
}

/// The row of the option at `index`: its key, its `label` and its `description`, all
/// wrapping. A click sends `on_press`; without it the row is muted and takes no click.
fn option_row<'a>(
    index: usize,
    label: &'a str,
    description: Option<&'a str>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    let mut words = Column::new().spacing(2).width(Length::Fill);
    words = words.push(text(label).font(theme::UI_MEDIUM).line_height(LINE).style(
        move |theme: &Theme| {
            if enabled {
                style::ink(|tokens| tokens.text)(theme)
            } else {
                style::muted(theme)
            }
        },
    ));
    if let Some(description) = description {
        words = words.push(text(description).line_height(LINE).style(style::soft));
    }
    let mut line = row![].spacing(12).align_y(Alignment::Start);
    if let Some(key) = KEYS.get(index) {
        line = line.push(container(badge::key_cap(*key)).padding([2, 0]));
    }
    button(line.push(words))
        .padding(PADDING)
        .width(Length::Fill)
        .style(|theme: &Theme, status| {
            let tokens = theme::tokens(theme);
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then(|| tokens.border_soft.into()),
                text_color: tokens.text,
                ..button::Style::default()
            }
        })
        .on_press_maybe(on_press)
        .into()
}

/// The last row of the card: the `Other` field holding `other` with `Send`, and `Cancel`.
/// While the session is `running`, Send and Enter in the field send nothing.
fn answer_row<'a>(id: &QuestionId, other: &str, running: bool) -> Element<'a, Message> {
    let on_input = id.clone();
    let send =
        (!running && !other.trim().is_empty()).then(|| message(id, QuestionMessage::SendOther));
    let field = text_input(OTHER, other)
        .id(widget(id, "other"))
        .on_input(move |other| message(&on_input, QuestionMessage::EditOther(other)))
        .on_submit_maybe((!running).then(|| message(id, QuestionMessage::SendOther)))
        .padding([7, 10])
        .style(field);
    let send = button(text("Send"))
        .padding([7, 14])
        .style(style::outlined_button)
        .on_press_maybe(send);
    let cancel = button(text("Cancel"))
        .padding([7, 14])
        .style(style::outlined_button)
        .on_press(message(id, QuestionMessage::Cancel));
    row![
        text("Other").font(theme::UI_MEDIUM).style(style::muted),
        field,
        container(send).id(widget(id, "send")),
        cancel,
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

/// The dimmed card of a closed `question`: its header with how it was closed, `outcome`, and
/// by whom, `decider`, when the orchestrator answered it, and the question's text. It takes
/// no input.
pub(crate) fn closed<'a>(
    question: &'a Question,
    outcome: &'a QuestionOutcome,
    decider: Decider,
) -> Element<'a, Message> {
    let answered = match decider {
        Decider::User => ANSWERED.to_owned(),
        Decider::Orchestrator => format!("{ANSWERED} {BY_ORCHESTRATOR}"),
    };
    let how = match outcome {
        QuestionOutcome::Answered(answer) if answer.trim().is_empty() => answered,
        QuestionOutcome::Answered(answer) => format!("{answered}: {answer}"),
        QuestionOutcome::Cancelled => CANCELLED.to_owned(),
        QuestionOutcome::Superseded => SUPERSEDED.to_owned(),
    };
    let header = row![
        text("Question")
            .size(theme::SMALL)
            .font(theme::UI_SEMIBOLD)
            .style(style::muted),
        text(how).size(theme::SMALL).style(style::soft),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let content = column![
        header,
        text(question.text.as_str())
            .line_height(LINE)
            .style(style::muted),
    ]
    .spacing(4);
    container(content)
        .padding(PADDING)
        .width(Length::Fill)
        .style(|theme: &Theme| {
            let tokens = theme::tokens(theme);
            container::Style {
                border: Border::default()
                    .rounded(style::CARD_RADIUS)
                    .width(1)
                    .color(tokens.border_soft),
                ..container::Style::default()
            }
        })
        .into()
}

/// The id of the widget `name` on the card of the question `id`.
fn widget(id: &QuestionId, name: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("question-{id}-{name}"))
}

/// `message` for the card of the question `id`.
fn message(id: &QuestionId, message: QuestionMessage) -> Message {
    Message::Session(SessionMessage::Question(id.clone(), message))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use iced::keyboard::{self, Key, Modifiers, key};
    use iced::widget::Id;
    use iced::{Event, event, window};
    use iced_test::simulator;
    use partitur_core::testing::Call;
    use partitur_core::{
        Decider, Entry, PartiturTool, PromptContent, Question, QuestionId, QuestionOption,
        QuestionOutcome,
    };

    use super::{ANSWERED, BY_ORCHESTRATOR, CANCELLED, OTHER, QuestionMessage, SUPERSEDED, closed};
    use crate::app::{BareKey, Message, State, Tab, bare_key, key_message, update, view};
    use crate::session_view::{PROMPT, SessionMessage};
    use crate::test_support::{Plain, deliver_tool, end_turn, plain};

    /// The question of the plan's execution, with a description for its first option.
    fn execution() -> Question {
        Question {
            text: "How should the plan be executed?".into(),
            options: vec![
                QuestionOption {
                    label: "Subagent-driven".into(),
                    description: Some("fresh subagent per task".into()),
                },
                QuestionOption {
                    label: "Inline".into(),
                    description: None,
                },
            ],
        }
    }

    /// A plain session on the Session tab that asked [`execution`] in its running turn.
    fn asked() -> Plain {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;
        ask(&mut fixture);
        fixture
    }

    /// The session of `fixture` asks [`execution`]; returns the question's id.
    fn ask(fixture: &mut Plain) -> QuestionId {
        deliver_tool(
            &mut fixture.state,
            fixture.session.clone(),
            PartiturTool::AskUser(execution()),
        );
        assert_eq!(fixture.state.error, None);
        fixture.state.app.current_question().cloned().unwrap()
    }

    /// [`asked`], with the turn that asked over.
    fn waiting() -> (Plain, QuestionId) {
        let mut fixture = asked();
        end_turn(&mut fixture.state, &fixture.session);
        let id = fixture.state.app.current_question().cloned().unwrap();
        (fixture, id)
    }

    /// The text of the backend's last call when it is a prompt.
    fn last_prompt(fixture: &Plain) -> Option<String> {
        match fixture.backend.calls().last()? {
            Call::Prompt { content, .. } => match content.first()? {
                PromptContent::Text(text) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// How the question `id` of the current session ended, `None` while it is open.
    fn outcome(state: &State, id: &QuestionId) -> Option<QuestionOutcome> {
        let model = state.app.session_view().unwrap();
        model
            .entries
            .iter()
            .find_map(|entry| match entry {
                Entry::Question {
                    id: asked, outcome, ..
                } if asked == id => Some(outcome.clone()),
                _ => None,
            })
            .unwrap()
    }

    fn apply(state: &mut State, messages: impl IntoIterator<Item = Message>) {
        for message in messages {
            let _task = update(state, message);
        }
    }

    /// Whether `message` is one of a question card's.
    fn answers(message: &Message) -> bool {
        matches!(message, Message::Session(SessionMessage::Question(..)))
    }

    fn on_card(id: &QuestionId, message: QuestionMessage) -> Message {
        Message::Session(SessionMessage::Question(id.clone(), message))
    }

    /// The `Send` button of the card of the question `id`.
    fn send(id: &QuestionId) -> Id {
        Id::from(format!("question-{id}-send"))
    }

    /// Clicks `target`, a text or a widget, and applies the messages the click sends, which
    /// it returns.
    fn click<S>(state: &mut State, target: S) -> Vec<Message>
    where
        S: iced_test::Selector + Send,
        S::Output: iced_test::selector::Bounded + Clone + Send + Sync + 'static,
    {
        let mut ui = simulator(view(state));
        ui.click(target).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        apply(state, messages.clone());
        messages
    }

    fn press(character: &str) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn the_card_shows_the_question_and_its_options() {
        let (fixture, _id) = waiting();

        let mut ui = simulator(view(&fixture.state));
        ui.find("Question").unwrap();
        ui.find("How should the plan be executed?").unwrap();
        ui.find("Subagent-driven").unwrap();
        ui.find("fresh subagent per task").unwrap();
        ui.find("Inline").unwrap();
        // The keys that pick the options, the field for an answer of one's own and its
        // buttons.
        ui.find("1").unwrap();
        ui.find("2").unwrap();
        ui.find("Other").unwrap();
        ui.find(OTHER).unwrap();
        assert_eq!(OTHER, "type your own answer…");
        ui.find("Send").unwrap();
        ui.find("Cancel").unwrap();
    }

    #[test]
    fn clicking_an_option_answers_with_its_label() {
        let (mut fixture, id) = waiting();

        let messages = click(&mut fixture.state, "Inline");

        assert_eq!(messages, [on_card(&id, QuestionMessage::Pick(1))]);
        assert_eq!(last_prompt(&fixture).as_deref(), Some("Inline"));
        assert_eq!(
            outcome(&fixture.state, &id),
            Some(QuestionOutcome::Answered("Inline".into()))
        );
        assert_eq!(fixture.state.app.current_question(), None);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn other_sends_the_typed_answer() {
        let (mut fixture, id) = waiting();
        let field = Id::from(format!("question-{id}-other"));

        let mut ui = simulator(view(&fixture.state));
        ui.click(field.clone()).unwrap();
        let _ = ui.typewrite("n");
        let typed: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            typed,
            [on_card(&id, QuestionMessage::EditOther("n".into()))]
        );
        // The simulator draws the field once, so the whole answer goes in as one edit.
        apply(
            &mut fixture.state,
            [on_card(
                &id,
                QuestionMessage::EditOther("  neither ".into()),
            )],
        );

        let mut ui = simulator(view(&fixture.state));
        ui.click(field).unwrap();
        let _ = ui.tap_key(key::Named::Enter);
        let sent: Vec<Message> = ui.into_messages().collect();
        assert_eq!(sent, [on_card(&id, QuestionMessage::SendOther)]);
        apply(&mut fixture.state, sent);

        assert_eq!(last_prompt(&fixture).as_deref(), Some("neither"));
        assert_eq!(
            outcome(&fixture.state, &id),
            Some(QuestionOutcome::Answered("neither".into()))
        );
        assert!(fixture.state.session.question_drafts.is_empty());
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_blank_other_answer_sends_nothing() {
        let (mut fixture, id) = waiting();
        let calls = fixture.backend.calls().len();

        apply(
            &mut fixture.state,
            [
                on_card(&id, QuestionMessage::EditOther("   ".into())),
                on_card(&id, QuestionMessage::SendOther),
            ],
        );

        assert_eq!(fixture.backend.calls().len(), calls);
        assert_eq!(outcome(&fixture.state, &id), None);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn send_sends_the_typed_answer() {
        let (mut fixture, id) = waiting();
        apply(
            &mut fixture.state,
            [on_card(&id, QuestionMessage::EditOther("neither".into()))],
        );

        let messages = click(&mut fixture.state, send(&id));

        assert_eq!(messages, [on_card(&id, QuestionMessage::SendOther)]);
        assert_eq!(last_prompt(&fixture).as_deref(), Some("neither"));
    }

    #[test]
    fn cancel_closes_the_card_and_focuses_the_prompt() {
        let (mut fixture, id) = waiting();
        apply(
            &mut fixture.state,
            [on_card(&id, QuestionMessage::EditOther("half".into()))],
        );
        let calls = fixture.backend.calls().len();

        let mut ui = simulator(view(&fixture.state));
        ui.click("Cancel").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [on_card(&id, QuestionMessage::Cancel)]);
        let task = update(&mut fixture.state, on_card(&id, QuestionMessage::Cancel));

        assert!(task.units() > 0, "the prompt box gets the focus");
        assert_eq!(PROMPT, "prompt");
        assert_eq!(
            outcome(&fixture.state, &id),
            Some(QuestionOutcome::Cancelled)
        );
        assert_eq!(fixture.backend.calls().len(), calls);
        assert_eq!(fixture.state.app.current_question(), None);
        assert!(fixture.state.session.question_drafts.is_empty());
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_cancel_whose_save_fails_still_closes_the_question_and_then_shows_the_error() {
        let (mut fixture, id) = waiting();
        apply(
            &mut fixture.state,
            [on_card(&id, QuestionMessage::EditOther("half".into()))],
        );
        // Closing the question saves the session list, which fails this time.
        fixture.store.fail_next_session_save();

        let task = update(&mut fixture.state, on_card(&id, QuestionMessage::Cancel));

        assert_eq!(task.units(), 1, "the focus moves to the prompt");
        assert_eq!(
            fixture
                .state
                .session
                .question_drafts
                .get(&(fixture.session.clone(), id)),
            None
        );
        assert_eq!(fixture.state.error, None);
        crate::test_support::settle_store(&mut fixture.state, &fixture.store);
        assert!(fixture.state.error.is_some());
    }

    #[test]
    fn digit_two_picks_the_second_option() {
        let (mut fixture, id) = waiting();

        apply(&mut fixture.state, [Message::Key(BareKey::Digit(2))]);

        assert_eq!(last_prompt(&fixture).as_deref(), Some("Inline"));
        assert_eq!(
            outcome(&fixture.state, &id),
            Some(QuestionOutcome::Answered("Inline".into()))
        );
    }

    #[test]
    fn a_digit_without_its_option_or_off_the_session_tab_picks_nothing() {
        let (mut fixture, id) = waiting();
        let calls = fixture.backend.calls().len();

        apply(&mut fixture.state, [Message::Key(BareKey::Digit(3))]);
        fixture.state.tab = Tab::Projects;
        apply(&mut fixture.state, [Message::Key(BareKey::Digit(1))]);

        assert_eq!(fixture.backend.calls().len(), calls);
        assert_eq!(outcome(&fixture.state, &id), None);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_digit_typed_in_the_prompt_picks_nothing() {
        let window = window::Id::unique();

        assert_eq!(
            key_message(press("2"), event::Status::Captured, window),
            None
        );
        assert_eq!(
            key_message(press("2"), event::Status::Ignored, window),
            Some(Message::Key(BareKey::Digit(2)))
        );
        for (digit, number) in ["1", "2", "3", "4", "5", "6"].into_iter().zip(1..) {
            assert_eq!(
                bare_key(&Key::Character(digit.into())),
                Some(BareKey::Digit(number))
            );
        }
        assert_eq!(bare_key(&Key::Character("7".into())), None);
        assert_eq!(bare_key(&Key::Character("0".into())), None);

        // The prompt box takes the digit as text.
        let (fixture, _id) = waiting();
        let mut ui = simulator(view(&fixture.state));
        ui.click(Id::new(PROMPT)).unwrap();
        assert_eq!(ui.typewrite("2"), event::Status::Captured);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages
                .iter()
                .all(|message| matches!(message, Message::Session(SessionMessage::Prompt(_)))),
            "{messages:?}"
        );
    }

    #[test]
    fn escape_cancels_the_open_question() {
        let (mut fixture, id) = waiting();
        let calls = fixture.backend.calls().len();

        fixture.state.tab = Tab::Projects;
        apply(&mut fixture.state, [Message::Key(BareKey::Escape)]);
        assert_eq!(outcome(&fixture.state, &id), None);

        fixture.state.tab = Tab::Session;
        let task = update(&mut fixture.state, Message::Key(BareKey::Escape));

        assert!(task.units() > 0, "the prompt box gets the focus");
        assert_eq!(
            outcome(&fixture.state, &id),
            Some(QuestionOutcome::Cancelled)
        );
        assert_eq!(fixture.backend.calls().len(), calls);
        // With no question open, Escape does nothing.
        let task = update(&mut fixture.state, Message::Key(BareKey::Escape));
        assert_eq!(task.units(), 0);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn options_are_disabled_while_the_turn_runs() {
        let mut fixture = asked();
        let id = fixture.state.app.current_question().cloned().unwrap();
        apply(
            &mut fixture.state,
            [on_card(&id, QuestionMessage::EditOther("neither".into()))],
        );
        let calls = fixture.backend.calls().len();

        let messages = click(&mut fixture.state, "Inline");
        let sent = click(&mut fixture.state, send(&id));
        let mut ui = simulator(view(&fixture.state));
        ui.click(Id::from(format!("question-{id}-other"))).unwrap();
        let _ = ui.tap_key(key::Named::Enter);
        let submitted: Vec<Message> = ui.into_messages().collect();
        assert!(!submitted.iter().any(answers), "{submitted:?}");
        apply(&mut fixture.state, [Message::Key(BareKey::Digit(2))]);

        // A press on a disabled option or Send only selects text in the transcript.
        assert!(
            !messages.iter().chain(&sent).any(answers),
            "{messages:?} {sent:?}"
        );
        assert_eq!(fixture.backend.calls().len(), calls);
        assert_eq!(outcome(&fixture.state, &id), None);
        assert_eq!(fixture.state.error, None);
        // Cancel stays enabled.
        let cancelled = click(&mut fixture.state, "Cancel");
        assert_eq!(cancelled, [on_card(&id, QuestionMessage::Cancel)]);
        assert_eq!(
            outcome(&fixture.state, &id),
            Some(QuestionOutcome::Cancelled)
        );
    }

    #[test]
    fn a_message_for_a_question_that_is_not_open_changes_nothing() {
        let (mut fixture, id) = waiting();
        let calls = fixture.backend.calls().len();

        let stale = QuestionId::new();
        apply(
            &mut fixture.state,
            [
                on_card(&stale, QuestionMessage::Pick(0)),
                on_card(&stale, QuestionMessage::EditOther("x".into())),
                on_card(&stale, QuestionMessage::Cancel),
            ],
        );

        assert_eq!(fixture.backend.calls().len(), calls);
        assert_eq!(outcome(&fixture.state, &id), None);
        assert!(fixture.state.session.question_drafts.is_empty());
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_closed_card_shows_its_outcome() {
        let (mut fixture, first) = waiting();
        apply(
            &mut fixture.state,
            [on_card(&first, QuestionMessage::Pick(1))],
        );
        // In the turn the answer started, the agent asks twice: the second replaces the first.
        let second = ask(&mut fixture);
        let third = ask(&mut fixture);
        end_turn(&mut fixture.state, &fixture.session);
        apply(
            &mut fixture.state,
            [on_card(&third, QuestionMessage::Cancel)],
        );

        assert_eq!(
            outcome(&fixture.state, &second),
            Some(QuestionOutcome::Superseded)
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("answered: Inline").unwrap();
        ui.find(CANCELLED).unwrap();
        ui.find(SUPERSEDED).unwrap();
        assert_eq!(CANCELLED, "cancelled");
        assert_eq!(ANSWERED, "answered");
        assert_eq!(SUPERSEDED, "replaced by a newer question");
        // A closed card offers no field and no option to pick.
        for card in [first, second, third] {
            assert!(ui.find(Id::from(format!("question-{card}-other"))).is_err());
            assert!(
                ui.find(Id::from(format!("question-{card}-option-1")))
                    .is_err()
            );
        }
    }

    #[test]
    fn a_blank_answer_reads_plain_answered() {
        let question = Question {
            text: "Which way?".into(),
            options: Vec::new(),
        };
        for blank in ["", "  "] {
            let outcome = QuestionOutcome::Answered(blank.into());
            let mut ui = simulator(closed(&question, &outcome, Decider::User));
            ui.find(ANSWERED).unwrap();
            assert!(ui.find("answered: ").is_err());
        }
    }

    #[test]
    fn a_card_the_orchestrator_answered_says_so() {
        let question = Question {
            text: "Merge or PR?".into(),
            options: Vec::new(),
        };
        let answered = QuestionOutcome::Answered("PR".into());
        let mut ui = simulator(closed(&question, &answered, Decider::Orchestrator));
        ui.find("answered by orchestrator: PR").unwrap();
        assert!(ui.find("answered: PR").is_err());

        let blank = QuestionOutcome::Answered(" ".into());
        let mut ui = simulator(closed(&question, &blank, Decider::Orchestrator));
        ui.find("answered by orchestrator").unwrap();

        let mut ui = simulator(closed(&question, &answered, Decider::User));
        ui.find("answered: PR").unwrap();
        assert_eq!(BY_ORCHESTRATOR, "by orchestrator");
    }
}
