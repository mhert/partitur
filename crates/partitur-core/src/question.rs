//! The question a session asks the user through the `ask_user` tool: its text, its
//! options, how it ends, and the checks and agent-facing copy that go with it.

use serde::{Deserialize, Serialize};

/// What the tool answers once the question is shown.
pub const QUESTION_RECORDED: &str = "The question is shown to the user. End your turn now; \
the user's answer arrives as your next prompt: one option's label, or text of their own.";

/// The instructions that tell an agent when to call `ask_user`.
pub const QUESTION_INSTRUCTIONS: &str = "<partitur-question>
When you ask the user to choose between named options, call ask_user of the MCP server \
\"partitur\" with the question and the options instead of writing them out, then end your \
turn. The user's next prompt is the answer: one option's label, or text of their own.
</partitur-question>";

/// One choice offered with a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// The text of the choice; it is also the answer when picked.
    pub label: String,
    /// A longer explanation shown next to the label.
    #[serde(default)]
    pub description: Option<String>,
}

/// A question with the options it offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// What is asked.
    pub text: String,
    /// The choices, two to six.
    pub options: Vec<QuestionOption>,
}

/// How a question ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionOutcome {
    /// The user answered with an option's label or text of their own.
    Answered(String),
    /// The user dismissed the question.
    Cancelled,
    /// A newer question or prompt replaced it before an answer.
    Superseded,
}

/// Checks that `question` can be shown.
///
/// # Errors
/// Returns the reason, worded for the agent, when the text is blank, there are not two to
/// six options, a label is blank or two labels are the same once trimmed.
pub fn check(question: &Question) -> Result<(), String> {
    if question.text.trim().is_empty() {
        return Err("the question is blank".into());
    }
    let count = question.options.len();
    if !(2..=6).contains(&count) {
        return Err(format!("ask_user takes 2 to 6 options, got {count}"));
    }
    let mut seen: Vec<&str> = Vec::new();
    for (index, option) in question.options.iter().enumerate() {
        let label = option.label.trim();
        if label.is_empty() {
            return Err(format!("option {} has a blank label", index + 1));
        }
        if seen.contains(&label) {
            return Err(format!("two options share the label `{label}`"));
        }
        seen.push(label);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn question(text: &str, labels: &[&str]) -> Question {
        Question {
            text: text.into(),
            options: labels
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).into(),
                    description: None,
                })
                .collect(),
        }
    }

    #[test]
    fn check_accepts_two_to_six_distinct_labels() {
        assert_eq!(check(&question("Which?", &["a", "b"])), Ok(()));
        assert_eq!(
            check(&question("Which?", &["a", "b", "c", "d", "e", "f"])),
            Ok(())
        );
    }

    #[test]
    fn check_refuses_a_blank_question() {
        assert_eq!(
            check(&question("  ", &["a", "b"])),
            Err("the question is blank".into())
        );
    }

    #[test]
    fn check_refuses_too_few_or_too_many_options() {
        assert_eq!(
            check(&question("Which?", &["a"])),
            Err("ask_user takes 2 to 6 options, got 1".into())
        );
        assert_eq!(
            check(&question("Which?", &["a", "b", "c", "d", "e", "f", "g"])),
            Err("ask_user takes 2 to 6 options, got 7".into())
        );
    }

    #[test]
    fn check_refuses_a_blank_label() {
        assert_eq!(
            check(&question("Which?", &["a", " "])),
            Err("option 2 has a blank label".into())
        );
    }

    #[test]
    fn check_refuses_duplicate_labels() {
        assert_eq!(
            check(&question("Which?", &["a", " a "])),
            Err("two options share the label `a`".into())
        );
    }

    #[test]
    fn question_updates_round_trip_through_json() {
        use crate::backend::Update;
        use crate::ids::QuestionId;

        let updates = [
            Update::QuestionAsked {
                id: QuestionId::new(),
                question: question("Which?", &["a", "b"]),
            },
            Update::QuestionClosed {
                id: QuestionId::new(),
                outcome: QuestionOutcome::Answered("a".into()),
            },
            Update::QuestionClosed {
                id: QuestionId::new(),
                outcome: QuestionOutcome::Cancelled,
            },
            Update::QuestionClosed {
                id: QuestionId::new(),
                outcome: QuestionOutcome::Superseded,
            },
        ];
        for update in updates {
            let json = serde_json::to_string(&update).unwrap();
            assert_eq!(serde_json::from_str::<Update>(&json).unwrap(), update);
        }
        let option: QuestionOption = serde_json::from_str(r#"{"label":"a"}"#).unwrap();
        assert_eq!(option.description, None);
    }
}
