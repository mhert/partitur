//! Partitur's MCP tool server, which every session calls over streamable HTTP.
//!
//! [`ToolServer::start`] serves the tools on a loopback port, on a thread of its own, and
//! hands every call it receives on as a [`ToolCall`]. Answering the call's reply answers
//! the agent; a call that is not answered within [`ANSWER_TIMEOUT`] fails.

mod reply;
mod server;

use std::io;
use std::net::Ipv4Addr;
use std::thread::JoinHandle;
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use futures::channel::oneshot;
use partitur_core::{SessionId, ToolCall};
use thiserror::Error;
use tokio::net::TcpListener;

/// How long a tool call waits for its answer before it fails.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// The name of the thread the server runs on.
const THREAD_NAME: &str = "partitur-mcp";

/// Why the tool server could not start.
#[derive(Debug, Error)]
pub enum McpError {
    /// No loopback port could be bound and served.
    #[error("failed to start Partitur's tool server")]
    Listen(#[source] io::Error),
    /// The server's async runtime could not be built.
    #[error("failed to build Partitur's tool server's runtime")]
    Runtime(#[source] io::Error),
    /// The server's thread could not be spawned.
    #[error("failed to spawn Partitur's tool server's thread")]
    Spawn(#[source] io::Error),
}

/// Where each session reaches the tool server.
#[derive(Debug, Clone)]
pub struct SessionUrls {
    /// The server's address as a URL, without a trailing slash.
    base: String,
}

impl SessionUrls {
    /// The URL that serves the tools of `session`. An orchestrator's URL also serves the
    /// orchestration tools.
    pub fn session_url(&self, session: &SessionId, orchestrator: bool) -> String {
        let suffix = if orchestrator { "/orchestrator" } else { "" };
        format!("{}/{session}{suffix}", self.base)
    }
}

/// The running tool server. Dropping it stops the server, ending calls still in flight, and
/// waits for its thread.
#[derive(Debug)]
pub struct ToolServer {
    urls: SessionUrls,
    /// Dropping it tells the server to stop.
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl ToolServer {
    /// Starts the server on a free loopback port and returns it with the calls it receives.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Runtime`] when the async runtime cannot be built,
    /// [`McpError::Listen`] when no loopback port can be bound and served, and
    /// [`McpError::Spawn`] when the server's thread cannot be spawned.
    pub fn start() -> Result<(Self, UnboundedReceiver<ToolCall>), McpError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(McpError::Runtime)?;
        let (listener, base) = listen(&runtime).map_err(McpError::Listen)?;
        let (calls, receiver) = unbounded();
        let (stop, stopped) = oneshot::channel::<()>();
        let router = server::router(calls, ANSWER_TIMEOUT);
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || {
                // Dropping the runtime when the stop comes cancels every connection with it.
                runtime.block_on(async move {
                    tokio::select! {
                        _ = axum::serve(listener, router).into_future() => {
                            tracing::warn!("Partitur's tool server stopped serving"); // coverage: axum::serve never finishes on its own (it retries failed accepts), so no test can reach this
                        }
                        _ = stopped => {}
                    }
                });
            })
            .map_err(McpError::Spawn)?;
        Ok((
            Self {
                urls: SessionUrls { base },
                stop: Some(stop),
                thread: Some(thread),
            },
            receiver,
        ))
    }

    /// Where each session reaches the server.
    pub fn urls(&self) -> SessionUrls {
        self.urls.clone()
    }
}

impl Drop for ToolServer {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            // A thread that panicked has stopped serving all the same.
            let _ = thread.join();
        }
    }
}

/// Binds a free loopback port, registered with `runtime`, and returns its listener with its
/// URL.
///
/// # Errors
///
/// Returns the I/O error when no port can be bound, the listener cannot be made
/// non-blocking or `runtime` cannot register it.
fn listen(runtime: &tokio::runtime::Runtime) -> io::Result<(TcpListener, String)> {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let base = format!("http://{}", listener.local_addr()?);
    let _runtime = runtime.enter();
    Ok((TcpListener::from_std(listener)?, base))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::mpsc;
    use std::time::Duration;

    use futures::StreamExt;
    use partitur_core::SessionId;
    use serde_json::{Value, json};

    use super::*;

    /// Posts `body` to `url` over a fresh TCP connection and returns the raw response, or the
    /// error the connection ended with.
    fn exchange(url: &str, body: &str) -> std::io::Result<String> {
        let (address, path) = url
            .strip_prefix("http://")
            .and_then(|rest| rest.split_once('/'))
            .unwrap();
        let request = format!(
            "POST /{path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(address)?;
        stream.write_all(request.as_bytes())?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response)
    }

    #[test]
    fn the_started_server_answers_a_ping_over_tcp() {
        let (server, _calls) = ToolServer::start().unwrap();
        let url = server.urls().session_url(&SessionId::new(), false);

        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        let response = exchange(&url, &body).unwrap();

        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK"), "response {response}");
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"jsonrpc": "2.0", "id": 1, "result": {}})
        );
    }

    #[tokio::test]
    async fn dropping_the_server_stops_it_with_a_call_in_flight() {
        let (server, mut calls) = ToolServer::start().unwrap();
        let url = server.urls().session_url(&SessionId::new(), true);
        let body = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {"name": "send_to_child", "arguments": {
                "session_id": SessionId::new().to_string(),
                "prompt": "Also add tests",
            }},
        })
        .to_string();
        let (ended, ended_receiver) = mpsc::channel();
        let client = std::thread::spawn(move || {
            let response = exchange(&url, &body);
            ended.send(()).unwrap();
            response
        });
        let call = tokio::time::timeout(Duration::from_secs(5), calls.next())
            .await
            .unwrap()
            .unwrap();
        assert!(call.reply.is_waiting());

        drop(server);

        assert!(!call.reply.is_waiting(), "the call is still served");
        ended_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the request outlived the server");
        let response = client.join().unwrap();
        assert!(
            response
                .as_ref()
                .map_or(true, |text| !text.contains("200 OK")),
            "response {response:?}"
        );
    }

    #[test]
    fn session_urls_name_the_session_and_its_kind() {
        let (server, _calls) = ToolServer::start().unwrap();
        let session = SessionId::new();

        let plain = server.urls().session_url(&session, false);
        let orchestrator = server.urls().session_url(&session, true);

        let port = plain
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix(&format!("/{session}")))
            .unwrap_or_else(|| panic!("url {plain}"));
        assert_ne!(port.parse::<u16>().unwrap(), 0);
        assert_eq!(orchestrator, format!("{plain}/orchestrator"));
    }

    #[test]
    fn errors_say_what_failed() {
        let cases = [
            (
                McpError::Listen(std::io::Error::other("no port")),
                "failed to start Partitur's tool server",
            ),
            (
                McpError::Runtime(std::io::Error::other("no runtime")),
                "failed to build Partitur's tool server's runtime",
            ),
            (
                McpError::Spawn(std::io::Error::other("no thread")),
                "failed to spawn Partitur's tool server's thread",
            ),
        ];

        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
            assert!(std::error::Error::source(&error).is_some());
        }
    }
}
