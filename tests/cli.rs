//! End-to-end tests driving the compiled binary against the stub server.

mod support;

use serde_json::{Value, json};
use support::{
    StubResponse, StubServer, TempDir, exit_code, project_fixture, run_binary, stderr, stdout,
};

const LOGIN: &str = r#"{
  "name": "Log in",
  "method": "POST",
  "path": "/auth/login",
  "body": { "username": "silver", "password": "piecesof8" },
  "outputs": [ { "token": "response.body.access_token" } ]
}"#;

const ME: &str = r#"{
  "method": "GET",
  "path": "/auth/me",
  "headers": { "Authorization": "Bearer ${token}" }
}"#;

/// A project with `auth/login` and `auth/me`, pointing at `server`.
fn auth_project(label: &str, server: &StubServer) -> TempDir {
    project_fixture(
        label,
        server,
        &[
            ("requests/auth/login.json", LOGIN),
            ("requests/auth/me.json", ME),
        ],
    )
}

#[test]
fn run_prints_the_status_headers_and_pretty_body() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::new(200, "OK")
                .header("Content-Type", "application/json")
                .body(r#"{"ships":[{"id":1}],"count":1}"#),
        ]
    });
    let dir = project_fixture(
        "cli-human",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{"method":"GET","path":"/ships?limit=2"}"#,
        )],
    );
    let output = run_binary(dir.path(), &["run", "ships/list"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("→ GET "), "{text}");
    assert!(text.contains("/ships?limit=2\n"), "{text}");
    assert!(text.contains("← 200 OK  "), "{text}");
    assert!(text.contains("content-type: application/json\n"), "{text}");
    assert!(text.contains("\"count\": 1"), "{text}");
    assert!(!text.contains("captured:"), "{text}");
    assert!(!text.contains('\u{1b}'), "no colour when piped: {text}");

    let received = server.finish();
    assert_eq!(received[0].method, "GET");
    assert_eq!(received[0].target, "/ships?limit=2");
}

#[test]
fn run_json_is_ndjson_with_the_pinned_fields() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::new(201, "Created")
                .header("Content-Type", "application/json")
                .header("X-Total-Count", "5")
                .body(r#"{"id":6,"name":"Curlyfries"}"#),
        ]
    });
    let dir = project_fixture(
        "cli-json",
        &server,
        &[(
            "requests/ships/create.json",
            r#"{
              "name": "Create ship",
              "method": "POST",
              "path": "/ships",
              "body": { "name": "Curlyfries" },
              "outputs": [ { "shipId": "response.body.id" } ]
            }"#,
        )],
    );
    let output = run_binary(dir.path(), &["run", "ships/create", "--json"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let line: Value = serde_json::from_str(lines[0]).expect("NDJSON line parses");
    assert_eq!(line["request"], json!("ships/create"));
    assert_eq!(line["name"], json!("Create ship"));
    assert_eq!(line["method"], json!("POST"));
    assert!(
        line["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:"),
        "{line}"
    );
    assert_eq!(line["status"], json!(201));
    assert_eq!(line["reason"], json!("Created"));
    assert!(line["durationMs"].is_u64(), "{line}");
    assert_eq!(line["body"], json!({ "id": 6, "name": "Curlyfries" }));
    assert_eq!(line["bodyText"], json!(r#"{"id":6,"name":"Curlyfries"}"#));
    assert_eq!(line["captured"], json!({ "shipId": 6 }));
    let headers = line["headers"].as_array().unwrap();
    assert!(
        headers
            .iter()
            .any(|h| h["name"] == json!("x-total-count") && h["value"] == json!("5")),
        "{line}"
    );
    assert!(
        headers
            .iter()
            .all(|h| h["name"].is_string() && h["value"].is_string()),
        "{line}"
    );
    let _ = server.finish();
}

#[test]
fn chained_captures_reach_the_next_request() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(200, "OK", r#"{"access_token":"tok-123"}"#),
            StubResponse::json(200, "OK", r#"{"username":"silver","role":"pirate"}"#),
        ]
    });
    let dir = auth_project("cli-chain", &server);
    let output = run_binary(dir.path(), &["run", "auth/login", "auth/me", "--json"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("NDJSON parses"))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["captured"], json!({ "token": "tok-123" }));
    assert_eq!(lines[1]["status"], json!(200));

    let received = server.finish();
    assert_eq!(received[1].header("authorization"), Some("Bearer tok-123"));
    assert!(dir.child(".curlyfries/session.json").is_file());
    let stored: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.child(".curlyfries/session.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored["version"], json!(1));
    assert_eq!(stored["varScopes"]["dev"]["token"], json!("tok-123"));
}

#[test]
fn captured_variables_persist_across_invocations() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(200, "OK", r#"{"access_token":"tok-123"}"#),
            StubResponse::json(200, "OK", r#"{"username":"silver"}"#),
        ]
    });
    let dir = auth_project("cli-persist", &server);
    let login = run_binary(dir.path(), &["run", "auth/login"]);
    assert_eq!(exit_code(&login), 0, "{}", stderr(&login));

    // Second invocation: no login, the token comes from the session file.
    let me = run_binary(dir.path(), &["run", "auth/me"]);
    assert_eq!(exit_code(&me), 0, "{}", stderr(&me));
    assert!(stdout(&me).contains("← 200 OK"), "{}", stdout(&me));

    let show = run_binary(dir.path(), &["session", "show", "--json"]);
    assert_eq!(exit_code(&show), 0, "{}", stderr(&show));
    let vars: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(vars, json!({ "token": "tok-123" }));

    let human = run_binary(dir.path(), &["session", "show"]);
    assert_eq!(stdout(&human), "dev:\n  token = \"tok-123\"\n");

    let received = server.finish();
    assert_eq!(received.len(), 2);
    assert_eq!(received[1].header("authorization"), Some("Bearer tok-123"));

    let clear = run_binary(dir.path(), &["session", "clear"]);
    assert_eq!(exit_code(&clear), 0, "{}", stderr(&clear));
    let after: Value = serde_json::from_str(&stdout(&run_binary(
        dir.path(),
        &["session", "show", "--json"],
    )))
    .unwrap();
    assert_eq!(after, json!({}));
    // A cleared session is still readable and empty.
    assert_eq!(exit_code(&run_binary(dir.path(), &["session", "show"])), 0);
}

