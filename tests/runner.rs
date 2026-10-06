//! Runner behaviour: id resolution, capture scoping and non-fatal save failures.

mod support;

use curlyfries::session::Session;
use serde_json::json;
use support::{
    StubResponse, StubServer, TempDir, environment, load_project, project_fixture, runner,
};

const LOGIN: &str = r#"{
  "name": "Log in",
  "method": "POST",
  "path": "/auth/login",
  "body": { "username": "silver", "password": "piecesof8" },
  "outputs": [ { "token": "response.body.access_token" } ]
}"#;

#[test]
fn capture_lands_in_the_environment_scope_and_is_persisted() {
    let server = StubServer::start(|_| {
        vec![StubResponse::json(
            200,
            "OK",
            r#"{"access_token":"tok-123"}"#,
        )]
    });
    let dir = project_fixture(
        "runner-scope",
        &server,
        &[("requests/auth/login.json", LOGIN)],
    );
    let (project, entries) = load_project(&dir);
    let (env_name, env) = environment(&project);
    let mut session = Session::load(&project.root, true).unwrap();

    let mut runner = runner(&project, &entries, &mut session, env_name, env);
    let outcome = runner.execute_id("auth/login.json").unwrap();
    assert_eq!(outcome.captured["token"], json!("tok-123"));
    assert_eq!(outcome.request.name, "Log in");
    assert!(runner.warnings.is_empty());
    assert_eq!(runner.session.vars("dev")["token"], json!("tok-123"));

    let reloaded = Session::load(&project.root, true).unwrap();
    assert_eq!(reloaded.vars("dev")["token"], json!("tok-123"));
    let _ = server.finish();
}

#[test]
fn unknown_ids_list_what_is_available() {
    let server = StubServer::start(|_| vec![]);
    let dir = project_fixture(
        "runner-unknown",
        &server,
        &[
            ("requests/auth/login.json", LOGIN),
            (
                "requests/auth/me.json",
                r#"{"method":"GET","path":"/auth/me"}"#,
            ),
        ],
    );
    let (project, entries) = load_project(&dir);
    let (env_name, env) = environment(&project);
    let mut session = Session::load(&project.root, true).unwrap();
    let mut runner = runner(&project, &entries, &mut session, env_name, env);
    let error = runner.execute_id("auth/me2").unwrap_err();
    let text = error.to_string();
    assert!(text.contains("unknown request `auth/me2`"), "{text}");
    assert!(text.contains("available: auth/login, auth/me"), "{text}");
    assert_eq!(error.exit_code(), 3);
}

#[test]
fn save_failures_warn_without_failing_the_request() {
    let server = StubServer::start(|_| {
        vec![StubResponse::json(
            200,
            "OK",
            r#"{"access_token":"tok-123"}"#,
        )]
    });
    let dir = project_fixture(
        "runner-save",
        &server,
        &[("requests/auth/login.json", LOGIN)],
    );
    // A file where the session directory should be makes every save fail.
    dir.write(".curlyfries", "not a directory");
    let (project, entries) = load_project(&dir);
    let (env_name, env) = environment(&project);
    let mut session = Session::load(&project.root, true).unwrap();
    let mut runner = runner(&project, &entries, &mut session, env_name, env);

    let outcome = runner.execute_id("auth/login").unwrap();
    assert_eq!(outcome.response.status, 200);
    assert_eq!(runner.warnings.len(), 1);
    assert!(
        runner.warnings[0].contains("warning: could not write session file"),
        "{:?}",
        runner.warnings
    );
    assert!(
        runner.warnings[0].contains(".curlyfries/session.json"),
        "the warning names the session file: {:?}",
        runner.warnings
    );
    // The capture is still usable in this invocation.
    assert_eq!(runner.session.vars("dev")["token"], json!("tok-123"));
    let _ = server.finish();
}

#[test]
fn captured_values_feed_later_requests() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(200, "OK", r#"{"access_token":"tok-123"}"#),
            StubResponse::json(200, "OK", r#"{"username":"silver"}"#),
        ]
    });
    let dir = project_fixture(
        "runner-chain",
        &server,
        &[
            ("requests/auth/login.json", LOGIN),
            (
                "requests/auth/me.json",
                r#"{
                  "method": "GET",
                  "path": "/auth/me",
                  "headers": { "Authorization": "Bearer ${token}" }
                }"#,
            ),
        ],
    );
    let (project, entries) = load_project(&dir);
    let (env_name, env) = environment(&project);
    let mut session = Session::load(&project.root, true).unwrap();
    {
        let mut runner = runner(&project, &entries, &mut session, env_name, env);
        assert_eq!(
            runner.execute_id("auth/login").unwrap().response.status,
            200
        );
        assert_eq!(runner.execute_id("auth/me").unwrap().response.status, 200);
        assert!(runner.warnings.is_empty());
    }
    let received = server.finish();
    assert_eq!(received.len(), 2);
    assert_eq!(received[1].header("authorization"), Some("Bearer tok-123"));
}

#[test]
fn a_second_scope_keeps_its_own_variables() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(200, "OK", r#"{"access_token":"tok-123"}"#),
            StubResponse::json(200, "OK", r#"{"access_token":"tok-123"}"#),
        ]
    });
    let dir = TempDir::new("runner-scopes");
    dir.write("curlyfries.json", "{}");
    dir.write(
        "environments/dev.json",
        &format!(r#"{{ "baseUrl": "{}" }}"#, server.base_url),
    );
    dir.write(
        "environments/stage.json",
        &format!(r#"{{ "baseUrl": "{}" }}"#, server.base_url),
    );
    dir.write("requests/auth/login.json", LOGIN);

    let project = curlyfries::project::discover(dir.path(), None).unwrap();
    let entries = curlyfries::project::collect_requests(&project).unwrap();

    let mut session = Session::load(&project.root, true).unwrap();
    for name in ["dev", "stage"] {
        let env = curlyfries::project::load_environment(&project, name).unwrap();
        let mut runner = runner(
            &project,
            &entries,
            &mut session,
            Some(name.to_string()),
            env,
        );
        runner.execute_id("auth/login").unwrap();
    }
    assert_eq!(session.vars("dev")["token"], json!("tok-123"));
    assert_eq!(session.vars("stage")["token"], json!("tok-123"));
    let reloaded = Session::load(&project.root, true).unwrap();
    assert_eq!(reloaded.vars("dev").len(), 1);
    assert_eq!(reloaded.vars("stage").len(), 1);
    let _ = server.finish();
}
