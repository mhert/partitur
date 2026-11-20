#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;

const DEFAULT_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src 'self' data:; connect-src 'none';";

/// The greatest height the host gives a widget, in logical pixels.
const MAX_HEIGHT: u32 = 640;

fn parse(out: &BridgeOut) -> Value {
    match out {
        BridgeOut::Send(text) => serde_json::from_str(text).unwrap(),
        other => panic!("expected Send, got {other:?}"),
    }
}

fn request(id: u64, method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn notification(method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

fn initialized(bridge: &mut WidgetBridge) -> Vec<BridgeOut> {
    bridge.receive(&request(1, "ui/initialize", &json!({})));
    bridge.receive(&notification("ui/notifications/initialized", &json!({})))
}

fn bridge() -> WidgetBridge {
    WidgetBridge::new(
        Some(r#"{"q":1}"#.to_owned()),
        Some(r#"{"content":[]}"#.to_owned()),
        WidgetTheme::Dark,
        MAX_HEIGHT,
    )
}

#[test]
fn initialize_returns_host_context_with_theme() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Dark, MAX_HEIGHT);
    let out = bridge.receive(&request(7, "ui/initialize", &json!({})));
    assert_eq!(out.len(), 1);
    let reply = parse(out.first().unwrap());
    assert_eq!(reply["jsonrpc"], "2.0");
    assert_eq!(reply["id"], 7);
    let result = &reply["result"];
    assert_eq!(result["protocolVersion"], "2026-01-26");
    assert_eq!(result["hostInfo"]["name"], "partitur");
    assert_eq!(result["hostInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(result["hostCapabilities"], json!({"openLinks": {}}));
    assert_eq!(
        result["hostContext"],
        json!({
            "theme": "dark",
            "displayMode": "inline",
            "availableDisplayModes": ["inline"],
            "platform": "desktop",
            "containerDimensions": {"maxHeight": 640}
        })
    );
}

#[test]
fn initialize_reports_light_theme() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, MAX_HEIGHT);
    let out = bridge.receive(&request(1, "ui/initialize", &json!({})));
    assert_eq!(
        parse(out.first().unwrap())["result"]["hostContext"]["theme"],
        "light"
    );
}

#[test]
fn host_notifications_wait_for_initialized() {
    let mut bridge = bridge();
    assert_eq!(bridge.result_arrived(r#"{"content":[1]}"#), vec![]);
    let out = bridge.receive(&request(1, "ui/initialize", &json!({})));
    assert_eq!(out.len(), 1);
    let out = bridge.receive(&notification("ui/notifications/initialized", &json!({})));
    assert_eq!(out.len(), 2);
    let input = parse(out.first().unwrap());
    assert_eq!(input["method"], "ui/notifications/tool-input");
    assert_eq!(input["params"], json!({"arguments": {"q": 1}}));
    assert!(input.get("id").is_none());
    let result = parse(out.get(1).unwrap());
    assert_eq!(result["method"], "ui/notifications/tool-result");
    assert_eq!(result["params"], json!({"content": [1]}));
}

#[test]
fn initialized_without_known_input_or_result_sends_nothing() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, MAX_HEIGHT);
    assert_eq!(initialized(&mut bridge), vec![]);
}

#[test]
fn initialized_with_unparsable_input_sends_empty_arguments() {
    let mut bridge = WidgetBridge::new(
        Some("nope".to_owned()),
        None,
        WidgetTheme::Light,
        MAX_HEIGHT,
    );
    let out = initialized(&mut bridge);
    assert_eq!(out.len(), 1);
    assert_eq!(
        parse(out.first().unwrap())["params"],
        json!({"arguments": {}})
    );
}

#[test]
fn initialized_with_unparsable_result_skips_it() {
    let mut bridge = WidgetBridge::new(
        None,
        Some("nope".to_owned()),
        WidgetTheme::Light,
        MAX_HEIGHT,
    );
    assert_eq!(initialized(&mut bridge), vec![]);
}

#[test]
fn result_after_initialized_is_sent_at_once() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, MAX_HEIGHT);
    initialized(&mut bridge);
    let out = bridge.result_arrived(r#"{"content":[2]}"#);
    assert_eq!(out.len(), 1);
    let sent = parse(out.first().unwrap());
    assert_eq!(sent["method"], "ui/notifications/tool-result");
    assert_eq!(sent["params"], json!({"content": [2]}));
}

#[test]
fn unparsable_result_after_initialized_is_ignored() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, MAX_HEIGHT);
    initialized(&mut bridge);
    assert_eq!(bridge.result_arrived("nope"), vec![]);
}

