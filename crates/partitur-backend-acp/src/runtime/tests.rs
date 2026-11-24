#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use futures::StreamExt;
use partitur_core::SessionFailure;

use super::*;

/// How long a test waits for an event before failing instead of hanging.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

fn claude() -> BackendId {
    BackendId::try_from("claude").unwrap()
}

/// The next event, or `None` once the stream ended; fails the test after
/// [`EVENT_TIMEOUT`].
fn next_event(events: &mut event_channel::UnboundedReceiver<BackendEvent>) -> Option<BackendEvent> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime
        .block_on(async { tokio::time::timeout(EVENT_TIMEOUT, events.next()).await })
        .expect("no event within the timeout")
}

/// A backend map whose only adapter, under [`claude`], cannot be spawned, so no test
/// runs a real process.
fn missing_agent() -> HashMap<BackendId, AcpConfig> {
    let config = AcpConfig {
        command: PathBuf::from("/nonexistent/partitur-test-agent"),
        ..AcpConfig::default()
    };
    HashMap::from([(claude(), config)])
}

/// The registry's entry for `session`, if it has one.
fn current(connections: &Connections, session: &SessionId) -> Option<Current> {
    lock(&connections.0).current.get(session).copied()
}

/// Opens a connection of `session` in `connections` and makes it live, as reporting the
/// session does.
fn live(connections: &Connections, session: &SessionId, events: &Events) -> Reporter {
    let reporter = connections.claim(session.clone(), events.clone());
    assert!(reporter.go_live(BackendEvent::Update {
        session: session.clone(),
        update: AgentUpdate::Notice("live".into()),
    }));
    reporter
}

fn notice(session: &SessionId, text: &str) -> BackendEvent {
    BackendEvent::Update {
        session: session.clone(),
        update: AgentUpdate::Notice(text.into()),
    }
}

fn assert_unavailable(result: &Result<(), BackendError>) {
    assert!(
        matches!(result, Err(BackendError::Unavailable(_))),
        "expected Unavailable, got {result:?}"
    );
}

