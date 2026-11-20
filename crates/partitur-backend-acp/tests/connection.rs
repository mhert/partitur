//! Drives the real [`AcpBackend`] through the [`Backend`] port against the scripted
//! `fake-agent` binary, one adapter process per session, and checks the events it reports.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use partitur_backend_acp::{AcpBackend, AcpConfig, AcpError, Resolution, ThinkingDisplay};
use partitur_core::{
    Backend, BackendError, BackendEvent, BackendId, BackendSessionId, ConfigCategory, ConfigChoice,
    ConfigId, ConfigOption, ConfigValueId, OrchestratorTool, PROPOSAL_RECORDED, PartiturTool,
    PermissionId, PermissionKind, PermissionOption, PermissionOptionId, PromptContent,
    ProposedChild, RepositoryId, RequestId, SessionId, StopReason, SuperpowersV7Event, ToolCall,
    ToolCallId, ToolContent, ToolKind, ToolRole, ToolStatus, Update, Widget, WidgetSlot,
};
use partitur_mcp::ToolServer;
use tokio::runtime::Runtime;
use tokio::time::timeout;

/// How long a test waits for the next event before it fails instead of hanging.
const WAIT: Duration = Duration::from_secs(5);
/// How long the cancel test waits: the slow turn only ends by itself after five seconds.
const SLOW_WAIT: Duration = Duration::from_secs(10);
/// How long the real adapter gets per event: it may have to be downloaded first.
const E2E_WAIT: Duration = Duration::from_secs(120);
/// How long shutdown waits for sessions before abandoning them (the runtime's grace period).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// The variable the fake agent's `env` script reports in the session ids it hands out.
const MARK_VAR: &str = "PARTITUR_FAKE_MARK";
/// The variable naming the file the fake agent logs the methods of its untyped requests to.
const LOG_VAR: &str = "PARTITUR_FAKE_LOG";

/// A running backend and the events it reported, beside the tool server its orchestrator
/// sessions call and the calls that server received.
struct Fake {
    backend: AcpBackend,
    events: UnboundedReceiver<BackendEvent>,
    tool_calls: UnboundedReceiver<ToolCall>,
    waiter: Runtime,
    /// Declared after the backend, so it stops only once every session has.
    _tool_server: ToolServer,
}

impl Fake {
    /// Starts a backend whose only adapter, [`claude`], is the fake agent playing `script`.
    fn start(script: &str) -> Self {
        Self::with(only_claude(fake_agent(script, Vec::new())))
    }

    /// Starts a backend serving every adapter in `backends`, its sessions opened with the URL
    /// of their role on a tool server of their own.
    fn with(backends: HashMap<BackendId, AcpConfig>) -> Self {
        Self::started_by(|tool_url| AcpBackend::start(backends, tool_url))
    }

    /// Starts a backend whose adapters are the ones `resolve` returns, its sessions opened
    /// like those of [`Fake::with`].
    fn resolving(resolve: impl FnOnce() -> Resolution + Send + 'static) -> Self {
        Self::started_by(|tool_url| AcpBackend::start_deferred(resolve, tool_url))
    }

    /// Starts the backend with `start`, handing it the URL lookup of a tool server of its
    /// own.
    fn started_by(
        start: impl FnOnce(ToolUrl) -> Result<(AcpBackend, UnboundedReceiver<BackendEvent>), AcpError>,
    ) -> Self {
        let (tool_server, tool_calls) = ToolServer::start().expect("the tool server starts");
        let urls = tool_server.urls();
        let (backend, events) = start(Box::new(move |session, role| {
            urls.session_url(session, role)
        }))
        .expect("the backend starts");
        let waiter = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("the waiting runtime builds");
        Self {
            backend,
            events,
            tool_calls,
            waiter,
            _tool_server: tool_server,
        }
    }

    /// The next event, failing the test when none arrives within [`WAIT`].
    fn next(&mut self) -> BackendEvent {
        self.next_within(WAIT)
    }

    fn next_within(&mut self, wait: Duration) -> BackendEvent {
        wait_for(&self.waiter, &mut self.events, wait).expect("the backend is still running")
    }

    /// The next call the tool server received, failing the test when none arrives within
    /// [`WAIT`].
    fn next_call(&mut self) -> ToolCall {
        wait_for(&self.waiter, &mut self.tool_calls, WAIT).expect("the tool server is running")
    }

    /// The next `count` events.
    fn take(&mut self, count: usize) -> Vec<BackendEvent> {
        (0..count).map(|_| self.next()).collect()
    }

    /// Creates a session and checks that it was reported with the fake's id and config.
    fn create(&mut self) -> SessionId {
        self.create_as(ToolRole::Plain)
    }

    /// Creates a session in `role` and checks that it was reported with the fake's id and
    /// config.
    fn create_as(&mut self, role: ToolRole) -> SessionId {
        let session = SessionId::new();
        self.backend
            .new_session(RequestId(1), session.clone(), &claude(), cwd(), role)
            .expect("the create is queued");
        assert_eq!(
            self.next(),
            BackendEvent::SessionCreated {
                request: RequestId(1),
                session: session.clone(),
                backend_session: BackendSessionId::new("fake-1"),
                config: fake_config("sonnet", "high"),
            }
        );
        session
    }

    /// Opens `session` on `backend` with `request` and checks that it was reported with
    /// `backend_session` as its id.
    fn create_on(
        &mut self,
        request: RequestId,
        session: &SessionId,
        backend: &BackendId,
        backend_session: &str,
    ) {
        self.backend
            .new_session(request, session.clone(), backend, cwd(), ToolRole::Plain)
            .expect("the create is queued");
        assert_eq!(
            self.next(),
            BackendEvent::SessionCreated {
                request,
                session: session.clone(),
                backend_session: BackendSessionId::new(backend_session),
                config: fake_config("sonnet", "high"),
            }
        );
    }

    fn prompt(&self, session: &SessionId) {
        self.backend
            .prompt(session.clone(), vec![PromptContent::Text("hi".to_owned())])
            .expect("the prompt is queued");
    }
}

/// Looks up the URL of a session's tools.
type ToolUrl = Box<dyn Fn(&SessionId, ToolRole) -> String + Send + Sync>;

/// The next item, or `None` once the channel closed; fails the test when neither happens
/// within `wait`.
fn wait_for<T>(waiter: &Runtime, events: &mut UnboundedReceiver<T>, wait: Duration) -> Option<T> {
    waiter
        .block_on(async { timeout(wait, events.next()).await })
        .expect("an event arrives within the timeout")
}

/// The backend most tests open their sessions on, the only one [`Fake::start`] configures.
fn claude() -> BackendId {
    backend("claude")
}

fn backend(id: &str) -> BackendId {
    BackendId::try_from(id).expect("a valid backend id")
}

/// The fake agent playing `script`, with `env` added to its environment.
fn fake_agent(script: &str, env: Vec<(String, String)>) -> AcpConfig {
    AcpConfig {
        command: PathBuf::from(env!("CARGO_BIN_EXE_fake-agent")),
        args: vec![script.to_owned()],
        env,
        thinking_display: ThinkingDisplay::Summarized,
    }
}

/// The fake agent's `env` script, whose sessions are called `fake-{mark}`.
fn marked(mark: &str) -> AcpConfig {
    fake_agent("env", vec![(MARK_VAR.to_owned(), mark.to_owned())])
}

/// A backend map with `config` as the only adapter, under [`claude`].
fn only_claude(config: AcpConfig) -> HashMap<BackendId, AcpConfig> {
    HashMap::from([(claude(), config)])
}

/// Backends `a` and `b`, the `env` script marked with their own id.
fn a_and_b() -> HashMap<BackendId, AcpConfig> {
    HashMap::from([(backend("a"), marked("a")), (backend("b"), marked("b"))])
}

/// The directory sessions are opened in; the fake agent ignores it.
fn cwd() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn update(session: &SessionId, update: Update) -> BackendEvent {
    BackendEvent::Update {
        session: session.clone(),
        update,
    }
}

/// The fake agent's config with `model` and `effort` as the current values, in the
/// `default` mode Partitur switched it to.
fn fake_config(model: &str, effort: &str) -> Vec<ConfigOption> {
    let mut config = modeless_config(model, effort);
    config.push(ConfigOption {
        id: ConfigId::new("mode"),
        name: "Mode".to_owned(),
        category: ConfigCategory::Mode,
        current: ConfigValueId::new("default"),
        choices: vec![choice("default", "Default"), choice("auto", "Auto")],
    });
    config
}

