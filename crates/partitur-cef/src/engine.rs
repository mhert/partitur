//! The CEF runtime behind [`CefWidgetHost`](crate::CefWidgetHost), on Linux and Windows.
//!
//! [`start`] runs CEF on a thread of its own, `cef-main`, with CEF's multi-threaded message loop,
//! and every [`Engine`] method only sends a command to that thread. Rendering is offscreen, the
//! cache lives in a temporary folder of this run that is removed at shutdown, remote debugging is
//! off, and extensions are disabled. Partitur's own executable is CEF's helper executable, so
//! [`subprocess`] must run before anything else in the binary.
//!
//! The OS process sandbox is used when it works. On Linux a sandbox that cannot start aborts the
//! whole process, so before CEF starts, the executable is run once more as a sandbox probe: that
//! process starts CEF with the sandbox and opens a page in a renderer. When the probe fails, CEF
//! runs with `--no-sandbox` and a warning names the reason. On Windows CEF needs a sandbox
//! bootstrap Partitur does not ship, so it always runs without the sandbox and says so.
//!
//! Dropping the engine closes every view, waits a short while for CEF to destroy the browsers and
//! shuts CEF down.

use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cef::rc::Rc as _;
use cef::{
    App, CefString, CommandLine, ImplApp, ImplCommandLine, LogSeverity, SchemeRegistrar, Settings,
    WrapApp, execute_process, wrap_app,
};
#[cfg(target_os = "linux")]
use futures::channel::mpsc::TryRecvError;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
#[cfg(target_os = "linux")]
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{
    ViewId, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
};

use crate::CefError;
use crate::scheme;
use crate::view::{Command, Shared, Views};

/// How long shutting down waits for CEF to destroy the open browsers.
const CLOSE_PATIENCE: Duration = Duration::from_secs(5);

/// How long the sandbox probe may take before it counts as failed.
#[cfg(target_os = "linux")]
const PROBE_PATIENCE: Duration = Duration::from_secs(30);

/// How long, once the probe has ended, its last words on standard error are waited for. A helper
/// that escaped the probe's process group may hold the pipe open for good.
#[cfg(target_os = "linux")]
const PROBE_WORDS_PATIENCE: Duration = Duration::from_secs(1);

/// The argument that runs this executable as the sandbox probe.
#[cfg(target_os = "linux")]
const SANDBOX_PROBE: &str = "--partitur-cef-sandbox-probe";

/// The switches the browser process starts with, each with its value if it has one. Extensions
/// stay off, so that none installed on the system (some desktops put theirs into Chromium's
/// system folder) runs next to a widget. On Linux CEF renders through the headless Ozone
/// platform: views are offscreen anyway, and under Wayland CEF would ignore the scale factor of
/// a resize and paint at logical size.
const BROWSER_SWITCHES: &[(&str, Option<&str>)] = &[
    ("disable-extensions", None),
    #[cfg(target_os = "linux")]
    ("ozone-platform", Some("headless")),
];

/// Whether CEF was started in this process already; it can be started only once.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Runs this process as a CEF helper or as the sandbox probe when it was started as one.
pub(crate) fn subprocess() -> Option<i32> {
    let mut args = std::env::args_os().map(|arg| arg.to_string_lossy().into_owned());
    let helper = args.any(|arg| arg.starts_with("--type="));
    if helper {
        return Some(run_helper()); // coverage: runs in a CEF helper process
    }
    #[cfg(target_os = "linux")]
    if std::env::args_os().any(|arg| arg == SANDBOX_PROBE) {
        return Some(run_probe_process()); // coverage: runs in the sandbox probe process
    }
    None
}

/// Hands a helper process over to CEF.
fn run_helper() -> i32 {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0); // coverage: runs in a CEF helper process
    let args = cef::args::Args::new(); // coverage: runs in a CEF helper process
    let main_args = args.as_main_args(); // coverage: runs in a CEF helper process
    let mut app = WidgetApp::new(); // coverage: runs in a CEF helper process
    let info = std::ptr::null_mut(); // coverage: runs in a CEF helper process
    execute_process(Some(main_args), Some(&mut app), info) // coverage: runs in a CEF helper process
}