#[test]
fn a_second_initialized_sends_input_and_result_again() {
    let mut bridge = bridge();
    initialized(&mut bridge);
    let again = bridge.receive(&notification("ui/notifications/initialized", &json!({})));
    assert_eq!(again.len(), 2);
}

#[test]
fn ping_and_display_mode_are_answered() {
    let mut bridge = bridge();
    let out = bridge.receive(&request(3, "ping", &json!({})));
    let reply = parse(out.first().unwrap());
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["result"], json!({}));
    let out = bridge.receive(&request(
        4,
        "ui/request-display-mode",
        &json!({"mode": "fullscreen"}),
    ));
    let reply = parse(out.first().unwrap());
    assert_eq!(reply["id"], 4);
    assert_eq!(reply["result"], json!({"mode": "inline"}));
}

#[test]
fn open_link_only_allows_web_schemes() {
    let mut bridge = bridge();
    for url in ["https://x", "http://x", "mailto:a@b.c"] {
        let out = bridge.receive(&request(5, "ui/open-link", &json!({"url": url})));
        assert_eq!(out.len(), 2, "{url}");
        assert_eq!(out.first(), Some(&BridgeOut::OpenLink(url.to_owned())));
        let reply = parse(out.get(1).unwrap());
        assert_eq!(reply["id"], 5);
        assert_eq!(reply["result"], json!({}));
    }
    for url in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "partitur-widget://a",
        "",
        "nonsense",
    ] {
        let out = bridge.receive(&request(6, "ui/open-link", &json!({"url": url})));
        assert_eq!(out.len(), 1, "{url}");
        let reply = parse(out.first().unwrap());
        assert_eq!(reply["id"], 6);
        assert_eq!(reply["error"]["code"], -32602);
    }
}

#[test]
fn web_links_are_http_https_and_mailto_with_something_after_the_scheme() {
    for url in ["https://x", "HTTP://x", "mailto:a@b.c", "MailTo:a"] {
        assert!(is_web_link(url), "{url}");
    }
    for url in ["https:", "mailto:", "ftp://x", "about:blank", "x", ""] {
        assert!(!is_web_link(url), "{url}");
    }
}

// A view opens a link only on the user's click: one link per press, within a moment of the
// press or of its release.
#[test]
fn a_link_opens_only_on_a_click_and_once_per_press() {
    let start = Instant::now();
    let at = |millis| start + Duration::from_millis(millis);
    let mut gate = LinkGate::default();
    assert!(!gate.admit(at(0)), "no click, no link");

    gate.pressed(at(0));
    assert!(gate.admit(at(10)));
    assert!(!gate.admit(at(20)), "one link per press");
    gate.released(at(30));
    assert!(
        !gate.admit(at(40)),
        "the release of a used press opens none"
    );

    // The link may come as late as the moment after the release, which renews the press.
    gate.pressed(at(1_000));
    gate.released(at(4_000));
    assert!(gate.admit(at(4_000) + LINK_PATIENCE));

    gate.pressed(at(10_000));
    assert!(gate.admit(at(10_000) + LINK_PATIENCE), "just within");
    gate.pressed(at(20_000));
    assert!(
        !gate.admit(at(20_000) + LINK_PATIENCE + Duration::from_nanos(1)),
        "just too late"
    );
    assert!(
        !gate.admit(at(20_000) + LINK_PATIENCE),
        "a late try used the press"
    );

    // A release without a press of its own opens nothing.
    gate.released(at(30_000));
    assert!(!gate.admit(at(30_010)));

    // A link from before the press does not count.
    gate.pressed(at(40_000));
    assert!(!gate.admit(at(39_000)));
}