#[test]
fn no_session_ignores_the_stored_captures() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(200, "OK", r#"{"access_token":"tok-123"}"#),
            StubResponse::json(200, "OK", r#"{"username":"silver"}"#),
        ]
    });
    let dir = auth_project("cli-no-session", &server);
    assert_eq!(
        exit_code(&run_binary(dir.path(), &["run", "auth/login"])),
        0
    );

    let me = run_binary(dir.path(), &["--no-session", "run", "auth/me"]);
    assert_eq!(exit_code(&me), 3);
    let text = stderr(&me);
    assert!(text.contains("undefined variable `token`"), "{text}");
    assert!(text.contains("/headers/Authorization"), "{text}");
    assert!(text.contains("value: \"Bearer ${token}\""), "{text}");
    assert!(
        text.contains("known: environment `dev` → baseUrl, limit"),
        "{text}"
    );
    assert!(stdout(&me).is_empty(), "{}", stdout(&me));
    let _ = server.finish();
}

#[test]
fn undefined_variables_are_loud() {
    let server = StubServer::start(|_| vec![]);
    let dir = auth_project("cli-undefined", &server);
    let output = run_binary(dir.path(), &["run", "auth/me"]);
    assert_eq!(exit_code(&output), 3);
    let text = stderr(&output);
    assert!(
        text.starts_with("error: undefined variable `token` in requests/auth/me.json\n  at:    /headers/Authorization"),
        "{text}"
    );
    assert!(text.contains("session"), "{text}");
    let _ = server.finish();
}

#[test]
fn fail_on_error_controls_the_exit_code() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(500, "Internal Server Error", r#"{"error":{"code":"boom"}}"#),
            StubResponse::json(500, "Internal Server Error", r#"{"error":{"code":"boom"}}"#),
        ]
    });
    let dir = project_fixture(
        "cli-fail-on-error",
        &server,
        &[(
            "requests/debug/boom.json",
            r#"{"method":"GET","path":"/_debug/status/500"}"#,
        )],
    );
    let plain = run_binary(dir.path(), &["run", "debug/boom"]);
    assert_eq!(exit_code(&plain), 0, "{}", stderr(&plain));
    assert!(
        stdout(&plain).contains("← 500 Internal Server Error"),
        "{}",
        stdout(&plain)
    );

    let strict = run_binary(dir.path(), &["run", "debug/boom", "--fail-on-error"]);
    assert_eq!(exit_code(&strict), 4, "{}", stderr(&strict));
    let _ = server.finish();
}

