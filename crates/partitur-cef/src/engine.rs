//! The CEF runtime of the widget engine helper, on Linux and Windows.
//!
//! [`serve`] runs CEF on the helper's main thread: `cef_initialize` there with CEF's
//! multi-threaded message loop off, then CEF's own message loop until shutdown. That thread is
//! CEF's UI thread, and every [`Views`] command runs on it as a task: the reader of standard
//! input and CEF's handlers queue each [`Command`] through [`Commands`], which posts one task to
//! the UI thread whenever the queue was empty, and that task carries out everything queued by
//! then. Nothing on the UI thread blocks; every wait is a delayed task. Rendering is offscreen,
//! the cache lives in a temporary folder made fresh for each start (a random name, mode 0700 on
//! Unix, never a folder that was there before) and removed at shutdown, remote debugging is off,
//! and extensions are disabled. The helper's executable is also CEF's helper executable, so
//! [`subprocess`] must run before anything else in it. It reads only the arguments before a
//! `--`.
//!
//! Without CEF's resources CEF aborts the whole process, so their folder is looked for first,
//! beside the executable, one folder up and in `../lib/partitur`, and a start without one logs
//! why and ends with exit code 1, as does a CEF that does not initialize. The helper says hello
//! ([`FromEngine::Hello`], with the protocol version and whether CEF runs sandboxed) once CEF's
//! browser context exists and the widget schemes are installed. A context that does not come
//! within ten seconds ends the helper with exit code 1 at once: CEF must not be shut down before
//! its context exists, which would crash it.
//!
//! On Linux CEF runs in the OS process sandbox unless the helper was told otherwise. A sandbox
//! that cannot start aborts the helper; one that starts but does not work is found by the probe,
//! which runs first unless the helper was told to skip it: before its hello the helper opens a
//! page in a sandboxed renderer and waits up to thirty seconds for the page's message. When it
//! comes, the probe's view closes and the hello goes out. When the page fails, or the time runs
//! out, the helper shuts down and ends with [`PROBE_FAILED`], and Partitur starts it again
//! without the sandbox. On Windows CEF needs its sandbox bootstrap, which Partitur does not ship
//! yet, so it always runs without the sandbox, says so in its log and in its hello.
//!
//! [`Command::Shutdown`], from Partitur or at the end of the input, closes every view, waits for
//! CEF to destroy the browsers, but at most five seconds, quits CEF's message loop and shuts CEF
//! down.

use std::cell::RefCell;
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cef::rc::Rc as _;
use cef::{
    App, BrowserProcessHandler, CefString, CommandLine, ImplApp, ImplBrowserProcessHandler,
    ImplCommandLine, ImplTask, LogSeverity, SchemeRegistrar, Settings, Task, ThreadId, WrapApp,
    WrapBrowserProcessHandler, WrapTask, execute_process, wrap_app, wrap_browser_process_handler,
    wrap_task,
};
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{ViewId, WidgetPage};
use partitur_widgets::protocol::{FromEngine, PROBE_FAILED, VERSION};

use crate::CefError;
use crate::scheme;
use crate::serve::{self, Out};
use crate::view::{Command, Shared, Views};

/// How long shutting down waits for CEF to destroy the open browsers, and how long the helper
/// waits after its input ended before it ends itself.
pub(crate) const CLOSE_PATIENCE: Duration = Duration::from_secs(5);

/// How long starting waits for CEF to initialize its browser context once `cef_initialize`
/// returned.
const CONTEXT_PATIENCE: Duration = Duration::from_secs(10);

/// How long the sandbox probe may take before it counts as failed.
#[cfg(target_os = "linux")]
const PROBE_PATIENCE: Duration = Duration::from_secs(30);

/// The view of the sandbox probe, one Partitur does not use.
const PROBE_VIEW: ViewId = ViewId(u64::MAX);

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

/// What a process was started as, read from its arguments.
#[derive(Debug, PartialEq, Eq)]
enum Started {
    /// A CEF helper: an argument `--type=…`.
    Helper,
    /// Not one.
    Other,
}