/// The config of the fake agent's `modeless` script, with `model` and `effort` as the
/// current values.
fn modeless_config(model: &str, effort: &str) -> Vec<ConfigOption> {
    vec![
        ConfigOption {
            id: ConfigId::new("model"),
            name: "Model".to_owned(),
            category: ConfigCategory::Model,
            current: ConfigValueId::new(model),
            choices: vec![choice("sonnet", "Sonnet"), choice("opus", "Opus")],
        },
        ConfigOption {
            id: ConfigId::new("effort"),
            name: "Effort".to_owned(),
            category: ConfigCategory::Effort,
            current: ConfigValueId::new(effort),
            choices: vec![choice("low", "Low"), choice("high", "High")],
        },
    ]
}

fn choice(id: &str, name: &str) -> ConfigChoice {
    ConfigChoice {
        id: ConfigValueId::new(id),
        name: name.to_owned(),
    }
}

/// The updates of the fake's turn before its tool call, from the start of its work on.
fn opening(session: &SessionId) -> Vec<BackendEvent> {
    vec![
        update(session, Update::TurnStarted),
        update(session, Update::AgentThought("thinking…".to_owned())),
        update(session, Update::AgentText("Hello ".to_owned())),
        update(session, Update::AgentText("world".to_owned())),
    ]
}

/// The updates of the fake's turn from its tool call on, the call ending with `status` and
/// `output`.
fn closing(session: &SessionId, status: ToolStatus, output: &str) -> Vec<BackendEvent> {
    vec![
        update(
            session,
            Update::ToolCall {
                id: ToolCallId::new("t1"),
                title: "Read README.md".to_owned(),
                kind: ToolKind::Read,
                status: ToolStatus::Pending,
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                call_result: None,
                widget: None,
            },
        ),
        update(
            session,
            Update::ToolCallUpdate {
                id: ToolCallId::new("t1"),
                title: None,
                kind: None,
                status: Some(status),
                content: Some(vec![ToolContent::Text(output.to_owned())]),
                raw_output: None,
                raw_input: None,
                call_result: None,
                widget: None,
            },
        ),
        update(
            session,
            Update::Usage {
                used: 10,
                size: 1000,
            },
        ),
        update(session, Update::TurnEnded(StopReason::EndTurn)),
    ]
}

/// The permission request the `asks` script sends; it is the session's first.
fn permission_request(session: &SessionId, id: u64) -> BackendEvent {
    update(
        session,
        Update::PermissionRequested {
            id: PermissionId(id),
            tool_call_id: ToolCallId::new("t1"),
            title: "Read README.md".to_owned(),
            kind: ToolKind::Read,
            options: vec![
                PermissionOption {
                    id: PermissionOptionId::new("allow-once"),
                    name: "Allow".to_owned(),
                    kind: PermissionKind::AllowOnce,
                },
                PermissionOption {
                    id: PermissionOptionId::new("reject"),
                    name: "Reject".to_owned(),
                    kind: PermissionKind::RejectOnce,
                },
            ],
            name: None,
            locations: Vec::new(),
            partitur_tool: false,
        },
    )
}

