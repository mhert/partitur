//! Drives the real [`EngineHost`] through the [`WidgetHost`] port against the scripted
//! `fake-widget-engine` binary and checks what it sends the engine and the events it reports.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedReceiver;
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{
    LatestFrame, Region, ViewId, WidgetCursor, WidgetHost, WidgetHostError, WidgetHostEvent,
    WidgetInput, WidgetPage,
};
use partitur_widgets::host::{EngineError, EngineHost, Launch, Patience, start};
use partitur_widgets::protocol::{NO_SANDBOX, VERSION};
use tempfile::TempDir;

/// How long a test waits for the next event before it fails instead of hanging.
const WAIT: Duration = Duration::from_secs(5);

const VIEW: ViewId = ViewId(1);

/// A launch of the fake engine playing `script`, logging to `log`, with `extra` arguments.
fn launch(script: &str, log: &Path, extra: &[&str]) -> Launch {
    let mut args: Vec<OsString> = vec![script.into(), "--log".into(), log.into()];
    args.extend(extra.iter().map(OsString::from));
    Launch {
        program: PathBuf::from(env!("CARGO_BIN_EXE_fake-widget-engine")),
        args,
        patience: Patience::default(),
    }
}

/// A fake engine playing `script`, started, with the folder of its log.
struct Fake {
    host: Option<EngineHost>,
    events: UnboundedReceiver<WidgetHostEvent>,
    dir: TempDir,
}

impl Fake {
    fn start(script: &str) -> Self {
        Self::start_with(script, &[], Patience::default())
    }

    fn start_with(script: &str, extra: &[&str], patience: Patience) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut launch = launch(script, &dir.path().join("log"), extra);
        launch.patience = patience;
        let (host, events) = start(launch).unwrap();
        Self {
            host: Some(host),
            events,
            dir,
        }
    }

    fn host(&self) -> &EngineHost {
        self.host.as_ref().unwrap()
    }

    fn log(&self) -> Vec<String> {
        read_log(self.dir.path())
    }

    fn next(&mut self) -> WidgetHostEvent {
        next_event(&mut self.events)
    }

    /// Drops the host, returning how long that took.
    fn drop_host(&mut self) -> Duration {
        let begun = Instant::now();
        drop(self.host.take());
        begun.elapsed()
    }
}

fn read_log(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The pid the fake engine logged first.
fn pid(log: &[String]) -> String {
    log.iter()
        .find_map(|line| line.strip_prefix("pid "))
        .expect("the fake logged no pid")
        .to_owned()
}

/// Whether the process the fake logged is gone.
#[cfg(unix)]
fn gone(log: &[String]) -> bool {
    !partitur_core::testing::alive(&pid(log))
}

#[cfg(windows)]
fn gone(log: &[String]) -> bool {
    let pid = pid(log);
    let listed = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .expect("tasklist runs");
    !String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{pid}\""))
}

fn next_event(events: &mut UnboundedReceiver<WidgetHostEvent>) -> WidgetHostEvent {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(event) = events.try_recv() {
            return event;
        }
        assert!(Instant::now() < deadline, "no event within {WAIT:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn page() -> WidgetPage {
    WidgetPage {
        html: "<p>hi</p>".to_owned(),
        csp: String::new(),
        theme: WidgetTheme::Light,
    }
}

fn frame_of(event: WidgetHostEvent) -> LatestFrame {
    match event {
        WidgetHostEvent::Frame { view, latest } => {
            assert_eq!(view, VIEW);
            latest
        }
        other => panic!("expected a frame, got {other:?}"),
    }
}

#[test]
fn a_started_engine_reports_whether_it_is_sandboxed() {
    let sandboxed = Fake::start("normal");
    assert!(sandboxed.host().sandboxed());
    let plain = Fake::start_with("normal", &[NO_SANDBOX], Patience::default());
    assert!(!plain.host().sandboxed());
}

#[test]
fn commands_reach_the_engine_and_events_come_back() {
    let mut fake = Fake::start("echo");
    let host = fake.host();
    host.open(VIEW, page()).unwrap();
    host.post(VIEW, r#"{"a":1}"#.to_owned()).unwrap();
    host.input(
        VIEW,
        WidgetInput::Press {
            x: 1.0,
            y: 2.0,
            button: 0,
        },
    )
    .unwrap();
    host.resize(VIEW, 30, 20, 2.0).unwrap();
    host.set_visible(VIEW, false).unwrap();
    host.set_theme(VIEW, WidgetTheme::Dark).unwrap();

    let latest = frame_of(fake.next());
    assert_eq!(latest.size(), (2, 2));
    assert_eq!(latest.picture().bgra, vec![7; 16]);
    assert_eq!(
        fake.next(),
        WidgetHostEvent::Message {
            view: VIEW,
            json: r#"{"a":1}"#.to_owned()
        }
    );
    assert_eq!(
        fake.next(),
        WidgetHostEvent::Cursor {
            view: VIEW,
            cursor: WidgetCursor::Pointer
        }
    );
    assert_eq!(
        fake.next(),
        WidgetHostEvent::OpenLink {
            view: VIEW,
            url: "https://example.com/".to_owned()
        }
    );
    // Theme is the last command; its echo shows every command before it arrived.
    assert_eq!(
        fake.next(),
        WidgetHostEvent::Message {
            view: VIEW,
            json: "theme".to_owned()
        }
    );
    let log = fake.log();
    let commands: Vec<&str> = log
        .iter()
        .map(String::as_str)
        .filter(|line| !line.starts_with("pid "))
        .collect();
    assert_eq!(
        commands,
        [
            r#"Open { view: ViewId(1), page: WidgetPage { html: "<p>hi</p>", csp: "", theme: Light } }"#,
            r#"Post { view: ViewId(1), json: "{\"a\":1}" }"#,
            "Input { view: ViewId(1), input: Press { x: 1.0, y: 2.0, button: 0 } }",
            "Resize { view: ViewId(1), width: 30, height: 20, scale: 2.0 }",
            "SetVisible { view: ViewId(1), visible: false }",
            "SetTheme { view: ViewId(1), theme: Dark }",
        ]
    );
}

#[test]
fn a_frame_of_another_size_replaces_the_picture() {
    let mut fake = Fake::start("resize");
    fake.host().open(VIEW, page()).unwrap();
    let latest = frame_of(fake.next());
    assert_eq!(latest.size(), (2, 2));
    latest.take().unwrap();
    fake.host().resize(VIEW, 3, 1, 1.0).unwrap();
    let again = frame_of(fake.next());
    assert_eq!(again, latest);
    assert_eq!(latest.size(), (3, 1));
    assert_eq!(latest.picture().bgra, vec![5; 12]);
    assert_eq!(latest.take().unwrap().regions(), [Region::whole(3, 1)]);
}

#[test]
fn frames_coalesce_while_the_reader_waits() {
    let mut fake = Fake::start("paint-fast");
    fake.host().open(VIEW, page()).unwrap();
    let latest = frame_of(fake.next());
    // The fake goes on painting; none of those paints is announced while this one waits.
    std::thread::sleep(Duration::from_millis(200));
    if let Ok(event) = fake.events.try_recv() {
        panic!("a second event waits: {event:?}");
    }
    // Once taken, the next paint is announced again.
    latest.take().unwrap();
    assert_eq!(frame_of(fake.next()), latest);
}

#[test]
fn a_wrong_version_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let error = start(launch("wrong-version", &dir.path().join("log"), &[])).unwrap_err();
    assert_eq!(error, EngineError::Version(VERSION + 1));
    assert!(gone(&read_log(dir.path())));
}

#[test]
fn no_hello_in_time_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut launch = launch("silent", &dir.path().join("log"), &[]);
    launch.patience.hello = Duration::from_millis(300);
    let begun = Instant::now();
    let error = start(launch).unwrap_err();
    assert!(matches!(error, EngineError::NoHello(_)), "{error:?}");
    assert!(begun.elapsed() < Duration::from_secs(5));
    assert!(gone(&read_log(dir.path())));
}

