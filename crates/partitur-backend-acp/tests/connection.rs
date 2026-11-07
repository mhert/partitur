//! Drives the real [`AcpBackend`] through the [`Backend`] port against the scripted
//! `fake-agent` binary, one adapter process per session, and checks the events it reports.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use partitur_backend_acp::{AcpBackend, AcpConfig, ThinkingDisplay};
use partitur_core::{
    Backend, BackendError, BackendEvent, BackendId, BackendSessionId, ConfigCategory, ConfigChoice,
    ConfigId, ConfigOption, ConfigValueId, PermissionId, PermissionKind, PermissionOption,
    PermissionOptionId, PromptContent, RequestId, SessionId, StopReason, ToolCallId, ToolContent,
    ToolKind, ToolStatus, Update,
};
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

/// A running backend and the events it reported.
struct Fake {
    backend: AcpBackend,
    events: UnboundedReceiver<BackendEvent>,
    waiter: Runtime,
}

impl Fake {
    /// Starts a backend whose only adapter, [`claude`], is the fake agent playing `script`.
    fn start(script: &str) -> Self {
        Self::with(only_claude(fake_agent(script, Vec::new())))
    }

    /// Starts a backend serving every adapter in `backends`.
    fn with(backends: HashMap<BackendId, AcpConfig>) -> Self {
        let (backend, events) = AcpBackend::start(backends).expect("the backend starts");
        let waiter = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("the waiting runtime builds");
        Self {
            backend,
            events,
            waiter,
        }
    }

    /// The next event, failing the test when none arrives within [`WAIT`].
    fn next(&mut self) -> BackendEvent {
        self.next_within(WAIT)
    }

    fn next_within(&mut self, wait: Duration) -> BackendEvent {
        wait_for(&self.waiter, &mut self.events, wait).expect("the backend is still running")
    }

    /// The next `count` events.
    fn take(&mut self, count: usize) -> Vec<BackendEvent> {
        (0..count).map(|_| self.next()).collect()
    }

    /// Creates a session and checks that it was reported with the fake's id and config.
    fn create(&mut self) -> SessionId {
        let session = SessionId::new();
        self.backend
            .new_session(RequestId(1), session.clone(), &claude(), cwd(), false)
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
            .new_session(request, session.clone(), backend, cwd(), false)
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

/// The next event, or `None` once the channel closed; fails the test when neither happens
/// within `wait`.
fn wait_for(
    waiter: &Runtime,
    events: &mut UnboundedReceiver<BackendEvent>,
    wait: Duration,
) -> Option<BackendEvent> {
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

/// The fake agent's config with `model` and `effort` as the current values.
fn fake_config(model: &str, effort: &str) -> Vec<ConfigOption> {
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

/// The updates of the fake's turn before its tool call.
fn opening(session: &SessionId) -> Vec<BackendEvent> {
    vec![
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
fn a_new_session_reports_its_backend_id_and_config_options() {
    let mut fake = Fake::start("happy");
    // `create` checks the reported id and the config the mode switch announced (effort
    // `high`), which replaced the one in the create response (effort `low`).
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
        .new_session(RequestId(6), session.clone(), &claude(), cwd(), false)
        .expect("the create is queued");
    // A mode switch would have announced effort `high`.
    assert_eq!(
        fake.take(2),
        [
            BackendEvent::SessionCreated {
                request: RequestId(6),
                session: session.clone(),
                backend_session: BackendSessionId::new("fake-1"),
                config: fake_config("sonnet", "low"),
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

#[test]
fn a_prompt_streams_thought_text_tool_call_update_usage_and_turn_end_in_order() {
    let mut fake = Fake::start("happy");
    let session = fake.create();
    fake.prompt(&session);
    let mut expected = opening(&session);
    expected.extend(closing(&session, ToolStatus::Completed, "ok"));
    assert_eq!(fake.take(expected.len()), expected);
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
    let mut expected = opening(&session);
    expected.extend(closing(&session, ToolStatus::Completed, "ok"));
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
fn a_refused_config_change_reports_a_notice() {
    // `opus` is a value the agent accepts for `model`, so only the option decides: the agent
    // changes nothing but `model` and refuses `effort`.
    let mut fake = Fake::start("happy");
    let session = fake.create();
    fake.backend
        .set_config(
            session.clone(),
            &ConfigId::new("effort"),
            &ConfigValueId::new("opus"),
        )
        .expect("the change is queued");
    let event = fake.next();
    assert!(
        matches!(
            &event,
            BackendEvent::Update { session: s, update: Update::Notice(text) }
                if *s == session && text.starts_with("could not change effort: ")
        ),
        "unexpected event: {event:?}"
    );
}

#[test]
fn a_loaded_session_does_not_forward_the_replayed_history() {
    let mut fake = Fake::start("happy");
    let session = SessionId::new();
    fake.backend
        .load_session(
            RequestId(2),
            session.clone(),
            &claude(),
            &BackendSessionId::new("fake-1"),
            cwd(),
            false,
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

    // The replayed "earlier reply" would arrive first if it had been forwarded.
    fake.prompt(&session);
    let mut expected = opening(&session);
    expected.extend(closing(&session, ToolStatus::Completed, "ok"));
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
            false,
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
fn a_failing_prompt_fails_the_turn_and_keeps_the_session() {
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
        .new_session(RequestId(4), session.clone(), &claude(), cwd(), false)
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
        fake.next(),
        update(&session, Update::TurnEnded(StopReason::EndTurn))
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
        fake.next(),
        update(&session, Update::AgentText("working…".to_owned()))
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
        .new_session(RequestId(5), SessionId::new(), &claude(), cwd(), false)
        .expect("the create is queued");
    let Fake {
        backend,
        mut events,
        waiter,
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
        .new_session(RequestId(7), session.clone(), &missing, cwd(), false)
        .expect("the create is queued");
    fake.backend
        .load_session(
            RequestId(8),
            session.clone(),
            &missing,
            &BackendSessionId::new("fake-1"),
            cwd(),
            false,
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
    let mut expected = opening(&session);
    expected.extend(closing(&session, ToolStatus::Completed, "ok"));
    assert_eq!(fake.take(expected.len()), expected);

    let Fake {
        backend,
        mut events,
        waiter,
    } = fake;
    backend.shutdown();
    assert_eq!(
        wait_for(&waiter, &mut events, WAIT),
        None,
        "neither connection reports anything more"
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
            false,
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
