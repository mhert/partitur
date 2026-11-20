//! Hosts MCP App widgets in windowless CEF browsers, behind the core's [`WidgetHost`] port.
//!
//! [`start`] runs CEF on a thread of its own, `cef-main`, and returns the host together with the
//! receiver of its [`WidgetHostEvent`]s. Every [`WidgetHost`] method only sends a command to that
//! thread. On Linux and Windows CEF runs its own message loop threads; on macOS `cef-main` drives
//! CEF's external message pump. Rendering is offscreen, the cache lives in a temporary folder of
//! this run that is removed at shutdown, remote debugging is off and no `file:` URL is reachable.
//! Partitur's own executable is CEF's helper executable, so the binary must call [`subprocess`]
//! before anything else and exit with its code when it returns one.
//!
//! The OS process sandbox is used where it can start. On Linux it needs unprivileged user
//! namespaces or a set-uid `chrome-sandbox`; when neither is there, CEF runs with `--no-sandbox`
//! and a warning is logged, because a sandbox that cannot start would abort the whole process.
//! On macOS and Windows the sandbox is not set up yet, so CEF runs without it there and says so
//! in the same warning.
//!
//! Dropping the host closes every view, waits a short while for CEF to destroy the browsers and
//! shuts CEF down.

#![expect(
    clippy::transmute_ptr_to_ptr,
    reason = "the expansion of the cef crate's wrap_* macros"
)]

mod scheme;
mod view;

use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cef::rc::Rc as _;
use cef::{
    App, BrowserProcessHandler, CefString, CommandLine, ImplApp, ImplBrowserProcessHandler,
    ImplCommandLine, LogSeverity, SchemeRegistrar, Settings, WrapApp, WrapBrowserProcessHandler,
    execute_process, wrap_app, wrap_browser_process_handler,
};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use partitur_core::widget_host::{
    ViewId, WidgetHost, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
};
use thiserror::Error;

use crate::view::{Command, Shared, Views};

/// How long shutting down waits for CEF to destroy the open browsers.
const CLOSE_PATIENCE: Duration = Duration::from_secs(5);

/// The switches the browser process starts with, each with its value if it has one. Extensions
/// stay off, so that none installed on the system (some desktops put theirs into Chromium's
/// system folder) runs next to a widget. On Linux CEF renders through the headless Ozone
/// platform: views are offscreen anyway, and under Wayland CEF would ignore the scale factor of
/// [`WidgetHost::resize`] and paint at logical size.
const BROWSER_SWITCHES: &[(&str, Option<&str>)] = &[
    ("disable-extensions", None),
    #[cfg(target_os = "linux")]
    ("ozone-platform", Some("headless")),
];

/// Whether CEF was started in this process already; it can be started only once.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Why [`start`] failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CefError {
    /// CEF itself did not start.
    #[error("CEF did not start: {0}")]
    Initialize(String),
    /// The thread CEF runs on could not be started or stopped early.
    #[error("the CEF thread failed: {0}")]
    Thread(String),
}

/// Runs this process as a CEF helper process when CEF started it as one.
///
/// CEF starts its helpers (renderer, GPU, zygote and so on) by running the executable again with
/// a `--type=` argument. For such a process this hands over to CEF and returns the exit code once
/// the helper's work is done; for any other process it returns `None` at once.
pub fn subprocess() -> Option<i32> {
    let helper = std::env::args_os().any(|arg| arg.to_string_lossy().starts_with("--type="));
    if !helper {
        return None;
    }
    Some(run_helper().unwrap_or(1)) // coverage: runs in a CEF helper process
}

/// Hands a helper process over to CEF, or returns `None` when CEF cannot be loaded.
#[cfg_attr(
    not(target_os = "macos"),
    expect(clippy::unnecessary_wraps, reason = "only macOS can fail to load CEF")
)]
fn run_helper() -> Option<i32> {
    #[cfg(target_os = "macos")] // coverage: runs in a CEF helper process
    let _library = load_library(true)?; // coverage: runs in a CEF helper process
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0); // coverage: runs in a CEF helper process
    let args = cef::args::Args::new(); // coverage: runs in a CEF helper process
    let main_args = args.as_main_args(); // coverage: runs in a CEF helper process
    let mut app = WidgetApp::new(None); // coverage: runs in a CEF helper process
    let info = std::ptr::null_mut(); // coverage: runs in a CEF helper process
    let code = execute_process(Some(main_args), Some(&mut app), info); // coverage: runs in a CEF helper process
    Some(code) // coverage: runs in a CEF helper process
}

