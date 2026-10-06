//! `outputs` expressions: `response.status`, `response.headers.<name>` and
//! `response.body<path>`.
//!
//! Expressions are parsed when a request file is loaded (so `list` reports
//! syntax errors) and evaluated after a response arrives.

use serde_json::Value;
use ureq::http::HeaderMap;

/// A parsed output expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// `response.status`
    Status,
    /// `response.body` followed by key/index steps.
    Body(Vec<Step>),
    /// `response.headers.<name>`
    Header(String),
}

/// One step into a JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `.name` or `["name"]`
    Key(String),
    /// `[0]`
    Index(usize),
}

/// The response as seen by output expressions.
pub struct ResponseView<'a> {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: &'a HeaderMap,
    /// Raw response body text.
    pub body_text: &'a str,
    /// Parsed response body, when the body is JSON.
    pub body_json: Option<&'a Value>,
}

/// Parses an expression, returning a human-readable reason on failure.
pub fn parse(src: &str) -> Result<Expr, String> {
    let text = src.trim();
    let rest = text
        .strip_prefix("response")
        .and_then(|r| r.strip_prefix('.'))
        .ok_or_else(|| format!("expression must start with `response.`, got `{text}`"))?;
    let (root, tail) = split_root(rest);
    match root {
        "status" => {
            if !tail.is_empty() {
                return Err(format!(
                    "`response.status` takes no further segments, got `{text}`"
                ));
            }
            Ok(Expr::Status)
        }
        "body" => Ok(Expr::Body(parse_steps(tail)?)),
        "headers" => Ok(Expr::Header(parse_header(tail, text)?)),
        other => Err(format!(
            "unknown response member `{other}` (expected: body, headers, status)"
        )),
    }
}

/// Splits `rest` into the identifier and everything from the next `.` or `[`.
fn split_root(rest: &str) -> (&str, &str) {
    match rest.find(['.', '[']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    }
}

fn parse_steps(mut tail: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    while !tail.is_empty() {
        if let Some(open) = tail.strip_prefix('[') {
            let close = open
                .find(']')
                .ok_or_else(|| format!("unclosed `[` in `{tail}`"))?;
            let inner = &open[..close];
            let rest = &open[close + 1..];
            if let Some(quoted) = inner.strip_prefix('"') {
                let key = quoted
                    .strip_suffix('"')
                    .ok_or_else(|| format!("unterminated quoted key `[{inner}]`"))?;
                if key.is_empty() {
                    return Err("empty key `[\"\"]` in expression".to_string());
                }
                steps.push(Step::Key(key.to_string()));
            } else if inner.is_empty() {
                return Err("empty `[]` in expression".to_string());
            } else {
                let index: usize = inner
                    .parse()
                    .map_err(|_| format!("`[{inner}]` is not a non-negative integer index"))?;
                steps.push(Step::Index(index));
            }
            tail = rest;
        } else if let Some(open) = tail.strip_prefix('.') {
            let (ident, rest) = split_root(open);
            if ident.is_empty() {
                return Err(format!("dangling `.` in `{tail}`"));
            }
            if !ident
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!(
                    "invalid path segment `{ident}` (use [\"{ident}\"] for unusual keys)"
                ));
            }
            steps.push(Step::Key(ident.to_string()));
            tail = rest;
        } else {
            return Err(format!("expected `.` or `[` in `{tail}`"));
        }
    }
    Ok(steps)
}

fn parse_header(tail: &str, whole: &str) -> Result<String, String> {
    if tail.is_empty() {
        return Err(format!(
            "expected a header name after `response.headers` in `{whole}`"
        ));
    }
    if let Some(name) = tail.strip_prefix('.') {
        if name.is_empty() {
            return Err(format!("dangling `.` in `{whole}`"));
        }
        if name.contains(['.', '[']) {
            return Err(format!(
                "header name `{name}` must use the quoted form `[\"{name}\"]`"
            ));
        }
        return Ok(name.to_string());
    }
    if let Some(open) = tail.strip_prefix('[') {
        let close = open
            .find(']')
            .ok_or_else(|| format!("unclosed `[` in `{tail}`"))?;
        let inner = &open[..close];
        let rest = &open[close + 1..];
        if !rest.is_empty() {
            return Err(format!("unexpected `{rest}` after the header name"));
        }
        let key = inner
            .strip_prefix('"')
            .and_then(|q| q.strip_suffix('"'))
            .ok_or_else(|| format!("header name must be quoted: `[{inner}]`"))?;
        if key.is_empty() {
            return Err("empty header name `[\"\"]` in expression".to_string());
        }
        return Ok(key.to_string());
    }
    Err(format!("expected `.` or `[` in `response.headers{tail}`"))
}