/// Runs a turn of the `asks` script, answers its permission request with `answer` and
/// checks that the tool call ends with `status` and `output`.
fn answer_permission(answer: Option<&str>, status: ToolStatus, output: &str) {
    let mut fake = Fake::start("asks");
    let session = fake.create();
    fake.prompt(&session);
    let mut expected = opening(&session);
    expected.push(permission_request(&session, 0));
    assert_eq!(fake.take(expected.len()), expected);

    fake.backend
        .respond_permission(
            session.clone(),
            PermissionId(0),
            answer.map(PermissionOptionId::new),
        )
        .expect("the answer is queued");
    let expected = closing(&session, status, output);
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn permission_ids_are_unique_across_the_sessions_of_one_backend() {
    let mut fake = Fake::start("asks");
    let first = fake.create();
    fake.prompt(&first);
    let mut expected = opening(&first);
    expected.push(permission_request(&first, 0));
    assert_eq!(fake.take(expected.len()), expected);

    let second = fake.create();
    fake.prompt(&second);
    let mut expected = opening(&second);
    expected.push(permission_request(&second, 1));
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn an_open_sent_before_the_backends_resolve_runs_once_they_do() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let mut fake = Fake::resolving(move || {
        released.recv().expect("the test releases the resolution");
        HashMap::from([(claude(), Ok(fake_agent("happy", Vec::new())))])
    });
    let session = SessionId::new();
    fake.backend
        .new_session(
            RequestId(1),
            session.clone(),
            &claude(),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the create is queued");
    let early = fake
        .waiter
        .block_on(async { timeout(Duration::from_millis(200), fake.events.next()).await });
    assert!(early.is_err(), "an event before the resolution: {early:?}");
    release.send(()).expect("the resolver waits");
    assert_eq!(
        fake.next(),
        BackendEvent::SessionCreated {
            request: RequestId(1),
            session,
            backend_session: BackendSessionId::new("fake-1"),
            config: fake_config("sonnet", "high"),
        }
    );
}

#[test]
fn session_starts_in_default_mode() {
    let mut fake = Fake::start("happy");
    // `create` checks the reported id and the config the mode switch announced (effort
    // `high`, mode `default`), which replaced the one in the create response (effort `low`,
    // mode `auto`). The fake agent refuses any other mode, which would fail the create.
    let session = fake.create();
    assert_eq!(
        fake.backend.cancel(session.clone()),
        Ok(()),
        "the created session is live"
    );
}

#[test]
fn a_session_without_modes_is_reported_without_a_mode_switch_and_with_a_notice() {
    let mut fake = Fake::start("modeless");
    let session = SessionId::new();
    fake.backend
        .new_session(
            RequestId(6),
            session.clone(),
            &claude(),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the create is queued");
    // A mode switch would have announced effort `high`.
    assert_eq!(
        fake.take(2),
        [
            BackendEvent::SessionCreated {
                request: RequestId(6),
                session: session.clone(),
                backend_session: BackendSessionId::new("fake-1"),
                config: modeless_config("sonnet", "low"),
            },
            update(
                &session,
                Update::Notice(
                    "this agent offers no default (ask) mode; permissions follow the agent's \
                     own policy"
                        .to_owned()
                )
            ),
        ]
    );
}

/// The events of one whole turn of the fake's `happy` script.
fn whole_turn(session: &SessionId) -> Vec<BackendEvent> {
    let mut expected = opening(session);
    expected.extend(closing(session, ToolStatus::Completed, "ok"));
    expected
}

#[test]
fn prompted_turn_ends_on_idle() {
    let mut fake = Fake::start("happy");
    let session = fake.create();
    fake.prompt(&session);
    let expected = whole_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
    // The next event answers a later command: the turn ended exactly once.
    fake.backend
        .set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("opus"),
        )
        .expect("the change is queued");
    assert_eq!(
        fake.next(),
        update(&session, Update::ConfigOptions(fake_config("opus", "high")))
    );
}

#[test]
fn agent_started_cycle_is_reported() {
    let mut fake = Fake::start("continues");
    let session = fake.create();
    fake.prompt(&session);
    let mut expected = whole_turn(&session);
    // The `idle` that only says the background work ended reports nothing.
    expected.extend([
        update(&session, Update::TurnStarted),
        update(&session, Update::AgentText("background done".to_owned())),
        update(&session, Update::TurnEnded(StopReason::EndTurn)),
    ]);
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn running_background_work_is_reported_around_the_turn_end() {
    let mut fake = Fake::start("subagent-runs");
    let session = fake.create();
    fake.prompt(&session);
    // The list names a subagent and a command, and both count. The turn-ending `idle`
    // repeats the list, which changes nothing, and the subagent's own list is not the
    // session's.
    let expected = vec![
        update(&session, Update::TurnStarted),
        update(&session, Update::RunningBackground(2)),
        update(&session, Update::AgentText("main done".to_owned())),
        update(&session, Update::TurnEnded(StopReason::EndTurn)),
        update(&session, Update::RunningBackground(0)),
    ];
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn stop_cancels_the_listed_subagents() {
    let mut fake = Fake::start("subagent-waits");
    let session = fake.create();
    fake.prompt(&session);
    let ended = vec![
        update(&session, Update::TurnStarted),
        update(&session, Update::RunningBackground(1)),
        update(&session, Update::AgentText("main done".to_owned())),
        update(&session, Update::TurnEnded(StopReason::EndTurn)),
    ];
    assert_eq!(fake.take(ended.len()), ended);
    // The turn is over, but the subagent it left running is stopped with the session: the
    // session's own cancel spares it, the cancel naming it stops it.
    fake.backend
        .cancel(session.clone())
        .expect("the cancel is queued");
    let stopped = vec![
        update(&session, Update::AgentText("subagent spared".to_owned())),
        update(&session, Update::RunningBackground(0)),
    ];
    assert_eq!(fake.take(stopped.len()), stopped);
}

#[test]
fn foreign_session_state_is_ignored() {
    // The subagent's own turn would start and end this one early, before its text.
    let mut fake = Fake::start("subagent");
    let session = fake.create();
    fake.prompt(&session);
    let expected = whole_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn prompt_cancelled_before_insertion_ends_turn_cancelled() {
    let mut fake = Fake::start("prompt-cancelled");
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(
        fake.next(),
        update(&session, Update::TurnEnded(StopReason::Cancelled))
    );
}

#[test]
fn a_permission_request_arrives_and_the_answer_completes_the_tool_call() {
    answer_permission(Some("allow-once"), ToolStatus::Completed, "ok");
}

#[test]
fn a_rejected_permission_fails_the_tool_call() {
    answer_permission(Some("reject"), ToolStatus::Failed, "reject");
}

#[test]
fn a_cancelled_permission_fails_the_tool_call() {
    answer_permission(None, ToolStatus::Failed, "cancelled");
}

#[test]
fn an_answer_to_an_unknown_permission_is_dropped_and_the_session_goes_on() {
    let mut fake = Fake::start("happy");
    let session = fake.create();
    fake.backend
        .respond_permission(
            session.clone(),
            PermissionId(7),
            Some(PermissionOptionId::new("allow-once")),
        )
        .expect("the answer is queued");
    fake.prompt(&session);
    let expected = whole_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn set_config_sends_the_option_and_reports_the_new_options() {
    let mut fake = Fake::start("happy");
    let session = fake.create();
    fake.backend
        .set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("opus"),
        )
        .expect("the change is queued");
    assert_eq!(
        fake.next(),
        update(&session, Update::ConfigOptions(fake_config("opus", "high")))
    );
}

#[test]
fn a_refused_config_change_is_reported_as_rejected() {
    // `opus` is a value the agent accepts for `model`, so only the option decides: the agent
    // changes nothing but `model` and refuses `effort`.
    // A model the agent does not offer is refused as well.
    let mut fake = Fake::start("happy");
    let session = fake.create();
    for (option, value) in [("effort", "opus"), ("model", "haiku")] {
        fake.backend
            .set_config(
                session.clone(),
                &ConfigId::new(option),
                &ConfigValueId::new(value),
            )
            .expect("the change is queued");
        let event = fake.next();
        assert!(
            matches!(
                &event,
                BackendEvent::Update {
                    session: s,
                    update: Update::ConfigRejected { option: rejected, value: refused, error },
                } if *s == session
                    && rejected.as_str() == option
                    && refused.as_str() == value
                    && error.contains("refusing")
            ),
            "unexpected event: {event:?}"
        );
    }
}

#[test]
fn a_prompt_refused_during_work_ends_the_turn_once() {
    let mut fake = Fake::start("refused-working");
    let session = fake.create();
    fake.prompt(&session);
    // Whether the refusal or the `idle` arrives first, the `idle` ends the work's turn and
    // the refusal ends nothing more.
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnStarted),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
    fake.backend
        .set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("opus"),
        )
        .expect("the change is queued");
    assert_eq!(
        fake.next(),
        update(&session, Update::ConfigOptions(fake_config("opus", "high")))
    );
}

#[test]
fn a_prompt_refused_during_work_that_goes_idle_unexplained_fails_once() {
    let mut fake = Fake::start("refused-idles");
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(fake.next(), update(&session, Update::TurnStarted));
    // The cancel makes the agent's work go idle without a stop reason, a full round trip
    // after the refusal; then a turn of the agent's own starts.
    fake.backend
        .cancel(session.clone())
        .expect("the cancel is queued");
    let failed = fake.next();
    assert!(
        matches!(
            &failed,
            BackendEvent::Update { session: s, update: Update::Failed(error) }
                if *s == session && error.contains("Authentication required")
        ),
        "unexpected event: {failed:?}"
    );
    assert_eq!(
        fake.take(3),
        [
            update(&session, Update::TurnStarted),
            update(&session, Update::AgentText("on its own".to_owned())),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
}

#[test]
fn resume_replays_and_drops_history() {
    let mut fake = Fake::start("happy");
    let session = SessionId::new();
    fake.backend
        .load_session(
            RequestId(2),
            session.clone(),
            &claude(),
            &BackendSessionId::new("fake-1"),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the load is queued");
    assert_eq!(
        fake.next(),
        BackendEvent::SessionLoaded {
            request: RequestId(2),
            session: session.clone(),
            config: fake_config("sonnet", "high"),
        }
    );

    // The replayed turn would arrive first if it had been forwarded, and a replayed state
    // would hide the start of the next turn.
    fake.prompt(&session);
    let expected = whole_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn a_failing_load_reports_request_failed() {
    let mut fake = Fake::start("load-fails");
    let session = SessionId::new();
    fake.backend
        .load_session(
            RequestId(3),
            session.clone(),
            &claude(),
            &BackendSessionId::new("gone"),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the load is queued");
    let event = fake.next();
    assert!(
        matches!(
            &event,
            BackendEvent::RequestFailed { request: RequestId(3), session: s, error }
                if *s == session && error.contains("no session gone")
        ),
        "unexpected event: {event:?}"
    );
    assert_eq!(
        fake.backend.cancel(session.clone()),
        Err(BackendError::UnknownSession(session))
    );
}

#[test]
fn prompt_rejected_fails_turn() {
    let mut fake = Fake::start("prompt-fails");
    let session = fake.create();
    // The second turn fails like the first, with no `SessionClosed` in between: the
    // session stays open after a failed turn.
    for _ in 0..2 {
        fake.prompt(&session);
        let event = fake.next();
        assert!(
            matches!(
                &event,
                BackendEvent::Update { session: s, update: Update::Failed(error) }
                    if *s == session && error.contains("Authentication required")
            ),
            "unexpected event: {event:?}"
        );
    }
}

#[test]
fn a_dying_agent_reports_request_failed_for_the_create() {
    let mut fake = Fake::start("dies");
    let session = SessionId::new();
    fake.backend
        .new_session(
            RequestId(4),
            session.clone(),
            &claude(),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the create is queued");
    let event = fake.next();
    assert!(
        matches!(
            &event,
            BackendEvent::RequestFailed { request: RequestId(4), session: s, .. } if *s == session
        ),
        "unexpected event: {event:?}"
    );
    assert_eq!(
        fake.backend.cancel(session.clone()),
        Err(BackendError::UnknownSession(session))
    );
}

#[test]
fn an_agent_that_exits_after_the_report_closes_the_session() {
    let mut fake = Fake::start("dies-later");
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnStarted),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
    assert_eq!(
        fake.next(),
        BackendEvent::SessionClosed {
            session: session.clone(),
            error: "the agent exited".to_owned(),
        }
    );
    assert_eq!(
        fake.backend.cancel(session.clone()),
        Err(BackendError::UnknownSession(session))
    );
}

#[test]
fn cancel_ends_the_turn_with_cancelled() {
    let mut fake = Fake::start("slow");
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnStarted),
            update(&session, Update::AgentText("working…".to_owned())),
        ]
    );
    let started = Instant::now();
    fake.backend
        .cancel(session.clone())
        .expect("the cancel is queued");
    assert_eq!(
        fake.next_within(SLOW_WAIT),
        update(&session, Update::TurnEnded(StopReason::Cancelled))
    );
    assert!(
        started.elapsed() < WAIT,
        "the turn ended on the cancel, not on its own"
    );
}

#[test]
fn shutdown_abandons_an_agent_that_never_answers() {
    // The session never gets past `initialize`, so its connection cannot notice that
    // shutdown closed its commands, and the agent outlives the grace period.
    let fake = Fake::start("hangs");
    fake.backend
        .new_session(
            RequestId(5),
            SessionId::new(),
            &claude(),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the create is queued");
    let Fake {
        backend,
        mut events,
        waiter,
        ..
    } = fake;

    let started = Instant::now();
    backend.shutdown();
    let waited = started.elapsed();
    assert!(
        waited >= SHUTDOWN_GRACE,
        "shutdown returned after {waited:?}"
    );
    assert!(
        waited < SHUTDOWN_GRACE + WAIT,
        "shutdown returned after {waited:?}"
    );
    assert_eq!(
        wait_for(&waiter, &mut events, WAIT),
        None,
        "an abandoned session reports nothing"
    );
}

#[test]
fn two_backends_spawn_with_their_own_environments() {
    let mut fake = Fake::with(a_and_b());
    fake.create_on(RequestId(1), &SessionId::new(), &backend("a"), "fake-a");
    fake.create_on(RequestId(2), &SessionId::new(), &backend("b"), "fake-b");
}

#[test]
fn an_unknown_backend_id_fails_the_request() {
    let mut fake = Fake::start("env");
    let session = SessionId::new();
    let missing = backend("missing");
    fake.backend
        .new_session(
            RequestId(7),
            session.clone(),
            &missing,
            cwd(),
            ToolRole::Plain,
        )
        .expect("the create is queued");
    fake.backend
        .load_session(
            RequestId(8),
            session.clone(),
            &missing,
            &BackendSessionId::new("fake-1"),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the load is queued");
    let failed = |request| BackendEvent::RequestFailed {
        request,
        session: session.clone(),
        error: "backend missing is not configured".to_owned(),
    };
    assert_eq!(fake.take(2), [failed(RequestId(7)), failed(RequestId(8))]);
    assert_eq!(
        fake.backend.cancel(session.clone()),
        Err(BackendError::UnknownSession(session.clone()))
    );
    // The configured backend still opens the session; unmarked, `env` names it so.
    fake.create_on(RequestId(9), &session, &claude(), "fake-unmarked");
}

#[test]
fn replacing_a_live_session_closes_the_old_connection_silently() {
    let mut fake = Fake::with(a_and_b());
    let session = SessionId::new();
    fake.create_on(RequestId(1), &session, &backend("a"), "fake-a");
    // A `SessionClosed` or `RequestFailed` from the old connection would arrive first.
    fake.create_on(RequestId(2), &session, &backend("b"), "fake-b");
    fake.prompt(&session);
    let expected = whole_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);

    let Fake {
        backend,
        mut events,
        waiter,
        ..
    } = fake;
    backend.shutdown();
    assert_eq!(
        wait_for(&waiter, &mut events, WAIT),
        None,
        "neither connection reports anything more"
    );
}

/// Shuts `fake` down and checks that nothing more was reported.
fn assert_silent_until_shutdown(fake: Fake, why: &str) {
    let Fake {
        backend,
        mut events,
        waiter,
        ..
    } = fake;
    backend.shutdown();
    assert_eq!(wait_for(&waiter, &mut events, WAIT), None, "{why}");
}

#[test]
fn closing_a_running_session_ends_it_silently() {
    let mut fake = Fake::start("slow");
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnStarted),
            update(&session, Update::AgentText("working…".to_owned())),
        ]
    );

    fake.backend
        .close_session(session.clone())
        .expect("the close is queued");

    // Commands run in order, so the close is done once the next session is reported; a
    // cancelled turn end or a `SessionClosed` of the closed one would arrive before it.
    let next = fake.create();
    assert_eq!(
        fake.backend.cancel(session.clone()),
        Err(BackendError::UnknownSession(session))
    );
    fake.backend.cancel(next).expect("the other session lives");
    assert_silent_until_shutdown(fake, "the closed session reports nothing more");
}

#[test]
fn closing_a_session_still_being_opened_drops_its_report() {
    let mut fake = Fake::start("happy");
    let opening = SessionId::new();
    fake.backend
        .new_session(
            RequestId(1),
            opening.clone(),
            &claude(),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the create is queued");

    fake.backend
        .close_session(opening.clone())
        .expect("a session being opened can be closed");
    fake.backend
        .close_session(opening.clone())
        .expect("closing it again is no error");

    // Its `SessionCreated` would arrive before the next session's.
    fake.create();
    assert_eq!(
        fake.backend.cancel(opening.clone()),
        Err(BackendError::UnknownSession(opening))
    );
    assert_silent_until_shutdown(fake, "the closed session reports nothing");
}

/// The children the fake agent's `orchestrates` script proposes.
fn proposed_children() -> OrchestratorTool {
    let child = |repository, prompt: &str, title: &str| ProposedChild {
        repository: RepositoryId::try_from(repository).expect("a valid repository id"),
        prompt: prompt.to_owned(),
        title: title.to_owned(),
        title_source: partitur_core::TitleSource::Given,
        base: None,
    };
    OrchestratorTool::ProposeChildren(vec![
        child("api", "Add the endpoint", "API endpoint"),
        child("web", "Add the page", "Web page"),
    ])
}

/// Runs a turn of the `orchestrates` script in `session`, checks that it starts and that its
/// tool call reaches the tool server's calls, answers it and checks that the agent streams
/// the answer back and ends the turn.
fn round_trip_a_tool_call(fake: &mut Fake, session: &SessionId) {
    fake.prompt(session);
    assert_eq!(fake.next(), update(session, Update::TurnStarted));
    let call = fake.next_call();
    assert_eq!(call.session, *session);
    assert_eq!(call.tool, proposed_children().into());

    call.reply.send(Ok(PROPOSAL_RECORDED.into()));
    assert_eq!(
        fake.take(2),
        [
            update(session, Update::AgentText(PROPOSAL_RECORDED.to_owned())),
            update(session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
}

#[test]
fn an_orchestrator_session_registers_the_tools_and_a_call_round_trips() {
    let mut fake = Fake::start("orchestrates");
    let session = fake.create_as(ToolRole::Orchestrator);
    round_trip_a_tool_call(&mut fake, &session);
}

/// Runs a turn of the `reports-tools` script in `session` and returns the tool server URL the
/// session was opened with, after checking that the agent's superpowers call reaches the tool
/// server's calls and that its answer is streamed back.
fn tools_url_of(fake: &mut Fake, session: &SessionId) -> String {
    fake.prompt(session);
    assert_eq!(fake.next(), update(session, Update::TurnStarted));
    let call = fake.next_call();
    assert_eq!(call.session, *session);
    assert_eq!(
        call.tool,
        PartiturTool::from(SuperpowersV7Event::StartBrainstorming)
    );

    call.reply.send(Ok("recorded".into()));
    let [text, ended] = fake.take(2).try_into().expect("two events");
    assert_eq!(
        ended,
        update(session, Update::TurnEnded(StopReason::EndTurn))
    );
    let BackendEvent::Update {
        update: Update::AgentText(text),
        ..
    } = text
    else {
        panic!("expected the agent's text, got {text:?}");
    };
    text.strip_suffix(" recorded")
        .expect("the answer follows the URL")
        .to_owned()
}

/// The path of `url` after the tool server's address.
fn path_of(url: &str) -> &str {
    url.strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, path)| path)
        .expect("an http URL with a path")
}

#[test]
fn a_repository_session_registers_the_tools_too() {
    let mut fake = Fake::start("reports-tools");
    let session = fake.create_as(ToolRole::Plain);
    let url = tools_url_of(&mut fake, &session);
    assert_eq!(path_of(&url), session.to_string());
}

#[test]
fn an_orchestrator_session_registers_the_orchestrator_url() {
    let mut fake = Fake::start("reports-tools");
    let session = fake.create_as(ToolRole::Orchestrator);
    let url = tools_url_of(&mut fake, &session);
    assert_eq!(path_of(&url), format!("{session}/orchestrator"));
}

#[test]
fn a_child_session_registers_the_child_url() {
    let mut fake = Fake::start("reports-tools");
    let session = fake.create_as(ToolRole::Child);
    let url = tools_url_of(&mut fake, &session);
    assert_eq!(path_of(&url), format!("{session}/child"));
}

#[test]
fn a_loaded_orchestrator_session_registers_the_tools_again() {
    let mut fake = Fake::start("orchestrates");
    let session = SessionId::new();
    fake.backend
        .load_session(
            RequestId(2),
            session.clone(),
            &claude(),
            &BackendSessionId::new("fake-1"),
            cwd(),
            ToolRole::Orchestrator,
        )
        .expect("the load is queued");
    assert_eq!(
        fake.next(),
        BackendEvent::SessionLoaded {
            request: RequestId(2),
            session: session.clone(),
            config: fake_config("sonnet", "high"),
        }
    );
    round_trip_a_tool_call(&mut fake, &session);
}

/// A fresh file for the fake agent to log the methods of its untyped requests to.
fn method_log() -> PathBuf {
    std::env::temp_dir().join(format!("partitur-fake-{}.log", SessionId::new()))
}

/// Starts a backend whose only adapter is the fake agent playing `script`, logging the methods
/// of its untyped requests to `log`.
fn logging(script: &str, log: &Path) -> Fake {
    let env = vec![(LOG_VAR.to_owned(), log.display().to_string())];
    Fake::with(only_claude(fake_agent(script, env)))
}

/// The methods the fake agent logged to `log`, oldest first; none when it logged nothing.
fn methods(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The Jira issue widget the `widgets` scripts serve.
fn jira_widget() -> Widget {
    Widget {
        server: "atlassian".to_owned(),
        uri: "ui://jira/issue".to_owned(),
        html: "<p>issue</p>".to_owned(),
        meta: "{}".to_owned(),
    }
}

/// The announcement of the widget tool call `id`, with its widget in `widget`.
fn widget_call(session: &SessionId, id: &str, widget: WidgetSlot) -> BackendEvent {
    update(
        session,
        Update::ToolCall {
            id: ToolCallId::new(id),
            title: "getJiraIssue".to_owned(),
            kind: ToolKind::Other,
            status: ToolStatus::Pending,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: Some(r#"{"issueKey":"PROJ-1"}"#.to_owned()),
            call_result: None,
            widget: Some(widget),
        },
    )
}

/// The change of the tool call `id` to `widget`, and nothing else.
fn widget_change(session: &SessionId, id: &str, widget: WidgetSlot) -> BackendEvent {
    update(
        session,
        Update::ToolCallUpdate {
            id: ToolCallId::new(id),
            title: None,
            kind: None,
            status: None,
            content: None,
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: Some(widget),
        },
    )
}

/// Checks that `event` completes the widget tool call `id` with its text and its MCP result.
fn assert_widget_result(event: &BackendEvent, session: &SessionId, id: &str) {
    let BackendEvent::Update {
        session: s,
        update:
            Update::ToolCallUpdate {
                id: call,
                title: None,
                kind: None,
                status: Some(ToolStatus::Completed),
                content: Some(content),
                raw_output: None,
                raw_input: None,
                call_result: Some(result),
                widget: None,
            },
    } = event
    else {
        panic!("expected the completion of {id}, got {event:?}");
    };
    assert_eq!((s, call.as_str()), (session, id));
    assert_eq!(
        *content,
        [ToolContent::Text("PROJ-1: Fix the login".to_owned())]
    );
    let result: serde_json::Value = serde_json::from_str(result).expect("the result is JSON");
    assert_eq!(
        result,
        serde_json::json!({
            "content": [{"type": "text", "text": "PROJ-1: Fix the login"}],
            "structuredContent": {"key": "PROJ-1", "summary": "Fix the login"},
            "isError": false,
        })
    );
}

/// The widget being read from the `atlassian` server.
fn loading() -> WidgetSlot {
    WidgetSlot::Loading {
        server: "atlassian".to_owned(),
        uri: "ui://jira/issue".to_owned(),
    }
}

#[test]
fn widget_tool_call_reports_loading_then_ready() {
    let log = method_log();
    let mut fake = logging("widgets", &log);
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(fake.next(), update(&session, Update::TurnStarted));
    assert_eq!(fake.next(), widget_call(&session, "w1", loading()));
    assert_widget_result(&fake.next(), &session, "w1");
    // The read is answered after the agent's turn, which never waits for it.
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
            widget_change(&session, "w1", WidgetSlot::Ready(jira_widget())),
        ]
    );
    assert_eq!(
        methods(&log),
        ["_partitur/widget_tools", "_partitur/read_widget"]
    );
}

#[test]
fn second_call_uses_cached_widget() {
    let log = method_log();
    let mut fake = logging("widgets", &log);
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(fake.take(5).len(), 5);
    fake.prompt(&session);
    assert_eq!(fake.next(), update(&session, Update::TurnStarted));
    let ready = WidgetSlot::Ready(jira_widget());
    assert_eq!(fake.next(), widget_call(&session, "w2", ready));
    assert_widget_result(&fake.next(), &session, "w2");
    assert_eq!(
        fake.next(),
        update(&session, Update::TurnEnded(StopReason::EndTurn))
    );
    assert_eq!(
        methods(&log),
        ["_partitur/widget_tools", "_partitur/read_widget"]
    );
}

#[test]
fn read_failure_reports_failed() {
    let mut fake = Fake::start("widgets-failing");
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(fake.next(), update(&session, Update::TurnStarted));
    assert_eq!(fake.next(), widget_call(&session, "w1", loading()));
    assert_widget_result(&fake.next(), &session, "w1");
    assert_eq!(
        fake.next(),
        update(&session, Update::TurnEnded(StopReason::EndTurn))
    );
    let failed = fake.next();
    let BackendEvent::Update {
        update:
            Update::ToolCallUpdate {
                id,
                widget: Some(WidgetSlot::Failed { server, uri, error }),
                ..
            },
        ..
    } = &failed
    else {
        panic!("expected the failed widget, got {failed:?}");
    };
    assert_eq!(
        (id.as_str(), server.as_str(), uri.as_str()),
        ("w1", "atlassian", "ui://jira/issue")
    );
    assert!(error.contains("Server not found: atlassian"), "{error}");
}

#[test]
fn no_capability_sends_no_widget_requests() {
    let log = method_log();
    let mut fake = logging("happy", &log);
    let session = fake.create();
    fake.prompt(&session);
    let expected = whole_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
    assert_eq!(methods(&log), Vec::<String>::new());
}

/// The announcement of the call `id` of `mcp__other__lookup`, a tool without a widget.
fn unknown_call(session: &SessionId, id: &str) -> BackendEvent {
    update(
        session,
        Update::ToolCall {
            id: ToolCallId::new(id),
            title: "lookup".to_owned(),
            kind: ToolKind::Other,
            status: ToolStatus::Pending,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
        },
    )
}

#[test]
fn late_widget_tool_is_found_on_refresh() {
    let log = method_log();
    let mut fake = logging("widgets-late", &log);
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(fake.next(), update(&session, Update::TurnStarted));
    // The list at the session's start named no tool, so the call carries no widget yet.
    let BackendEvent::Update {
        update: Update::ToolCall { id, widget, .. },
        ..
    } = fake.next()
    else {
        panic!("expected the widget tool call");
    };
    assert_eq!((id.as_str(), widget), ("w1", None));
    assert_widget_result(&fake.next(), &session, "w1");
    // The other unknown tool waits for the same list instead of asking for one of its own.
    assert_eq!(fake.next(), unknown_call(&session, "u1"));
    assert_eq!(
        fake.take(3),
        [
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
            widget_change(&session, "w1", loading()),
            widget_change(&session, "w1", WidgetSlot::Ready(jira_widget())),
        ]
    );

    // The tool still unknown after the list asks for no other.
    fake.prompt(&session);
    assert_eq!(fake.next(), update(&session, Update::TurnStarted));
    let ready = WidgetSlot::Ready(jira_widget());
    assert_eq!(fake.next(), widget_call(&session, "w2", ready));
    assert_widget_result(&fake.next(), &session, "w2");
    assert_eq!(
        fake.take(2),
        [
            unknown_call(&session, "u2"),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
    // The agent answers in order: once this change is answered, it took in every request sent
    // before it.
    fake.backend
        .set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("opus"),
        )
        .expect("the change is queued");
    assert_eq!(
        fake.next(),
        update(&session, Update::ConfigOptions(fake_config("opus", "high")))
    );
    assert_eq!(
        methods(&log),
        [
            "_partitur/widget_tools",
            "_partitur/widget_tools",
            "_partitur/read_widget"
        ]
    );
}

#[test]
fn a_server_status_notice_lists_the_widget_tools_again() {
    let log = method_log();
    let mut fake = logging("widgets-later", &log);
    let session = fake.create();
    fake.prompt(&session);
    assert_eq!(
        fake.take(3),
        [
            update(&session, Update::TurnStarted),
            update(
                &session,
                Update::Notice("MCP server atlassian connected".to_owned())
            ),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
    fake.prompt(&session);
    assert_eq!(fake.next(), update(&session, Update::TurnStarted));
    assert_eq!(fake.next(), widget_call(&session, "w1", loading()));
    assert_widget_result(&fake.next(), &session, "w1");
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
            widget_change(&session, "w1", WidgetSlot::Ready(jira_widget())),
        ]
    );
    assert_eq!(
        methods(&log),
        [
            "_partitur/widget_tools",
            "_partitur/widget_tools",
            "_partitur/read_widget"
        ]
    );
}

/// The fake ACP v1 agent playing `script`, with `args` after it.
fn fake_copilot(script: &str, args: Vec<String>) -> AcpConfig {
    AcpConfig {
        command: PathBuf::from(env!("CARGO_BIN_EXE_fake-copilot")),
        args: std::iter::once(script.to_owned()).chain(args).collect(),
        env: Vec::new(),
        thinking_display: ThinkingDisplay::Summarized,
    }
}

/// Starts a backend whose only adapter, [`claude`], is the fake v1 agent playing `script`.
fn copilot(script: &str) -> Fake {
    Fake::with(only_claude(fake_copilot(script, vec![])))
}

/// Copilot's asking mode, which Partitur switches its sessions to.
const AGENT_MODE: &str = "https://agentclientprotocol.com/protocol/session-modes#agent";

/// The current value of the option `id` in `config`, if it has one.
fn current<'a>(config: &'a [ConfigOption], id: &str) -> Option<&'a str> {
    config
        .iter()
        .find(|option| option.id.as_str() == id)
        .map(|option| option.current.as_str())
}

/// The ids of `config`'s options, in order.
fn config_ids(config: &[ConfigOption]) -> Vec<&str> {
    config.iter().map(|option| option.id.as_str()).collect()
}

/// Creates a session on the fake v1 agent, checks that it was reported as `cp-1`, and returns
/// it with the config it was reported with.
fn create_on_copilot(fake: &mut Fake) -> (SessionId, Vec<ConfigOption>) {
    create_on_copilot_as(fake, ToolRole::Plain)
}

/// Creates a session in `role` on the fake v1 agent, as [`create_on_copilot`] does.
fn create_on_copilot_as(fake: &mut Fake, role: ToolRole) -> (SessionId, Vec<ConfigOption>) {
    let session = SessionId::new();
    fake.backend
        .new_session(RequestId(1), session.clone(), &claude(), cwd(), role)
        .expect("the create is queued");
    let BackendEvent::SessionCreated {
        request,
        session: created,
        backend_session,
        config,
    } = fake.next()
    else {
        panic!("the session was not created");
    };
    assert_eq!(
        (request, created, backend_session),
        (RequestId(1), session.clone(), BackendSessionId::new("cp-1"))
    );
    (session, config)
}

/// The fake v1 agent's tool call `t1`, pending.
fn v1_call(session: &SessionId) -> BackendEvent {
    update(
        session,
        Update::ToolCall {
            id: ToolCallId::new("t1"),
            title: "Read README.md".to_owned(),
            kind: ToolKind::Read,
            status: ToolStatus::Pending,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
        },
    )
}

/// The fake v1 agent's change ending the tool call `t1` with `status` and `output`.
fn v1_call_ended(session: &SessionId, status: ToolStatus, output: &str) -> BackendEvent {
    update(
        session,
        Update::ToolCallUpdate {
            id: ToolCallId::new("t1"),
            title: None,
            kind: None,
            status: Some(status),
            content: Some(vec![ToolContent::Text(output.to_owned())]),
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: None,
        },
    )
}

/// The events of one whole turn of the fake v1 agent's `happy` script.
fn v1_turn(session: &SessionId) -> Vec<BackendEvent> {
    vec![
        update(session, Update::TurnStarted),
        update(session, Update::AgentText("Hello ".to_owned())),
        update(session, Update::AgentText("world".to_owned())),
        v1_call(session),
        v1_call_ended(session, ToolStatus::Completed, "ok"),
        update(session, Update::TurnEnded(StopReason::EndTurn)),
    ]
}

#[test]
fn a_v1_agent_opens_a_session() {
    let mut fake = copilot("happy");
    let (session, config) = create_on_copilot(&mut fake);
    assert_eq!(config_ids(&config), ["mode", "allow_all", "model"]);
    assert_eq!(fake.backend.cancel(session), Ok(()), "the session is live");
}

#[test]
fn a_v1_session_is_switched_to_asking() {
    let mut fake = copilot("happy");
    // The fake opens its sessions in `#autopilot` with `allow_all` on and accepts only the
    // asking values, so any other switch would fail the create.
    let (session, config) = create_on_copilot(&mut fake);
    assert_eq!(current(&config, "mode"), Some(AGENT_MODE));
    assert_eq!(current(&config, "allow_all"), Some("off"));
    assert_eq!(current(&config, "model"), Some("gpt-5"));
    // No notice follows: the agent asks before acting.
    assert_nothing_more(&mut fake, &session);
}

#[test]
fn a_v1_session_without_asking_settings_is_reported_with_the_notice() {
    let mut fake = copilot("modeless");
    let session = SessionId::new();
    fake.backend
        .new_session(
            RequestId(1),
            session.clone(),
            &claude(),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the create is queued");
    let created = fake.next();
    assert!(
        matches!(
            &created,
            BackendEvent::SessionCreated { session: s, config, .. }
                if *s == session && config_ids(config) == ["model"]
        ),
        "unexpected event: {created:?}"
    );
    assert_eq!(
        fake.next(),
        update(
            &session,
            Update::Notice(
                "this agent offers no default (ask) mode; permissions follow the agent's own \
                 policy"
                    .to_owned()
            )
        )
    );
}

#[test]
fn a_v1_agent_runs_in_the_process_its_v2_initialize_started() {
    let log = method_log();
    let mut config = fake_copilot("happy", vec![]);
    config.env = vec![(LOG_VAR.to_owned(), log.display().to_string())];
    let mut fake = Fake::with(only_claude(config));
    let (session, _) = create_on_copilot(&mut fake);
    fake.prompt(&session);
    let expected = v1_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
    // A second process would have been initialized again, by the v1 client.
    assert_eq!(
        methods(&log),
        [
            "initialize",
            "session/new",
            "session/set_config_option",
            "session/set_config_option",
            "session/prompt"
        ]
    );
    let _ = std::fs::remove_file(log);
}

#[test]
fn a_v1_turn_streams_and_ends_on_the_response() {
    let mut fake = copilot("happy");
    let (session, _) = create_on_copilot(&mut fake);
    fake.prompt(&session);
    let expected = v1_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
    // The next event answers a later command: the turn ended exactly once.
    fake.backend
        .set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("gpt-5"),
        )
        .expect("the change is queued");
    let next = fake.next();
    assert!(
        matches!(
            &next,
            BackendEvent::Update {
                update: Update::ConfigOptions(_),
                ..
            }
        ),
        "unexpected event: {next:?}"
    );
}

#[test]
fn a_v1_load_replays_into_a_closed_gate() {
    let mut fake = copilot("happy");
    let session = SessionId::new();
    fake.backend
        .load_session(
            RequestId(2),
            session.clone(),
            &claude(),
            &BackendSessionId::new("cp-1"),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the load is queued");
    let loaded = fake.next();
    assert!(
        matches!(
            &loaded,
            BackendEvent::SessionLoaded { request: RequestId(2), session: s, config }
                if *s == session
                    && config_ids(config) == ["mode", "allow_all", "model"]
                    && current(config, "mode") == Some(AGENT_MODE)
                    && current(config, "allow_all") == Some("off")
        ),
        "unexpected event: {loaded:?}"
    );
    // The replayed `earlier` and `earlier reply` would arrive first if they were forwarded.
    fake.prompt(&session);
    let expected = v1_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
}

#[test]
fn a_failing_v1_load_reports_request_failed() {
    let mut fake = copilot("load-fails");
    let session = SessionId::new();
    fake.backend
        .load_session(
            RequestId(3),
            session.clone(),
            &claude(),
            &BackendSessionId::new("gone"),
            cwd(),
            ToolRole::Plain,
        )
        .expect("the load is queued");
    let event = fake.next();
    assert!(
        matches!(
            &event,
            BackendEvent::RequestFailed { request: RequestId(3), session: s, error }
                if *s == session && error.contains("no session gone")
        ),
        "unexpected event: {event:?}"
    );
}

#[test]
fn a_refused_v1_prompt_fails_the_turn() {
    let mut fake = copilot("prompt-fails");
    let (session, _) = create_on_copilot(&mut fake);
    for _ in 0..2 {
        fake.prompt(&session);
        assert_eq!(fake.next(), update(&session, Update::TurnStarted));
        let failed = fake.next();
        assert!(
            matches!(
                &failed,
                BackendEvent::Update { session: s, update: Update::Failed(error) }
                    if *s == session && error.contains("Authentication required")
            ),
            "unexpected event: {failed:?}"
        );
    }
}

/// Runs a turn of the fake v1 agent's `asks` script, answers its permission request with
/// `answer` and checks that the tool call ends with `status` and `output`.
fn answer_v1_permission(answer: Option<&str>, status: ToolStatus, output: &str) {
    let mut fake = copilot("asks");
    let (session, _) = create_on_copilot(&mut fake);
    fake.prompt(&session);
    let asked = update(
        &session,
        Update::PermissionRequested {
            id: PermissionId(0),
            tool_call_id: ToolCallId::new("t1"),
            title: "Read README.md".to_owned(),
            kind: ToolKind::Read,
            options: vec![
                PermissionOption {
                    id: PermissionOptionId::new("allow_once"),
                    name: "Allow".to_owned(),
                    kind: PermissionKind::AllowOnce,
                },
                PermissionOption {
                    id: PermissionOptionId::new("reject_once"),
                    name: "Reject".to_owned(),
                    kind: PermissionKind::RejectOnce,
                },
            ],
            name: None,
            locations: Vec::new(),
            partitur_tool: false,
        },
    );
    let expected = vec![
        update(&session, Update::TurnStarted),
        v1_call(&session),
        asked,
    ];
    assert_eq!(fake.take(expected.len()), expected);
    fake.backend
        .respond_permission(
            session.clone(),
            PermissionId(0),
            answer.map(PermissionOptionId::new),
        )
        .expect("the answer is queued");
    assert_eq!(
        fake.take(2),
        [
            v1_call_ended(&session, status, output),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
}

#[test]
fn a_v1_permission_request_is_asked_and_the_answer_reaches_the_agent() {
    answer_v1_permission(Some("allow_once"), ToolStatus::Completed, "ok");
}

#[test]
fn a_rejected_v1_permission_fails_the_tool_call() {
    answer_v1_permission(Some("reject_once"), ToolStatus::Failed, "reject_once");
}

#[test]
fn a_cancelled_v1_permission_reaches_the_agent_as_cancelled() {
    answer_v1_permission(None, ToolStatus::Failed, "cancelled");
}

#[test]
fn a_v1_config_change_reports_the_new_options_or_the_refusal() {
    let mut fake = copilot("happy");
    let (session, _) = create_on_copilot(&mut fake);
    let set = |fake: &Fake, option: &str, value: &str| {
        fake.backend
            .set_config(
                session.clone(),
                &ConfigId::new(option),
                &ConfigValueId::new(value),
            )
            .expect("the change is queued");
    };
    set(&fake, "model", "claude-sonnet-4.5");
    let changed = fake.next();
    assert!(
        matches!(
            &changed,
            BackendEvent::Update { session: s, update: Update::ConfigOptions(config) }
                if *s == session
                    && config.iter().any(|option| option.id.as_str() == "model"
                        && option.current.as_str() == "claude-sonnet-4.5")
        ),
        "unexpected event: {changed:?}"
    );
    // The fake accepts only the asking values of `mode` and `allow_all`.
    let autopilot = "https://agentclientprotocol.com/protocol/session-modes#autopilot";
    for (refused_option, refused_value) in
        [("model", "haiku"), ("mode", autopilot), ("allow_all", "on")]
    {
        set(&fake, refused_option, refused_value);
        let refused = fake.next();
        assert!(
            matches!(
                &refused,
                BackendEvent::Update {
                    session: s,
                    update: Update::ConfigRejected { option, value, error },
                } if *s == session
                    && option.as_str() == refused_option
                    && value.as_str() == refused_value
                    && error.contains("refusing")
            ),
            "unexpected event: {refused:?}"
        );
    }
}

/// Checks that the next event of `fake` answers a config change asked for now: nothing else
/// was still on its way, so a turn ended exactly once.
fn assert_nothing_more(fake: &mut Fake, session: &SessionId) {
    fake.backend
        .set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("gpt-5"),
        )
        .expect("the change is queued");
    let next = fake.next();
    assert!(
        matches!(
            &next,
            BackendEvent::Update {
                update: Update::ConfigOptions(_),
                ..
            }
        ),
        "unexpected event: {next:?}"
    );
}

/// The events of a turn of the fake v1 agent that reports Copilot's session events: the
/// `assistant.idle` ends the turn and opens one item of background work, which the
/// `session.idle` ends.
fn copilot_turn(session: &SessionId) -> Vec<BackendEvent> {
    vec![
        update(session, Update::TurnStarted),
        update(session, Update::AgentText("done".to_owned())),
        update(session, Update::RunningBackground(1)),
        update(session, Update::TurnEnded(StopReason::EndTurn)),
        update(session, Update::RunningBackground(0)),
    ]
}

#[test]
fn copilot_events_end_the_turn() {
    // `background` sends the `session.idle`, and so the prompt's answer, 300 ms after the
    // `assistant.idle`: the turn ends before the answer arrives.
    for script in ["events", "background"] {
        let mut fake = copilot(script);
        let (session, _) = create_on_copilot(&mut fake);
        let prompted = Instant::now();
        fake.prompt(&session);
        let mut expected = copilot_turn(&session);
        let work_ended = expected.pop().expect("the background work ends");
        assert_eq!(fake.take(expected.len()), expected, "{script}");
        assert_eq!(fake.next(), work_ended, "{script}");
        if script == "background" {
            assert!(
                prompted.elapsed() >= Duration::from_millis(300),
                "the background work ended before it ran its 300 ms"
            );
        }
        assert_nothing_more(&mut fake, &session);
    }
}

#[test]
fn subagent_updates_are_dropped() {
    let log = method_log();
    let mut config = fake_copilot("subagent", vec![]);
    config.env = vec![(LOG_VAR.to_owned(), log.display().to_string())];
    let mut fake = Fake::with(only_claude(config));
    let (session, _) = create_on_copilot(&mut fake);
    fake.prompt(&session);
    // The subagent's text and its `assistant.idle`, both sent before `done`, would show up
    // before it.
    let expected = copilot_turn(&session);
    assert_eq!(fake.take(expected.len()), expected);
    assert_nothing_more(&mut fake, &session);
    assert!(
        methods(&log).contains(&"sent the subagent's updates".to_owned()),
        "the subagent's updates were sent"
    );
    let _ = std::fs::remove_file(log);
}

#[test]
fn a_cancelled_v1_turn_ends_on_the_response() {
    let mut fake = copilot("slow");
    let (session, _) = create_on_copilot(&mut fake);
    fake.prompt(&session);
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnStarted),
            update(&session, Update::AgentText("working…".to_owned())),
        ]
    );
    let started = Instant::now();
    fake.backend
        .cancel(session.clone())
        .expect("the cancel is queued");
    assert_eq!(
        fake.next_within(SLOW_WAIT),
        update(&session, Update::TurnEnded(StopReason::Cancelled))
    );
    assert!(
        started.elapsed() < WAIT,
        "the turn ended on the cancel, not on its own"
    );
}

#[test]
fn a_prompt_during_an_open_prompt_waits() {
    let mut fake = copilot("interrupts");
    let (session, _) = create_on_copilot(&mut fake);
    // Sent at once, the second prompt would cancel the first, which would then stream
    // `Info: Operation cancelled by user` and end cancelled.
    fake.prompt(&session);
    fake.prompt(&session);
    let expected = [copilot_turn(&session), copilot_turn(&session)].concat();
    assert_eq!(fake.take(expected.len()), expected);
    assert_nothing_more(&mut fake, &session);
}

#[test]
#[allow(
    clippy::indexing_slicing,
    reason = "indexing a JSON value never panics"
)]
fn the_interrupts_fake_cancels_an_unanswered_prompt_on_a_second_one() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    // Partitur never sends a second prompt early, so the fake's Copilot-like interruption is
    // driven over the wire directly.
    let mut agent = Command::new(env!("CARGO_BIN_EXE_fake-copilot"))
        .arg("interrupts")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the fake starts");
    let mut stdin = agent.stdin.take().expect("its stdin");
    let mut lines = BufReader::new(agent.stdout.take().expect("its stdout")).lines();
    let mut send = move |id: u64, method: &str, params: serde_json::Value| {
        let request =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{request}").expect("the request is written");
    };
    // The messages up to the answer to `id`, that answer last.
    let mut until_answer = |id: u64| {
        let mut messages = Vec::new();
        for line in lines.by_ref() {
            let message: serde_json::Value =
                serde_json::from_str(&line.expect("a line")).expect("a message");
            let answer = message["id"] == id;
            messages.push(message);
            if answer {
                return messages;
            }
        }
        panic!("the fake exited before answering {id}");
    };
    send(1, "initialize", serde_json::json!({"protocolVersion": 1}));
    until_answer(1);
    send(
        2,
        "session/new",
        serde_json::json!({"cwd": "/", "mcpServers": []}),
    );
    until_answer(2);
    let prompt = |text: &str| {
        let block = serde_json::json!({"type": "text", "text": text});
        serde_json::json!({"sessionId": "cp-1", "prompt": [block]})
    };
    send(3, "session/prompt", prompt("a"));
    send(4, "session/prompt", prompt("b"));
    let first = until_answer(3);
    let answer = first.last().expect("the answer");
    assert_eq!(answer["result"]["stopReason"], "cancelled");
    let interrupted = first.iter().any(|message| {
        message["params"]["update"]["content"]["text"] == "Info: Operation cancelled by user"
    });
    assert!(interrupted, "the first turn streamed its interruption");
    let second = until_answer(4);
    let answer = second.last().expect("the answer");
    assert_eq!(answer["result"]["stopReason"], "end_turn");
    drop(send); // closes stdin, so the fake exits
    agent.wait().expect("the fake exits");
}