/// Holds the backend's event receiver and records, when dropped, whether the channel is
/// closed: it closes only once the runtime loop and every session task have ended and dropped
/// their senders.
struct Witness {
    events: event_channel::UnboundedReceiver<BackendEvent>,
    runtime_ended: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Witness {
    fn drop(&mut self) {
        // Events still queued come first; `Closed` is the end of a closed channel; an open one
        // with nothing waiting is `Empty`.
        let ended = loop {
            match self.events.try_recv() {
                Ok(_) => {}
                Err(error) => break matches!(error, futures::channel::mpsc::TryRecvError::Closed),
            }
        };
        self.runtime_ended
            .store(ended, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The guard handed to the backend: drops the witness, which needs the backend's events and
/// so is only put in the shared slot once the backend started.
struct Guard(Arc<Mutex<Option<Witness>>>);

impl Drop for Guard {
    fn drop(&mut self) {
        drop(lock(&self.0).take());
    }
}

/// How the backend under test is stopped.
#[derive(Debug, Clone, Copy)]
enum Stop {
    Drop,
    Shutdown,
}

#[test]
fn tool_server_guard_drops_after_sessions_close() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for (deferred, stop) in [
        (false, Stop::Drop),
        (true, Stop::Drop),
        (false, Stop::Shutdown),
    ] {
        let runtime_ended = Arc::new(AtomicBool::new(false));
        // The guard needs the backend's events, which exist only once it started: hand them
        // over through a slot the guard reads when dropped.
        let slot = Arc::new(Mutex::new(None::<Witness>));
        let guard = Guard(slot.clone());
        let (backend, events) = if deferred {
            AcpBackend::start_deferred(claude_not_on_path, |_, _| String::new(), guard).unwrap()
        } else {
            AcpBackend::start(missing_agent(), |_, _| String::new(), guard).unwrap()
        };
        *lock(&slot) = Some(Witness {
            events,
            runtime_ended: runtime_ended.clone(),
        });
        create(&backend, 1, &SessionId::new());
        drop(slot);
        match stop {
            Stop::Drop => drop(backend),
            Stop::Shutdown => backend.shutdown(),
        }
        assert!(
            runtime_ended.load(Ordering::SeqCst),
            "the guard dropped before the runtime ended ({deferred}, {stop:?})"
        );
    }
}

#[test]
fn commands_after_shutdown_return_unavailable() {
    let (mut backend, _events) =
        AcpBackend::start(missing_agent(), |_, _| String::new(), ()).unwrap();
    backend.stop();
    let session = SessionId::new();
    let cwd = Path::new("/work");
    assert_unavailable(&backend.new_session(
        RequestId(1),
        session.clone(),
        &claude(),
        cwd,
        ToolRole::Plain,
    ));
    assert_unavailable(&backend.load_session(
        RequestId(2),
        session.clone(),
        &claude(),
        &BackendSessionId::new("b"),
        cwd,
        ToolRole::Plain,
    ));
    assert_unavailable(&backend.prompt(session.clone(), vec![PromptContent::Text("hi".into())]));
    assert_unavailable(&backend.cancel(session.clone()));
    assert_unavailable(&backend.close_session(session.clone()));
    assert_unavailable(&backend.set_config(
        session.clone(),
        &ConfigId::new("model"),
        &ConfigValueId::new("x"),
    ));
    assert_unavailable(&backend.respond_permission(session, PermissionId(1), None));
    assert_eq!(
        BackendError::Unavailable("the ACP runtime has stopped".into()).to_string(),
        "backend unavailable: the ACP runtime has stopped"
    );
}

#[test]
fn commands_for_a_session_that_is_not_open_are_rejected() {
    let (backend, _events) = AcpBackend::start(missing_agent(), |_, _| String::new(), ()).unwrap();
    let session = SessionId::new();
    let unknown = Err(BackendError::UnknownSession(session.clone()));
    assert_eq!(backend.prompt(session.clone(), Vec::new()), unknown);
    assert_eq!(backend.cancel(session.clone()), unknown);
    assert_eq!(
        backend.set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("x")
        ),
        unknown
    );
    assert_eq!(
        backend.respond_permission(session.clone(), PermissionId(1), None),
        unknown
    );
    backend.shutdown();
}

#[test]
fn an_agent_that_cannot_start_fails_the_request() {
    let (backend, mut events) =
        AcpBackend::start(missing_agent(), |_, _| String::new(), ()).unwrap();
    let created = SessionId::new();
    let loaded = SessionId::new();
    backend
        .new_session(
            RequestId(1),
            created.clone(),
            &claude(),
            Path::new("/work"),
            ToolRole::Plain,
        )
        .unwrap();
    backend
        .load_session(
            RequestId(2),
            loaded.clone(),
            &claude(),
            &BackendSessionId::new("b-1"),
            Path::new("/work"),
            ToolRole::Plain,
        )
        .unwrap();
    let failed: Vec<_> = (0..2).map(|_| next_event(&mut events)).collect();
    // The spawn error's wording belongs to the OS; only its presence is pinned.
    for (expected_request, expected_session) in
        [(RequestId(1), created.clone()), (RequestId(2), loaded)]
    {
        assert!(
            failed.iter().any(|event| matches!(
                event,
                Some(BackendEvent::RequestFailed { request, session, error })
                    if *request == expected_request
                        && *session == expected_session
                        && !error.to_string().is_empty()
            )),
            "no RequestFailed for {expected_request} in {failed:?}"
        );
    }
    assert_eq!(
        backend.prompt(created.clone(), Vec::new()),
        Err(BackendError::UnknownSession(created))
    );
    backend.shutdown();
    assert_eq!(next_event(&mut events), None);
}

#[test]
fn a_command_for_a_live_session_is_queued() {
    let (backend, mut events) =
        AcpBackend::start(missing_agent(), |_, _| String::new(), ()).unwrap();
    let session = SessionId::new();
    // Stands in for a session whose connection is up; the runtime has no route for it
    // and drops the command, as it does for a connection that just ended.
    let (unheard, _inbox) = event_channel::unbounded();
    live(&backend.connections, &session, &unheard);
    assert_eq!(backend.cancel(session.clone()), Ok(()));
    assert_eq!(
        backend.respond_permission(session, PermissionId(7), None),
        Ok(())
    );
    backend.shutdown();
    assert_eq!(next_event(&mut events), None);
}

#[test]
fn closing_a_session_the_backend_never_opened_is_no_error() {
    let (backend, mut events) =
        AcpBackend::start(missing_agent(), |_, _| String::new(), ()).unwrap();
    assert_eq!(backend.close_session(SessionId::new()), Ok(()));
    backend.shutdown();
    assert_eq!(next_event(&mut events), None);
}

#[test]
fn an_event_nobody_receives_is_dropped() {
    let (events, receiver) = event_channel::unbounded();
    drop(receiver);
    emit(
        &events,
        BackendEvent::Update {
            session: SessionId::new(),
            update: partitur_core::AgentUpdate::Notice("unheard".into()),
        },
    );
    assert!(events.is_closed());
}

#[test]
fn dropping_the_backend_shuts_the_runtime_down() {
    let (backend, mut events) =
        AcpBackend::start(missing_agent(), |_, _| String::new(), ()).unwrap();
    let registry = Arc::clone(&backend.connections.0);
    drop(backend);
    // The runtime thread was joined, so it no longer holds the registry.
    assert_eq!(Arc::strong_count(&registry), 1);
    assert_eq!(next_event(&mut events), None);
}

#[test]
fn start_with_no_backends_is_an_error() {
    let started = AcpBackend::start(HashMap::new(), |_, _| String::new(), ());
    assert!(matches!(started, Err(AcpError::NoBackends)));
    assert_eq!(AcpError::NoBackends.to_string(), "no backend is configured");
}

#[test]
fn opening_a_session_on_an_unknown_backend_fails_its_request() {
    let session = SessionId::new();
    let start = SessionStart {
        request: RequestId(4),
        session: session.clone(),
        open: Open::New,
        cwd: PathBuf::from("/work"),
        role: ToolRole::Plain,
    };
    assert_eq!(
        not_configured(&BackendId::try_from("gone").unwrap(), start),
        BackendEvent::RequestFailed {
            request: RequestId(4),
            session,
            error: SessionFailure::Unresolved("backend gone is not configured".into()),
        }
    );
}

#[test]
fn a_claimed_session_is_live_only_once_reported() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let reporter = connections.claim(session.clone(), events);
    assert!(!connections.is_live(&session));
    assert!(reporter.go_live(notice(&session, "created")));
    assert!(connections.is_live(&session));
    assert!(!connections.is_live(&SessionId::new()));
    reporter.update(AgentUpdate::Notice("update".into()));
    assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "created")));
    assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "update")));
}

