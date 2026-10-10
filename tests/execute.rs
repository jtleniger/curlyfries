//! HTTP execution tests against the stub server.

mod support;

use std::path::Path;
use std::time::Duration;

use curlyfries::error::Error;
use curlyfries::execute::{self, ExecResult};
use curlyfries::request::RequestDef;
use curlyfries::scopes::Scopes;
use curlyfries::{Map, request};
use serde_json::{Value, json};
use support::StubServer;

fn map(pairs: &[(&str, Value)]) -> Map {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

fn def(id: &str, value: Value) -> RequestDef {
    request::validate(id, Path::new("requests/test.json"), &value)
        .unwrap_or_else(|error| panic!("test request must be valid: {error}"))
}

/// Renders and sends `def` against the stub, with fresh in-memory scopes.
#[allow(clippy::result_large_err)]
fn send(
    def: &RequestDef,
    env: &Map,
    timeout: Option<Duration>,
    follow_redirects: bool,
) -> Result<ExecResult, Error> {
    let env = curlyfries::variables::Variables::from_values(env.clone());
    let session = curlyfries::variables::Variables::default();
    let scopes = Scopes {
        env_name: Some("dev"),
        env: &env,
        session: &session,
    };
    let resolved = execute::render_request(def, &scopes)?;
    execute::execute(
        &execute::client(timeout),
        &resolved,
        timeout,
        follow_redirects,
    )
}

#[test]
fn base_url_join_preserves_the_query_string() {
    let server = StubServer::start(|_| vec![support::StubResponse::json(200, "OK", "{}")]);
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def(
        "ships/list",
        json!({ "method": "GET", "path": "/ships?limit=2&sort=-launchedYear" }),
    );
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.status, 200);
    let received = server.finish();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].target, "/ships?limit=2&sort=-launchedYear");
}

#[test]
fn absolute_paths_bypass_the_base_url() {
    let server = StubServer::start(|_| vec![support::StubResponse::json(200, "OK", "{}")]);
    let absolute = format!("{}/direct", server.base_url);
    let env = map(&[("baseUrl", json!("http://127.0.0.1:9"))]);
    let request = def("x", json!({ "method": "GET", "path": absolute }));
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(server.finish()[0].target, "/direct");
}

#[test]
fn missing_base_url_fails_before_sending() {
    let env = map(&[("limit", json!(2))]);
    let request = def("ships/list", json!({ "method": "GET", "path": "/ships" }));
    let error = send(&request, &env, None, false).unwrap_err();
    assert!(matches!(error, Error::MissingBaseUrl { .. }), "{error}");
    assert_eq!(error.exit_code(), 3);
}