/// The sandbox probe: starts CEF with the sandbox, opens a page that posts a message, and exits
/// with 0 once the message arrives. A sandbox that cannot start aborts this process instead.
#[cfg(target_os = "linux")]
fn run_probe_process() -> i32 {
    let Ok((engine, mut events)) = start_with(true) else {
        return 1; // coverage: runs in the sandbox probe process
    }; // coverage: runs in the sandbox probe process
    let page = WidgetPage {
        html: "<script>parent.postMessage({probe: true}, '*')</script>".into(), // coverage: runs in the sandbox probe process
        csp: "default-src 'none'; script-src 'unsafe-inline'".into(), // coverage: runs in the sandbox probe process
        theme: WidgetTheme::Light, // coverage: runs in the sandbox probe process
    }; // coverage: runs in the sandbox probe process
    if engine.send(Command::Open(ViewId(0), page)).is_err() {
        return 1; // coverage: runs in the sandbox probe process
    } // coverage: runs in the sandbox probe process
    let deadline = Instant::now() + PROBE_PATIENCE; // coverage: runs in the sandbox probe process
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(WidgetHostEvent::Message { .. }) => return 0, // coverage: runs in the sandbox probe process
            Ok(WidgetHostEvent::Failed { .. }) | Err(TryRecvError::Closed) => return 1, // coverage: runs in the sandbox probe process
            Ok(WidgetHostEvent::Frame { .. }) => {} // coverage: runs in the sandbox probe process
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)), // coverage: runs in the sandbox probe process
        } // coverage: runs in the sandbox probe process
    } // coverage: runs in the sandbox probe process
    1 // coverage: runs in the sandbox probe process
}

/// Runs `command` as a sandbox probe and waits up to `patience` for it: `Ok` when it exits
/// successfully, otherwise the reason, with the last line it wrote to standard error.
///
/// The probe runs in a process group of its own, and when it fails or overstays `patience` the
/// whole group is killed, CEF's helpers with it. Its last words are then waited for only
/// [`PROBE_WORDS_PATIENCE`] long, so that a helper which left the group and still holds standard
/// error cannot hold up the start either.
#[cfg(target_os = "linux")]
fn run_probe(mut command: std::process::Command, patience: Duration) -> Result<(), String> {
    use std::io::BufRead as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("the sandbox probe did not start: {error}"))?;
    let (lines, words) = mpsc::channel();
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
    }
    let deadline = Instant::now() + patience;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => break Err(format!("the sandbox probe failed ({status})")),
            Ok(None) if Instant::now() >= deadline => {
                break Err(format!(
                    "the sandbox probe did not finish within {patience:?}"
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => break Err(format!("the sandbox probe could not be watched: {error}")),
        }
    };
    if outcome.is_err() {
        kill_group(child.id());
        let _ = child.kill();
        let _ = child.wait();
    }
    let until = Instant::now() + PROBE_WORDS_PATIENCE;
    let mut said = None;
    while let Ok(line) = words.recv_timeout(until.saturating_duration_since(Instant::now())) {
        if !line.trim().is_empty() {
            said = Some(line);
        }
    }
    outcome.map_err(|reason| match said {
        Some(line) => format!("{reason}: {line}"),
        None => reason,
    })
}

/// Kills the process group `group` with `kill(1)`, which needs no `unsafe` here.
#[cfg(target_os = "linux")]
fn kill_group(group: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Why CEF must run without the OS process sandbox here, or `None` when the sandbox works.
#[cfg(target_os = "linux")]
fn sandbox_blocker() -> Option<String> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return Some(format!("no path to this executable: {error}")),
    };
    let mut command = std::process::Command::new(exe);
    command.arg(SANDBOX_PROBE);
    run_probe(command, PROBE_PATIENCE).err()
}

