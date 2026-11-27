//! The helper's two pipes: Partitur's commands on standard input, the engine's events and frames
//! on standard output, each served by a thread of its own so that CEF's UI thread never waits on
//! either.
//!
//! The reader, `widget-engine-in`, decodes each [`ToEngine`] and queues it for the UI thread as a
//! [`Command`], which posts the task that carries it out there. When its input ends, or cannot be
//! read, it queues [`Command::Shutdown`] and gives CEF [`CLOSE_PATIENCE`] to shut down; a helper
//! still running then ends itself with exit code 0. So the helper never outlives Partitur, even
//! when CEF hangs.
//!
//! The writer, `widget-engine-out`, owns standard output and writes what it is handed ([`Out`]),
//! in order, each message flushed as it goes. Events go out as they are. A painted view only
//! wakes it, once per picture the writer has not taken yet: it then takes the view's damage and
//! writes the damaged regions of the picture as they are by then, copied under the slot's lock
//! (`protocol::frame_regions`, in as many frames as the size limit takes). So however fast a page
//! paints, at most one picture per view waits for the pipe, and a slow reader in Partitur
//! throttles the helper instead of queueing frames. The writer ends on [`Out::End`] or when
//! Partitur's end of the pipe is gone.

use std::io::{self, BufReader, Read, Write};
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};

use partitur_core::widget_host::{LatestFrame, ViewId};
use partitur_widgets::protocol::{
    FromEngine, ProtocolError, ToEngine, frame_regions, read_to_engine, write_from_engine,
};

use crate::engine::{CLOSE_PATIENCE, Commands};
use crate::view::Command;

/// What the writer is handed.
pub(crate) enum Out {
    /// An event, written as it is.
    Event(FromEngine),
    /// `view` painted into its slot, whose damage was empty before.
    Frame(ViewId, LatestFrame),
    /// Nothing more comes.
    End,
}

/// Starts the writer of standard output, which writes what `outgoing` hands it.
pub(crate) fn writer(outgoing: Receiver<Out>) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("widget-engine-out".into())
        .spawn(move || write_out(&mut io::stdout().lock(), &outgoing))
}

/// Starts the reader of standard input, which queues each command on `commands`. It is never
/// joined: it waits on the input until Partitur closes it.
pub(crate) fn reader(commands: Commands) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("widget-engine-in".into())
        .spawn(move || {
            read_in(io::stdin().lock(), &commands);
            thread::sleep(CLOSE_PATIENCE);
            tracing::warn!(
                "CEF did not shut down within {CLOSE_PATIENCE:?} of the end of the input; the \
                 widget engine ends itself"
            );
            std::process::exit(0);
        })
}

/// Queues every command read from `input` on `commands`, then [`Command::Shutdown`] once the
/// input ends or cannot be read.
fn read_in(input: impl Read, commands: &Commands) {
    let mut input = BufReader::new(input);
    loop {
        match read_to_engine(&mut input) {
            Ok(Some(message)) => commands.send(command(message)),
            Ok(None) => break,
            Err(error) => {
                tracing::warn!("Partitur's commands could not be read: {error}");
                break;
            }
        }
    }
    commands.send(Command::Shutdown);
}

/// The command for the UI thread that `message` asks for.
fn command(message: ToEngine) -> Command {
    match message {
        ToEngine::Open { view, page } => Command::Open(view, page),
        ToEngine::Post { view, json } => Command::Post(view, json),
        ToEngine::Resize {
            view,
            width,
            height,
            scale,
        } => Command::Resize(view, width, height, scale),
        ToEngine::SetVisible { view, visible } => Command::SetVisible(view, visible),
        ToEngine::Input { view, input } => Command::Input(view, input),
        ToEngine::SetTheme { view, theme } => Command::SetTheme(view, theme),
        ToEngine::Close { view } => Command::Close(view),
        ToEngine::Shutdown => Command::Shutdown,
    }
}

/// Writes what `outgoing` hands over to `out` until [`Out::End`], the end of the queue, or a
/// write that fails.
fn write_out(out: &mut impl Write, outgoing: &Receiver<Out>) {
    for item in outgoing {
        let written = match item {
            Out::Event(event) => write_from_engine(out, &event),
            Out::Frame(view, latest) => write_frame(out, view, &latest),
            Out::End => break,
        }
        .and_then(|()| out.flush().map_err(ProtocolError::from));
        if let Err(error) = written {
            tracing::warn!("the widget engine's output could not be written: {error}");
            break;
        }
    }
}

/// Takes the damage of `latest` and writes the regions it names, if there are any.
fn write_frame(
    out: &mut impl Write,
    view: ViewId,
    latest: &LatestFrame,
) -> Result<(), ProtocolError> {
    let Some(damage) = latest.take() else {
        return Ok(());
    };
    let frames = latest.read(|picture| frame_regions(view, picture, &damage));
    frames
        .into_iter()
        .try_for_each(|frame| write_from_engine(out, &FromEngine::Frame(frame)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::mpsc;

    use partitur_core::widget_host::Region;
    use partitur_widgets::protocol::{FrameRegions, read_from_engine};

    use super::*;

    /// Every message in `bytes`.
    fn messages(bytes: &[u8]) -> Vec<FromEngine> {
        let mut input = bytes;
        std::iter::from_fn(|| read_from_engine(&mut input).unwrap()).collect()
    }

    // A wake writes what the view painted since the last one, and one whose damage another wake
    // took already writes nothing.
    #[test]
    fn a_painted_view_goes_out_as_its_damaged_regions() {
        let latest = LatestFrame::default();
        assert!(latest.paint(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8], [Region::whole(2, 1)]));
        latest.take();
        assert!(latest.paint(
            2,
            1,
            &[9, 9, 9, 9, 10, 11, 12, 13],
            [Region {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            }]
        ));
        let (out, outgoing) = mpsc::channel();
        let view = ViewId(4);
        out.send(Out::Event(FromEngine::Hello {
            version: 1,
            sandboxed: true,
        }))
        .unwrap();
        out.send(Out::Frame(view, latest.clone())).unwrap();
        out.send(Out::Frame(view, latest.clone())).unwrap();
        out.send(Out::End).unwrap();
        out.send(Out::Event(FromEngine::Message {
            view,
            json: "after the end".into(),
        }))
        .unwrap();
        let mut written = Vec::new();
        write_out(&mut written, &outgoing);
        assert_eq!(
            messages(&written),
            [
                FromEngine::Hello {
                    version: 1,
                    sandboxed: true,
                },
                FromEngine::Frame(FrameRegions {
                    view,
                    width: 2,
                    height: 1,
                    regions: vec![(
                        Region {
                            x: 1,
                            y: 0,
                            width: 1,
                            height: 1,
                        },
                        vec![10, 11, 12, 13],
                    )],
                }),
            ]
        );
        assert!(latest.take().is_none());
    }

    // The writer stops at the first write that fails, as when Partitur is gone: it returns
    // while its queue is still open.
    #[test]
    fn a_broken_pipe_ends_the_writer() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (out, outgoing) = mpsc::channel();
        out.send(Out::Event(FromEngine::Hello {
            version: 1,
            sandboxed: false,
        }))
        .unwrap();
        write_out(&mut Broken, &outgoing);
        drop(out);
    }
}
