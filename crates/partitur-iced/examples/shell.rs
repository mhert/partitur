//! Opens the Partitur window over in-memory fakes, to check the window shell by hand: the
//! system's decorations, the toolbar under them, and the colors following the desktop's
//! light or dark preference.
//!
//! The fake git port answers what the app asks every 100 ms, so the sheets that wait for git
//! load as they do in the app.
//!
//! ```text
//! cargo run -p partitur-iced --example shell
//! ```

use std::thread;
use std::time::Duration;

use partitur_core::testing::{MemoryGit, MemoryStore, RecordingBackend, single_claude};
use partitur_core::{App, GlobalSettings};

/// How often the fake git port answers what waits.
const GIT_ANSWER_INTERVAL: Duration = Duration::from_millis(100);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (backends, default) = single_claude();
    let git = MemoryGit::new();
    let app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(MemoryStore::new()),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
    )?;
    // Nothing reports backend events or calls tools here; keeping the senders alive keeps
    // the streams open.
    let (_sender, events) = futures::channel::mpsc::unbounded();
    let (git_sender, git_events) = futures::channel::mpsc::unbounded();
    // The process ends with the window, and the thread with it.
    thread::spawn(move || {
        loop {
            thread::sleep(GIT_ANSWER_INTERVAL);
            for event in git.answer() {
                if git_sender.unbounded_send(event).is_err() {
                    return;
                }
            }
        }
    });
    let (_caller, tool_calls) = futures::channel::mpsc::unbounded();
    partitur_iced::run(app, events, git_events, tool_calls)?;
    Ok(())
}