#[test]
fn open_link_without_url_is_invalid_params() {
    let mut bridge = bridge();
    let out = bridge.receive(&request(8, "ui/open-link", &json!({})));
    assert_eq!(out.len(), 1);
    assert_eq!(parse(out.first().unwrap())["error"]["code"], -32602);
    let out = bridge.receive(&request(9, "ui/open-link", &json!({"url": 5})));
    assert_eq!(parse(out.first().unwrap())["error"]["code"], -32602);
}

#[test]
fn size_changed_resizes() {
    let mut bridge = bridge();
    let out = bridge.receive(&notification(
        "ui/notifications/size-changed",
        &json!({"width": 300, "height": 240}),
    ));
    assert_eq!(out, vec![BridgeOut::Resize { height: 240 }]);
}

#[test]
fn size_changed_rounds_fractions_up_and_ignores_bad_heights() {
    let mut bridge = bridge();
    let out = bridge.receive(&notification(
        "ui/notifications/size-changed",
        &json!({"height": 240.2}),
    ));
    assert_eq!(out, vec![BridgeOut::Resize { height: 241 }]);
    for params in [
        json!({}),
        json!({"height": -4}),
        json!({"height": "x"}),
        json!({"height": 0}),
        json!({"height": 0.0}),
    ] {
        assert_eq!(
            bridge.receive(&notification("ui/notifications/size-changed", &params)),
            vec![]
        );
    }
}

#[test]
fn tools_call_is_refused() {
    let mut bridge = bridge();
    let out = bridge.receive(&request(11, "tools/call", &json!({"name": "x"})));
    assert_eq!(out.len(), 1);
    let reply = parse(out.first().unwrap());
    assert_eq!(reply["id"], 11);
    assert_eq!(reply["error"]["code"], -32601);
    assert_eq!(reply["error"]["message"], "not supported by Partitur");
}

#[test]
fn unknown_request_gets_method_not_found() {
    let mut bridge = bridge();
    for method in [
        "resources/read",
        "ui/message",
        "ui/update-model-context",
        "whatever",
    ] {
        let out = bridge.receive(&request(12, method, &json!({})));
        assert_eq!(out.len(), 1, "{method}");
        let reply = parse(out.first().unwrap());
        assert_eq!(reply["id"], 12);
        assert_eq!(reply["error"]["code"], -32601);
        assert_eq!(reply["error"]["message"], "not supported by Partitur");
    }
}

#[test]
fn string_ids_are_echoed() {
    let mut bridge = bridge();
    let text = json!({"jsonrpc": "2.0", "id": "abc", "method": "ping"}).to_string();
    let reply = parse(bridge.receive(&text).first().unwrap());
    assert_eq!(reply["id"], "abc");
}

#[test]
fn other_notifications_are_ignored() {
    let mut bridge = bridge();
    assert_eq!(
        bridge.receive(&notification("notifications/message", &json!({}))),
        vec![]
    );
}