#[test]
fn every_claim_gets_its_own_generation() {
    let (events, _inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let first = connections.claim(SessionId::new(), events.clone());
    let second = connections.claim(SessionId::new(), events);
    assert_eq!((first.generation, second.generation), (0, 1));
}

#[test]
fn a_replaced_connection_reports_nothing_and_leaves_its_successor_alone() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let old = live(&connections, &session, &events);
    let new = connections.claim(session.clone(), events);
    assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "live")));
    // The newer connection is not live until it reports the session.
    assert!(!connections.is_live(&session));

    old.update(AgentUpdate::Notice("late".into()));
    assert!(!old.go_live(notice(&session, "reopened")));
    old.retire(|_| Some(notice(&session, "ended")));
    assert!(inbox.try_recv().is_err());
    assert_eq!(
        current(&connections, &session),
        Some(Current {
            generation: new.generation,
            live: false
        })
    );
}

#[test]
fn retiring_the_current_connection_forgets_the_session() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let reporter = live(&connections, &session, &events);
    let _ = inbox.try_recv();
    reporter.retire(|live| Some(notice(&session, &format!("live: {live}"))));
    assert_eq!(current(&connections, &session), None);
    assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "live: true")));

    // A connection that never reported its session ends without having been live, and
    // an ending that needs no event sends none.
    let unreported = connections.claim(session.clone(), events);
    unreported.retire(|live| Some(notice(&session, &format!("live: {live}"))));
    assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "live: false")));
    let closed = connections.claim(session.clone(), unreported.events.clone());
    closed.retire(|_| None);
    assert_eq!(current(&connections, &session), None);
    assert!(inbox.try_recv().is_err());
}

/// A route to a connection numbered `generation`, and the receiving end of its commands.
fn route_to(generation: Generation) -> (Route, mpsc::UnboundedReceiver<SessionCommand>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (Route { generation, sender }, receiver)
}

#[test]
fn commands_reach_the_session_they_name() {
    let session = SessionId::new();
    let (to_session, mut receiver) = route_to(0);
    let mut routes = HashMap::from([(session.clone(), to_session)]);
    route(&mut routes, &session, SessionCommand::Cancel);
    assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
    assert!(routes.contains_key(&session));
}

