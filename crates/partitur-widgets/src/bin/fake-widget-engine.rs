//! A scripted widget engine speaking the [`protocol`](partitur_widgets::protocol) over stdio, for
//! the host's integration tests.
//!
//! Its arguments are `<script> [--log <path>] [--no-sandbox] [--partitur-no-probe]`. With
//! `--log`, it writes its pid to that file as `pid <n>` first, then the `Debug` form of every
//! command it reads, one line each, and `exit` when it exits on a shutdown. Its hello says it
//! runs sandboxed unless it was given [`NO_SANDBOX`]. Unless a script says otherwise, it exits
//! when its input ends, and on [`ToEngine::Shutdown`] it waits a fifth of a second before it
//! logs `exit` and exits, so a test sees that the host waited for it.
//!
//! The first argument picks the script, each one function below:
//!
//! - `normal`: says hello and does nothing more.
//! - `echo`: answers `Open` with a 2×2 frame of bytes `7`, `Post` with a `Message` of the same
//!   JSON, an `Input` press with a `Cursor` of [`WidgetCursor::Pointer`] and an `OpenLink` to
//!   `https://example.com/`, `SetTheme` with a `Message` of `theme`, and `Close` with one more
//!   frame of the closed view and then a `Message` of `closed`.
//! - `resize`: answers `Open` with a 2×2 frame of bytes `7` and `Resize` with a 3×1 frame of
//!   bytes `5`.
//! - `paint-fast`: once a view opens, paints it 1×1 over and over, as fast as the pipe takes it.
//! - `wrong-version`: says hello in the version after [`VERSION`].
//! - `silent`: never says hello and sleeps thirty seconds.
//! - `ignore-shutdown`: reads and logs commands but neither shutdown nor the end of its input
//!   ends it; it sleeps thirty seconds after its input ends.
//! - `truncated-frame`: answers `Open` with a frame cut off inside its body, then exits.
//! - `log`: writes `first line` and `second line` to its standard error, then plays `normal`.
//! - `exit`: says hello and exits at once.
//!
//! An unknown script exits with a failure before its hello.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