#[test]
fn unknown_request_ids_list_the_available_ones() {
    let server = StubServer::start(|_| vec![]);
    let dir = auth_project("cli-unknown-id", &server);
    let output = run_binary(dir.path(), &["run", "ships/crete"]);
    assert_eq!(exit_code(&output), 3);
    let text = stderr(&output);
    assert!(text.contains("unknown request `ships/crete`"), "{text}");
    assert!(text.contains("available: auth/login, auth/me"), "{text}");
    let _ = server.finish();
}

#[test]
fn list_prints_ids_and_validates_the_project() {
    let server = StubServer::start(|_| vec![]);
    let dir = auth_project("cli-list", &server);
    let output = run_binary(dir.path(), &["list"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output), "POST auth/login — Log in\nGET auth/me\n");

    let listed = run_binary(dir.path(), &["list", "--json"]);
    let array: Value = serde_json::from_str(&stdout(&listed)).unwrap();
    assert_eq!(
        array,
        json!([
            { "id": "auth/login", "method": "POST", "name": "Log in", "path": "requests/auth/login.json" },
            { "id": "auth/me", "method": "GET", "name": "auth/me", "path": "requests/auth/me.json" }
        ])
    );
    let _ = server.finish();
}

#[test]
fn list_reports_broken_request_files() {
    let server = StubServer::start(|_| vec![]);
    let dir = project_fixture(
        "cli-list-broken",
        &server,
        &[(
            "requests/broken.json",
            r#"{ "method": "GET", "path": "/x", "ouputs": [] }"#,
        )],
    );
    let output = run_binary(dir.path(), &["list"]);
    assert_eq!(exit_code(&output), 3);
    let text = stderr(&output);
    assert!(
        text.contains("invalid request file requests/broken.json"),
        "{text}"
    );
    assert!(text.contains("unknown key `ouputs`"), "{text}");
    assert!(text.contains("at:    /ouputs"), "{text}");
    let _ = server.finish();
}

#[test]
fn timeouts_exit_with_one() {
    let server = StubServer::start_silent();
    let dir = project_fixture(
        "cli-timeout",
        &server,
        &[(
            "requests/debug/slow.json",
            r#"{"method":"GET","path":"/_debug/slow?ms=3000"}"#,
        )],
    );
    let output = run_binary(dir.path(), &["--timeout", "1", "run", "debug/slow"]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("timed out after 1s"), "{text}");
    assert!(text.contains("/_debug/slow?ms=3000"), "{text}");
    let _ = server.finish();
}

#[test]
fn running_outside_a_project_explains_how_to_fix_it() {
    let dir = TempDir::new("cli-no-project");
    let output = run_binary(dir.path(), &["list"]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("no curlyfries.json found in"), "{text}");
    assert!(text.contains("-C <dir>"), "{text}");
}