/// Why CEF must run without the OS process sandbox here: Windows needs the sandbox bootstrap.
#[cfg(not(target_os = "linux"))]
fn sandbox_blocker() -> Option<String> {
    Some("CEF needs its sandbox bootstrap on Windows".into())
}

/// Starts CEF, sandboxed when the sandbox works, and returns the engine and its events.
pub(crate) fn start() -> Result<(Engine, UnboundedReceiver<WidgetHostEvent>), CefError> {
    let blocker = sandbox_blocker();
    if let Some(reason) = &blocker {
        tracing::warn!("CEF runs without the process sandbox: {reason}");
    }
    start_with(blocker.is_none())
}

/// Starts CEF on the `cef-main` thread, with the OS process sandbox when `sandbox` holds.
fn start_with(sandbox: bool) -> Result<(Engine, UnboundedReceiver<WidgetHostEvent>), CefError> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Err(CefError::Initialize(
            "CEF was already started in this process".into(),
        ));
    }
    let (events, receiver) = unbounded();
    let (commands, incoming) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    let thread = {
        let commands = commands.clone();
        std::thread::Builder::new()
            .name("cef-main".into())
            .spawn(move || cef_main(&incoming, commands, events, &ready, sandbox))
            .map_err(|error| CefError::Thread(error.to_string()))?
    };
    let outcome = started
        .recv()
        .unwrap_or_else(|_| Err(CefError::Thread("it stopped before CEF started".into())));
    match outcome {
        Ok(()) => Ok((
            Engine {
                commands,
                thread: Some(thread),
                sandboxed: sandbox,
            },
            receiver,
        )),
        Err(error) => {
            let _ = thread.join();
            Err(error)
        }
    }
}

/// The handle to the running CEF: the way to the `cef-main` thread.
pub(crate) struct Engine {
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
    sandboxed: bool,
}

impl Engine {
    fn send(&self, command: Command) -> Result<(), WidgetHostError> {
        self.commands
            .send(command)
            .map_err(|_| WidgetHostError::Send("the CEF thread has stopped".into()))
    }

    pub(crate) fn sandboxed(&self) -> bool {
        self.sandboxed
    }

    pub(crate) fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError> {
        self.send(Command::Open(view, page))
    }

    pub(crate) fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError> {
        self.send(Command::Post(view, json))
    }

    pub(crate) fn resize(
        &self,
        view: ViewId,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<(), WidgetHostError> {
        self.send(Command::Resize(view, width, height, scale))
    }

    pub(crate) fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError> {
        self.send(Command::SetVisible(view, visible))
    }

    pub(crate) fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError> {
        self.send(Command::Input(view, input))
    }

    pub(crate) fn close(&self, view: ViewId) -> Result<(), WidgetHostError> {
        self.send(Command::Close(view))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The body of the `cef-main` thread: starts CEF, carries out commands until
/// [`Command::Shutdown`], then closes every view and shuts CEF down.
fn cef_main(
    incoming: &Receiver<Command>,
    commands: Sender<Command>,
    events: UnboundedSender<WidgetHostEvent>,
    ready: &Sender<Result<(), CefError>>,
    sandbox: bool,
) {
    let cache = match CacheDir::create() {
        Ok(cache) => cache,
        Err(error) => {
            let _ = ready.send(Err(CefError::Initialize(format!(
                "no cache folder: {error}"
            ))));
            return;
        }
    };
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let settings = match settings(&cache.path, sandbox) {
        Ok(settings) => settings,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let args = cef::args::Args::new();
    let mut app = WidgetApp::new();
    if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        let _ = ready.send(Err(CefError::Initialize(format!(
            "cef_initialize failed with exit code {}",
            cef::get_exit_code()
        ))));
        return;
    }
    let shared = Arc::new(Shared {
        events,
        commands,
        pages: Arc::new(Mutex::new(std::collections::HashMap::new())),
        owners: Arc::new(Mutex::new(std::collections::HashMap::new())),
    });
    if !scheme::install(&shared.pages, &shared.owners, &shared.events) {
        cef::shutdown();
        let _ = ready.send(Err(CefError::Initialize(
            "the widget schemes could not be registered".into(),
        )));
        return;
    }
    let _ = ready.send(Ok(()));

    let mut views = Views::new(shared);
    while let Ok(command) = incoming.recv() {
        if matches!(command, Command::Shutdown) {
            break;
        }
        views.handle(command);
    }
    views.close_all();
    let deadline = Instant::now() + CLOSE_PATIENCE;
    while views.has_browsers() {
        match incoming.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(command) => views.handle(command),
            Err(_) => break,
        }
    }
    cef::shutdown();
    drop(cache);
}

/// The temporary folder this run's CEF cache lives in, removed when dropped.
struct CacheDir {
    path: PathBuf,
}

impl CacheDir {
    fn create() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("partitur-cef-{}", std::process::id()));
        std::fs::create_dir_all(path.join("cache"))?;
        Ok(Self { path })
    }
}