#[test]
fn malformed_json_is_ignored() {
    let mut bridge = bridge();
    assert_eq!(bridge.receive("{not json"), vec![]);
    assert_eq!(bridge.receive("[1,2]"), vec![]);
    assert_eq!(bridge.receive(r#"{"jsonrpc":"2.0"}"#), vec![]);
    assert_eq!(bridge.receive(r#"{"id":1,"result":{}}"#), vec![]);
}

#[test]
fn theme_change_before_initialized_updates_context_only() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, MAX_HEIGHT);
    assert_eq!(bridge.theme_changed(WidgetTheme::Dark), vec![]);
    let out = bridge.receive(&request(1, "ui/initialize", &json!({})));
    assert_eq!(
        parse(out.first().unwrap())["result"]["hostContext"]["theme"],
        "dark"
    );
}

#[test]
fn theme_change_after_initialized_notifies() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, MAX_HEIGHT);
    initialized(&mut bridge);
    let out = bridge.theme_changed(WidgetTheme::Dark);
    assert_eq!(out.len(), 1);
    let sent = parse(out.first().unwrap());
    assert_eq!(sent["method"], "ui/notifications/host-context-changed");
    assert_eq!(sent["params"], json!({"theme": "dark"}));
    let out = bridge.theme_changed(WidgetTheme::Light);
    assert_eq!(
        parse(out.first().unwrap())["params"],
        json!({"theme": "light"})
    );
}

