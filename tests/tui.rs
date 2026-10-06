//! Full-screen TUI behaviour, driven headlessly through a `TestBackend`.

mod support;

use std::fs;
use std::time::Duration;

use curlyfries::tui::TuiConfig;
use curlyfries::tui::app::App;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use support::{StubResponse, StubServer, load_project, project_fixture};

const LOGIN: &str = r#"{
  "name": "Log in",
  "method": "POST",
  "path": "/auth/login",
  "body": { "username": "silver" },
  "outputs": [ { "token": "response.body.access_token" } ]
}"#;

const SHIPS: &str = r#"{
  "method": "GET",
  "path": "/ships?limit=${limit}"
}"#;

const CREATE: &str = r#"{
  "method": "POST",
  "path": "/ships",
  "outputs": [ { "shipId": "response.body.id" } ]
}"#;

/// A key press with no modifiers.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A key press with modifiers.
fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

/// Renders one frame and flattens the terminal buffer into text.
fn rendered(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(|frame| app.draw(frame)).expect("frame draws");
    let buffer = terminal.backend().buffer();
    let stride = buffer.area.width as usize;
    let mut text = String::new();
    for (index, cell) in buffer.content.iter().enumerate() {
        if index > 0 && index % stride == 0 {
            text.push('\n');
        }
        text.push_str(cell.symbol());
    }
    text
}

/// Opens the fixture project with the given config.
fn open(config: TuiConfig, dir: &support::TempDir) -> App {
    let (project, entries) = load_project(dir);
    App::new(config, project, entries).expect("app builds")
}

fn config() -> TuiConfig {
    TuiConfig {
        env: None,
        overrides: curlyfries::Map::new(),
        no_session: false,
        timeout: Some(Duration::from_secs(5)),
    }
}

#[test]
fn tree_lists_requests_and_folders() {
    let server = StubServer::start(|_| Vec::new());
    let dir = project_fixture(
        "tui-tree",
        &server,
        &[
            ("requests/auth/login.json", LOGIN),
            ("requests/ships/list.json", SHIPS),
        ],
    );
    let mut app = open(config(), &dir);
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("auth/"), "{screen}");
    assert!(screen.contains("ships/"), "{screen}");
    assert!(screen.contains("POST"), "{screen}");
    assert!(screen.contains("GET"), "{screen}");
}

#[test]
fn enter_runs_the_selected_request_and_shows_the_response() {
    let server = StubServer::start(|_| {
        vec![StubResponse::json(
            200,
            "OK",
            r#"{"access_token":"tok-123"}"#,
        )]
    });
    let dir = project_fixture("tui-run", &server, &[("requests/auth/login.json", LOGIN)]);
    let mut app = open(config(), &dir);
    app.select_id("auth/login");
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("200 OK"), "{screen}");
    assert!(screen.contains("token"), "{screen}");
    assert!(screen.contains("tok-123"), "{screen}");

    let text = fs::read_to_string(dir.child(".curlyfries/session.json")).expect("session file");
    let stored: serde_json::Value = serde_json::from_str(&text).expect("session JSON");
    assert_eq!(
        stored["varScopes"]["dev"]["token"],
        json!("tok-123"),
        "{text}"
    );
    let _ = server.finish();
}

#[test]
fn failed_output_keeps_the_response_and_shows_a_section() {
    let server = StubServer::start(|_| {
        vec![StubResponse::json(
            400,
            "Bad Request",
            r#"{"error":"ship already exists"}"#,
        )]
    });
    let dir = project_fixture(
        "tui-output-error",
        &server,
        &[("requests/ships/create.json", CREATE)],
    );
    let mut app = open(config(), &dir);
    app.select_id("ships/create");
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    let screen = rendered(&mut app, 100, 40);
    assert!(screen.contains("400 Bad Request"), "{screen}");
    assert!(screen.contains("Output error"), "{screen}");
    assert!(screen.contains("no key `id`"), "{screen}");
    assert!(screen.contains("ship already exists"), "{screen}");

    // The error text wraps rather than being clipped when the pane narrows.
    let narrow = rendered(&mut app, 70, 40);
    assert!(narrow.contains("400 Bad Request"), "{narrow}");
    assert!(narrow.contains("available"), "{narrow}");

    let _ = server.finish();
}

#[test]
fn switching_environment_changes_the_scope() {
    let server = StubServer::start(|_| Vec::new());
    let dir = project_fixture(
        "tui-env",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships" }"#,
        )],
    );
    dir.write(
        "environments/prod.json",
        &format!(r#"{{ "baseUrl": "{}" }}"#, server.base_url),
    );
    let mut app = open(config(), &dir);
    let before = rendered(&mut app, 120, 40);
    assert!(before.contains("env: dev"), "{before}");
    assert!(before.contains("Captures — dev"), "{before}");

    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    let after = rendered(&mut app, 120, 40);
    assert!(after.contains("env: prod"), "{after}");
    assert!(after.contains("Captures — prod"), "{after}");
}

#[test]
fn resolved_and_definition_request_views_differ() {
    let server = StubServer::start(|_| Vec::new());
    let dir = project_fixture("tui-views", &server, &[("requests/ships/list.json", SHIPS)]);
    let mut app = open(config(), &dir);
    let resolved = rendered(&mut app, 120, 40);
    assert!(resolved.contains("limit=2"), "{resolved}");
    assert!(!resolved.contains("${limit}"), "{resolved}");
    assert!(resolved.contains("url"), "{resolved}");

    app.on_key(key(KeyCode::Char('v')));
    let definition = rendered(&mut app, 120, 40);
    assert!(definition.contains("${limit}"), "{definition}");
    assert!(definition.contains("Request — definition"), "{definition}");
}

#[test]
fn no_session_marks_captures_disabled() {
    let server = StubServer::start(|_| Vec::new());
    let dir = project_fixture(
        "tui-no-session",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships" }"#,
        )],
    );
    let mut conf = config();
    conf.no_session = true;
    let mut app = open(conf, &dir);
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("session disabled"), "{screen}");
    assert!(screen.contains("session: off"), "{screen}");
}

#[test]
fn help_overlay_lists_the_bindings() {
    let server = StubServer::start(|_| Vec::new());
    let dir = project_fixture(
        "tui-help",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships" }"#,
        )],
    );
    let mut app = open(config(), &dir);
    app.on_key(key_mod(KeyCode::Char('?'), KeyModifiers::SHIFT));
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("Help"), "{screen}");
    assert!(screen.contains("run the selected request"), "{screen}");
    app.on_key(key(KeyCode::Esc));
    let closed = rendered(&mut app, 120, 40);
    assert!(!closed.contains("run the selected request"), "{closed}");
}