#[test]
fn post_sends_typed_json_and_a_default_content_type() {
    let server =
        StubServer::start(|_| vec![support::StubResponse::json(201, "Created", r#"{"id":6}"#)]);
    let env = map(&[
        ("baseUrl", json!(server.base_url)),
        ("shipName", json!("Curlyfries")),
        ("capacity", json!(42)),
    ]);
    let request = def(
        "ships/create",
        json!({
            "method": "post",
            "path": "/ships",
            "body": { "name": "${shipName}", "crewCapacity": "${capacity}" }
        }),
    );
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.status, 201);
    assert_eq!(result.body_json, Some(json!({ "id": 6 })));

    let received = server.finish();
    let sent = &received[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(
        sent.body_text(),
        r#"{"name":"Curlyfries","crewCapacity":42}"#
    );
    assert_eq!(sent.body_json()["crewCapacity"], json!(42));
    assert_eq!(
        sent.header("user-agent"),
        Some(concat!("curlyfries/", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn user_supplied_content_type_and_user_agent_win() {
    let server = StubServer::start(|_| vec![support::StubResponse::new(200, "OK")]);
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def(
        "x",
        json!({
            "method": "POST",
            "path": "/x",
            "headers": { "Content-Type": "text/plain", "User-Agent": "custom/9" },
            "body": "hello"
        }),
    );
    send(&request, &env, None, false).unwrap();
    let received = server.finish();
    assert_eq!(received[0].header("content-type"), Some("text/plain"));
    assert_eq!(received[0].header("user-agent"), Some("custom/9"));
    assert_eq!(received[0].body_text(), "\"hello\"");
}

#[test]
fn get_without_a_body_sends_no_bytes() {
    let server = StubServer::start(|_| vec![support::StubResponse::new(204, "No Content")]);
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def("x", json!({ "method": "GET", "path": "/x" }));
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.status, 204);
    assert_eq!(result.body_text, "");
    assert_eq!(result.body_json, None);
    let received = server.finish();
    assert_eq!(received[0].body.len(), 0);
    assert_eq!(received[0].header("content-length"), None);
}

#[test]
fn headers_and_status_come_back_in_wire_order() {
    let server = StubServer::start(|_| {
        vec![
            support::StubResponse::new(200, "OK")
                .header("X-Total-Count", "5")
                .header("Content-Type", "application/json")
                .body(r#"{"ships":[{"id":1,"name":"Sea Serpent"}]}"#),
        ]
    });
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def("ships/list", json!({ "method": "GET", "path": "/ships" }));
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.reason, "OK");
    // ureq's http stack normalises header names to lower case, so the recorded
    // names are lower-cased while values and order stay exactly as sent.
    assert_eq!(result.headers[0].0, "content-length");
    let names: Vec<&str> = result
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let total = names
        .iter()
        .position(|name| *name == "x-total-count")
        .unwrap();
    let content = names
        .iter()
        .position(|name| *name == "content-type")
        .unwrap();
    assert!(total < content, "wire order is preserved: {names:?}");
    assert_eq!(result.headers[total].1, "5");
    assert_eq!(result.header_map.get("x-total-count").unwrap(), "5");
    assert!(result.body_json.is_some());
    let _ = server.finish();
}

#[test]
fn non_json_bodies_keep_their_text() {
    let server = StubServer::start(|_| {
        vec![
            support::StubResponse::new(200, "OK")
                .header("Content-Type", "text/plain")
                .body("not json at all"),
        ]
    });
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def("x", json!({ "method": "GET", "path": "/x" }));
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.body_json, None);
    assert_eq!(result.body_text, "not json at all");
    let _ = server.finish();
}

#[test]
fn error_statuses_are_normal_results() {
    let server = StubServer::start(|_| {
        vec![support::StubResponse::json(
            404,
            "Not Found",
            r#"{"error":{"code":"not_found","message":"no such ship"}}"#,
        )]
    });
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def(
        "ships/missing",
        json!({ "method": "GET", "path": "/ships/9999" }),
    );
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.status, 404);
    assert_eq!(result.reason, "Not Found");
    assert_eq!(
        result.body_json.unwrap()["error"]["code"],
        json!("not_found")
    );
    let _ = server.finish();
}

#[test]
fn redirects_are_not_followed_unless_enabled() {
    let off = StubServer::start(|base_url| {
        vec![
            support::StubResponse::new(302, "Found")
                .header("Location", &format!("{base_url}/second")),
            support::StubResponse::json(200, "OK", r#"{"moved":true}"#),
        ]
    });
    let off_url = off.base_url.clone();
    let env = map(&[("baseUrl", json!(off_url.clone()))]);
    let request = def("x", json!({ "method": "GET", "path": "/start" }));
    let result = send(&request, &env, None, false).unwrap();
    assert_eq!(result.status, 302);
    assert_eq!(
        result.unfollowed_redirect.as_deref(),
        Some(format!("{off_url}/second").as_str())
    );
    assert_eq!(result.final_url, format!("{off_url}/start"));
    let targets: Vec<String> = off.finish().into_iter().map(|r| r.target).collect();
    assert_eq!(targets, vec!["/start"]);

    let on = StubServer::start(|base_url| {
        vec![
            support::StubResponse::new(302, "Found")
                .header("Location", &format!("{base_url}/second")),
            support::StubResponse::json(200, "OK", r#"{"moved":true}"#),
        ]
    });
    let on_url = on.base_url.clone();
    let env = map(&[("baseUrl", json!(on_url.clone()))]);
    let result = send(&request, &env, None, true).unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.body_json, Some(json!({ "moved": true })));
    assert_eq!(result.final_url, format!("{on_url}/second"));
    assert_eq!(result.unfollowed_redirect, None);
    let targets: Vec<String> = on.finish().into_iter().map(|r| r.target).collect();
    assert_eq!(targets, vec!["/start", "/second"]);
}

#[test]
fn same_origin_redirects_carry_the_request_headers() {
    let server = StubServer::start(|base_url| {
        vec![
            support::StubResponse::new(302, "Found")
                .header("Location", &format!("{base_url}/second")),
            support::StubResponse::json(200, "OK", "{}"),
        ]
    });
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def(
        "x",
        json!({
            "method": "GET",
            "path": "/start",
            "headers": { "Authorization": "Bearer x" }
        }),
    );
    let result = send(&request, &env, None, true).unwrap();
    assert_eq!(result.status, 200);
    let received = server.finish();
    assert_eq!(received.len(), 2);
    assert_eq!(received[1].header("authorization"), Some("Bearer x"));
}

#[test]
fn cross_origin_redirect_is_refused_and_leaks_nothing() {
    let victim = StubServer::start(|_| vec![support::StubResponse::json(200, "OK", "{}")]);
    let victim_url = victim.base_url.clone();
    let attacker = StubServer::start(move |_| {
        vec![
            support::StubResponse::new(302, "Found")
                .header("Location", &format!("{victim_url}/steal")),
        ]
    });
    let env = map(&[("baseUrl", json!(attacker.base_url))]);
    let request = def(
        "x",
        json!({
            "method": "GET",
            "path": "/start",
            "headers": { "Authorization": "Bearer x" }
        }),
    );
    let error = send(&request, &env, None, true).unwrap_err();
    assert!(matches!(error, Error::Redirect { .. }), "{error}");
    assert_eq!(error.exit_code(), 1);
    let _ = attacker.finish();
    assert_eq!(victim.request_count(), 0);
}

#[test]
fn slow_responses_hit_the_timeout() {
    let server = StubServer::start_silent();
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def(
        "x",
        json!({ "method": "GET", "path": "/_debug/slow?ms=3000" }),
    );
    let error = send(&request, &env, Some(Duration::from_secs(1)), false).unwrap_err();
    match error {
        Error::Timeout { seconds, url } => {
            assert_eq!(seconds, 1);
            assert!(url.ends_with("/_debug/slow?ms=3000"), "{url}");
        }
        other => panic!("wrong error: {other}"),
    }
    let received = server.finish();
    assert_eq!(received.len(), 1, "the request reached the server");
}

#[test]
fn refused_connections_are_transport_errors() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let env = map(&[("baseUrl", json!(format!("http://{address}")))]);
    let request = def("x", json!({ "method": "GET", "path": "/nope" }));
    let error = send(&request, &env, Some(Duration::from_secs(5)), false).unwrap_err();
    let text = error.to_string();
    assert!(text.starts_with("error: transport error:"), "{text}");
    assert!(
        text.contains(&format!("url: http://{address}/nope")),
        "{text}"
    );
    assert_eq!(error.exit_code(), 1);
}

#[test]
fn captures_cover_nested_paths_status_and_headers() {
    let server = StubServer::start(|_| {
        vec![
            support::StubResponse::new(200, "OK")
                .header("X-Total-Count", "5")
                .header("Content-Type", "application/json")
                .body(r#"{"pirates":[{"name":"Mad Meg Hawkins"},{"name":"One-Legged Willie"}]}"#),
        ]
    });
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def(
        "ships/detail",
        json!({
            "method": "GET",
            "path": "/ships/1",
            "outputs": {
                "firstPirateName": "response.body.pirates[0].name",
                "shipCount": "response.headers.x-total-count",
                "status": "response.status"
            }
        }),
    );
    let env = curlyfries::variables::Variables::from_values(env);
    let session = curlyfries::variables::Variables::default();
    let scopes = Scopes {
        env_name: Some("dev"),
        env: &env,
        session: &session,
    };
    let resolved = execute::render_request(&request, &scopes).unwrap();
    let result = execute::execute(&execute::client(None), &resolved, None, false).unwrap();
    let captured = execute::capture(&request.outputs, &result, &request.file).unwrap();
    assert_eq!(
        captured.get("firstPirateName"),
        Some(&json!("Mad Meg Hawkins"))
    );
    assert_eq!(captured.get("shipCount"), Some(&json!("5")));
    assert_eq!(captured.get("status"), Some(&json!(200)));
    let _ = server.finish();
}

#[test]
fn failed_captures_store_nothing() {
    let server = StubServer::start(|_| vec![support::StubResponse::json(200, "OK", r#"{"id":6}"#)]);
    let env = map(&[("baseUrl", json!(server.base_url))]);
    let request = def(
        "ships/create",
        json!({
            "method": "POST",
            "path": "/ships",
            "body": {},
            "outputs": { "shipId": "response.body.id", "nope": "response.body.missing" }
        }),
    );
    let env = curlyfries::variables::Variables::from_values(env);
    let session = curlyfries::variables::Variables::default();
    let scopes = Scopes {
        env_name: Some("dev"),
        env: &env,
        session: &session,
    };
    let resolved = execute::render_request(&request, &scopes).unwrap();
    let result = execute::execute(&execute::client(None), &resolved, None, false).unwrap();
    let error = execute::capture(&request.outputs, &result, &request.file).unwrap_err();
    assert!(matches!(error, Error::Output { .. }), "{error}");
    // The caller's map is untouched: nothing was captured.
    assert!(session.is_empty());
    let _ = server.finish();
}
