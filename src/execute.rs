//! HTTP execution and output capture.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use ureq::http::{HeaderMap, HeaderName, HeaderValue, Method, Request};

use crate::error::Error;
use crate::expression::{self, ResponseView};
use crate::request::{self, RequestDef};
use crate::scopes::Scopes;
use crate::template::RenderError;
use crate::{Map, template};

/// A request with every template resolved and the URL fully built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRequest {
    /// Request id.
    pub id: String,
    /// File the request came from.
    pub def_file: PathBuf,
    /// Rendered display name.
    pub name: String,
    /// Upper-case HTTP method.
    pub method: String,
    /// Absolute URL.
    pub url: String,
    /// Resolved request headers, in file order plus a default `Content-Type`.
    pub headers: Vec<(String, String)>,
    /// Serialized request body.
    pub body: Option<String>,
}

/// A received response.
#[derive(Debug)]
pub struct ExecResult {
    /// Status code.
    pub status: u16,
    /// Canonical reason phrase (may be empty).
    pub reason: String,
    /// Response headers in wire order, with original casing.
    pub headers: Vec<(String, String)>,
    /// The same headers as a map, for case-insensitive lookups.
    pub header_map: HeaderMap,
    /// Raw body text.
    pub body_text: String,
    /// Parsed body, when the body is JSON.
    pub body_json: Option<Value>,
    /// Time from starting the exchange to receiving the body, in milliseconds.
    pub duration_ms: u64,
}

/// Builds the HTTP agent. `None` means no timeouts at all.
pub fn client(timeout: Option<Duration>) -> ureq::Agent {
    let mut builder = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(format!("curlyfries/{}", env!("CARGO_PKG_VERSION")));
    if let Some(duration) = timeout {
        builder = builder
            .timeout_global(Some(duration))
            .timeout_connect(Some(duration))
            .timeout_recv_response(Some(duration))
            .timeout_recv_body(Some(duration));
    }
    builder.build().new_agent()
}

/// Joins a resolved request path onto `baseUrl`, or uses it verbatim when it is
/// already absolute.
pub fn build_url(
    request_id: &str,
    def_file: &Path,
    resolved_path: &str,
    scopes: &Scopes<'_>,
) -> Result<String, Error> {
    if resolved_path.starts_with("http://") || resolved_path.starts_with("https://") {
        return Ok(resolved_path.to_string());
    }
    let missing = || Error::MissingBaseUrl {
        request_id: request_id.to_string(),
        path: def_file.to_path_buf(),
        resolved: resolved_path.to_string(),
        env: scopes.env_name.map(|name| name.to_string()),
    };
    let Some(Value::String(base)) = scopes.lookup("baseUrl") else {
        return Err(missing());
    };
    if base.trim().is_empty() {
        return Err(missing());
    }
    Ok(format!(
        "{}/{}",
        base.trim_end_matches('/'),
        resolved_path.trim_start_matches('/')
    ))
}

/// Resolves every template in `def` into a ready-to-send request.
pub fn render_request(def: &RequestDef, scopes: &Scopes<'_>) -> Result<ResolvedRequest, Error> {
    let lookup = scopes.lookup_fn();
    let path = render_string(def, "/path", &def.path, scopes)?;
    let url = build_url(&def.id, &def.file, &path, scopes)?;
    let name = render_string(def, "/name", &def.name, scopes)?;
    if name.is_empty() {
        return Err(Error::Schema {
            path: def.file.clone(),
            pointer: "/name".to_string(),
            message: "`name` rendered to an empty string".to_string(),
        });
    }

    let mut headers = Vec::with_capacity(def.headers.len() + 1);
    for (key, value) in &def.headers {
        let pointer = format!("/headers/{}", request::pointer_token(key));
        let name = template::parse(key)
            .map_err(|problem| request::template_error(&def.file, &pointer, problem, key))?;
        let name = name
            .render_string(&lookup)
            .map_err(|error| render_failure(def, &pointer, key, scopes, error))?;
        if name.is_empty() {
            return Err(Error::Schema {
                path: def.file.clone(),
                pointer,
                message: format!("header name `{key}` rendered to an empty string"),
            });
        }
        let value = render_string(def, &pointer, value, scopes)?;
        headers.push((name, value));
    }

    let body = match &def.body {
        None => None,
        Some(value) => {
            // Report the raw text a user wrote, not its JSON serialization.
            let raw = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let rendered = template::render_tree(value, &lookup)
                .map_err(|error| render_failure(def, "/body", &raw, scopes, error))?;
            Some(serde_json::to_string(&rendered).expect("JSON values always serialize"))
        }
    };
    if body.is_some()
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }

    Ok(ResolvedRequest {
        id: def.id.clone(),
        def_file: def.file.clone(),
        name,
        method: def.method.clone(),
        url,
        headers,
        body,
    })
}

