//! Parsed Markdown for the agent's answers, kept between frames and fed incrementally.
//!
//! Parsing happens when the transcript changes, never while drawing: the update logic calls
//! [`MarkdownCache::sync`] after every backend event and session switch, and the view only
//! reads [`MarkdownCache::items`].

use std::collections::HashMap;

use iced::widget::markdown;
use partitur_core::{Entry, SessionId, SessionViewModel};

/// The parsed Markdown of every agent answer seen so far, by session and entry index.
#[derive(Debug, Default)]
pub struct MarkdownCache {
    sessions: HashMap<SessionId, HashMap<usize, Parsed>>,
}

/// One answer's Markdown and how many bytes of its text went into it.
#[derive(Debug, Default)]
struct Parsed {
    bytes: usize,
    content: markdown::Content,
}

impl MarkdownCache {
    /// Brings the session's answers up to date with `model`'s entries.
    ///
    /// An answer only grows while it streams, so only the text after the bytes already parsed
    /// is pushed into its Markdown; the parsed prefix is never read again. An answer whose
    /// text got shorter, or no longer continues at the parsed length, is parsed afresh.
    ///
    /// # Examples
    ///
    /// ```
    /// use partitur_core::testing::single_claude;
    /// use partitur_core::{Entry, SessionId, SessionViewModel, Status};
    /// use partitur_iced::markdown_cache::MarkdownCache;
    ///
    /// let (backends, backend) = single_claude();
    /// let session = SessionId::new();
    /// let entries = [Entry::Agent("Hello **world**".into())];
    /// let model = SessionViewModel {
    ///     id: session.clone(),
    ///     breadcrumb: Vec::new(),
    ///     status: Status::Running,
    ///     attention: false,
    ///     orchestrator: false,
    ///     repositories: Vec::new(),
    ///     entries: &entries,
    ///     revision: 1,
    ///     usage: None,
    ///     model: None,
    ///     effort: None,
    ///     auto_allow: false,
    ///     backends,
    ///     backend,
    /// };
    ///
    /// let mut cache = MarkdownCache::default();
    /// cache.sync(&model);
    /// assert_eq!(cache.items(&session, 0).map(<[_]>::len), Some(1));
    /// ```
    pub fn sync(&mut self, model: &SessionViewModel<'_>) {
        let answers = self.sessions.entry(model.id.clone()).or_default();
        for (index, entry) in model.entries.iter().enumerate() {
            if let Entry::Agent(text) = entry {
                answers.entry(index).or_default().catch_up(text);
            }
        }
    }

    /// The parsed answer at `index` in the session's entries, if it was synced.
    pub fn items(&self, session: &SessionId, index: usize) -> Option<&[markdown::Item]> {
        let parsed = self.sessions.get(session)?.get(&index)?;
        Some(parsed.content.items())
    }

    /// Drops everything parsed for `session`, for a session that no longer exists.
    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
    }

    /// How many bytes of the answer at `index` have been parsed.
    #[cfg(test)]
    fn parsed_bytes(&self, session: &SessionId, index: usize) -> Option<usize> {
        Some(self.sessions.get(session)?.get(&index)?.bytes)
    }
}

impl Parsed {
    /// Pushes the part of `text` after the parsed bytes, or starts over when `text` does not
    /// extend what was parsed.
    fn catch_up(&mut self, text: &str) {
        if text.get(self.bytes..).is_none() {
            *self = Self::default();
        }
        if let Some(rest) = text.get(self.bytes..).filter(|rest| !rest.is_empty()) {
            self.content.push_str(rest);
            self.bytes = text.len();
        }
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

    use iced::Theme;
    use iced::widget::markdown;
    use partitur_core::testing::single_claude;
    use partitur_core::{Entry, SessionId, SessionViewModel, Status};

    use super::*;

    fn model<'a>(session: &SessionId, entries: &'a [Entry]) -> SessionViewModel<'a> {
        let (backends, backend) = single_claude();
        SessionViewModel {
            id: session.clone(),
            breadcrumb: Vec::new(),
            status: Status::Running,
            attention: false,
            orchestrator: false,
            repositories: Vec::new(),
            entries,
            revision: 0,
            usage: None,
            model: None,
            effort: None,
            auto_allow: false,
            backends,
            backend,
        }
    }

    /// The text of the single paragraph `items` hold.
    fn paragraph(items: &[markdown::Item]) -> String {
        let [markdown::Item::Paragraph(text)] = items else {
            panic!("expected one paragraph, got {items:?}");
        };
        text.spans(markdown::Style::from(&Theme::Light))
            .iter()
            .map(|span| span.text.as_ref())
            .collect()
    }

    #[test]
    fn sync_parses_incrementally_and_never_reparses_the_prefix() {
        let session = SessionId::new();
        let mut cache = MarkdownCache::default();

        cache.sync(&model(
            &session,
            &[Entry::User("hi".into()), Entry::Agent("Hel".into())],
        ));
        assert_eq!(cache.parsed_bytes(&session, 1), Some(3));

        // Only the bytes after the parsed prefix are read, so a changed prefix stays as it
        // was parsed: the cache shows "Hel", not "HEL".
        cache.sync(&model(
            &session,
            &[Entry::User("hi".into()), Entry::Agent("HELlo **w**".into())],
        ));
        assert_eq!(cache.parsed_bytes(&session, 1), Some(11));
        assert_eq!(paragraph(cache.items(&session, 1).unwrap()), "Hello w");
        assert!(cache.items(&session, 0).is_none());
    }

    #[test]
    fn sync_with_a_shorter_text_restarts_the_entry() {
        let session = SessionId::new();
        let mut cache = MarkdownCache::default();
        cache.sync(&model(&session, &[Entry::Agent("Hello world".into())]));

        cache.sync(&model(&session, &[Entry::Agent("Bye".into())]));

        assert_eq!(cache.parsed_bytes(&session, 0), Some(3));
        assert_eq!(paragraph(cache.items(&session, 0).unwrap()), "Bye");
    }

    #[test]
    fn forget_drops_only_that_sessions_entries() {
        let (one, two) = (SessionId::new(), SessionId::new());
        let mut cache = MarkdownCache::default();
        cache.sync(&model(&one, &[Entry::Agent("one".into())]));
        cache.sync(&model(&two, &[Entry::Agent("two".into())]));

        cache.forget(&one);

        assert!(cache.items(&one, 0).is_none());
        assert_eq!(paragraph(cache.items(&two, 0).unwrap()), "two");
    }
}