/// What `args` (the program's arguments, without its name) start this process as. Only the
/// arguments before a `--` count: after it come another program's, which may well carry a
/// `--type=` of their own.
fn started_as<I>(args: I) -> Started
where
    I: IntoIterator,
    I::Item: AsRef<std::ffi::OsStr>,
{
    let helper = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .take_while(|arg| arg != "--")
        .any(|arg| arg.starts_with("--type="));
    if helper {
        Started::Helper
    } else {
        Started::Other
    }
}

/// Runs this process as a CEF helper when it was started as one.
pub(crate) fn subprocess() -> Option<i32> {
    match started_as(std::env::args_os().skip(1)) {
        Started::Helper => Some(run_helper()),
        Started::Other => None,
    }
}

/// Hands a helper process over to CEF.
fn run_helper() -> i32 {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let main_args = args.as_main_args();
    let mut app = WidgetApp::new(false);
    let info = std::ptr::null_mut();
    execute_process(Some(main_args), Some(&mut app), info)
}

/// Whether CEF runs in the OS process sandbox when `asked` is whether it should: on Linux as
/// asked.
#[cfg(not(windows))]
fn sandboxed(asked: bool) -> bool {
    asked
}

/// Whether CEF runs in the OS process sandbox: never on Windows, where cef-rs sandboxes only
/// through CEF's bootstrap executable, which Partitur does not ship yet.
#[cfg(windows)]
fn sandboxed(asked: bool) -> bool {
    if asked {
        tracing::warn!(
            "CEF runs without the process sandbox: on Windows CEF is sandboxed only through its \
             bootstrap executable, which Partitur does not ship"
        );
    }
    false
}

/// Runs the widget engine until it is shut down, with the OS process sandbox when `sandbox`
/// holds and, on Linux, the sandbox probe first when `probe` holds too; see the module
/// documentation. Returns the exit code: 0, 1 when CEF did not start, or [`PROBE_FAILED`].
pub(crate) fn serve(sandbox: bool, probe: bool) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            tracing::error!("no path to the widget engine's executable: {error}");
            return 1;
        }
    };
    if resources_dir(&exe).is_none() {
        tracing::error!(
            "CEF's resources are neither beside {} nor one folder up nor in ../lib/partitur",
            exe.display()
        );
        return 1;
    }
    let sandbox = sandboxed(sandbox);
    let probe = cfg!(target_os = "linux") && sandbox && probe;
    let cache = match CacheDir::create() {
        Ok(cache) => cache,
        Err(error) => {
            tracing::error!("CEF has no cache folder: {error}");
            return 1;
        }
    };
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let settings = match settings(cache.path(), sandbox) {
        Ok(settings) => settings,
        Err(error) => {
            tracing::error!("{error}");
            return 1;
        }
    };
    let (out, outgoing) = mpsc::channel();
    let writer = match serve::writer(outgoing) {
        Ok(writer) => writer,
        Err(error) => {
            tracing::error!("the widget engine's output has no thread: {error}");
            return 1;
        }
    };
    let (queue, incoming) = mpsc::channel();
    let commands = Commands::new(queue);
    let shared = Arc::new(Shared {
        out: out.clone(),
        commands: commands.clone(),
        pages: Arc::new(Mutex::new(std::collections::HashMap::new())),
        owners: Arc::new(Mutex::new(std::collections::HashMap::new())),
        probing: AtomicBool::new(false),
    });
    LOOP.with(|cell| {
        *cell.borrow_mut() = Some(Loop::new(incoming, shared, sandbox, probe));
    });
    let args = cef::args::Args::new();
    let mut app = WidgetApp::new(true);
    let exit = if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) == 1
    {
        run(commands)
    } else {
        tracing::error!(
            "CEF did not start: cef_initialize failed with exit code {}",
            cef::get_exit_code()
        );
        LOOP.with(|cell| cell.borrow_mut().take());
        1
    };
    drop(cache);
    let _ = out.send(Out::End);
    let _ = writer.join();
    exit
}