#[test]
fn commands_for_unknown_or_closed_sessions_are_dropped() {
    let open = SessionId::new();
    let closed = SessionId::new();
    let (open_route, mut open_receiver) = route_to(0);
    let (closed_route, closed_receiver) = route_to(1);
    drop(closed_receiver);
    let mut routes = HashMap::from([(open.clone(), open_route), (closed.clone(), closed_route)]);
    route(&mut routes, &SessionId::new(), SessionCommand::Cancel);
    route(&mut routes, &closed, SessionCommand::Cancel);
    assert!(!routes.contains_key(&closed));
    assert!(routes.contains_key(&open));
    assert!(open_receiver.try_recv().is_err());
}

/// Runs `task` in a [`JoinSet`] and returns how it ended, as the runtime sees it.
fn ended(task: impl Future<Output = ()> + Send + 'static) -> Result<(task::Id, ()), JoinError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut tasks = JoinSet::new();
        tasks.spawn(task);
        tasks.join_next_with_id().await.unwrap()
    })
}

fn owned_by(
    ended: &Result<(task::Id, ()), JoinError>,
    reporter: Reporter,
) -> HashMap<task::Id, Owner> {
    let id = match ended {
        Ok((id, ())) => *id,
        Err(error) => error.id(),
    };
    let owner = Owner {
        request: RequestId(3),
        reporter,
    };
    HashMap::from([(id, owner)])
}

#[test]
fn a_session_task_that_returned_is_only_forgotten() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let reporter = live(&connections, &session, &events);
    let _ = inbox.try_recv();
    let returned = ended(async {});
    let mut owners = owned_by(&returned, reporter);
    let (to_session, _receiver) = route_to(0);
    let mut routes = HashMap::from([(session.clone(), to_session)]);
    settle(returned, &mut owners, &mut routes);
    assert!(owners.is_empty());
    assert!(routes.contains_key(&session));
    assert!(connections.is_live(&session));
    assert!(inbox.try_recv().is_err());
}

#[test]
fn a_panicked_session_task_before_its_report_fails_the_request() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let reporter = connections.claim(session.clone(), events);
    let panicked = ended(async { panic!("session task panic") });
    let (to_session, _receiver) = route_to(reporter.generation);
    let mut owners = owned_by(&panicked, reporter);
    let mut routes = HashMap::from([(session.clone(), to_session)]);
    settle(panicked, &mut owners, &mut routes);
    assert!(owners.is_empty());
    assert!(routes.is_empty());
    assert_eq!(current(&connections, &session), None);
    assert!(matches!(
        inbox.try_recv(),
        Ok(BackendEvent::RequestFailed {
            request: RequestId(3),
            session: failed,
            error: SessionFailure::Internal(error),
        }) if failed == session && error.starts_with("the session task failed: ")
    ));
}

#[test]
fn a_panicked_session_task_after_its_report_closes_the_session() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let reporter = live(&connections, &session, &events);
    let _ = inbox.try_recv();
    let panicked = ended(async { panic!("session task panic") });
    let (to_session, _receiver) = route_to(reporter.generation);
    let mut owners = owned_by(&panicked, reporter);
    let mut routes = HashMap::from([(session.clone(), to_session)]);
    settle(panicked, &mut owners, &mut routes);
    assert!(routes.is_empty());
    assert!(!connections.is_live(&session));
    assert!(matches!(
        inbox.try_recv(),
        Ok(BackendEvent::SessionClosed { session: closed, error: SessionFailure::Internal(error) })
            if closed == session && error.starts_with("the session task failed: ")
    ));
}

#[test]
fn a_panicked_task_of_a_replaced_connection_leaves_the_newer_one_alone() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let old = live(&connections, &session, &events);
    let new = live(&connections, &session, &events);
    let _ = (inbox.try_recv(), inbox.try_recv());
    let panicked = ended(async { panic!("session task panic") });
    let mut owners = owned_by(&panicked, old);
    let (to_new, _receiver) = route_to(new.generation);
    let mut routes = HashMap::from([(session.clone(), to_new)]);
    settle(panicked, &mut owners, &mut routes);
    assert!(owners.is_empty());
    // The newer connection keeps its route and its live entry, and nothing is reported.
    assert!(routes.contains_key(&session));
    assert!(connections.is_live(&session));
    assert!(inbox.try_recv().is_err());
}

