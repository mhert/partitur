//! The helpers the scripted fake agents share: the line transport over stdin and stdout, the
//! call log, the invalid-params error, the title of the fake tool call, and a minimal MCP
//! client that calls a tool of Partitur's loopback tool server over plain HTTP/1.1.
//!
//! Each binary includes this file with `#[path = "fake/common.rs"] mod common;`, so a binary
//! that does not use one of the items would otherwise warn about it.

#![allow(dead_code)]

use std::io::{self, BufRead, Stdout, Write};

use agent_client_protocol::Lines;
use futures::channel::mpsc;
use futures::{Sink, Stream};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The environment variable naming the file the fake appends the methods it served to.
pub const LOG_VAR: &str = "PARTITUR_FAKE_LOG";

/// Stdin and stdout as line streams.
///
/// The crate's own `Stdio` transport does not flush before `connect_with` returns, so a
/// script that exits right after answering could lose that answer; `Lines` writes every
/// accepted line before the connection ends. Stdin is read on its own thread, so a waiting
/// read never stalls the connection's writes.
pub fn stdio() -> io::Result<
    Lines<
        impl Sink<String, Error = io::Error> + Send + 'static,
        impl Stream<Item = io::Result<String>> + Send + 'static,
    >,
> {
    let (lines, incoming) = mpsc::unbounded();
    std::thread::Builder::new()
        .name("stdin".to_owned())
        .spawn(move || {
            // Forwards lines until stdin ends or the connection stops listening.
            let _ = io::stdin()
                .lock()
                .lines()
                .try_for_each(|line| lines.unbounded_send(line));
        })?;
    let outgoing = futures::sink::unfold(
        io::stdout(),
        |mut stdout: Stdout, line: String| async move {
            writeln!(stdout, "{line}")?;
            stdout.flush()?;
            Ok(stdout)
        },
    );
    Ok(Lines::new(outgoing, incoming))
}

/// Appends `method`, or any other line, to the file [`LOG_VAR`] names, if it names one.
pub fn log_method(method: &str) {
    if let Some(path) = std::env::var_os(LOG_VAR) {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path);
        let _ = file.and_then(|mut file| writeln!(file, "{method}"));
    }
}

/// An invalid-params error explained by `message`.
pub fn invalid_params(message: String) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(message)
}

/// The title of the fake agents' one tool call.
pub fn tool_title() -> String {
    "Read README.md".to_owned()
}

/// A client of the MCP tool server at an `http://{host}:{port}/{path}` URL.
pub struct McpClient<'a> {
    url: &'a str,
}

impl<'a> McpClient<'a> {
    /// A client of the tool server at `url`.
    pub fn new(url: &'a str) -> Self {
        Self { url }
    }

    /// Calls `propose_children`, proposing the children `api` and `web`, and returns the text
    /// of the result.
    ///
    /// # Errors
    ///
    /// Returns what went wrong when the server cannot be reached or its answer has no text.
    pub async fn propose_children(&self) -> std::result::Result<String, String> {
        let arguments = json!({"children": [
            {"repository": "api", "prompt": "Add the endpoint", "title": "API endpoint"},
            {"repository": "web", "prompt": "Add the page", "title": "Web page"},
        ]});
        self.call_tool("propose_children", arguments).await
    }

    /// Calls the tool `name` with `arguments` and returns the text of the result.
    ///
    /// # Errors
    ///
    /// Returns what went wrong when the server cannot be reached or its answer has no text.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> std::result::Result<String, String> {
        let call = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        });
        let response = post(self.url, &call.to_string())
            .await
            .map_err(|error| format!("the tool call failed: {error}"))?;
        result_text(&response)
    }
}

/// POSTs `body` as JSON to `url`, an `http://` URL, and returns the whole raw response.
///
/// # Errors
///
/// Returns the I/O error of the exchange, or an invalid-input error for a URL that is not
/// `http://{host}:{port}/{path}`.
async fn post(url: &str, body: &str) -> io::Result<Vec<u8>> {
    let (host, path) = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, url.to_owned()))?;
    let request = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(host).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}

/// The text of the first content block of the JSON-RPC result in the raw HTTP `response`.
///
/// # Errors
///
/// Returns what is missing when the response has no body, no JSON or no such text.
fn result_text(response: &[u8]) -> std::result::Result<String, String> {
    let body = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .and_then(|end| response.get(end + 4..))
        .ok_or("the tool server's answer has no body")?;
    let answer: Value = serde_json::from_slice(body).map_err(|error| error.to_string())?;
    answer
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("the tool server's answer has no text: {answer}"))
}

#[cfg(test)]
mod tests {
    use super::result_text;

    #[test]
    fn the_result_text_is_read_from_the_body_of_the_answer() {
        let answer = b"HTTP/1.1 200 OK\r\n\r\n{\"result\":{\"content\":[{\"text\":\"recorded\"}]}}";
        assert_eq!(result_text(answer), Ok("recorded".to_owned()));
        assert_eq!(
            result_text(b"HTTP/1.1 200 OK"),
            Err("the tool server's answer has no body".to_owned())
        );
        assert!(result_text(b"HTTP/1.1 200 OK\r\n\r\n{").is_err());
        assert_eq!(
            result_text(b"HTTP/1.1 200 OK\r\n\r\n{}"),
            Err("the tool server's answer has no text: {}".to_owned())
        );
    }
}