/// Runs CEF's message loop once CEF is initialized, until the loop quits, and shuts CEF down;
/// returns the exit code.
fn run(commands: Commands) -> i32 {
    // Without the reader nothing could ever shut the helper down; CEF has no context yet, so it
    // must not be shut down either.
    if let Err(error) = serve::reader(commands) {
        tracing::error!("the widget engine's input has no thread: {error}");
        std::process::exit(1);
    }
    post(Job::ContextLate, CONTEXT_PATIENCE);
    cef::run_message_loop();
    // The views, and the browsers they hold, go before CEF does.
    let exit = LOOP
        .with(|cell| cell.borrow_mut().take())
        .map_or(1, |done| done.exit);
    cef::shutdown();
    exit
}

/// The way to CEF's UI thread: queues a command and wakes the thread for it.
#[derive(Clone)]
pub(crate) struct Commands {
    queue: Sender<Command>,
    /// Whether a task that carries out the queue is posted and has not started yet.
    posted: Arc<AtomicBool>,
}

impl Commands {
    fn new(queue: Sender<Command>) -> Self {
        Self {
            queue,
            posted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Queues `command` for the UI thread, posting the task that carries out the queue unless
    /// one is posted already.
    pub(crate) fn send(&self, command: Command) {
        if self.queue.send(command).is_ok()
            && !self.posted.swap(true, Ordering::SeqCst)
            && !post(Job::Drain, Duration::ZERO)
        {
            self.posted.store(false, Ordering::SeqCst);
        }
    }
}

/// What a task posted to the UI thread does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    /// Carries out every queued command.
    Drain,
    /// CEF's browser context is initialized.
    Context,
    /// The context's patience ran out.
    ContextLate,
    /// The probe's patience ran out.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    ProbeLate,
    /// The patience for the browsers to close ran out.
    CloseLate,
}

/// Posts `job` to the UI thread, to run after `delay`; returns whether CEF took it.
fn post(job: Job, delay: Duration) -> bool {
    let mut task = UiTask::new(job);
    let posted = if delay.is_zero() {
        cef::post_task(ThreadId::UI, Some(&mut task))
    } else {
        let delay = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
        cef::post_delayed_task(ThreadId::UI, Some(&mut task), delay)
    };
    posted == 1
}

/// Runs `job` on the loop. When the loop is busy, as it is when CEF calls a handler from within
/// a job, the job is posted to run once that one returns.
fn run_job(job: Job) {
    LOOP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut running) => {
            if let Some(running) = running.as_mut() {
                running.run(job);
            }
        }
        Err(_) => {
            post(job, Duration::ZERO);
        }
    });
}

wrap_task! {
    struct UiTask {
        job: Job,
    }

    impl Task {
        fn execute(&self) {
            run_job(self.job);
        }
    }
}

thread_local! {
    /// The loop of CEF's UI thread, from before CEF starts until its message loop quits.
    static LOOP: RefCell<Option<Loop>> = const { RefCell::new(None) };
}

/// Where the engine stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// CEF has no browser context yet.
    Starting,
    /// The sandbox probe runs.
    Probing,
    /// The hello went out, and Partitur's views are served.
    Serving,
    /// The browsers are closing, and then CEF's message loop quits.
    Closing,
    /// CEF's message loop was asked to quit.
    Done,
}

/// The state of CEF's UI thread: the views and where the engine stands.
struct Loop {
    incoming: Receiver<Command>,
    shared: Arc<Shared>,
    /// The views, once the context exists.
    views: Option<Views>,
    sandbox: bool,
    probe: bool,
    phase: Phase,
    /// Whether a shutdown was asked for before the context existed.
    stop_at_context: bool,
    /// The exit code once the loop has quit.
    exit: i32,
}

impl Loop {
    fn new(incoming: Receiver<Command>, shared: Arc<Shared>, sandbox: bool, probe: bool) -> Self {
        Self {
            incoming,
            shared,
            views: None,
            sandbox,
            probe,
            phase: Phase::Starting,
            stop_at_context: false,
            exit: 0,
        }
    }