/// Loads the CEF framework of the app bundle, which macOS needs before any other CEF call.
#[cfg(target_os = "macos")]
fn load_library(helper: bool) -> Option<cef::library_loader::LibraryLoader> {
    let exe = std::env::current_exe().ok()?;
    let framework = exe
        .parent()?
        .join(if helper { "../../.." } else { "../Frameworks" })
        .join("Chromium Embedded Framework.framework/Chromium Embedded Framework");
    if !framework.exists() {
        return None;
    }
    let loader = cef::library_loader::LibraryLoader::new(&exe, helper);
    loader.load().then_some(loader)
}

/// Starts CEF on the `cef-main` thread and returns the host and the receiver of its events.
///
/// # Errors
///
/// Returns [`CefError::Initialize`] when CEF was started in this process before or does not
/// start, and [`CefError::Thread`] when its thread cannot be started.
pub fn start() -> Result<(CefWidgetHost, UnboundedReceiver<WidgetHostEvent>), CefError> {
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
            .spawn(move || cef_main(&incoming, commands, events, &ready))
            .map_err(|error| CefError::Thread(error.to_string()))?
    };
    let outcome = started
        .recv()
        .unwrap_or_else(|_| Err(CefError::Thread("it stopped before CEF started".into())));
    match outcome {
        Ok(()) => Ok((
            CefWidgetHost {
                commands,
                thread: Some(thread),
            },
            receiver,
        )),
        Err(error) => {
            let _ = thread.join();
            Err(error)
        }
    }
}

/// The [`WidgetHost`] that renders widgets with CEF. See [`start`].
pub struct CefWidgetHost {
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl CefWidgetHost {
    fn send(&self, command: Command) -> Result<(), WidgetHostError> {
        self.commands
            .send(command)
            .map_err(|_| WidgetHostError::Send("the CEF thread has stopped".into()))
    }
}

impl WidgetHost for CefWidgetHost {
    fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError> {
        self.send(Command::Open(view, page))
    }

    fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError> {
        self.send(Command::Post(view, json))
    }

    fn resize(
        &self,
        view: ViewId,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<(), WidgetHostError> {
        self.send(Command::Resize(view, width, height, scale))
    }

    fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError> {
        self.send(Command::SetVisible(view, visible))
    }

    fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError> {
        self.send(Command::Input(view, input))
    }

    fn close(&self, view: ViewId) -> Result<(), WidgetHostError> {
        self.send(Command::Close(view))
    }
}

impl Drop for CefWidgetHost {
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
    #[cfg(target_os = "macos")]
    let Some(_library) = load_library(false) else {
        let _ = ready.send(Err(CefError::Initialize(
            "the CEF framework is not in the app bundle".into(),
        )));
        return;
    };
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let sandbox = sandbox_blocker();
    if let Some(reason) = &sandbox {
        tracing::warn!("CEF runs without the process sandbox: {reason}");
    }
    let settings = match settings(&cache.path, sandbox.is_none()) {
        Ok(settings) => settings,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let args = cef::args::Args::new();
    let mut app = WidgetApp::new(Some(commands.clone()));
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
    });
    if !scheme::install(&shared.pages, &shared.events) {
        cef::shutdown();
        let _ = ready.send(Err(CefError::Initialize(
            "the widget schemes could not be registered".into(),
        )));
        return;
    }
    let _ = ready.send(Ok(()));

    let mut views = Views::new(shared);
    let mut pump = Pump::default();
    while let Some(command) = pump.next(incoming, None) {
        if matches!(command, Command::Shutdown) {
            break;
        }
        views.handle(command);
    }
    views.close_all();
    let deadline = Instant::now() + CLOSE_PATIENCE;
    while views.has_browsers() {
        match pump.next(incoming, Some(deadline)) {
            Some(command) => views.handle(command),
            None => break,
        }
    }
    cef::shutdown();
    drop(cache);
}

/// Waits for the next command, and on macOS runs CEF's message loop work when it is due.
#[derive(Default)]
struct Pump {
    /// When CEF asked for its message loop to run next (external pump only).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    due: Option<Instant>,
}

impl Pump {
    /// The next command, or `None` when every sender is gone or `deadline` passed.
    #[cfg(not(target_os = "macos"))]
    #[allow(clippy::unused_self)]
    fn next(&mut self, incoming: &Receiver<Command>, deadline: Option<Instant>) -> Option<Command> {
        match deadline {
            None => incoming.recv().ok(),
            Some(deadline) => incoming
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok(),
        }
    }