/// Renders one string slot of a request.
fn render_string(
    def: &RequestDef,
    pointer: &str,
    raw: &str,
    scopes: &Scopes<'_>,
) -> Result<String, Error> {
    let parsed = template::parse(raw)
        .map_err(|problem| request::template_error(&def.file, pointer, problem, raw))?;
    let lookup = scopes.lookup_fn();
    parsed
        .render_string(&lookup)
        .map_err(|error| render_failure(def, pointer, raw, scopes, error))
}

/// Turns a render failure into an error naming the file, pointer and value.
fn render_failure(
    def: &RequestDef,
    pointer: &str,
    raw: &str,
    scopes: &Scopes<'_>,
    error: RenderError,
) -> Error {
    match error {
        RenderError::MissingVar(name) => Error::UndefinedVariable {
            path: def.file.clone(),
            pointer: pointer.to_string(),
            value: raw.to_string(),
            name,
            env: scopes.env_name.map(|name| name.to_string()),
            env_keys: scopes.env_keys(),
            session_keys: scopes.session_keys(),
        },
        RenderError::NullInStringSlot(name) => Error::Schema {
            path: def.file.clone(),
            pointer: pointer.to_string(),
            message: format!("variable `{name}` is null and cannot be used here"),
        },
        RenderError::EmptyKey(key) => Error::Schema {
            path: def.file.clone(),
            pointer: pointer.to_string(),
            message: format!("`{key}` templated to an empty string"),
        },
        RenderError::Random(error) => Error::Schema {
            path: def.file.clone(),
            pointer: pointer.to_string(),
            message: error.to_string(),
        },
        RenderError::Syntax(problem) => request::template_error(&def.file, pointer, problem, raw),
    }
}

/// Sends `req`, returning the response even for 4xx and 5xx statuses.
pub fn execute(
    agent: &ureq::Agent,
    req: &ResolvedRequest,
    timeout: Option<Duration>,
) -> Result<ExecResult, Error> {
    let transport = |message: String| Error::Transport {
        url: req.url.clone(),
        message,
    };
    let method = Method::from_bytes(req.method.as_bytes())
        .map_err(|error| transport(format!("invalid method `{}`: {error}", req.method)))?;
    let uri: ureq::http::Uri = req
        .url
        .parse()
        .map_err(|error| transport(format!("invalid URL: {error}")))?;

    let mut header_map = HeaderMap::new();
    for (name, value) in &req.headers {
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Schema {
            path: req.def_file.clone(),
            pointer: "/headers".to_string(),
            message: format!("invalid header name `{name}`"),
        })?;
        let header_value = HeaderValue::from_str(value).map_err(|_| Error::Schema {
            path: req.def_file.clone(),
            pointer: "/headers".to_string(),
            message: format!(
                "invalid value for header `{name}` (control characters are not allowed)"
            ),
        })?;
        header_map.append(header_name, header_value);
    }

    let mut builder = Request::builder().method(method).uri(uri);
    *builder
        .headers_mut()
        .expect("request builders always hold headers") = header_map;

    let started = Instant::now();
    let outcome = match &req.body {
        Some(text) => match builder.body(text.clone()) {
            Ok(request) => agent.run(request),
            Err(error) => return Err(transport(error.to_string())),
        },
        None => match builder.body(()) {
            Ok(request) => agent.run(request),
            Err(error) => return Err(transport(error.to_string())),
        },
    };
    let mut response = match outcome {
        Ok(response) => response,
        Err(ureq::Error::Timeout(_)) => {
            return Err(Error::Timeout {
                url: req.url.clone(),
                seconds: timeout.map(|d| d.as_secs()).unwrap_or(0),
            });
        }
        Err(error) => return Err(transport(describe(error))),
    };
    let duration_ms = started.elapsed().as_millis() as u64;

    let status = response.status().as_u16();
    let reason = response
        .status()
        .canonical_reason()
        .unwrap_or("")
        .to_string();
    let header_map = response.headers().clone();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    let body_text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| transport(describe(error)))?;
    let body_json = serde_json::from_str::<Value>(&body_text).ok();

    Ok(ExecResult {
        status,
        reason,
        headers,
        header_map,
        body_text,
        body_json,
        duration_ms,
    })
}