    fn run(&mut self, job: Job) {
        match job {
            Job::Drain => self.drain(),
            Job::Context => self.context(),
            Job::ContextLate if self.phase == Phase::Starting => {
                tracing::error!(
                    "CEF did not initialize its context within {CONTEXT_PATIENCE:?}; the widget \
                     engine ends"
                );
                std::process::exit(1);
            }
            #[cfg(target_os = "linux")]
            Job::ProbeLate if self.phase == Phase::Probing => {
                self.probe_failed(&format!("it did not finish within {PROBE_PATIENCE:?}"));
            }
            Job::CloseLate if self.phase == Phase::Closing => self.quit(),
            Job::ContextLate | Job::ProbeLate | Job::CloseLate => {}
        }
    }

    /// Carries out every queued command.
    fn drain(&mut self) {
        // Cleared first, so that a command queued from here on posts another task.
        self.shared.commands.posted.store(false, Ordering::SeqCst);
        while let Ok(command) = self.incoming.try_recv() {
            self.handle(command);
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Shutdown => self.close(),
            Command::Probed(outcome) => {
                if self.phase == Phase::Probing {
                    match outcome {
                        Ok(()) => self.probed(),
                        Err(reason) => self.probe_failed(&reason),
                    }
                }
            }
            // No view opens once the browsers are closing.
            Command::Open(..) if matches!(self.phase, Phase::Closing | Phase::Done) => {}
            command => {
                if let Some(views) = self.views.as_mut() {
                    views.handle(command);
                    if self.phase == Phase::Closing && !views.has_browsers() {
                        self.quit();
                    }
                }
            }
        }
    }

    /// CEF's browser context exists: installs the schemes, then runs the probe or says hello.
    fn context(&mut self) {
        if self.phase != Phase::Starting {
            return;
        }
        self.views = Some(Views::new(Arc::clone(&self.shared)));
        if !scheme::install(&self.shared) {
            tracing::error!("CEF did not start: the widget schemes could not be registered");
            self.exit = 1;
            self.close();
        } else if self.stop_at_context {
            self.close();
        } else if self.probe {
            self.start_probe();
        } else {
            self.hello();
        }
    }

    /// Opens the probe's view, whose page posts a message once it runs in a sandboxed renderer.
    fn start_probe(&mut self) {
        self.phase = Phase::Probing;
        self.shared.probing.store(true, Ordering::SeqCst);
        let page = WidgetPage {
            html: "<script>parent.postMessage({probe: true}, '*')</script>".into(),
            csp: "default-src 'none'; script-src 'unsafe-inline'".into(),
            theme: WidgetTheme::Light,
        };
        if let Some(views) = self.views.as_mut() {
            views.handle(Command::Open(PROBE_VIEW, page));
        }
        #[cfg(target_os = "linux")]
        post(Job::ProbeLate, PROBE_PATIENCE);
    }

    /// The probe's page posted its message: its view closes and the hello goes out.
    fn probed(&mut self) {
        if let Some(views) = self.views.as_mut() {
            views.close(PROBE_VIEW);
        }
        self.hello();
    }

    /// The probe failed for `reason`: the engine shuts down and ends with [`PROBE_FAILED`].
    fn probe_failed(&mut self, reason: &str) {
        tracing::warn!("the sandbox probe failed: {reason}");
        self.exit = PROBE_FAILED;
        self.close();
    }

    /// Says hello, before anything the views report goes out.
    fn hello(&mut self) {
        let _ = self.shared.out.send(Out::Event(FromEngine::Hello {
            version: VERSION,
            sandboxed: self.sandbox,
        }));
        self.shared.probing.store(false, Ordering::SeqCst);
        self.phase = Phase::Serving;
    }

    /// Closes every view and quits CEF's message loop once the browsers are gone or
    /// [`CLOSE_PATIENCE`] ran out. Before the context exists it only remembers to.
    fn close(&mut self) {
        match self.phase {
            Phase::Starting if self.views.is_none() => self.stop_at_context = true,
            Phase::Closing | Phase::Done => {}
            _ => {
                self.phase = Phase::Closing;
                let gone = self.views.as_mut().is_none_or(|views| {
                    views.close_all();
                    !views.has_browsers()
                });
                if gone {
                    self.quit();
                } else {
                    post(Job::CloseLate, CLOSE_PATIENCE);
                }
            }
        }
    }