    /// The next command, or `None` when every sender is gone or `deadline` passed. CEF's work runs
    /// when it asked for it, and at least thirty times a second.
    #[cfg(target_os = "macos")]
    fn next(&mut self, incoming: &Receiver<Command>, deadline: Option<Instant>) -> Option<Command> {
        const IDLE: Duration = Duration::from_millis(33);
        loop {
            let now = Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                return None;
            }
            let due = self.due.unwrap_or(now + IDLE);
            let wait = deadline.map_or(due, |deadline| due.min(deadline));
            match incoming.recv_timeout(wait.saturating_duration_since(now)) {
                Ok(Command::Pump(delay)) => {
                    let at = now + Duration::from_millis(u64::try_from(delay).unwrap_or(0));
                    self.due = Some(self.due.map_or(at, |due| due.min(at)));
                }
                Ok(command) => return Some(command),
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() >= due {
                        self.due = None;
                        cef::do_message_loop_work();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
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
        multi_threaded_message_loop: c_int::from(!cfg!(target_os = "macos")),
        external_message_pump: c_int::from(cfg!(target_os = "macos")),
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

/// Why the OS process sandbox cannot start here, or `None` when it can.
#[cfg(target_os = "linux")]
fn sandbox_blocker() -> Option<String> {
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| text.trim().to_owned())
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let suid_helper = exe_dir.is_some_and(|dir| {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir.join("chrome-sandbox"))
            .is_ok_and(|meta| meta.uid() == 0 && meta.mode() & 0o4000 != 0)
    });
    linux_sandbox_blocker(
        read("/proc/self/status").as_deref(),
        read("/proc/sys/kernel/unprivileged_userns_clone").as_deref(),
        read("/proc/sys/user/max_user_namespaces").as_deref(),
        read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").as_deref(),
        suid_helper,
    )
}

/// Why the sandbox cannot start, from what the kernel reports: the process status, the three
/// switches for unprivileged user namespaces, and whether a set-uid `chrome-sandbox` is there.
#[cfg(target_os = "linux")]
fn linux_sandbox_blocker(
    status: Option<&str>,
    userns_clone: Option<&str>,
    max_namespaces: Option<&str>,
    apparmor_restricts: Option<&str>,
    suid_helper: bool,
) -> Option<String> {
    let root = status.is_some_and(|status| {
        status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .and_then(|ids| ids.split_whitespace().next())
            == Some("0")
    });
    if root {
        return Some("it cannot run as root".into());
    }
    if suid_helper {
        return None;
    }
    if userns_clone == Some("0") {
        return Some("unprivileged user namespaces are switched off".into());
    }
    if max_namespaces == Some("0") {
        return Some("user namespaces are limited to none".into());
    }
    if apparmor_restricts == Some("1") {
        return Some("AppArmor restricts unprivileged user namespaces".into());
    }
    None
}

/// Why the OS process sandbox cannot start here. On macOS the helpers would have to start the
/// sandbox themselves through `libcef_sandbox`, and on Windows CEF needs the sandbox bootstrap
/// executable; Partitur has neither yet.
#[cfg(not(target_os = "linux"))]
fn sandbox_blocker() -> Option<String> {
    Some(if cfg!(target_os = "macos") {
        "the helper processes do not start the macOS sandbox".into()
    } else {
        "CEF needs its sandbox bootstrap on Windows".into()
    })
}

wrap_app! {
    struct WidgetApp {
        commands: Option<Sender<Command>>,
    }

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

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            self.commands.clone().map(PumpRequests::new)
        }
    }
}

wrap_browser_process_handler! {
    struct PumpRequests {
        commands: Sender<Command>,
    }

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            let _ = self.commands.send(Command::Pump(delay_ms));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_normal_process_is_no_helper() {
        assert_eq!(subprocess(), None);
    }

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
    #[test]
    fn the_sandbox_is_blocked_only_for_a_reason() {
        let user = Some("Name:\tpartitur\nUid:\t1000\t1000\t1000\t1000\n");
        let root = Some("Uid:\t0\t0\t0\t0\n");
        assert_eq!(
            linux_sandbox_blocker(user, Some("1"), Some("100"), Some("0"), false),
            None
        );
        assert_eq!(linux_sandbox_blocker(user, None, None, None, false), None);
        assert!(linux_sandbox_blocker(root, None, None, None, true).is_some());
        assert!(linux_sandbox_blocker(user, Some("0"), None, None, false).is_some());
        assert!(linux_sandbox_blocker(user, None, Some("0"), None, false).is_some());
        assert!(linux_sandbox_blocker(user, None, None, Some("1"), false).is_some());
        assert_eq!(
            linux_sandbox_blocker(user, Some("0"), Some("0"), Some("1"), true),
            None
        );
    }

    // Linux only: on macOS CEF strings need the framework loaded first, and the paths compare with
    // Unix separators.
    #[cfg(target_os = "linux")]
    #[test]
    fn settings_are_offscreen_cached_in_root_and_private() {
        let root = Path::new("/tmp/partitur-cef-settings");
        let made = settings(root, true).unwrap();
        assert_eq!(made.windowless_rendering_enabled, 1);
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
