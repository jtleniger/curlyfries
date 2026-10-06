//! Human and NDJSON rendering of executed requests.

use std::io::{self, Write};

use serde_json::Value;

use crate::Map;
use crate::execute::{ExecResult, ResolvedRequest};
use crate::request::RequestDef;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

/// How long a captured string may be before it is truncated in human output.
const CAPTURE_LIMIT: usize = 60;

/// Rendering switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderOpts {
    /// Print the resolved request headers and body before the response.
    pub verbose: bool,
    /// Colour the status line (caller decides based on the terminal).
    pub color: bool,
}

/// Writes the human-readable result block.
pub fn human(
    out: &mut impl Write,
    req: &ResolvedRequest,
    res: &ExecResult,
    captured: &Map,
    opts: RenderOpts,
) -> io::Result<()> {
    writeln!(out, "→ {} {}", req.method, req.url)?;
    if opts.verbose {
        if !req.headers.is_empty() {
            writeln!(out, "  request headers:")?;
            for (name, value) in &req.headers {
                writeln!(out, "    {name}: {value}")?;
            }
        }
        if let Some(body) = &req.body {
            writeln!(out, "  request body:")?;
            writeln!(out, "    {body}")?;
        }
    }

    let head = if res.reason.is_empty() {
        format!("← {}", res.status)
    } else {
        format!("← {} {}", res.status, res.reason)
    };
    if opts.color
        && let Some(color) = status_color(res.status)
    {
        write!(out, "{color}{head}{RESET}")?;
    } else {
        write!(out, "{head}")?;
    }
    writeln!(out, "  {}ms", res.duration_ms)?;

    for (name, value) in &res.headers {
        writeln!(out, "{name}: {value}")?;
    }
    writeln!(out)?;

    if !res.body_text.is_empty() {
        match &res.body_json {
            Some(body) => writeln!(out, "{}", pretty(body))?,
            None => {
                write!(out, "{}", res.body_text)?;
                if !res.body_text.ends_with('\n') {
                    writeln!(out)?;
                }
            }
        }
    }

    if !captured.is_empty() {
        writeln!(out)?;
        writeln!(out, "captured:")?;
        for (name, value) in captured {
            writeln!(out, "  {name} = {}", captured_value(value))?;
        }
    }
    Ok(())
}

/// One NDJSON record for `run --json`.
pub fn json_line(req: &ResolvedRequest, res: &ExecResult, captured: &Map) -> Value {
    serde_json::json!({
        "request": req.id,
        "name": req.name,
        "method": req.method,
        "url": req.url,
        "status": res.status,
        "reason": res.reason,
        "durationMs": res.duration_ms,
        "headers": res
            .headers
            .iter()
            .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
            .collect::<Vec<Value>>(),
        "body": res.body_json.clone().unwrap_or(Value::Null),
        "bodyText": res.body_text,
        "captured": Value::Object(captured.clone()),
    })
}

/// The list label of a request: `METHOD id`, plus `— name` when it differs.
pub fn request_label(def: &RequestDef) -> String {
    if def.name == def.id {
        format!("{} {}", def.method, def.id)
    } else {
        format!("{} {} — {}", def.method, def.id, def.name)
    }
}

/// The human block for one session scope.
pub fn scope_line(name: &str, vars: &Map) -> String {
    if vars.is_empty() {
        return format!("{name}: (no variables)");
    }
    let mut out = format!("{name}:");
    for (var, value) in vars {
        out.push_str(&format!("\n  {var} = {}", compact(value)));
    }
    out
}

/// Compact JSON for a value.
pub fn compact(value: &Value) -> String {
    serde_json::to_string(value).expect("JSON values always serialize")
}

/// Pretty JSON for a value.
pub fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON values always serialize")
}

/// A captured value as printed by `human`: compact JSON, strings truncated.
pub fn captured_value(value: &Value) -> String {
    match value {
        Value::String(text) => {
            let mut chars = text.chars();
            let short: String = chars.by_ref().take(CAPTURE_LIMIT).collect();
            if chars.next().is_some() {
                compact(&Value::String(format!("{short}…")))
            } else {
                compact(value)
            }
        }
        other => compact(other),
    }
}

fn status_color(status: u16) -> Option<&'static str> {
    match status_class(status) {
        StatusClass::Success => Some(GREEN),
        StatusClass::ClientError => Some(YELLOW),
        StatusClass::ServerError => Some(RED),
        StatusClass::Redirect | StatusClass::Other => None,
    }
}

/// Broad class of an HTTP status, for callers that pick their own colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    /// 2xx.
    Success,
    /// 3xx.
    Redirect,
    /// 4xx.
    ClientError,
    /// 5xx.
    ServerError,
    /// Anything else (1xx, 0, or out of range).
    Other,
}