/// Starts a backend whose only adapter is the fake v1 agent playing `script`, logging the
/// methods of the requests it receives to `log`.
fn logging_copilot(script: &str, log: &Path) -> Fake {
    let mut config = fake_copilot(script, vec![]);
    config.env = vec![(LOG_VAR.to_owned(), log.display().to_string())];
    Fake::with(only_claude(config))
}

/// How many prompts the fake agent logged to `log`.
fn prompts(log: &Path) -> usize {
    methods(log)
        .iter()
        .filter(|method| *method == "session/prompt")
        .count()
}

#[test]
fn cancel_drops_held_prompts() {
    let log = method_log();
    let mut fake = logging_copilot("slow", &log);
    let (session, _) = create_on_copilot(&mut fake);
    fake.prompt(&session);
    fake.prompt(&session);
    fake.backend
        .cancel(session.clone())
        .expect("the cancel is queued");
    // The held prompt's turn is the one the agent's work ends: one end, cancelled.
    assert_eq!(
        fake.take(3),
        [
            update(&session, Update::TurnStarted),
            update(&session, Update::AgentText("working…".to_owned())),
            update(&session, Update::TurnEnded(StopReason::Cancelled)),
        ]
    );
    // `assert_nothing_more`'s config change is a barrier: it reaches the agent after anything
    // sent before it, so a held prompt sent after all would be in the log once it is answered.
    assert_nothing_more(&mut fake, &session);
    assert_eq!(prompts(&log), 1, "the held prompt never reached the agent");
    let _ = std::fs::remove_file(log);
}