impl Drop for CacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn cef_string(path: &Path) -> CefString {
    CefString::from(path.to_string_lossy().as_ref())
}

/// CEF's settings for this run, cached under `root` and sandboxed when `sandbox` holds.
fn settings(root: &Path, sandbox: bool) -> Result<Settings, CefError> {
    let exe = std::env::current_exe()
        .map_err(|error| CefError::Initialize(format!("no path to this executable: {error}")))?;
    let resources = resources_dir(&exe);
    Ok(Settings {
        no_sandbox: c_int::from(!sandbox),
        browser_subprocess_path: cef_string(&exe),
        multi_threaded_message_loop: 1,
        windowless_rendering_enabled: 1,
        root_cache_path: cef_string(root),
        cache_path: cef_string(&root.join("cache")),
        persist_session_cookies: 0,
        remote_debugging_port: 0,
        log_severity: LogSeverity::ERROR,
        resources_dir_path: resources.as_deref().map(cef_string).unwrap_or_default(),
        locales_dir_path: resources
            .as_deref()
            .map(|dir| cef_string(&dir.join("locales")))
            .unwrap_or_default(),
        ..Settings::default()
    })
}

/// The folder holding CEF's resources (`icudtl.dat`, the `.pak` files and `locales`): next to
/// the executable, one folder up (where Cargo puts them for test binaries in `deps`), or in
/// `../lib/partitur` as packaged on Linux. `None` leaves the choice to CEF.
fn resources_dir(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [
        Some(dir.to_path_buf()),
        dir.parent().map(Path::to_path_buf),
        dir.parent().map(|up| up.join("lib/partitur")),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| candidate.join("icudtl.dat").is_file())
}