use std::fs::File;
use std::io::{BufReader, Stdout, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use partitur_core::widget_host::{Region, ViewId, WidgetCursor, WidgetInput};
use partitur_widgets::protocol::{
    FrameRegions, FromEngine, NO_SANDBOX, ToEngine, VERSION, read_to_engine, write_from_engine,
};

/// How long the scripts that should be ended by the host sleep.
const HANG: Duration = Duration::from_secs(30);

/// How long a shutdown takes.
const SHUTDOWN: Duration = Duration::from_millis(200);

/// The fake's two outputs, shared by its threads.
#[derive(Clone)]
struct Fake {
    stdout: Arc<Mutex<Stdout>>,
    log: Arc<Mutex<Option<File>>>,
    sandboxed: bool,
}

impl Fake {
    fn new(args: &[String]) -> Self {
        let log = args
            .iter()
            .position(|arg| arg == "--log")
            .and_then(|at| args.get(at + 1))
            .map(|path| File::create(path).expect("the log cannot be created"));
        Self {
            stdout: Arc::new(Mutex::new(std::io::stdout())),
            log: Arc::new(Mutex::new(log)),
            sandboxed: !args.iter().any(|arg| arg == NO_SANDBOX),
        }
    }

    /// Writes `line` to the log, if there is one.
    fn log(&self, line: &str) {
        if let Some(file) = self.log.lock().expect("the log").as_mut() {
            writeln!(file, "{line}").expect("the log cannot be written");
        }
    }

    /// Sends `event`, returning whether it went out.
    fn send(&self, event: &FromEngine) -> bool {
        write_from_engine(&mut *self.stdout.lock().expect("stdout"), event).is_ok()
    }

    fn hello(&self) {
        self.hello_in(VERSION);
    }

    fn hello_in(&self, version: u32) {
        self.send(&FromEngine::Hello {
            version,
            sandboxed: self.sandboxed,
        });
    }

    /// A view's frame of `width` by `height` pixels, every byte `byte`.
    fn frame(&self, view: ViewId, width: u32, height: u32, byte: u8) -> bool {
        self.send(&FromEngine::Frame(FrameRegions {
            view,
            width,
            height,
            regions: vec![(
                Region::whole(width, height),
                vec![byte; (width * height * 4) as usize],
            )],
        }))
    }

    /// Reads and logs every command, handing each but a shutdown to `on_command`. A shutdown
    /// exits after [`SHUTDOWN`]; so does the end of the input, at once.
    fn serve(&self, mut on_command: impl FnMut(&Self, ToEngine)) {
        let mut stdin = BufReader::new(std::io::stdin());
        while let Ok(Some(command)) = read_to_engine(&mut stdin) {
            self.log(&format!("{command:?}"));
            if command == ToEngine::Shutdown {
                std::thread::sleep(SHUTDOWN);
                self.log("exit");
                std::process::exit(0);
            }
            on_command(self, command);
        }
    }
}

fn normal(fake: &Fake) {
    fake.hello();
    fake.serve(|_, _| {});
}

fn echo(fake: &Fake) {
    fake.hello();
    fake.serve(|fake, command| match command {
        ToEngine::Open { view, .. } => {
            fake.frame(view, 2, 2, 7);
        }
        ToEngine::Post { view, json } => {
            fake.send(&FromEngine::Message { view, json });
        }
        ToEngine::Input {
            view,
            input: WidgetInput::Press { .. },
        } => {
            fake.send(&FromEngine::Cursor {
                view,
                cursor: WidgetCursor::Pointer,
            });
            fake.send(&FromEngine::OpenLink {
                view,
                url: "https://example.com/".to_owned(),
            });
        }
        ToEngine::SetTheme { view, .. } => {
            fake.send(&FromEngine::Message {
                view,
                json: "theme".to_owned(),
            });
        }
        ToEngine::Close { view } => {
            fake.frame(view, 2, 2, 8);
            fake.send(&FromEngine::Message {
                view,
                json: "closed".to_owned(),
            });
        }
        _ => {}
    });
}

fn resize(fake: &Fake) {
    fake.hello();
    fake.serve(|fake, command| match command {
        ToEngine::Open { view, .. } => {
            fake.frame(view, 2, 2, 7);
        }
        ToEngine::Resize { view, .. } => {
            fake.frame(view, 3, 1, 5);
        }
        _ => {}
    });
}

fn paint_fast(fake: &Fake) {
    fake.hello();
    fake.serve(|fake, command| {
        if let ToEngine::Open { view, .. } = command {
            let painter = fake.clone();
            std::thread::spawn(move || {
                let mut byte = 0_u8;
                while painter.frame(view, 1, 1, byte) {
                    byte = byte.wrapping_add(1);
                }
            });
        }
    });
}

fn wrong_version(fake: &Fake) {
    fake.hello_in(VERSION + 1);
    fake.serve(|_, _| {});
}

fn silent(_fake: &Fake) {
    std::thread::sleep(HANG); // coverage: the host kills this process when no hello came
}

fn ignore_shutdown(fake: &Fake) {
    fake.hello();
    let mut stdin = BufReader::new(std::io::stdin());
    while let Ok(Some(command)) = read_to_engine(&mut stdin) {
        fake.log(&format!("{command:?}"));
    }
    std::thread::sleep(HANG); // coverage: the host kills this process after its patience
}

fn truncated_frame(fake: &Fake) {
    fake.hello();
    fake.serve(|fake, command| {
        if let ToEngine::Open { view, .. } = command {
            let mut whole = Vec::new();
            let frame = FromEngine::Frame(FrameRegions {
                view,
                width: 1,
                height: 1,
                regions: vec![(Region::whole(1, 1), vec![0; 4])],
            });
            write_from_engine(&mut whole, &frame).expect("a frame");
            let mut stdout = fake.stdout.lock().expect("stdout");
            let _ = stdout.write_all(&whole[..whole.len() - 2]);
            let _ = stdout.flush();
            std::process::exit(0);
        }
    });
}

fn log(fake: &Fake) {
    eprintln!("first line");
    eprintln!("second line");
    normal(fake);
}

fn exit(fake: &Fake) {
    fake.hello();
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let fake = Fake::new(&args);
    fake.log(&format!("pid {}", std::process::id()));
    let script: fn(&Fake) = match args.get(1).map(String::as_str) {
        Some("normal") => normal,
        Some("echo") => echo,
        Some("resize") => resize,
        Some("paint-fast") => paint_fast,
        Some("wrong-version") => wrong_version,
        Some("silent") => silent,
        Some("ignore-shutdown") => ignore_shutdown,
        Some("truncated-frame") => truncated_frame,
        Some("log") => log,
        Some("exit") => exit,
        other => {
            eprintln!("unknown script {other:?}");
            return ExitCode::FAILURE;
        }
    };
    script(&fake);
    ExitCode::SUCCESS
}