#[test]
fn a_prompt_held_behind_background_work_is_ended_by_cancel() {
    let log = method_log();
    let mut fake = logging_copilot("lingers", &log);
    let (session, _) = create_on_copilot(&mut fake);
    fake.prompt(&session);
    let mut expected = copilot_turn(&session);
    let work_ended = expected.pop().expect("the background work ends");
    assert_eq!(fake.take(expected.len()), expected);
    // The first prompt is answered only once the background work ends, which `lingers` keeps
    // running until the cancel: the second waits for it, and the cancel ends the turn the core
    // opened for it.
    fake.prompt(&session);
    fake.backend
        .cancel(session.clone())
        .expect("the cancel is queued");
    assert_eq!(
        fake.take(2),
        [
            update(&session, Update::TurnEnded(StopReason::Cancelled)),
            work_ended,
        ]
    );
    assert_nothing_more(&mut fake, &session);
    assert_eq!(prompts(&log), 1, "the held prompt never reached the agent");
    let _ = std::fs::remove_file(log);
}

/// Starts a turn of the fake v1 agent's `orchestrates` script in a new orchestrator session
/// and checks that it reports the call of Partitur's `propose_children` and asks permission
/// for it, flagged as Partitur's own tool by the name its title gives.
fn orchestrate_on_copilot(fake: &mut Fake) -> SessionId {
    let (session, _) = create_on_copilot_as(fake, ToolRole::Orchestrator);
    fake.prompt(&session);
    let call = update(
        &session,
        Update::ToolCall {
            id: ToolCallId::new("p1"),
            title: "partitur-propose_children".to_owned(),
            kind: ToolKind::Other,
            status: ToolStatus::Pending,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
        },
    );
    let option = |id: &str, name: &str, kind| PermissionOption {
        id: PermissionOptionId::new(id),
        name: name.to_owned(),
        kind,
    };
    let asked = update(
        &session,
        Update::PermissionRequested {
            id: PermissionId(0),
            tool_call_id: ToolCallId::new("p1"),
            title: "propose_children".to_owned(),
            kind: ToolKind::Other,
            options: vec![
                option("allow_once", "Allow", PermissionKind::AllowOnce),
                option("allow_always", "Always allow", PermissionKind::AllowAlways),
                option("reject_once", "Reject", PermissionKind::RejectOnce),
            ],
            name: Some("mcp__partitur__propose_children".to_owned()),
            locations: Vec::new(),
            partitur_tool: true,
        },
    );
    let expected = vec![update(&session, Update::TurnStarted), call, asked];
    assert_eq!(fake.take(expected.len()), expected);
    session
}

