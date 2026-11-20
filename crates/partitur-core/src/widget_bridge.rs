//! The host side of the MCP Apps widget protocol (version `2026-01-26`), as a pure state machine.
//!
//! A widget page talks to Partitur in JSON-RPC 2.0 text messages. `WidgetBridge` takes the
//! messages the page sends, the tool result and theme changes as input, and returns
//! `BridgeOut` values for its caller to carry out: text to send to the page, a link to
//! open, or a new height for the view.
//!
//! The bridge answers `ui/initialize`, `ping`, `ui/request-display-mode` and `ui/open-link`.
//! A link opens only for `http`, `https` and `mailto` URLs ([`is_web_link`]), the rule the host
//! also applies to a link the user follows in the page. Every other request, including
//! `tools/call`, `resources/read`, `ui/message` and `ui/update-model-context`, is refused with
//! error `-32601`. The `ui/notifications/size-changed` notification becomes a resize, unless
//! its height is 0, which a page reports before it has laid itself out; other notifications
//! and text that is not a JSON-RPC message are ignored.
//!
//! The bridge answers `ui/open-link` for every web link, but whether the link opens is up to
//! the view's [`LinkGate`]: a view opens a link only on the user's click, one per press.
//!
//! The host context that `ui/initialize` returns holds the theme and the container's
//! dimensions: the view's laid-out width once it is known, and the greatest height the host
//! gives the view, which the bridge is made with.
//!
//! Nothing goes to the page unprompted until it sends `ui/notifications/initialized`. Then the
//! tool input and, when known, the tool result follow in that order. A result that arrives
//! later is sent at once, and so are a theme change and a new width, each as a
//! `ui/notifications/host-context-changed`. Before `initialized`, a theme change or a width
//! only updates the context that `ui/initialize` returns.
//!
//! `content_security_policy` builds the `Content-Security-Policy` header of a widget page from
//! the `_meta` JSON of its resource, as SEP-1865 does, and keeps only the declared domains that
//! are web origins.

use std::time::{Duration, Instant};

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
    width: Option<u32>,
    max_height: u32,
}

impl WidgetBridge {
    /// A bridge for a widget whose tool input and result may already be known, as JSON text,
    /// and whose view is at most `max_height` logical pixels tall.
    #[must_use]
    pub fn new(
        input: Option<String>,
        result: Option<String>,
        theme: WidgetTheme,
        max_height: u32,
    ) -> Self {
        Self {
            input,
            result,
            theme,
            initialized: false,
            width: None,
            max_height,
        }
    }

    /// Records the width the view was laid out at, in logical pixels, and tells the page when
    /// it is ready for it and the width is new.
    pub fn width_changed(&mut self, width: u32) -> Vec<BridgeOut> {
        if self.width == Some(width) {
            return Vec::new();
        }
        self.width = Some(width);
        if self.initialized {
            vec![send(&json!({
                "jsonrpc": "2.0",
                "method": "ui/notifications/host-context-changed",
                "params": {"containerDimensions": self.container_dimensions()},
            }))]
        } else {
            Vec::new()
        }
    }