fn describe(error: ureq::Error) -> String {
    match error {
        ureq::Error::Io(source) => source.to_string(),
        ureq::Error::StatusCode(code) => format!("unexpected status {code}"),
        other => other.to_string(),
    }
}

/// Evaluates every `outputs` expression. Nothing is returned unless all of them
/// succeed, so a failed capture stores no variables at all.
pub fn capture(
    outputs: &[(String, String)],
    res: &ExecResult,
    def_file: &Path,
) -> Result<Map, Error> {
    let view = ResponseView {
        status: res.status,
        headers: &res.header_map,
        body_text: &res.body_text,
        body_json: res.body_json.as_ref(),
    };
    let mut captured = Map::new();
    for (key, source) in outputs {
        let failed = |reason: String| Error::Output {
            path: def_file.to_path_buf(),
            key: key.clone(),
            expression: source.clone(),
            reason,
        };
        let expr = expression::parse(source).map_err(failed)?;
        let value = expression::evaluate(&expr, &view).map_err(failed)?;
        captured.insert(key.clone(), value);
    }
    Ok(captured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scopes_of<'a>(env: &'a Map, session: &'a Map, overrides: &'a Map) -> Scopes<'a> {
        Scopes {
            env_name: Some("dev"),
            env,
            session,
            overrides,
        }
    }

    fn map(pairs: &[(&str, Value)]) -> Map {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn absolute_paths_pass_through() {
        let env = map(&[("baseUrl", json!("http://127.0.0.1:1"))]);
        let empty = Map::new();
        let scopes = scopes_of(&env, &empty, &empty);
        assert_eq!(
            build_url(
                "x",
                Path::new("requests/x.json"),
                "https://example.test/a?b=1",
                &scopes
            )
            .unwrap(),
            "https://example.test/a?b=1"
        );
    }

    #[test]
    fn base_url_is_joined_without_double_slashes() {
        let empty = Map::new();
        let overrides = map(&[("baseUrl", json!("http://host:4000/"))]);
        let scopes = scopes_of(&empty, &empty, &overrides);
        assert_eq!(
            build_url("x", Path::new("requests/x.json"), "/ships?limit=2", &scopes).unwrap(),
            "http://host:4000/ships?limit=2"
        );
        let env = map(&[("baseUrl", json!("http://host:4000"))]);
        let scopes = scopes_of(&env, &empty, &empty);
        assert_eq!(
            build_url("x", Path::new("requests/x.json"), "ships", &scopes).unwrap(),
            "http://host:4000/ships"
        );
        assert_eq!(
            build_url("x", Path::new("requests/x.json"), "/", &scopes).unwrap(),
            "http://host:4000/"
        );
    }

    #[test]
    fn missing_base_url_names_the_environment() {
        let empty = Map::new();
        let scopes = scopes_of(&empty, &empty, &empty);
        let err = build_url(
            "ships/list",
            Path::new("requests/ships/list.json"),
            "/ships",
            &scopes,
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("error: no base URL for request `ships/list`"),
            "{text}"
        );
        assert!(text.contains("path: \"/ships\""), "{text}");
        assert!(text.contains("environments/dev.json"), "{text}");
        assert_eq!(err.exit_code(), 3);

        let null_base = map(&[("baseUrl", json!(null))]);
        let scopes = scopes_of(&empty, &empty, &null_base);
        assert!(build_url("x", Path::new("requests/x.json"), "/ships", &scopes).is_err());
        let empty_base = map(&[("baseUrl", json!(""))]);
        let scopes = scopes_of(&empty, &empty, &empty_base);
        assert!(build_url("x", Path::new("requests/x.json"), "/ships", &scopes).is_err());
    }

    #[test]
    fn render_request_resolves_every_slot() {
        let def = request::validate(
            "ships/create",
            Path::new("requests/ships/create.json"),
            &json!({
                "method": "POST",
                "path": "/ships?limit=${limit}",
                "headers": { "Authorization": "Bearer ${token}", "X-Num": "${limit}" },
                "body": { "capacity": "${limit}", "label": "${empty}" },
                "outputs": [ { "id": "response.body.id" } ]
            }),
        )
        .unwrap();
        let env = map(&[
            ("baseUrl", json!("http://host:4000")),
            ("limit", json!(42)),
            ("token", json!("abc")),
            ("empty", json!("")),
        ]);
        let empty = Map::new();
        let scopes = scopes_of(&env, &empty, &empty);
        let resolved = render_request(&def, &scopes).unwrap();
        assert_eq!(resolved.url, "http://host:4000/ships?limit=42");
        assert_eq!(resolved.name, "ships/create");
        assert_eq!(
            resolved.headers,
            vec![
                ("Authorization".to_string(), "Bearer abc".to_string()),
                ("X-Num".to_string(), "42".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]
        );
        assert_eq!(
            resolved.body.as_deref(),
            Some(r#"{"capacity":42,"label":""}"#)
        );
    }

    #[test]
    fn render_request_keeps_an_explicit_content_type() {
        let def = request::validate(
            "x",
            Path::new("requests/x.json"),
            &json!({
                "method": "POST",
                "path": "http://host/x",
                "headers": { "content-type": "text/plain" },
                "body": "hello"
            }),
        )
        .unwrap();
        let empty = Map::new();
        let scopes = scopes_of(&empty, &empty, &empty);
        let resolved = render_request(&def, &scopes).unwrap();
        assert_eq!(resolved.body.as_deref(), Some("\"hello\""));
        assert_eq!(
            resolved.headers,
            vec![("content-type".to_string(), "text/plain".to_string())]
        );
    }

    #[test]
    fn render_request_attaches_the_pointer_of_the_failing_slot() {
        let def = request::validate(
            "ships/create",
            Path::new("requests/ships/create.json"),
            &json!({
                "method": "POST",
                "path": "/ships",
                "headers": { "Authorization": "Bearer ${token}" },
                "body": { "name": "${shipName}" }
            }),
        )
        .unwrap();
        let env = map(&[("baseUrl", json!("http://host")), ("apiKey", json!("k"))]);
        let empty = Map::new();
        let scopes = scopes_of(&env, &empty, &empty);

        let err = render_request(&def, &scopes).unwrap_err();
        match &err {
            Error::UndefinedVariable {
                pointer,
                name,
                env_keys,
                ..
            } => {
                assert_eq!(pointer, "/headers/Authorization");
                assert_eq!(name, "token");
                assert_eq!(env_keys, &vec!["apiKey".to_string(), "baseUrl".to_string()]);
            }
            other => panic!("wrong error: {other}"),
        }

        let session = map(&[("token", json!("abc"))]);
        let scopes = scopes_of(&env, &session, &empty);
        let err = render_request(&def, &scopes).unwrap_err();
        match &err {
            Error::UndefinedVariable { pointer, name, .. } => {
                assert_eq!(pointer, "/body");
                assert_eq!(name, "shipName");
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn body_render_errors_show_the_raw_template_text() {
        let def = request::validate(
            "auth/login",
            Path::new("requests/auth/login.json"),
            &json!({ "method": "POST", "path": "/auth/login", "body": "${pirate}" }),
        )
        .unwrap();
        let env = map(&[("baseUrl", json!("http://host"))]);
        let empty = Map::new();
        let scopes = scopes_of(&env, &empty, &empty);
        let err = render_request(&def, &scopes).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("at:    /body"), "{text}");
        assert!(text.contains(r#"value: "${pirate}""#), "{text}");
    }

    #[test]
    fn render_request_rejects_null_in_string_slots() {
        let def = request::validate(
            "x",
            Path::new("requests/x.json"),
            &json!({ "method": "GET", "path": "/ships/${limit}" }),
        )
        .unwrap();
        let env = map(&[("baseUrl", json!("http://host")), ("limit", json!(null))]);
        let empty = Map::new();
        let scopes = scopes_of(&env, &empty, &empty);
        let err = render_request(&def, &scopes).unwrap_err();
        assert!(
            err.to_string().contains("variable `limit` is null"),
            "{err}"
        );
    }

    #[test]
    fn capture_is_all_or_nothing() {
        let res = ExecResult {
            status: 201,
            reason: "Created".to_string(),
            headers: vec![("x-total-count".to_string(), "5".to_string())],
            header_map: HeaderMap::new(),
            body_text: r#"{"id":6,"name":"Curlyfries","nested":{"deep":[{"v":1}]}}"#.to_string(),
            body_json: serde_json::from_str(
                r#"{"id":6,"name":"Curlyfries","nested":{"deep":[{"v":1}]}}"#,
            )
            .ok(),
            duration_ms: 3,
        };
        let def_file = Path::new("requests/ships/create.json");
        let captured = capture(
            &[
                ("shipId".to_string(), "response.body.id".to_string()),
                ("shipName".to_string(), "response.body.name".to_string()),
                (
                    "deep".to_string(),
                    "response.body.nested.deep[0].v".to_string(),
                ),
                ("status".to_string(), "response.status".to_string()),
            ],
            &res,
            def_file,
        )
        .unwrap();
        assert_eq!(
            captured,
            map(&[
                ("shipId", json!(6)),
                ("shipName", json!("Curlyfries")),
                ("deep", json!(1)),
                ("status", json!(201)),
            ])
        );

        let err = capture(
            &[
                ("ok".to_string(), "response.body.id".to_string()),
                ("bad".to_string(), "response.body.missing".to_string()),
            ],
            &res,
            def_file,
        )
        .unwrap_err();
        match &err {
            Error::Output {
                key,
                expression,
                reason,
                ..
            } => {
                assert_eq!(key, "bad");
                assert_eq!(expression, "response.body.missing");
                assert!(reason.contains("no key `missing`"), "{reason}");
            }
            other => panic!("wrong error: {other}"),
        }
        assert!(
            err.to_string()
                .contains("output `bad` failed in requests/ships/create.json")
        );
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn capture_uses_headers_case_insensitively() {
        let mut header_map = HeaderMap::new();
        header_map.insert("X-Total-Count", "5".parse().unwrap());
        let res = ExecResult {
            status: 200,
            reason: "OK".to_string(),
            headers: vec![("X-Total-Count".to_string(), "5".to_string())],
            header_map,
            body_text: "not json".to_string(),
            body_json: None,
            duration_ms: 1,
        };
        let captured = capture(
            &[(
                "count".to_string(),
                "response.headers.x-total-count".to_string(),
            )],
            &res,
            Path::new("requests/ships/list.json"),
        )
        .unwrap();
        assert_eq!(captured["count"], json!("5"));

        let err = capture(
            &[("body".to_string(), "response.body.x".to_string())],
            &res,
            Path::new("requests/ships/list.json"),
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("response body is not JSON"), "{text}");
        assert!(text.contains("content-type: (none)"), "{text}");
    }

    #[test]
    fn client_is_buildable_with_and_without_a_timeout() {
        let _ = client(None);
        let _ = client(Some(Duration::from_secs(1)));
    }
}