    fn quit(&mut self) {
        if self.phase != Phase::Done {
            self.phase = Phase::Done;
            cef::quit_message_loop();
        }
    }
}

/// The temporary folder this run's CEF cache lives in, removed when dropped.
///
/// It is made fresh for every start under a random name and
/// with mode 0700 on Unix, and making it fails rather than take a folder that is already
/// there. A folder an earlier run left behind, killed before it could remove it, is never
/// taken up again, and no other user can have put one in its place.
struct CacheDir {
    dir: tempfile::TempDir,
}

impl CacheDir {
    fn create() -> std::io::Result<Self> {
        Self::create_in(&std::env::temp_dir())
    }

    /// A fresh cache folder in `parent`, holding an empty `cache` folder.
    fn create_in(parent: &Path) -> std::io::Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("partitur-cef-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let dir = builder.tempdir_in(parent)?;
        std::fs::create_dir(dir.path().join("cache"))?;
        Ok(Self { dir })
    }

    fn path(&self) -> &Path {
        self.dir.path()
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
        multi_threaded_message_loop: 0,
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

// Tells the loop, on CEF's UI thread, when CEF's browser context is initialized.
wrap_browser_process_handler! {
    struct WidgetBrowserProcess {}

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            run_job(Job::Context);
        }
    }
}

// The application of every CEF process; in the browser process (`browser`) it tells the loop
// when the browser context is initialized.
wrap_app! {
    struct WidgetApp {
        browser: bool,
    }

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            self.browser.then(WidgetBrowserProcess::new)
        }

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
    fn a_type_switch_makes_a_helper() {
        assert_eq!(
            started_as(["--type=renderer", "--lang=en-US"]),
            Started::Helper
        );
        assert_eq!(
            started_as(["--lang=en-US", "--type=gpu-process"]),
            Started::Helper
        );
        assert_eq!(started_as(["--smoke"]), Started::Other);
        assert_eq!(started_as(Vec::<String>::new()), Started::Other);
    }

    // What comes after a separator is another program's, whatever its own arguments say.
    #[test]
    fn arguments_after_a_separator_are_another_programs() {
        assert_eq!(
            started_as(["--log", "--", "node", "agent.js", "--type=module"]),
            Started::Other
        );
        assert_eq!(started_as(["--", "--type=renderer"]), Started::Other);
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

    // Each start gets a fresh folder of its own, private to the user, never one that was there
    // before, such as one an earlier run left behind or another user put in its place.
    #[test]
    fn every_start_gets_a_fresh_private_cache_folder() {
        let parent = tempfile::tempdir().unwrap();
        let left_behind = parent
            .path()
            .join(format!("partitur-cef-{}", std::process::id()));
        std::fs::create_dir_all(left_behind.join("cache")).unwrap();
        std::fs::write(left_behind.join("cache/old"), b"storage").unwrap();
        let first = CacheDir::create_in(parent.path()).unwrap();
        let second = CacheDir::create_in(parent.path()).unwrap();
        assert_ne!(first.path(), second.path());
        for cache in [&first, &second] {
            assert_ne!(cache.path(), left_behind);
            assert!(cache.path().starts_with(parent.path()));
            let names: Vec<_> = std::fs::read_dir(cache.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(names, ["cache"], "a fresh folder holds only the cache");
            assert_eq!(
                std::fs::read_dir(cache.path().join("cache"))
                    .unwrap()
                    .count(),
                0
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = std::fs::metadata(cache.path())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o700, "{mode:o}");
            }
        }
        // Dropped, the folder is removed.
        let gone = first.path().to_path_buf();
        drop(first);
        assert!(!gone.exists());
        assert!(
            left_behind.join("cache/old").exists(),
            "another folder stays"
        );
    }

    // The paths compare with Unix separators.
    #[cfg(target_os = "linux")]
    #[test]
    fn settings_are_offscreen_cached_in_root_and_private() {
        let root = Path::new("/tmp/partitur-cef-settings");
        let made = settings(root, true).unwrap();
        assert_eq!(made.windowless_rendering_enabled, 1);
        assert_eq!(made.multi_threaded_message_loop, 0);
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