#[test]
fn an_engine_that_ends_before_its_hello_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let error = start(launch("no-such-script", &dir.path().join("log"), &[])).unwrap_err();
    assert!(matches!(error, EngineError::NoHello(_)), "{error:?}");
}

#[test]
fn a_missing_engine_fails_the_start() {
    let launch = Launch {
        program: PathBuf::from("/nonexistent"),
        args: Vec::new(),
        patience: Patience::default(),
    };
    let error = start(launch).unwrap_err();
    assert!(matches!(error, EngineError::Spawn(_)), "{error:?}");
}

#[test]
fn dropping_the_host_shuts_the_engine_down() {
    let mut fake = Fake::start("normal");
    fake.drop_host();
    let log = fake.log();
    // The fake takes a while to exit after shutdown: the host waited for it, not killed it.
    assert_eq!(log[log.len() - 2..], ["Shutdown", "exit"]);
    assert!(gone(&log));
}

#[test]
fn an_engine_that_ignores_shutdown_is_ended() {
    let patience = Patience {
        close: Duration::from_millis(300),
        ..Patience::default()
    };
    let mut fake = Fake::start_with("ignore-shutdown", &[], patience);
    let took = fake.drop_host();
    assert!(took < Duration::from_secs(2), "dropping took {took:?}");
    let log = fake.log();
    assert_eq!(log.last().map(String::as_str), Some("Shutdown"));
    assert!(gone(&log));
}

#[test]
fn a_closed_view_paints_no_more() {
    let mut fake = Fake::start("echo");
    fake.host().open(VIEW, page()).unwrap();
    frame_of(fake.next()).take().unwrap();
    fake.host().close(VIEW).unwrap();
    // The fake paints the view once more after its close, then says so.
    assert_eq!(
        fake.next(),
        WidgetHostEvent::Message {
            view: VIEW,
            json: "closed".to_owned()
        }
    );
}

#[test]
fn commands_to_a_gone_engine_fail_to_send() {
    let fake = Fake::start("exit");
    let deadline = Instant::now() + WAIT;
    let error = loop {
        match fake.host().post(VIEW, "{}".to_owned()) {
            Ok(()) => assert!(Instant::now() < deadline, "every post went out"),
            Err(error) => break error,
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(matches!(error, WidgetHostError::Send(_)), "{error:?}");
}

/// A writer into a shared buffer, for the log test's subscriber.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_engines_log_goes_to_tracing_line_by_line() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let mut fake = Fake::start("log");
    fake.drop_host();
    let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    // Other tests' engines may log too, once the subscriber is set; only this one's lines count.
    let line = |words: &str| {
        text.lines()
            .find(|line| line.ends_with(words))
            .unwrap_or_else(|| panic!("no line {words:?} in {text}"))
            .to_owned()
    };
    for line in [line(" first line"), line(" second line")] {
        assert!(line.contains("INFO"), "{line}");
        assert!(line.contains("partitur_widgets::engine"), "{line}");
    }
}
