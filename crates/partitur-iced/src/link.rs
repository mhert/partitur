//! Opening a link clicked in a transcript with the session's `link_opener`.
//!
//! The opener runs on its own thread (`link-opener`), so a slow start holds up nothing: the
//! update loop only waits, as a task, for the process to be spawned. The thread then waits for
//! the process to exit so it does not linger as a zombie. Its input and output are closed. A
//! program that cannot be started is reported as an error message for the error bar. Without a
//! configured command the link goes to the system's default handler (`open` on macOS,
//! `xdg-open` on Linux).
//!
//! A configured command runs with the process environment added to the inherited one. That
//! environment may still be read from the login shell; the `link-opener` thread waits for it,
//! never the update loop. The system's handler ignores it.

use std::process::{Command, Stdio};
use std::thread;

use futures::channel::oneshot;
use iced::Task;
use partitur_core::{LinkOpener, ProcessEnv, no_window};

use crate::app::Message;

/// Opens `link` with `opener` in the background, a configured command running in `env`; a
/// failure ends as a [`Message::Error`].
pub fn open(opener: LinkOpener, link: String, env: ProcessEnv) -> Task<Message> {
    Task::future(spawn(opener, link, env)).then(|result| match result {
        Ok(()) => Task::none(),
        Err(error) => Task::done(Message::Error(error)),
    })
}

/// Starts `opener` on `link` on a thread of its own, a configured command once `env` is known
/// and with it, and resolves once the process is spawned, with why it could not be when it
/// was not.
async fn spawn(opener: LinkOpener, link: String, env: ProcessEnv) -> Result<(), String> {
    let (sender, receiver) = oneshot::channel();
    let fail = |error: std::io::Error| format!("cannot open {link} with {opener}: {error}");
    let started = {
        let opener = opener.clone();
        let link = link.clone();
        thread::Builder::new()
            .name("link-opener".into())
            .spawn(move || {
                let child = match &opener {
                    LinkOpener::System => open::that_detached(&link).map(|()| None),
                    LinkOpener::Command { program, args } => no_window(&mut Command::new(program))
                        .args(args)
                        .arg(&link)
                        .envs(env.wait().iter().map(|(name, value)| (name, value)))
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .map(Some),
                };
                match child {
                    Ok(None) => {
                        let _ = sender.send(Ok(()));
                    }
                    Ok(Some(mut child)) => {
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

    use std::time::{Duration, Instant};

    use futures::executor::block_on;

    use super::*;

    fn no_env() -> ProcessEnv {
        ProcessEnv::ready(Vec::new())
    }

    #[test]
    fn a_command_runs_with_the_process_environment() {
        let dir = tempfile::TempDir::new().unwrap();
        let written = dir.path().join("x");
        // The link is the script's `$0`: the file the variable is written to.
        let opener = LinkOpener::Command {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "printf %s \"$PARTITUR_LINK_TEST\" > \"$0\"".into(),
            ],
        };
        let (env, setter) = ProcessEnv::pending();
        let spawned = thread::spawn({
            let link = written.display().to_string();
            move || block_on(spawn(opener, link, env))
        });
        setter.set(vec![("PARTITUR_LINK_TEST".into(), "from the shell".into())]);
        assert_eq!(spawned.join().unwrap(), Ok(()));

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut content = String::new();
        while content.is_empty() && Instant::now() < deadline {
            content = std::fs::read_to_string(&written).unwrap_or_default();
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(content, "from the shell");
    }

    #[test]
    fn a_program_that_starts_opens_the_link() {
        let opener = LinkOpener::parse("true --ignored").unwrap();
        assert_eq!(
            block_on(spawn(opener, "https://example.com".into(), no_env())),
            Ok(())
        );
    }

    #[test]
    fn a_missing_program_is_reported_with_the_link_and_the_command() {
        let opener = LinkOpener::parse("partitur-no-such-opener -x").unwrap();
        let error = block_on(spawn(opener, "https://example.com".into(), no_env())).unwrap_err();
        assert!(
            error.starts_with("cannot open https://example.com with partitur-no-such-opener -x: "),
            "{error}"
        );
    }

    #[test]
    fn opening_is_a_task() {
        let opener = LinkOpener::parse("true").unwrap();
        assert!(open(opener, "https://example.com".into(), no_env()).units() > 0);
    }
}