/// The change ending the `orchestrates` script's call `p1` with `status` and `output`.
fn proposal_ended(session: &SessionId, status: ToolStatus, output: &str) -> BackendEvent {
    update(
        session,
        Update::ToolCallUpdate {
            id: ToolCallId::new("p1"),
            title: None,
            kind: None,
            status: Some(status),
            content: Some(vec![ToolContent::Text(output.to_owned())]),
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: None,
        },
    )
}

#[test]
fn partitur_tools_are_flagged_on_v1() {
    let mut fake = copilot("orchestrates");
    let session = orchestrate_on_copilot(&mut fake);
    fake.backend
        .respond_permission(
            session.clone(),
            PermissionId(0),
            Some(PermissionOptionId::new("reject_once")),
        )
        .expect("the answer is queued");
    assert_eq!(
        fake.take(2),
        [
            proposal_ended(&session, ToolStatus::Failed, "reject_once"),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
}

#[test]
fn a_v1_orchestrator_proposes_children() {
    let mut fake = copilot("orchestrates");
    let session = orchestrate_on_copilot(&mut fake);
    fake.backend
        .respond_permission(
            session.clone(),
            PermissionId(0),
            Some(PermissionOptionId::new("allow_once")),
        )
        .expect("the answer is queued");
    let call = fake.next_call();
    assert_eq!(call.session, session);
    assert_eq!(call.tool, proposed_children().into());
    call.reply.send(Ok(PROPOSAL_RECORDED.into()));
    assert_eq!(
        fake.take(3),
        [
            proposal_ended(&session, ToolStatus::Completed, PROPOSAL_RECORDED),
            update(&session, Update::AgentText(PROPOSAL_RECORDED.to_owned())),
            update(&session, Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
}

/// Runs a prompt through the real Claude Code adapter. Needs a logged-in Claude Code and
/// network access, so it only runs when asked for: `PARTITUR_E2E=1 cargo test -- --ignored`.
#[test]
#[ignore = "drives the real Claude Code adapter; set PARTITUR_E2E=1 and pass --ignored"]
fn real_adapter_round_trip() {
    if std::env::var_os("PARTITUR_E2E").is_none() {
        return;
    }
    let mut real = Fake::with(only_claude(AcpConfig::default()));
    let session = SessionId::new();
    real.backend
        .new_session(
            RequestId(1),
            session.clone(),
            &claude(),
            &std::env::temp_dir(),
            ToolRole::Plain,
        )
        .expect("the create is queued");
    let created = real.next_within(E2E_WAIT);
    assert!(
        matches!(&created, BackendEvent::SessionCreated { session: s, .. } if *s == session),
        "unexpected event: {created:?}"
    );

    real.backend
        .prompt(
            session.clone(),
            vec![PromptContent::Text(
                "Reply with the single word pong and nothing else.".to_owned(),
            )],
        )
        .expect("the prompt is queued");
    let mut answer = String::new();
    loop {
        match real.next_within(E2E_WAIT) {
            BackendEvent::Update {
                update: Update::AgentText(text),
                ..
            } => answer.push_str(&text),
            BackendEvent::Update {
                update: Update::PermissionRequested { id, .. },
                ..
            } => real
                .backend
                .respond_permission(session.clone(), id, None)
                .expect("the answer is queued"),
            BackendEvent::Update {
                update: Update::TurnEnded(reason),
                ..
            } => {
                assert_eq!(reason, StopReason::EndTurn);
                break;
            }
            BackendEvent::Update { .. } => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(
        answer.to_lowercase().contains("pong"),
        "unexpected answer: {answer}"
    );
}
