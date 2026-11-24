//! The port through which the core reads the stored transcript of a session that is not open,
//! without waiting for the read.
//!
//! A [`TranscriptReader`] is a command port like [`Backend`](crate::Backend): asking it to read
//! a session's updates returns at once, and the updates, or why they could not be read, are
//! handed to the callback that came with the request later, on any thread, exactly once. The
//! core uses it to answer `read_session` for a session whose transcript is not in memory.
//! [`NoReader`] reads nothing: it answers every request at once with
//! [`StoreError::Unavailable`], so `read_session` of a session that is not open says that
//! reading other sessions is not available.

use crate::ids::{ProjectId, SessionId};
use crate::record::TranscriptRecord;
use crate::store::StoreError;

/// What a [`TranscriptReader`] calls with a session's updates, in the order they were
/// appended, or with why they could not be read.
pub type ReadDone = Box<dyn FnOnce(Result<Vec<TranscriptRecord>, StoreError>) + Send>;

/// Reads the stored updates of a session off the caller's thread.
pub trait TranscriptReader: Send {
    /// Asks for every update of `session` in `project`. Returns at once; `done` runs later,
    /// on any thread, exactly once.
    fn read(&self, project: ProjectId, session: SessionId, done: ReadDone);
}

/// The detail of the error [`NoReader`] answers with.
const NOT_AVAILABLE: &str = "reading other sessions is not available";

/// A [`TranscriptReader`] that reads nothing and refuses every read at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoReader;

impl TranscriptReader for NoReader {
    fn read(&self, _project: ProjectId, _session: SessionId, done: ReadDone) {
        done(Err(StoreError::Unavailable(NOT_AVAILABLE.to_owned())));
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

    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn no_reader_refuses_at_once() {
        let answer = Arc::new(Mutex::new(None));
        let kept = Arc::clone(&answer);

        NoReader.read(
            ProjectId::new(),
            SessionId::new(),
            Box::new(move |result| {
                *kept.lock().unwrap() = Some(result.map_err(|error| error.to_string()));
            }),
        );

        assert_eq!(
            answer.lock().unwrap().take(),
            Some(Err("reading other sessions is not available".to_owned()))
        );
    }
}
