//! Opening a link clicked in a transcript with the session's `link_opener`.
//!
//! The opener runs on its own thread (`link-opener`), so a slow start holds up nothing: the
//! update loop only waits, as a task, for the process to be spawned. Another thread
//! (`link-reaper`) then waits for the process to exit so it does not linger as a zombie. Its
//! input and output are closed. A program that cannot be started is reported as an error
//! message for the error bar. Without a configured command the link goes to the system's
//! default handler (`open` on macOS, `xdg-open` on Linux).
//!
//! A configured command runs with the process environment added to the inherited one. That
//! environment may still be read from the login shell; the `link-opener` thread waits for it,
//! never the update loop. The system's handler ignores it.

use std::process::{Child, Command, Stdio};
use std::thread;

use iced::Task;
use partitur_core::{LinkOpener, ProcessEnv, no_window};

use crate::app::Message;
use crate::background::off_loop;

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
    let started = off_loop("link-opener", {
        let opener = opener.clone();
        let link = link.clone();
        move || match &opener {
            LinkOpener::System => open::that_detached(&link),
            LinkOpener::Command { program, args } => no_window(&mut Command::new(program))
                .args(args)
                .arg(&link)
                .envs(env.wait().iter().map(|(name, value)| (name, value)))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(reap),
        }
    })
    .await;
    match started {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(format!("cannot open {link} with {opener}: {error}")),
        // The thread could not start or did not end with an answer.
        None => Err(format!("cannot open {link} with {opener}")),
    }
}

/// Waits for `child` to exit on a thread of its own (`link-reaper`), so it does not linger as
/// a zombie.
fn reap(mut child: Child) {
    // Without the thread the process is left to exit unwatched.
    let _ = thread::Builder::new()
        .name("link-reaper".into())
        .spawn(move || child.wait());
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

    /// A command that starts and exits at once, whatever arguments follow it.
    const STARTS: &str = if cfg!(windows) {
        "cmd /c exit 0"
    } else {
        "true"
    };

    /// A program and arguments that write `$PARTITUR_LINK_TEST` into the file named by the
    /// argument appended after them, which is where the link goes.
    fn write_variable_into_link() -> (String, Vec<String>) {
        if cfg!(windows) {
            // PowerShell joins the words after `-Command` into one script, the link last.
            let script = "Set-Content -NoNewline -Value $env:PARTITUR_LINK_TEST -LiteralPath";
            let args = ["-NoProfile", "-NonInteractive", "-Command", script];
            ("powershell".into(), args.map(String::from).to_vec())
        } else {
            // The link is the script's `$0`.
            let script = "printf %s \"$PARTITUR_LINK_TEST\" > \"$0\"";
            ("sh".into(), vec!["-c".into(), script.into()])
        }
    }

    #[test]
    fn a_command_runs_with_the_process_environment() {
        let dir = tempfile::TempDir::new().unwrap();
        let written = dir.path().join("x");
        // The link is the file the variable is written to.
        let (program, args) = write_variable_into_link();
        let opener = LinkOpener::Command { program, args };
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
        let opener = LinkOpener::parse(&format!("{STARTS} --ignored")).unwrap();
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
        let opener = LinkOpener::parse(STARTS).unwrap();
        assert!(open(opener, "https://example.com".into(), no_env()).units() > 0);
    }
}