wrap_app! {
    struct WidgetApp;

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let browser = process_type.is_none_or(|kind| kind.to_string().is_empty());
            if let Some(command_line) = command_line
                && browser
            {
                for (switch, value) in BROWSER_SWITCHES {
                    let switch = CefString::from(*switch);
                    match value {
                        Some(value) => command_line
                            .append_switch_with_value(Some(&switch), Some(&CefString::from(*value))),
                        None => command_line.append_switch(Some(&switch)),
                    }
                }
            }
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if let Some(registrar) = registrar {
                scheme::register(*registrar);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn resources_are_found_next_to_one_folder_up_or_in_lib() {
        let root = std::env::temp_dir().join(format!("partitur-cef-test-{}", std::process::id()));
        let bin = root.join("usr/bin");
        let lib = root.join("usr/lib/partitur");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        let exe = bin.join("partitur");
        assert_eq!(resources_dir(&exe), None);
        std::fs::write(lib.join("icudtl.dat"), b"").unwrap();
        assert_eq!(resources_dir(&exe), Some(root.join("usr/lib/partitur")));
        std::fs::write(root.join("usr/icudtl.dat"), b"").unwrap();
        assert_eq!(resources_dir(&exe), Some(root.join("usr")));
        std::fs::write(bin.join("icudtl.dat"), b"").unwrap();
        assert_eq!(resources_dir(&exe), Some(bin));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn sh(script: &str) -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_probe_that_exits_cleanly_passes() {
        assert_eq!(run_probe(sh("exit 0"), Duration::from_secs(10)), Ok(()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_probe_that_fails_or_aborts_names_its_last_words() {
        let failed = run_probe(
            sh("echo first >&2; echo 'No usable sandbox!' >&2; exit 1"),
            Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(failed.starts_with("the sandbox probe failed"), "{failed}");
        assert!(failed.ends_with(": No usable sandbox!"), "{failed}");
        let aborted = run_probe(sh("kill -TRAP $$"), Duration::from_secs(10)).unwrap_err();
        assert!(aborted.contains("SIGTRAP"), "{aborted}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_probe_that_hangs_is_stopped() {
        let started = Instant::now();
        let hung = run_probe(sh("exec sleep 30"), Duration::from_millis(200)).unwrap_err();
        assert!(hung.contains("did not finish within"), "{hung}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    // A grandchild that holds standard error, as CEF's helpers do, dies with the probe's group.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_hung_probe_is_stopped_with_its_whole_group() {
        let pid_file =
            std::env::temp_dir().join(format!("partitur-cef-probe-{}", std::process::id()));
        let script = format!("sleep 60 & echo $! > '{}'; sleep 60", pid_file.display());
        let started = Instant::now();
        let hung = run_probe(sh(&script), Duration::from_millis(500)).unwrap_err();
        assert!(hung.contains("did not finish within"), "{hung}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let grandchild = std::fs::read_to_string(&pid_file).unwrap();
        std::fs::remove_file(&pid_file).unwrap();
        let status = Path::new("/proc").join(grandchild.trim()).join("status");
        let gone = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&status).is_ok_and(|text| !text.contains("State:\tZ"))
            && Instant::now() < gone
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let state = std::fs::read_to_string(&status).unwrap_or_default();
        assert!(
            state.is_empty() || state.contains("State:\tZ"),
            "the grandchild lives on: {state}"
        );
    }

    // One that left the group and still holds standard error delays the start only briefly.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_helper_that_left_the_group_cannot_hold_the_start_up() {
        let started = Instant::now();
        let hung =
            run_probe(sh("setsid sleep 5 & sleep 60"), Duration::from_millis(200)).unwrap_err();
        assert!(hung.contains("did not finish within"), "{hung}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_probe_that_cannot_start_says_so() {
        let missing = run_probe(
            std::process::Command::new("/nonexistent/partitur-probe"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(
            missing.starts_with("the sandbox probe did not start"),
            "{missing}"
        );
    }

    // The paths compare with Unix separators.
    #[cfg(target_os = "linux")]
    #[test]
    fn settings_are_offscreen_cached_in_root_and_private() {
        let root = Path::new("/tmp/partitur-cef-settings");
        let made = settings(root, true).unwrap();
        assert_eq!(made.windowless_rendering_enabled, 1);
        assert_eq!(made.multi_threaded_message_loop, 1);
        assert_eq!(made.no_sandbox, 0);
        assert_eq!(made.remote_debugging_port, 0);
        assert_eq!(
            made.root_cache_path.to_string(),
            "/tmp/partitur-cef-settings"
        );
        assert_eq!(
            made.cache_path.to_string(),
            "/tmp/partitur-cef-settings/cache"
        );
        assert_eq!(
            made.browser_subprocess_path.to_string(),
            std::env::current_exe().unwrap().to_string_lossy()
        );
        assert_eq!(settings(root, false).unwrap().no_sandbox, 1);
    }
}