/// Evaluates an expression, returning a human-readable reason on failure.
pub fn evaluate(expr: &Expr, view: &ResponseView<'_>) -> Result<Value, String> {
    match expr {
        Expr::Status => Ok(Value::Number(view.status.into())),
        Expr::Header(name) => match view.headers.get(name.as_str()) {
            Some(value) => value
                .to_str()
                .map(|s| Value::String(s.to_string()))
                .map_err(|_| format!("header `{name}` is not valid text")),
            None => Err(format!(r#"no header `{name}` in the response"#)),
        },
        Expr::Body(steps) => {
            let Some(body) = view.body_json else {
                let content_type = view
                    .headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("(none)");
                return Err(format!(
                    "response body is not JSON (content-type: {content_type})"
                ));
            };
            let mut current = body;
            for step in steps {
                current = match step {
                    Step::Key(key) => match current {
                        Value::Object(map) => map.get(key).ok_or_else(|| {
                            format!(
                                "no key `{key}` in object (available: {})",
                                available_keys(map)
                            )
                        })?,
                        other => {
                            return Err(format!(
                                "cannot read key `{key}` of {} (expected an object)",
                                type_name(other)
                            ));
                        }
                    },
                    Step::Index(index) => match current {
                        Value::Array(items) => items.get(*index).ok_or_else(|| {
                            format!("index [{index}] is out of range (length {})", items.len())
                        })?,
                        other => {
                            return Err(format!(
                                "cannot index {} with [{index}] (expected an array)",
                                type_name(other)
                            ));
                        }
                    },
                };
            }
            Ok(current.clone())
        }
    }
}

/// Sorted object keys, at most ten of them.
fn available_keys(map: &serde_json::Map<String, Value>) -> String {
    let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    let total = keys.len();
    let shown = keys.iter().take(10).copied().collect::<Vec<_>>().join(", ");
    if total > 10 {
        format!("{shown}, … {} more", total - 10)
    } else if shown.is_empty() {
        "(none)".to_string()
    } else {
        shown
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evaluate_src(src: &str, view: &ResponseView<'_>) -> Result<Value, String> {
        evaluate(&parse(src).expect("parses"), view)
    }

    fn view<'a>(status: u16, headers: &'a HeaderMap, body: &'a Value) -> ResponseView<'a> {
        ResponseView {
            status,
            headers,
            body_text: "{}",
            body_json: Some(body),
        }
    }

    #[test]
    fn parse_accepts_the_grammar() {
        assert_eq!(parse(" response.status ").unwrap(), Expr::Status);
        assert_eq!(parse("response.body").unwrap(), Expr::Body(vec![]));
        assert_eq!(
            parse("response.body.a.b").unwrap(),
            Expr::Body(vec![Step::Key("a".into()), Step::Key("b".into())])
        );
        assert_eq!(
            parse("response.body.pirates[0].name").unwrap(),
            Expr::Body(vec![
                Step::Key("pirates".into()),
                Step::Index(0),
                Step::Key("name".into())
            ])
        );
        assert_eq!(
            parse(r#"response.body["odd.key"][2]"#).unwrap(),
            Expr::Body(vec![Step::Key("odd.key".into()), Step::Index(2)])
        );
        assert_eq!(
            parse("response.headers.Content-Type").unwrap(),
            Expr::Header("Content-Type".into())
        );
        assert_eq!(
            parse(r#"response.headers["Content-Type"]"#).unwrap(),
            Expr::Header("Content-Type".into())
        );
        assert_eq!(
            parse("response.body.x-1").unwrap(),
            Expr::Body(vec![Step::Key("x-1".into())])
        );
    }

    #[test]
    fn parse_rejects_bad_expressions() {
        for (src, needle) in [
            ("status", "must start with `response.`"),
            ("response", "must start with `response.`"),
            ("response.nope", "unknown response member `nope`"),
            ("response.status.ok", "takes no further segments"),
            ("response.body.", "dangling `.`"),
            ("response.body.a.", "dangling `.`"),
            ("response.headers", "expected a header name"),
            ("response.headers.", "dangling `.`"),
            ("response.body[0", "unclosed `[`"),
            ("response.body[]", "empty `[]`"),
            ("response.body[]x]", "empty `[]`"),
            (r#"response.body["a]"#, "unterminated quoted key"),
            ("response.body[abc]", "not a non-negative integer index"),
            ("response.body.a b", "invalid path segment"),
            ("response.body.a:b", "invalid path segment"),
            ("response.headers.nope.extra", "must use the quoted form"),
        ] {
            match parse(src) {
                Err(reason) => assert!(
                    reason.contains(needle),
                    "`{src}` → `{reason}` (wanted `{needle}`)"
                ),
                Ok(expr) => panic!("`{src}` parsed as {expr:?}"),
            }
        }
    }

    #[test]
    fn evaluate_status_and_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-total-count", "5".parse().unwrap());
        let body = json!({});
        let view = view(201, &headers, &body);
        assert_eq!(evaluate_src("response.status", &view).unwrap(), json!(201));
        assert_eq!(
            evaluate_src("response.headers.x-total-count", &view).unwrap(),
            json!("5")
        );
        assert_eq!(
            evaluate_src("response.headers.Content-Type", &view).unwrap(),
            json!("application/json")
        );
        assert_eq!(
            evaluate_src(r#"response.headers["X-Total-Count"]"#, &view).unwrap(),
            json!("5")
        );
        let missing = evaluate_src("response.headers.nope", &view).unwrap_err();
        assert!(missing.contains("no header `nope`"), "{missing}");
    }

    #[test]
    fn evaluate_body_paths() {
        let headers = HeaderMap::new();
        let body = json!({
            "pirates": [ { "name": "Mad Meg Hawkins" }, { "name": "One-Legged Willie" } ],
            "user": { "role": "pirate" },
            "odd.key": 3
        });
        let view = view(200, &headers, &body);
        assert_eq!(
            evaluate_src("response.body.pirates[0].name", &view).unwrap(),
            json!("Mad Meg Hawkins")
        );
        assert_eq!(
            evaluate_src(r#"response.body["odd.key"]"#, &view).unwrap(),
            json!(3)
        );
        assert_eq!(evaluate_src("response.body", &view).unwrap(), body);
        let missing = evaluate_src("response.body.nope", &view).unwrap_err();
        assert!(missing.contains("no key `nope`"), "{missing}");
        assert!(missing.contains("available:"), "{missing}");
        let range = evaluate_src("response.body.pirates[9]", &view).unwrap_err();
        assert!(
            range.contains("out of range") && range.contains("length 2"),
            "{range}"
        );
        let on_array = evaluate_src("response.body.pirates.name", &view).unwrap_err();
        assert!(on_array.contains("expected an object"), "{on_array}");
        let on_object = evaluate_src("response.body.user[0]", &view).unwrap_err();
        assert!(on_object.contains("expected an array"), "{on_object}");
    }

    #[test]
    fn evaluate_non_json_body() {
        let headers = HeaderMap::new();
        let view = ResponseView {
            status: 200,
            headers: &headers,
            body_text: "hello",
            body_json: None,
        };
        let err = evaluate_src("response.body.x", &view).unwrap_err();
        assert!(err.contains("response body is not JSON"), "{err}");
        assert!(err.contains("content-type: (none)"), "{err}");
        assert_eq!(evaluate_src("response.status", &view).unwrap(), json!(200));
    }

    #[test]
    fn available_keys_are_trimmed() {
        let mut map = serde_json::Map::new();
        for i in 0..12 {
            map.insert(format!("k{i:02}"), json!(i));
        }
        let listed = available_keys(&map);
        assert!(listed.starts_with("k00, k01"), "{listed}");
        assert!(listed.ends_with("… 2 more"), "{listed}");
    }
}
