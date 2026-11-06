//! Opens the Partitur window over in-memory fakes, to check the window shell by hand: no
//! system decorations, the title bar moves the window, a double click maximizes, the three
//! controls work and the colors follow the desktop theme.
//!
//! ```text
//! cargo run -p partitur-iced --example shell
//! ```

use partitur_core::App;
use partitur_core::testing::{MemoryStore, RecordingBackend, single_claude};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (backends, default) = single_claude();
    let app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(MemoryStore::new()),
        backends,
        default,
    )?;
    // Nothing reports events here; keeping the sender alive keeps the stream open.
    let (_sender, events) = futures::channel::mpsc::unbounded();
    partitur_iced::run(app, events)?;
    Ok(())
}
