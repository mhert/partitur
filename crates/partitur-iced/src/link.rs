//! Opening a link clicked in a transcript with the session's `link_opener`.
//!
//! The opener runs on its own thread (`link-opener`), so a slow start holds up nothing: the
//! update loop only waits, as a task, for the process to be spawned. The thread then waits for
//! the process to exit so it does not linger as a zombie. Its input and output are closed. A
//! program that cannot be started is reported as an error message for the error bar.

use std::process::{Command, Stdio};
use std::thread;

use futures::channel::oneshot;
use iced::Task;
use partitur_core::LinkOpener;

use crate::app::Message;

/// Opens `link` with `opener` in the background; a failure ends as a [`Message::Error`].
pub fn open(opener: LinkOpener, link: String) -> Task<Message> {
    Task::future(spawn(opener, link)).then(|result| match result {
        Ok(()) => Task::none(),
        Err(error) => Task::done(Message::Error(error)),
    })
}

/// Starts `opener` on `link` on a thread of its own and resolves once the process is
/// spawned, with why it could not be when it was not.
async fn spawn(opener: LinkOpener, link: String) -> Result<(), String> {
    let (sender, receiver) = oneshot::channel();
    let fail = |error: std::io::Error| format!("cannot open {link} with {opener}: {error}");
    let started = {
        let opener = opener.clone();
        let link = link.clone();
        thread::Builder::new()
            .name("link-opener".into())
            .spawn(move || {
                let child = Command::new(opener.program())
                    .args(opener.args_for(&link))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                match child {
                    Ok(mut child) => {
                        // The receiver may be gone when the window closed meanwhile.
                        let _ = sender.send(Ok(()));
                        let _ = child.wait();
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                    }
                }
            })
    };
    started.map_err(fail)?;
    match receiver.await {
        Ok(spawned) => spawned.map_err(fail),
        // The thread always sends before it ends, so a dropped sender means it panicked.
        Err(oneshot::Canceled) => Err(format!("cannot open {link} with {opener}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use futures::executor::block_on;

    use super::*;

    #[test]
    fn a_program_that_starts_opens_the_link() {
        let opener = LinkOpener::parse("true --ignored").unwrap();
        assert_eq!(
            block_on(spawn(opener, "https://example.com".into())),
            Ok(())
        );
    }

    #[test]
    fn a_missing_program_is_reported_with_the_link_and_the_command() {
        let opener = LinkOpener::parse("partitur-no-such-opener -x").unwrap();
        let error = block_on(spawn(opener, "https://example.com".into())).unwrap_err();
        assert!(
            error.starts_with("cannot open https://example.com with partitur-no-such-opener -x: "),
            "{error}"
        );
    }

    #[test]
    fn opening_is_a_task() {
        let opener = LinkOpener::parse("true").unwrap();
        assert!(open(opener, "https://example.com".into()).units() > 0);
    }
}
