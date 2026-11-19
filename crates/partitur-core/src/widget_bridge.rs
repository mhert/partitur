//! The host side of the MCP Apps widget protocol (version `2026-01-26`), as a pure state machine.
//!
//! A widget page talks to Partitur in JSON-RPC 2.0 text messages. `WidgetBridge` takes the
//! messages the page sends, the tool result and theme changes as input, and returns
//! `BridgeOut` values for its caller to carry out: text to send to the page, a link to
//! open, or a new height for the view.
//!
//! The bridge answers `ui/initialize`, `ping`, `ui/request-display-mode` and `ui/open-link`.
//! A link opens only for `http`, `https` and `mailto` URLs. Every other request, including
//! `tools/call`, `resources/read`, `ui/message` and `ui/update-model-context`, is refused with
//! error `-32601`. The `ui/notifications/size-changed` notification becomes a resize; other
//! notifications and text that is not a JSON-RPC message are ignored.
//!
//! Nothing goes to the page unprompted until it sends `ui/notifications/initialized`. Then the
//! tool input and, when known, the tool result follow in that order. A result that arrives
//! later is sent at once, and so is a theme change. Before `initialized`, a theme change only
//! updates the context that `ui/initialize` returns.
//!
//! `content_security_policy` builds the `Content-Security-Policy` header of a widget page from
//! the `_meta` JSON of its resource.

use serde_json::{Map, Value, json};

/// The protocol version the host speaks.
const PROTOCOL_VERSION: &str = "2026-01-26";

/// JSON-RPC error code for a method the host does not offer.
const METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC error code for parameters the host refuses.
const INVALID_PARAMS: i64 = -32602;

/// The color scheme the widget is told to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetTheme {
    /// The light theme.
    Light,
    /// The dark theme.
    Dark,
}

impl WidgetTheme {
    /// The name the protocol uses for this theme.
    fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// What the caller of the bridge has to carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeOut {
    /// JSON-RPC text to deliver to the widget page.
    Send(String),
    /// A link the widget asked to open; its scheme has been checked.
    OpenLink(String),
    /// The widget asked for this content height, in logical pixels.
    Resize {
        /// The requested height.
        height: u32,
    },
}

/// The host side of one widget's conversation with its page.
#[derive(Debug)]
pub struct WidgetBridge {
    input: Option<String>,
    result: Option<String>,
    theme: WidgetTheme,
    initialized: bool,
}

impl WidgetBridge {
    /// A bridge for a widget whose tool input and result may already be known, as JSON text.
    #[must_use]
    pub fn new(input: Option<String>, result: Option<String>, theme: WidgetTheme) -> Self {
        Self {
            input,
            result,
            theme,
            initialized: false,
        }
    }

    /// Handles one message from the page and returns what to do about it.
    pub fn receive(&mut self, message: &str) -> Vec<BridgeOut> {
        let Ok(Value::Object(message)) = serde_json::from_str::<Value>(message) else {
            return Vec::new();
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Vec::new();
        };
        let params = message.get("params");
        match message.get("id") {
            Some(id) => self.request(id, method, params),
            None => self.notification(method, params),
        }
    }