#[test]
fn default_csp_when_meta_has_none() {
    assert_eq!(content_security_policy("{}"), DEFAULT_CSP);
    assert_eq!(content_security_policy("nope"), DEFAULT_CSP);
    assert_eq!(content_security_policy(r#"{"ui":{}}"#), DEFAULT_CSP);
    assert_eq!(content_security_policy(r#"{"ui":{"csp":5}}"#), DEFAULT_CSP);
    assert_eq!(
        content_security_policy(r#"{"ui":{"csp":null}}"#),
        DEFAULT_CSP
    );
    assert_eq!(content_security_policy(r#"{"ui":5}"#), DEFAULT_CSP);
}

/// The policy of a resource whose `ui.csp` declares no domain at all: the construction of
/// SEP-1865 with every list empty.
const DECLARED_NONE: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; \
     style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; \
     font-src 'self'; media-src 'self' data:; frame-src 'none'; object-src 'none'; \
     base-uri 'self';";

#[test]
fn csp_lists_declared_domains() {
    let meta = json!({"ui": {"csp": {
        "connectDomains": ["https://api.example", "wss://live.example"],
        "resourceDomains": ["https://a.example", "https://*.b.example"],
        "frameDomains": ["https://frame.example"],
        "baseUriDomains": ["https://base.example"]
    }}})
    .to_string();
    assert_eq!(
        content_security_policy(&meta),
        "default-src 'none'; \
         script-src 'self' 'unsafe-inline' https://a.example https://*.b.example; \
         style-src 'self' 'unsafe-inline' https://a.example https://*.b.example; \
         connect-src 'self' https://api.example wss://live.example; \
         img-src 'self' data: https://a.example https://*.b.example; \
         font-src 'self' https://a.example https://*.b.example; \
         media-src 'self' data: https://a.example https://*.b.example; \
         frame-src https://frame.example; \
         object-src 'none'; \
         base-uri https://base.example;"
    );
}

#[test]
fn a_declared_csp_without_domains_is_built_like_any_other() {
    assert_eq!(
        content_security_policy(r#"{"ui":{"csp":{}}}"#),
        DECLARED_NONE
    );
    assert_eq!(
        content_security_policy(
            r#"{"ui":{"csp":{"connectDomains":[],"resourceDomains":[],"frameDomains":[],"baseUriDomains":[]}}}"#
        ),
        DECLARED_NONE
    );
}

#[test]
fn csp_each_list_applies_on_its_own() {
    let connect = json!({"ui": {"csp": {"connectDomains": ["https://c.example"]}}}).to_string();
    assert_eq!(
        content_security_policy(&connect),
        DECLARED_NONE.replace(
            "connect-src 'self';",
            "connect-src 'self' https://c.example;"
        )
    );
    let resource = json!({"ui": {"csp": {"resourceDomains": ["https://r.example"]}}}).to_string();
    let with_resource = DECLARED_NONE
        .replace("'unsafe-inline';", "'unsafe-inline' https://r.example;")
        .replace("data:;", "data: https://r.example;")
        .replace("font-src 'self';", "font-src 'self' https://r.example;");
    assert_eq!(content_security_policy(&resource), with_resource);
    assert!(with_resource.contains("script-src 'self' 'unsafe-inline' https://r.example;"));
    let frame = json!({"ui": {"csp": {"frameDomains": ["https://f.example"]}}}).to_string();
    assert_eq!(
        content_security_policy(&frame),
        DECLARED_NONE.replace("frame-src 'none';", "frame-src https://f.example;")
    );
    let base = json!({"ui": {"csp": {"baseUriDomains": ["https://b.example"]}}}).to_string();
    assert_eq!(
        content_security_policy(&base),
        DECLARED_NONE.replace("base-uri 'self';", "base-uri https://b.example;")
    );
}

#[test]
fn csp_drops_entries_that_could_inject_directives() {
    let meta = json!({"ui": {"csp": {"connectDomains": [
        "https://ok.example", "https://x; script-src *", "a b", "", 5, "it's"
    ]}}})
    .to_string();
    assert_eq!(
        content_security_policy(&meta),
        DECLARED_NONE.replace(
            "connect-src 'self';",
            "connect-src 'self' https://ok.example;"
        )
    );
}

// The lists come from an untrusted server: only web origins count, never a wildcard for
// everything, a bare scheme or a scheme of Partitur's own, which would let the widget nest its
// proxy page.
#[test]
fn csp_keeps_only_web_origins() {
    let kept = [
        "https://ok.example",
        "http://plain.example",
        "wss://live.example",
        "ws://live.example:8080",
        "https://*.example.net",
        "HTTPS://Upper.Example",
        "https://cdn.example/path/",
        "https://localhost:3000",
    ];
    let dropped = [
        "*",
        "https:",
        "https://",
        "https://*",
        "https://*.",
        "https://a.*.example",
        "https://ex*ample.com",
        "partitur-widget:",
        "partitur-widget://view-1/",
        "partitur-app://view-1",
        "data:",
        "blob:",
        "file:",
        "file:///etc",
        "filesystem:",
        "javascript:alert(1)",
        "'self'",
        "'unsafe-eval'",
        "'nonce-abc'",
        "example.com",
        "*.example.com",
        "ftp://example.com",
        "https://exa mple.com",
        "https://example.com:port",
        "https://user@example.com",
        "https://..example.com",
        "https://example..com",
        "https://example.com/a?b",
        "https://example.com/#x",
    ];
    let meta =
        json!({"ui": {"csp": {"frameDomains": kept.iter().chain(&dropped).collect::<Vec<_>>()}}})
            .to_string();
    assert_eq!(
        content_security_policy(&meta),
        DECLARED_NONE.replace(
            "frame-src 'none';",
            &format!("frame-src {};", kept.join(" "))
        )
    );
}

#[test]
fn a_width_before_initialize_goes_into_the_host_context() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, 480);
    assert_eq!(bridge.width_changed(700), vec![]);
    let out = bridge.receive(&request(1, "ui/initialize", &json!({})));
    assert_eq!(
        parse(out.first().unwrap())["result"]["hostContext"]["containerDimensions"],
        json!({"width": 700, "maxHeight": 480})
    );
}

#[test]
fn a_width_change_after_initialized_notifies_once() {
    let mut bridge = WidgetBridge::new(None, None, WidgetTheme::Light, MAX_HEIGHT);
    initialized(&mut bridge);
    let changed = |out: Vec<BridgeOut>| {
        assert_eq!(out.len(), 1, "{out:?}");
        let sent = parse(out.first().unwrap());
        assert_eq!(sent["jsonrpc"], "2.0");
        assert_eq!(sent["method"], "ui/notifications/host-context-changed");
        sent["params"].clone()
    };
    assert_eq!(
        changed(bridge.width_changed(700)),
        json!({"containerDimensions": {"width": 700, "maxHeight": 640}})
    );
    assert_eq!(bridge.width_changed(700), vec![]);
    assert_eq!(
        changed(bridge.width_changed(512)),
        json!({"containerDimensions": {"width": 512, "maxHeight": 640}})
    );
}