#[test]
fn a_task_the_runtime_does_not_own_is_ignored() {
    let panicked = ended(async { panic!("session task panic") });
    let mut routes = HashMap::from([(SessionId::new(), route_to(0).0)]);
    settle(panicked, &mut HashMap::new(), &mut routes);
    assert_eq!(routes.len(), 1);
}

#[test]
fn closing_all_sessions_cancels_their_turns_and_drops_their_routes() {
    let (to_session, mut receiver) = route_to(0);
    let mut routes = HashMap::from([(SessionId::new(), to_session)]);
    close_all(&mut routes);
    assert!(routes.is_empty());
    assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn closing_a_session_silences_it_cancels_its_turn_and_drops_its_route() {
    let (events, mut inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let other = SessionId::new();
    let reporter = live(&connections, &session, &events);
    live(&connections, &other, &events);
    let _ = (inbox.try_recv(), inbox.try_recv());
    let (to_session, mut receiver) = route_to(reporter.generation);
    let (to_other, _other_receiver) = route_to(1);
    let mut routes = HashMap::from([(session.clone(), to_session), (other.clone(), to_other)]);

    close(&mut routes, &connections, &session);

    assert!(!routes.contains_key(&session));
    assert!(routes.contains_key(&other));
    assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
    assert_eq!(current(&connections, &session), None);
    assert!(connections.is_live(&other));
    reporter.update(AgentUpdate::Notice("late".into()));
    reporter.retire(|_| Some(notice(&session, "ended")));
    assert!(inbox.try_recv().is_err());
}

#[test]
fn closing_a_session_without_a_route_only_forgets_it() {
    let (events, _inbox) = event_channel::unbounded();
    let connections = Connections::default();
    let session = SessionId::new();
    let opening = connections.claim(session.clone(), events);
    let (to_other, _receiver) = route_to(5);
    let mut routes = HashMap::from([(SessionId::new(), to_other)]);

    close(&mut routes, &connections, &session);

    assert_eq!(routes.len(), 1);
    assert_eq!(current(&connections, &session), None);
    assert!(!opening.go_live(notice(&session, "created")));
}

#[test]
fn the_default_config_runs_the_pinned_claude_adapter_through_npx() {
    let config = AcpConfig::default();
    assert_eq!(config.command, PathBuf::from("npx"));
    assert_eq!(
        config.args,
        ["--yes", "@agentclientprotocol/claude-agent-acp@0.108.0"]
    );
    assert_eq!(ADAPTER_COMMAND, "claude-agent-acp");
    assert!(config.env.is_empty());
    assert_eq!(config.thinking_display, ThinkingDisplay::Summarized);
    assert_eq!(config.instructions_dir, None);
    assert_eq!(ThinkingDisplay::default(), ThinkingDisplay::Summarized);
    assert_eq!(ThinkingDisplay::Summarized.env_value(), "summarized");
    assert_eq!(ThinkingDisplay::Omitted.env_value(), "omitted");
}

/// A folder of its own under the temporary folder, not yet created.
fn fresh_dir() -> PathBuf {
    std::env::temp_dir().join(format!("partitur-instructions-{}", SessionId::new()))
}

#[test]
fn write_instructions_rewrites_only_on_change() {
    let dir = fresh_dir().join("agent-instructions");
    let file = write_instructions(&dir).unwrap();
    assert_eq!(file, dir.join("partitur.instructions.md"));
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        partitur_core::tool_instructions()
    );
    // Back-dated, so a rewrite would show in the modification time.
    let long_ago = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86_400);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    assert_eq!(write_instructions(&dir).unwrap(), file);
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), long_ago);
    // Stale instructions are replaced.
    fs::write(&file, "stale").unwrap();
    write_instructions(&dir).unwrap();
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        partitur_core::tool_instructions()
    );
    fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn instructions_that_cannot_be_written_are_an_error() {
    let blocker = fresh_dir();
    fs::write(&blocker, "a file, not a folder").unwrap();
    let error = write_instructions(&blocker.join("agent-instructions")).unwrap_err();
    assert!(matches!(error, AcpError::Instructions(_)), "{error:?}");
    assert_eq!(error.to_string(), "failed to write Partitur's instructions");
    assert!(std::error::Error::source(&error).is_some());
    // A folder whose file cannot be written fails too.
    let dir = fresh_dir();
    fs::create_dir_all(dir.join(INSTRUCTIONS_FILE)).unwrap();
    assert!(matches!(
        write_instructions(&dir),
        Err(AcpError::Instructions(_))
    ));
    fs::remove_file(blocker).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn every_instructions_folder_is_written_once_and_the_failed_ones_are_returned() {
    let good = fresh_dir();
    let blocker = fresh_dir();
    fs::write(&blocker, "a file, not a folder").unwrap();
    let bad = blocker.join("agent-instructions");
    let with = |dir: &Path| {
        Ok(Arc::new(AcpConfig {
            instructions_dir: Some(dir.to_path_buf()),
            ..AcpConfig::default()
        }))
    };
    let backends: Backends = HashMap::from([
        (BackendId::try_from("a").unwrap(), with(&good)),
        (BackendId::try_from("b").unwrap(), with(&good)),
        (BackendId::try_from("c").unwrap(), with(&bad)),
        (
            BackendId::try_from("d").unwrap(),
            Ok(Arc::new(AcpConfig::default())),
        ),
        (
            BackendId::try_from("e").unwrap(),
            Err(SessionFailure::Unresolved("missing".to_owned())),
        ),
    ]);
    assert_eq!(write_every_instructions(&backends), HashSet::from([bad]));
    assert!(good.join(INSTRUCTIONS_FILE).is_file());
    fs::remove_dir_all(good).unwrap();
    fs::remove_file(blocker).unwrap();
}

#[test]
fn start_errors_name_what_failed() {
    let error = AcpError::Spawn(io::Error::other("no threads"));
    assert_eq!(error.to_string(), "failed to spawn the ACP runtime thread");
    assert!(std::error::Error::source(&error).is_some());

    let error = AcpError::Runtime(io::Error::other("no runtime"));
    assert_eq!(error.to_string(), "failed to build the ACP runtime");
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn every_session_is_opened_with_the_url_the_lookup_gives_for_its_role() {
    let (commands, mut queued) = mpsc::unbounded_channel();
    // No runtime thread: the test reads the queued commands itself.
    let backend = AcpBackend {
        commands,
        connections: Connections::default(),
        tool_url: Box::new(|session, role| format!("http://tools/{session}/{role:?}")),
        thread: None,
        tools: None,
    };
    let orchestrator = SessionId::new();
    let cwd = Path::new("/work");
    backend
        .new_session(
            RequestId(1),
            orchestrator.clone(),
            &claude(),
            cwd,
            ToolRole::Orchestrator,
        )
        .unwrap();
    let plain = SessionId::new();
    backend
        .load_session(
            RequestId(2),
            plain.clone(),
            &claude(),
            &BackendSessionId::new("b"),
            cwd,
            ToolRole::Plain,
        )
        .unwrap();
    let child = SessionId::new();
    backend
        .new_session(RequestId(3), child.clone(), &claude(), cwd, ToolRole::Child)
        .unwrap();
    for (session, role) in [
        (orchestrator, "Orchestrator"),
        (plain, "Plain"),
        (child, "Child"),
    ] {
        assert!(matches!(
            queued.try_recv(),
            Ok(Command::Open { tools_url, .. }) if tools_url == format!("http://tools/{session}/{role}")
        ));
    }
}

#[test]
fn the_backend_shows_its_connections_in_debug_output() {
    let (backend, _events) = AcpBackend::start(missing_agent(), |_, _| String::new(), ()).unwrap();
    assert!(format!("{backend:?}").starts_with("AcpBackend { connections: "));
    backend.shutdown();
}

/// The reason the backend [`claude`] failed to resolve in the deferred tests.
const NOT_ON_PATH: &str = "the adapter command x of backend claude is not on PATH";

/// A resolution where [`claude`] failed for [`NOT_ON_PATH`].
fn claude_not_on_path() -> Resolution {
    HashMap::from([(
        claude(),
        Err(SessionFailure::Unresolved(NOT_ON_PATH.to_owned())),
    )])
}

/// Queues creating `session` on [`claude`] with `request`.
fn create(backend: &AcpBackend, request: u64, session: &SessionId) {
    backend
        .new_session(
            RequestId(request),
            session.clone(),
            &claude(),
            Path::new("/work"),
            ToolRole::Plain,
        )
        .unwrap();
}

/// The event failing `request` for `session`, whose backend could not be resolved for
/// `reason`.
fn failed(request: u64, session: &SessionId, reason: &str) -> BackendEvent {
    BackendEvent::RequestFailed {
        request: RequestId(request),
        session: session.clone(),
        error: SessionFailure::Unresolved(reason.to_owned()),
    }
}

#[test]
fn a_backend_that_failed_to_resolve_fails_its_open_with_the_reason() {
    let (backend, mut events) =
        AcpBackend::start_deferred(claude_not_on_path, |_, _| String::new(), ()).unwrap();
    let session = SessionId::new();
    create(&backend, 1, &session);
    assert_eq!(
        next_event(&mut events),
        Some(failed(1, &session, NOT_ON_PATH))
    );
    backend.shutdown();
    assert_eq!(next_event(&mut events), None);
}

#[test]
fn shutdown_while_the_backends_resolve_does_not_wait_for_them() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (backend, mut events) = AcpBackend::start_deferred(
        move || {
            let _ = released.recv();
            claude_not_on_path()
        },
        |_, _| String::new(),
        (),
    )
    .unwrap();
    create(&backend, 1, &SessionId::new());
    let started = std::time::Instant::now();
    drop(backend);
    assert!(started.elapsed() < Duration::from_secs(1));
    // The held open was dropped, not failed.
    assert_eq!(next_event(&mut events), None);
    drop(release);
}

#[test]
fn opens_held_until_the_backends_resolve_run_in_order_without_closed_sessions() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (backend, mut events) = AcpBackend::start_deferred(
        move || {
            let _ = released.recv();
            claude_not_on_path()
        },
        |_, _| String::new(),
        (),
    )
    .unwrap();
    let (first, closed, last) = (SessionId::new(), SessionId::new(), SessionId::new());
    create(&backend, 1, &first);
    create(&backend, 2, &closed);
    create(&backend, 3, &last);
    // The registry entry stands in for a connection the close forgets, which shows when
    // the runtime took the close; only then are the backends resolved.
    let (unheard, _inbox) = event_channel::unbounded();
    backend.connections.claim(closed.clone(), unheard);
    backend.close_session(closed.clone()).unwrap();
    let waiting = std::time::Instant::now();
    while current(&backend.connections, &closed).is_some() {
        assert!(
            waiting.elapsed() < EVENT_TIMEOUT,
            "the close was never taken"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    release.send(()).unwrap();
    assert_eq!(
        next_event(&mut events),
        Some(failed(1, &first, NOT_ON_PATH))
    );
    assert_eq!(next_event(&mut events), Some(failed(3, &last, NOT_ON_PATH)));
    // Opens after the resolution are not held.
    let later = SessionId::new();
    create(&backend, 4, &later);
    assert_eq!(
        next_event(&mut events),
        Some(failed(4, &later, NOT_ON_PATH))
    );
    backend.shutdown();
    assert_eq!(next_event(&mut events), None);
}

#[test]
fn a_resolver_that_gave_up_leaves_every_backend_unconfigured() {
    let (resolved, resolution) = oneshot::channel();
    drop(resolved);
    let (backend, mut events) = AcpBackend::launch(resolution, |_, _| String::new(), ()).unwrap();
    let session = SessionId::new();
    create(&backend, 1, &session);
    assert_eq!(
        next_event(&mut events),
        Some(failed(1, &session, "backend claude is not configured"))
    );
    backend.shutdown();
    assert_eq!(next_event(&mut events), None);
}

#[test]
fn a_backend_the_resolution_lacks_is_not_configured() {
    let (backend, mut events) =
        AcpBackend::start_deferred(Resolution::new, |_, _| String::new(), ()).unwrap();
    let session = SessionId::new();
    create(&backend, 1, &session);
    assert_eq!(
        next_event(&mut events),
        Some(failed(1, &session, "backend claude is not configured"))
    );
    backend.shutdown();
}

#[test]
fn a_poisoned_lock_still_yields_its_data() {
    let mutex = Arc::new(Mutex::new(1));
    let poisoner = Arc::clone(&mutex);
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.lock().unwrap();
        panic!("poison the lock");
    })
    .join();
    assert!(mutex.is_poisoned());
    assert_eq!(*lock(&mutex), 1);
}