#[test]
fn var_overrides_environment_values_and_parses_json() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "cli-var",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{"method":"GET","path":"/ships?limit=${limit}"}"#,
        )],
    );
    let output = run_binary(dir.path(), &["--var", "limit=7", "run", "ships/list"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let received = server.finish();
    assert_eq!(received[0].target, "/ships?limit=7");

    // A string override keeps its literal value.
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "cli-var-string",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{"method":"GET","path":"/ships?q=${limit}"}"#,
        )],
    );
    let output = run_binary(dir.path(), &["--var", "limit=seven", "run", "ships/list"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let received = server.finish();
    assert_eq!(received[0].target, "/ships?q=seven");

    // A malformed override is a usage error.
    let server = StubServer::start(|_| vec![]);
    let dir = auth_project("cli-var-bad", &server);
    let output = run_binary(dir.path(), &["--var", "novalue", "run", "auth/me"]);
    assert_eq!(exit_code(&output), 2);
    assert!(
        stderr(&output).contains("expected NAME=VALUE"),
        "{}",
        stderr(&output)
    );
    let bad_name = run_binary(dir.path(), &["--var", "1limit=7", "run", "auth/me"]);
    assert_eq!(exit_code(&bad_name), 2);
    assert!(
        stderr(&bad_name).contains("invalid variable name `1limit`"),
        "{}",
        stderr(&bad_name)
    );
    let _ = server.finish();
}

#[test]
fn timeout_zero_disables_timeouts() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "cli-timeout-zero",
        &server,
        &[("requests/where.json", r#"{"method":"GET","path":"/where"}"#)],
    );
    let output = run_binary(dir.path(), &["--timeout", "0", "run", "where"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).contains("← 200 OK"), "{}", stdout(&output));
    let _ = server.finish();
}

#[test]
fn a_var_can_supply_the_base_url() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = TempDir::new("cli-var-base-url");
    dir.write("curlyfries.json", "{}");
    dir.write("requests/where.json", r#"{"method":"GET","path":"/where"}"#);

    let missing = run_binary(dir.path(), &["run", "where"]);
    assert_eq!(exit_code(&missing), 3);
    assert!(
        stderr(&missing).contains("no base URL for request `where`"),
        "{}",
        stderr(&missing)
    );
    assert!(
        stderr(&missing).contains("an environment file"),
        "{}",
        stderr(&missing)
    );

    let base = format!("baseUrl={}", server.base_url);
    let output = run_binary(dir.path(), &["--var", &base, "run", "where"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let received = server.finish();
    assert_eq!(received[0].target, "/where");
}

#[test]
fn env_selects_between_environment_files() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(200, "OK", "{}"),
            StubResponse::json(200, "OK", "{}"),
        ]
    });
    let dir = project_fixture(
        "cli-env",
        &server,
        &[("requests/where.json", r#"{"method":"GET","path":"/where"}"#)],
    );
    // `dev` is the default environment and points at the stub.
    let dev = run_binary(dir.path(), &["run", "where"]);
    assert_eq!(exit_code(&dev), 0, "{}", stderr(&dev));
    // `stage` points at a different base path on the same server.
    dir.write(
        "environments/stage.json",
        &format!(r#"{{ "baseUrl": "{}/stage" }}"#, server.base_url),
    );
    let stage = run_binary(dir.path(), &["--env", "stage", "run", "where"]);
    assert_eq!(exit_code(&stage), 0, "{}", stderr(&stage));

    let received = server.finish();
    assert_eq!(received[0].target, "/where");
    assert_eq!(received[1].target, "/stage/where");

    let unknown = run_binary(dir.path(), &["--env", "nope", "run", "where"]);
    assert_eq!(exit_code(&unknown), 3);
    assert!(
        stderr(&unknown).contains("unknown environment `nope`"),
        "{}",
        stderr(&unknown)
    );
}

#[test]
fn help_and_version_exit_cleanly() {
    let dir = TempDir::new("cli-help");
    let help = run_binary(dir.path(), &["--help"]);
    assert_eq!(exit_code(&help), 0);
    let text = stdout(&help);
    for flag in ["--var", "--env", "--no-session", "--timeout", "--project"] {
        assert!(text.contains(flag), "help is missing {flag}: {text}");
    }
    assert!(
        text.contains("run") && text.contains("list") && text.contains("session"),
        "{text}"
    );

    let sub_help = run_binary(dir.path(), &["run", "--help"]);
    assert_eq!(exit_code(&sub_help), 0);
    assert!(
        stdout(&sub_help).contains("--fail-on-error"),
        "{}",
        stdout(&sub_help)
    );

    let version = run_binary(dir.path(), &["--version"]);
    assert_eq!(exit_code(&version), 0);
    assert!(
        stdout(&version).contains("curlyfries 0.1.0"),
        "{}",
        stdout(&version)
    );

    let usage = run_binary(dir.path(), &["run"]);
    assert_eq!(exit_code(&usage), 2);
    let unknown_flag = run_binary(dir.path(), &["--nope"]);
    assert_eq!(exit_code(&unknown_flag), 2);
}

#[test]
fn interactive_mode_requires_a_terminal() {
    let server = StubServer::start(|_| vec![]);
    let dir = auth_project("cli-not-a-terminal", &server);
    let output = run_binary(dir.path(), &[]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(
        text.contains("interactive mode requires a terminal"),
        "{text}"
    );
    assert!(text.contains("curlyfries run <id>"), "{text}");
    let _ = server.finish();
}

#[test]
fn a_project_without_requests_cannot_run_interactively() {
    let dir = TempDir::new("cli-empty");
    dir.write("curlyfries.json", "{}");
    dir.mkdir("requests");
    let output = run_binary(dir.path(), &[]);
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).contains("no request files found"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_missing_requests_directory_names_the_manifest_key() {
    let dir = TempDir::new("cli-missing-dir");
    dir.write("curlyfries.json", r#"{ "requestsDir": "specs" }"#);
    let output = run_binary(dir.path(), &["list"]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("requests directory not found"), "{text}");
    assert!(text.contains("specs"), "{text}");
}

#[test]
fn project_flag_overrides_discovery() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "cli-project-flag",
        &server,
        &[("requests/where.json", r#"{"method":"GET","path":"/where"}"#)],
    );
    let elsewhere = TempDir::new("cli-elsewhere");
    let project = dir.path().to_string_lossy().into_owned();
    let output = run_binary(elsewhere.path(), &["-C", &project, "run", "where"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).contains("← 200 OK"), "{}", stdout(&output));
    let _ = server.finish();
}

#[test]
fn a_broken_session_file_is_an_error() {
    let server = StubServer::start(|_| vec![]);
    let dir = auth_project("cli-broken-session", &server);
    dir.write(".curlyfries/session.json", r#"{ "version": 2 }"#);
    let output = run_binary(dir.path(), &["run", "auth/me"]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("unreadable session file"), "{text}");
    assert!(
        text.contains("unsupported version 2 (expected 1)"),
        "{text}"
    );
    assert!(text.contains("session clear --all"), "{text}");
    let _ = server.finish();
}

#[test]
fn verbose_shows_the_resolved_request() {
    let server = StubServer::start(|_| vec![StubResponse::json(201, "Created", "{}")]);
    let dir = project_fixture(
        "cli-verbose",
        &server,
        &[(
            "requests/ships/create.json",
            r#"{
              "method": "POST",
              "path": "/ships",
              "headers": { "X-Trace": "${limit}" },
              "body": { "capacity": "${limit}" }
            }"#,
        )],
    );
    let output = run_binary(dir.path(), &["run", "ships/create", "--verbose"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("  request headers:\n    X-Trace: 2\n"),
        "{text}"
    );
    assert!(
        text.contains("  request body:\n    {\"capacity\":2}\n"),
        "{text}"
    );
    assert!(text.contains("Content-Type: application/json"), "{text}");
    let _ = server.finish();
}

#[test]
fn response_body_escapes_are_stripped() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::new(200, "OK")
                .header("Content-Type", "text/plain")
                .body("ok\u{1b}]52;c;cHdk\u{7}\n"),
        ]
    });
    let dir = project_fixture(
        "cli-escape-body",
        &server,
        &[("requests/data.json", r#"{"method":"GET","path":"/data"}"#)],
    );
    let output = run_binary(dir.path(), &["run", "data"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("ok"), "{text:?}");
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(!text.contains('\u{7}'), "{text:?}");
    let _ = server.finish();
}

#[test]
fn output_error_keys_are_escaped() {
    let server = StubServer::start(|_| {
        vec![StubResponse::json(
            200,
            "OK",
            "{\"\\u001b]52;c;a2V5\\u0007\":1,\"ok\":true}",
        )]
    });
    let dir = project_fixture(
        "cli-escape-output",
        &server,
        &[(
            "requests/data.json",
            r#"{
              "method": "GET",
              "path": "/data",
              "outputs": [ { "missing": "response.body.nope" } ]
            }"#,
        )],
    );
    let output = run_binary(dir.path(), &["run", "data"]);
    assert_eq!(exit_code(&output), 3);
    let text = stderr(&output);
    assert!(text.contains("no key `nope`"), "{text:?}");
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(!text.contains('\u{7}'), "{text:?}");
    let _ = server.finish();
}

#[test]
fn redirects_are_not_followed_by_default() {
    let server = StubServer::start(|base_url| {
        vec![
            StubResponse::new(302, "Found").header("Location", &format!("{base_url}/second")),
            StubResponse::json(200, "OK", "{}"),
        ]
    });
    let dir = project_fixture(
        "cli-redirect-off",
        &server,
        &[("requests/start.json", r#"{"method":"GET","path":"/start"}"#)],
    );
    let output = run_binary(dir.path(), &["run", "start"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("← 302 Found"),
        "{}",
        stdout(&output)
    );
    let text = stderr(&output);
    assert!(
        text.contains("warning: response is a redirect to"),
        "{text:?}"
    );
    assert_eq!(server.request_count(), 1);
    let _ = server.finish();
}

#[test]
fn follow_redirects_follows_same_origin() {
    let server = StubServer::start(|base_url| {
        vec![
            StubResponse::new(302, "Found").header("Location", &format!("{base_url}/second")),
            StubResponse::json(200, "OK", "{}"),
        ]
    });
    let dir = project_fixture(
        "cli-redirect-on",
        &server,
        &[("requests/start.json", r#"{"method":"GET","path":"/start"}"#)],
    );
    dir.write(
        "curlyfries.json",
        r#"{ "defaultEnvironment": "dev", "followRedirects": true }"#,
    );
    let output = run_binary(dir.path(), &["run", "start"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("← 200 OK"), "{text}");
    assert!(text.contains("redirected:"), "{text}");
    assert_eq!(server.request_count(), 2);
    let _ = server.finish();
}