/// Classifies `status` by its hundreds digit.
pub fn status_class(status: u16) -> StatusClass {
    match status / 100 {
        2 => StatusClass::Success,
        3 => StatusClass::Redirect,
        4 => StatusClass::ClientError,
        5 => StatusClass::ServerError,
        _ => StatusClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute::ExecResult;
    use crate::request;
    use serde_json::json;
    use std::path::Path;
    use ureq::http::HeaderMap;

    fn resolved(method: &str, url: &str) -> ResolvedRequest {
        ResolvedRequest {
            id: "ships/list".to_string(),
            def_file: "requests/ships/list.json".into(),
            name: "List ships".to_string(),
            method: method.to_string(),
            url: url.to_string(),
            headers: vec![("X-Trace".to_string(), "curlyfries".to_string())],
            body: None,
        }
    }

    fn result(status: u16, reason: &str, body: &str) -> ExecResult {
        ExecResult {
            status,
            reason: reason.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("x-total-count".to_string(), "5".to_string()),
            ],
            header_map: HeaderMap::new(),
            body_text: body.to_string(),
            body_json: serde_json::from_str(body).ok(),
            duration_ms: 12,
        }
    }

    fn render(req: &ResolvedRequest, res: &ExecResult, captured: &Map, opts: RenderOpts) -> String {
        let mut out = Vec::new();
        human(&mut out, req, res, captured, opts).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn status_line_and_headers() {
        let text = render(
            &resolved("GET", "http://127.0.0.1:4000/ships?limit=2"),
            &result(200, "OK", r#"{"a":1}"#),
            &Map::new(),
            RenderOpts::default(),
        );
        let first = text.lines().next().unwrap();
        assert_eq!(first, "→ GET http://127.0.0.1:4000/ships?limit=2");
        let second = text.lines().nth(1).unwrap();
        assert_eq!(second, "← 200 OK  12ms");
        assert!(
            text.contains("\ncontent-type: application/json\n"),
            "{text}"
        );
        assert!(text.contains("x-total-count: 5\n"), "{text}");
        assert!(!text.contains("captured:"), "{text}");
        assert!(text.contains("{\n  \"a\": 1\n}\n"), "{text}");
    }

    #[test]
    fn color_follows_the_status_class() {
        let req = resolved("GET", "http://host/x");
        let green = render(
            &req,
            &result(200, "OK", "{}"),
            &Map::new(),
            RenderOpts {
                verbose: false,
                color: true,
            },
        );
        assert!(
            green.contains(&format!("{GREEN}← 200 OK{RESET}  12ms")),
            "{green}"
        );
        let yellow = render(
            &req,
            &result(404, "Not Found", "{}"),
            &Map::new(),
            RenderOpts {
                verbose: false,
                color: true,
            },
        );
        assert!(
            yellow.contains(&format!("{YELLOW}← 404 Not Found{RESET}")),
            "{yellow}"
        );
        let red = render(
            &req,
            &result(500, "Internal Server Error", "{}"),
            &Map::new(),
            RenderOpts {
                verbose: false,
                color: true,
            },
        );
        assert!(
            red.contains(&format!("{RED}← 500 Internal Server Error{RESET}")),
            "{red}"
        );
        let plain = render(
            &req,
            &result(302, "Found", "{}"),
            &Map::new(),
            RenderOpts {
                verbose: false,
                color: true,
            },
        );
        assert!(plain.contains("← 302 Found  12ms"), "{plain}");
        assert!(!green.contains(RESET) || green.contains("→ GET"), "{green}");
    }

    #[test]
    fn non_json_bodies_print_raw() {
        let text = render(
            &resolved("GET", "http://host/x"),
            &result(200, "OK", "hello"),
            &Map::new(),
            RenderOpts::default(),
        );
        assert!(text.ends_with("hello\n"), "{text}");
        let empty = render(
            &resolved("DELETE", "http://host/x"),
            &result(204, "No Content", ""),
            &Map::new(),
            RenderOpts::default(),
        );
        assert!(empty.contains("← 204 No Content  12ms\n"), "{empty}");
        assert!(empty.ends_with("x-total-count: 5\n\n"), "{empty}");
        assert!(!empty.contains("captured:"), "{empty}");
    }

    #[test]
    fn captured_block_truncates_long_strings() {
        let mut captured = Map::new();
        captured.insert("token".to_string(), json!("e".repeat(80)));
        captured.insert("shipId".to_string(), json!(6));
        captured.insert("flag".to_string(), json!(true));
        captured.insert("short".to_string(), json!("abc"));
        let text = render(
            &resolved("POST", "http://host/x"),
            &result(201, "Created", "{}"),
            &captured,
            RenderOpts::default(),
        );
        assert!(text.contains("\ncaptured:\n"), "{text}");
        assert!(
            text.contains(&format!("  token = \"{}…\"", "e".repeat(CAPTURE_LIMIT))),
            "{text}"
        );
        assert!(text.contains("  shipId = 6\n"), "{text}");
        assert!(text.contains("  flag = true\n"), "{text}");
        assert!(text.contains("  short = \"abc\"\n"), "{text}");
    }

    #[test]
    fn verbose_prints_the_resolved_request() {
        let mut req = resolved("POST", "http://host/ships");
        req.body = Some(r#"{"name":"Curlyfries"}"#.to_string());
        let text = render(
            &req,
            &result(201, "Created", "{}"),
            &Map::new(),
            RenderOpts {
                verbose: true,
                color: false,
            },
        );
        assert!(
            text.contains("  request headers:\n    X-Trace: curlyfries\n"),
            "{text}"
        );
        assert!(
            text.contains("  request body:\n    {\"name\":\"Curlyfries\"}\n"),
            "{text}"
        );
    }

    #[test]
    fn json_line_has_the_pinned_fields() {
        let mut captured = Map::new();
        captured.insert("token".to_string(), json!("abc"));
        let line = json_line(
            &resolved("POST", "http://host/ships"),
            &result(201, "Created", r#"{"id":6}"#),
            &captured,
        );
        assert_eq!(line["request"], json!("ships/list"));
        assert_eq!(line["name"], json!("List ships"));
        assert_eq!(line["method"], json!("POST"));
        assert_eq!(line["url"], json!("http://host/ships"));
        assert_eq!(line["status"], json!(201));
        assert_eq!(line["reason"], json!("Created"));
        assert_eq!(line["durationMs"], json!(12));
        assert_eq!(
            line["headers"],
            json!([
                { "name": "content-type", "value": "application/json" },
                { "name": "x-total-count", "value": "5" }
            ])
        );
        assert_eq!(line["body"], json!({ "id": 6 }));
        assert_eq!(line["bodyText"], json!("{\"id\":6}"));
        assert_eq!(line["captured"], json!({ "token": "abc" }));

        let non_json = json_line(
            &resolved("GET", "http://host/x"),
            &result(200, "OK", "nope"),
            &Map::new(),
        );
        assert_eq!(non_json["body"], Value::Null);
        assert_eq!(non_json["bodyText"], json!("nope"));
        assert_eq!(non_json["captured"], json!({}));
    }

    #[test]
    fn request_labels_include_the_name_when_it_differs() {
        let named = request::validate(
            "ships/create",
            Path::new("requests/ships/create.json"),
            &json!({ "method": "post", "path": "/ships", "name": "Create ship" }),
        )
        .unwrap();
        assert_eq!(request_label(&named), "POST ships/create — Create ship");
        let unnamed = request::validate(
            "ships/list",
            Path::new("requests/ships/list.json"),
            &json!({ "method": "GET", "path": "/ships" }),
        )
        .unwrap();
        assert_eq!(request_label(&unnamed), "GET ships/list");
    }

    #[test]
    fn scope_lines_cover_every_value_type() {
        let mut vars = Map::new();
        vars.insert("s".to_string(), json!("text"));
        vars.insert("n".to_string(), json!(7));
        vars.insert("b".to_string(), json!(false));
        vars.insert("a".to_string(), json!([1, 2]));
        vars.insert("o".to_string(), json!({ "k": "v" }));
        vars.insert("z".to_string(), json!(null));
        let text = scope_line("dev", &vars);
        assert!(text.starts_with("dev:\n"), "{text}");
        assert!(text.contains("\n  s = \"text\""), "{text}");
        assert!(text.contains("\n  n = 7"), "{text}");
        assert!(text.contains("\n  b = false"), "{text}");
        assert!(text.contains("\n  a = [1,2]"), "{text}");
        assert!(text.contains("\n  o = {\"k\":\"v\"}"), "{text}");
        assert!(text.contains("\n  z = null"), "{text}");
        assert_eq!(scope_line("(none)", &Map::new()), "(none): (no variables)");
    }

    #[test]
    fn captured_values_are_compact_json() {
        assert_eq!(captured_value(&json!({ "a": 1 })), "{\"a\":1}");
        assert_eq!(captured_value(&json!(7)), "7");
        // Exactly at the limit stays untouched; one more character truncates.
        let at_limit: String = "e".repeat(CAPTURE_LIMIT);
        assert_eq!(
            captured_value(&json!(at_limit.clone())),
            compact(&json!(at_limit))
        );
        let over: String = "e".repeat(CAPTURE_LIMIT + 1);
        let mut expected = "e".repeat(CAPTURE_LIMIT);
        expected.push('…');
        assert_eq!(
            captured_value(&json!(over)),
            compact(&Value::String(expected))
        );
    }
}