    /// The `containerDimensions` of the host context: the view's width once it is known, and
    /// the greatest height it may take.
    fn container_dimensions(&self) -> Value {
        let mut dimensions = Map::new();
        if let Some(width) = self.width {
            dimensions.insert("width".into(), width.into());
        }
        dimensions.insert("maxHeight".into(), self.max_height.into());
        Value::Object(dimensions)
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
                // A page that has not laid itself out yet reports 0; its height stays.
                .filter(|height| *height > 0)
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
                "containerDimensions": self.container_dimensions(),
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
#[must_use]
pub fn is_web_link(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && ["http", "https", "mailto"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

/// How long after the user's press on a view, or its release, a link from the view still opens.
pub const LINK_PATIENCE: Duration = Duration::from_secs(2);

/// Whether a link a view reports may open: only on the user's click, once per press.
///
/// A press on the view gives it one link, which it may use until [`LINK_PATIENCE`] after the
/// press or, when the button comes up first, after the release. Whatever the view asks for
/// without such a press, a page that opens links on its own or many on one click, opens
/// nothing. This holds for a link the page asks for (`ui/open-link`) and for one the user
/// followed in it alike.
#[derive(Debug, Default)]
pub struct LinkGate {
    /// When the press the view may still open a link for came, or its release after it.
    press: Option<Instant>,
}

impl LinkGate {
    /// Records the user's press of a mouse button on the view at `at`.
    pub fn pressed(&mut self, at: Instant) {
        self.press = Some(at);
    }

    /// Records the release of a mouse button on the view at `at`; the link of a press not used
    /// yet may then come until a moment after the release.
    pub fn released(&mut self, at: Instant) {
        if let Some(press) = &mut self.press {
            *press = at;
        }
    }

    /// Whether a link that came at `at` may open; it uses up the press either way.
    pub fn admit(&mut self, at: Instant) -> bool {
        self.press
            .take()
            .and_then(|press| at.checked_duration_since(press))
            .is_some_and(|since| since <= LINK_PATIENCE)
    }
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
/// Without a `ui.csp` object the result is the restrictive default policy of SEP-1865. With one,
/// the policy is built the way the specification's "CSP Construction from Metadata" does, every
/// directive in its order: `resourceDomains` are added to `script-src`, `style-src`, `img-src`,
/// `font-src` and `media-src`, `connectDomains` to `connect-src`, `frameDomains` make
/// `frame-src` (`'none'` without any) and `baseUriDomains` make `base-uri` (`'self'` without
/// any), and `object-src` is `'none'`.
///
/// The lists come from an untrusted server, so only entries that are a web origin are kept
/// ([`is_web_origin`]); the rest, such as `*`, a bare scheme, `data:`, `blob:`, `file:` or a
/// scheme of Partitur's own, are dropped.
#[must_use]
pub fn content_security_policy(meta: &str) -> String {
    let csp = serde_json::from_str::<Value>(meta)
        .ok()
        .and_then(|meta| meta.get("ui")?.get("csp").cloned());
    let Some(Value::Object(csp)) = csp else {
        return DEFAULT_POLICY.to_owned();
    };
    let connect = domains(&csp, "connectDomains");
    let resources = domains(&csp, "resourceDomains");
    let frames = domains(&csp, "frameDomains");
    let bases = domains(&csp, "baseUriDomains");
    let or = |sources: Vec<String>, otherwise: &str| {
        if sources.is_empty() {
            vec![otherwise.to_owned()]
        } else {
            sources
        }
    };
    let directives: [(&str, &[&str], Vec<String>); 10] = [
        ("default-src", &["'none'"], Vec::new()),
        (
            "script-src",
            &["'self'", "'unsafe-inline'"],
            resources.clone(),
        ),
        (
            "style-src",
            &["'self'", "'unsafe-inline'"],
            resources.clone(),
        ),
        ("connect-src", &["'self'"], connect),
        ("img-src", &["'self'", "data:"], resources.clone()),
        ("font-src", &["'self'"], resources.clone()),
        ("media-src", &["'self'", "data:"], resources),
        ("frame-src", &[], or(frames, "'none'")),
        ("object-src", &["'none'"], Vec::new()),
        ("base-uri", &[], or(bases, "'self'")),
    ];
    let mut policy = String::new();
    for (name, defaults, sources) in directives {
        if !policy.is_empty() {
            policy.push(' ');
        }
        policy.push_str(name);
        for source in defaults
            .iter()
            .copied()
            .chain(sources.iter().map(String::as_str))
        {
            policy.push(' ');
            policy.push_str(source);
        }
        policy.push(';');
    }
    policy
}

/// The policy of a widget whose resource declares no `ui.csp`: the restrictive default
/// SEP-1865 requires, word for word.
const DEFAULT_POLICY: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src 'self' data:; \
    connect-src 'none';";

/// The entries of one domain list in `csp` that are web origins.
fn domains(csp: &Map<String, Value>, key: &str) -> Vec<String> {
    csp.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|domain| is_web_origin(domain))
        .map(str::to_owned)
        .collect()
}

/// Whether `source` is a web origin a widget may declare: `http`, `https`, `ws` or `wss`, then
/// a host whose first label may be `*` for every subdomain, an optional port (digits or `*`)
/// and an optional path of plain characters. Nothing else is: no bare `*`, no scheme alone, no
/// other scheme, no keyword such as `'self'`, no user, query or fragment.
#[must_use]
pub fn is_web_origin(source: &str) -> bool {
    let Some((scheme, rest)) = source.split_once("://") else {
        return false;
    };
    if !["http", "https", "ws", "wss"]
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    {
        return false;
    }
    let (authority, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, ""),
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let port_ok = port.is_none_or(|port| {
        port == "*" || (!port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
    });
    let host = host.strip_prefix("*.").unwrap_or(host);
    let host_ok = host.split('.').all(|label| {
        !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    let path_ok = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '.' | '_' | '~' | '%'));
    port_ok && host_ok && path_ok
}

#[cfg(test)]
mod tests;