    /// Records the tool result and sends it when the page is ready for it.
    pub fn result_arrived(&mut self, result: &str) -> Vec<BridgeOut> {
        self.result = Some(result.to_owned());
        if self.initialized {
            self.result_notification().into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Records the new theme and tells the page when it is ready for it.
    pub fn theme_changed(&mut self, theme: WidgetTheme) -> Vec<BridgeOut> {
        self.theme = theme;
        if self.initialized {
            vec![send(&json!({
                "jsonrpc": "2.0",
                "method": "ui/notifications/host-context-changed",
                "params": {"theme": theme.name()},
            }))]
        } else {
            Vec::new()
        }
    }

    fn request(&mut self, id: &Value, method: &str, params: Option<&Value>) -> Vec<BridgeOut> {
        match method {
            "ui/initialize" => vec![reply(id, &self.initialize_result())],
            "ping" => vec![reply(id, &json!({}))],
            "ui/request-display-mode" => vec![reply(id, &json!({"mode": "inline"}))],
            "ui/open-link" => {
                let url = params
                    .and_then(|params| params.get("url"))
                    .and_then(Value::as_str)
                    .filter(|url| is_web_link(url));
                match url {
                    Some(url) => vec![BridgeOut::OpenLink(url.to_owned()), reply(id, &json!({}))],
                    None => vec![error(
                        id,
                        INVALID_PARAMS,
                        "only http, https and mailto links",
                    )],
                }
            }
            _ => vec![error(id, METHOD_NOT_FOUND, "not supported by Partitur")],
        }
    }

    fn notification(&mut self, method: &str, params: Option<&Value>) -> Vec<BridgeOut> {
        match method {
            "ui/notifications/initialized" => {
                self.initialized = true;
                let arguments = self
                    .input
                    .as_deref()
                    .and_then(|input| serde_json::from_str::<Value>(input).ok())
                    .unwrap_or_else(|| json!({}));
                let mut out = Vec::new();
                if self.input.is_some() {
                    out.push(send(&json!({
                        "jsonrpc": "2.0",
                        "method": "ui/notifications/tool-input",
                        "params": {"arguments": arguments},
                    })));
                }
                out.extend(self.result_notification());
                out
            }
            "ui/notifications/size-changed" => params
                .and_then(|params| params.get("height"))
                .and_then(height)
                .map(|height| BridgeOut::Resize { height })
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "hostInfo": {"name": "partitur", "version": env!("CARGO_PKG_VERSION")},
            "hostCapabilities": {"openLinks": {}},
            "hostContext": {
                "theme": self.theme.name(),
                "displayMode": "inline",
                "availableDisplayModes": ["inline"],
                "platform": "desktop",
            },
        })
    }

    /// The `tool-result` notification for the known result, if it parses.
    fn result_notification(&self) -> Option<BridgeOut> {
        let result = serde_json::from_str::<Value>(self.result.as_deref()?).ok()?;
        Some(send(&json!({
            "jsonrpc": "2.0",
            "method": "ui/notifications/tool-result",
            "params": result,
        })))
    }
}

/// Whether a link may be handed to the system: only the web and mail schemes may.
fn is_web_link(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && ["http", "https", "mailto"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

/// A non-negative height in pixels, rounded up.
fn height(value: &Value) -> Option<u32> {
    if let Some(whole) = value.as_u64() {
        return Some(u32::try_from(whole).unwrap_or(u32::MAX));
    }
    let fraction = value.as_f64().filter(|fraction| *fraction >= 0.0)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rounded = fraction.ceil().min(f64::from(u32::MAX)) as u32;
    Some(rounded)
}

fn send(message: &Value) -> BridgeOut {
    BridgeOut::Send(message.to_string())
}

fn reply(id: &Value, result: &Value) -> BridgeOut {
    send(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error(id: &Value, code: i64, message: &str) -> BridgeOut {
    send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

/// The `Content-Security-Policy` of a widget page, from the resource's `_meta` JSON.
///
/// `ui.csp` lists the domains the widget may use: `connectDomains` for `connect-src`,
/// `resourceDomains` for images, styles, fonts and media, `frameDomains` for `frame-src` and
/// `baseUriDomains` for `base-uri`. Declared domains are added to the sources the default
/// policy already has for that directive. A directive without declared domains and without
/// a default is left out, which `default-src 'none'` covers. Without a usable `csp` the
/// result is the default policy. Entries that are not plain source expressions are dropped.
#[must_use]
pub fn content_security_policy(meta: &str) -> String {
    let csp = serde_json::from_str::<Value>(meta)
        .ok()
        .and_then(|meta| meta.get("ui")?.get("csp").cloned())
        .and_then(|csp| match csp {
            Value::Object(csp) => Some(csp),
            _ => None,
        })
        .unwrap_or_default();
    let connect = domains(&csp, "connectDomains");
    let resources = domains(&csp, "resourceDomains");
    let frames = domains(&csp, "frameDomains");
    let bases = domains(&csp, "baseUriDomains");

    let mut policy = String::from("default-src 'none'; script-src 'self' 'unsafe-inline';");
    let with_resources = |defaults: &[&str]| -> Vec<String> {
        defaults
            .iter()
            .map(|source| (*source).to_owned())
            .chain(resources.iter().cloned())
            .collect()
    };
    let directives = [
        ("style-src", with_resources(&["'self'", "'unsafe-inline'"])),
        ("img-src", with_resources(&["'self'", "data:"])),
        ("media-src", with_resources(&["'self'", "data:"])),
        (
            "connect-src",
            if connect.is_empty() {
                vec!["'none'".to_owned()]
            } else {
                connect
            },
        ),
        ("font-src", resources.clone()),
        ("frame-src", frames),
        ("base-uri", bases),
    ];
    for (name, sources) in directives {
        if !sources.is_empty() {
            policy.push(' ');
            policy.push_str(name);
            for source in sources {
                policy.push(' ');
                policy.push_str(&source);
            }
            policy.push(';');
        }
    }
    policy
}

/// The usable entries of one domain list in `csp`: strings that cannot end a source list.
fn domains(csp: &Map<String, Value>, key: &str) -> Vec<String> {
    csp.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|domain| {
            !domain.is_empty()
                && !domain.chars().any(|c| {
                    c.is_whitespace() || c.is_control() || matches!(c, ';' | ',' | '\'' | '"')
                })
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests;
