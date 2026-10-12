//! Full-screen TUI behaviour, driven headlessly through a `TestBackend`.

mod support;

use std::fs;
use std::time::Duration;

use curlyfries::project::Project;
use curlyfries::tui::app::App;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use support::{StubResponse, StubServer, TempDir, load_project, project_fixture};

const LOGIN: &str = r#"{
  "name": "Log in",
  "method": "POST",
  "path": "/auth/login",
  "body": { "username": "silver" },
  "outputs": { "token": { "value": "response.body.access_token" } }
}"#;

const SHIPS: &str = r#"{
  "method": "GET",
  "path": "/ships?limit=${limit}"
}"#;

const CREATE: &str = r#"{
  "method": "POST",
  "path": "/ships",
  "outputs": { "shipId": { "value": "response.body.id" } }
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

/// Builds an app over the fixture project.
fn build(dir: &TempDir) -> (Project, Vec<curlyfries::project::Entry>) {
    load_project(dir)
}

/// Opens the fixture project.
fn open(dir: &TempDir) -> App {
    let (project, entries) = build(dir);
    App::new(project, entries).expect("app builds")
}

/// The parsed session file.
fn session_json(dir: &TempDir) -> Value {
    let text = fs::read_to_string(dir.child(".curlyfries/session.json")).expect("session file");
    serde_json::from_str(&text).expect("session JSON")
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
    let mut app = open(&dir);
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("auth/"), "{screen}");
    assert!(screen.contains("ships/"), "{screen}");
    // Labels are method + name (or the id's leaf), never the folder path.
    assert!(screen.contains("POST Log in"), "{screen}");
    assert!(screen.contains("GET list"), "{screen}");
    assert!(!screen.contains("POST auth/login"), "{screen}");
    assert!(!screen.contains("GET ships/list"), "{screen}");
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
    let mut app = open(&dir);
    app.select_id("auth/login");
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("200 OK"), "{screen}");
    assert!(screen.contains("token"), "{screen}");
    assert!(screen.contains("tok-123"), "{screen}");

    assert_eq!(
        session_json(&dir)["varScopes"]["dev"]["token"],
        json!({ "value": "tok-123" })
    );
    let _ = server.finish();
}

#[test]
fn secret_values_are_masked_in_the_ui() {
    let server = StubServer::start(|_| {
        vec![StubResponse::json(
            200,
            "OK",
            r#"{"access_token":"tok-123"}"#,
        )]
    });
    let dir = project_fixture(
        "tui-secret",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{
              "method": "GET",
              "path": "/ships",
              "headers": { "Authorization": "Bearer ${apiKey}" },
              "outputs": { "token": { "secret": true, "value": "response.body.access_token" } }
            }"#,
        )],
    );
    dir.write(
        "environments/dev.json",
        &format!(
            r#"{{ "baseUrl": {{ "value": "{}" }}, "apiKey": {{ "secret": true, "value": "s3cret" }} }}"#,
            server.base_url
        ),
    );
    let mut app = open(&dir);
    app.select_id("ships/list");
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    let mask = curlyfries::variables::SECRET_MASK;
    let screen = rendered(&mut app, 120, 40);
    // The resolved request pane masks the secret header...
    assert!(
        screen.contains(&format!("Authorization: Bearer {mask}")),
        "{screen}"
    );
    // ...the captures strip masks the secret capture...
    assert!(screen.contains(&format!("token = {mask}")), "{screen}");
    // ...but the response body is server data and is never masked.
    assert!(screen.contains("tok-123"), "{screen}");
    assert!(!screen.contains("s3cret"), "{screen}");

    // The secret capture is still persisted, wrapped, so chains survive a restart.
    assert_eq!(
        session_json(&dir)["varScopes"]["dev"]["token"],
        json!({ "secret": true, "value": "tok-123" })
    );
    let _ = server.finish();
}

#[test]
fn pick_slots_open_a_picker_and_send_the_chosen_element() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", r#"[{"id":7}]"#)]);
    let dir = project_fixture(
        "tui-pick",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{
              "method": "GET",
              "path": "/ships?class=${shipClasses[]}",
              "outputs": { "ids": { "value": "response.body[].id" } }
            }"#,
        )],
    );
    dir.write(
        "environments/dev.json",
        &format!(
            r#"{{ "baseUrl": {{ "value": "{}" }}, "shipClasses": {{ "value": ["frigate", "sloop"] }} }}"#,
            server.base_url
        ),
    );
    let mut app = open(&dir);
    app.select_id("ships/list");

    // The resolved request keeps the placeholder until a value is chosen.
    let before = rendered(&mut app, 120, 40);
    assert!(before.contains("class=${shipClasses[]}"), "{before}");

    // `Esc` closes the picker without sending anything.
    app.on_key(key(KeyCode::Enter));
    let picking = rendered(&mut app, 120, 40);
    assert!(picking.contains("Pick shipClasses"), "{picking}");
    assert!(picking.contains("frigate"), "{picking}");
    assert!(picking.contains("sloop"), "{picking}");
    assert_eq!(server.request_count(), 0, "nothing is sent before the pick");
    app.on_key(key(KeyCode::Esc));
    assert!(
        !rendered(&mut app, 120, 40).contains("Pick shipClasses"),
        "Esc closes the picker"
    );
    assert_eq!(server.request_count(), 0, "Esc sends nothing");

    // Down then Enter chooses the second element and sends it.
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    let received = server.finish();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "GET");
    assert_eq!(received[0].target, "/ships?class=sloop");
    assert_eq!(
        session_json(&dir)["varScopes"]["dev"]["ids"],
        json!({ "value": [7] })
    );
}

#[test]
fn prompt_slots_ask_for_text_and_send_it() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "tui-prompt",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{
              "method": "GET",
              "path": "/ships",
              "query": { "limit": "${limit?}" }
            }"#,
        )],
    );
    let mut app = open(&dir);
    app.select_id("ships/list");

    // The resolved request keeps the placeholder until a value is entered.
    let before = rendered(&mut app, 120, 40);
    assert!(before.contains("limit=${limit?}"), "{before}");

    // `Enter` opens the text modal and nothing is sent yet.
    app.on_key(key(KeyCode::Enter));
    let prompting = rendered(&mut app, 120, 40);
    assert!(prompting.contains("Input limit"), "{prompting}");
    assert_eq!(
        server.request_count(),
        0,
        "nothing is sent before the prompt"
    );

    // `Esc` closes the modal without sending anything.
    app.on_key(key(KeyCode::Esc));
    assert!(
        !rendered(&mut app, 120, 40).contains("Input limit"),
        "Esc closes the prompt"
    );
    assert_eq!(server.request_count(), 0, "Esc sends nothing");

    // Type `5` and submit: the sent URL carries the typed text.
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('5')));
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    let received = server.finish();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "GET");
    assert_eq!(received[0].target, "/ships?limit=5");
}

#[test]
fn text_prompts_do_not_coerce_and_json_prompts_do() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "tui-prompt-types",
        &server,
        &[
            (
                "requests/ships/text.json",
                r#"{ "method": "POST", "path": "/ships", "body": { "id": "${id?}" } }"#,
            ),
            (
                "requests/ships/json.json",
                r#"{ "method": "POST", "path": "/ships", "body": { "crewCapacity": "${n?json}" } }"#,
            ),
        ],
    );
    let mut app = open(&dir);

    // `${id?}` typed `3` sends the string `"3"`, never the number.
    app.select_id("ships/text");
    app.on_key(key(KeyCode::Enter));
    assert!(rendered(&mut app, 120, 40).contains("Input id"));
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));
    let received = server.finish();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].body_json()["id"], json!("3"));

    // `${n?json}` typed `3` sends the number.
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "tui-prompt-json",
        &server,
        &[(
            "requests/ships/json.json",
            r#"{ "method": "POST", "path": "/ships", "body": { "crewCapacity": "${n?json}" } }"#,
        )],
    );
    let mut app = open(&dir);
    app.select_id("ships/json");
    app.on_key(key(KeyCode::Enter));
    assert!(rendered(&mut app, 120, 40).contains("Input n (JSON)"));
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));
    let received = server.finish();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].body_json()["crewCapacity"], json!(3));

    // Invalid JSON is refused: the modal stays open and nothing is sent, until
    // a quoted string is entered.
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let dir = project_fixture(
        "tui-prompt-json-invalid",
        &server,
        &[(
            "requests/ships/json.json",
            r#"{ "method": "POST", "path": "/ships", "body": { "crewCapacity": "${n?json}" } }"#,
        )],
    );
    let mut app = open(&dir);
    app.select_id("ships/json");
    app.on_key(key(KeyCode::Enter));
    for character in "frigate".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    app.on_key(key(KeyCode::Enter));
    let refused = rendered(&mut app, 120, 40);
    assert!(
        refused.contains("Input n (JSON)"),
        "modal stays open: {refused}"
    );
    assert!(refused.contains("not valid JSON"), "{refused}");
    assert_eq!(server.request_count(), 0, "invalid JSON sends nothing");

    // Submitting a quoted string succeeds and sends the JSON string.
    for _ in 0.."frigate".len() {
        app.on_key(key(KeyCode::Backspace));
    }
    app.on_key(key(KeyCode::Char('"')));
    app.on_key(key(KeyCode::Char('f')));
    app.on_key(key(KeyCode::Char('"')));
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));
    let received = server.finish();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].body_json()["crewCapacity"], json!("f"));
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
    let mut app = open(&dir);
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
        &format!(r#"{{ "baseUrl": {{ "value": "{}" }} }}"#, server.base_url),
    );
    let mut app = open(&dir);
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
    let mut app = open(&dir);
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
fn clear_scope_needs_confirmation_and_removes_the_scope() {
    let server = StubServer::start(|_| {
        vec![StubResponse::json(
            200,
            "OK",
            r#"{"access_token":"tok-123"}"#,
        )]
    });
    let dir = project_fixture("tui-clear", &server, &[("requests/auth/login.json", LOGIN)]);
    let mut app = open(&dir);
    app.select_id("auth/login");
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));
    assert_eq!(
        session_json(&dir)["varScopes"]["dev"]["token"],
        json!({ "value": "tok-123" })
    );

    // `x` asks first and changes nothing on its own.
    app.on_key(key(KeyCode::Char('x')));
    let asking = rendered(&mut app, 120, 40);
    assert!(asking.contains("Clear captures"), "{asking}");
    assert!(
        asking.contains("delete every captured variable in scope `dev`"),
        "{asking}"
    );
    assert_eq!(
        session_json(&dir)["varScopes"]["dev"]["token"],
        json!({ "value": "tok-123" })
    );

    // `Esc` cancels and leaves the scope alone.
    app.on_key(key(KeyCode::Esc));
    let cancelled = rendered(&mut app, 120, 40);
    assert!(
        !cancelled.contains("delete every captured variable"),
        "{cancelled}"
    );
    assert_eq!(
        session_json(&dir)["varScopes"]["dev"]["token"],
        json!({ "value": "tok-123" })
    );

    // `y` confirms and removes the active scope.
    app.on_key(key(KeyCode::Char('x')));
    app.on_key(key(KeyCode::Char('y')));
    let cleared = rendered(&mut app, 120, 40);
    assert!(cleared.contains("no captured variables"), "{cleared}");
    assert!(
        session_json(&dir)["varScopes"].get("dev").is_none(),
        "scope must be gone"
    );
    let _ = server.finish();
}

#[test]
fn clear_all_removes_every_scope() {
    let server = StubServer::start(|_| Vec::new());
    let dir = project_fixture(
        "tui-clear-all",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships" }"#,
        )],
    );
    dir.write(
        ".curlyfries/session.json",
        r#"{ "version": 3, "updatedAtUnix": 1, "varScopes": {
            "dev": { "token": { "value": "a" } },
            "stage": { "token": { "value": "b" } }
        } }"#,
    );
    let mut app = open(&dir);
    app.on_key(key(KeyCode::Char('X')));
    let asking = rendered(&mut app, 120, 40);
    assert!(
        asking.contains("delete every captured variable in every scope"),
        "{asking}"
    );
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(session_json(&dir)["varScopes"], json!({}));
}

#[test]
fn manifest_timeout_reaches_the_client() {
    let server = StubServer::start_silent();
    let dir = project_fixture(
        "tui-timeout",
        &server,
        &[(
            "requests/debug/slow.json",
            r#"{"method":"GET","path":"/_debug/slow?ms=3000"}"#,
        )],
    );
    dir.write(
        "curlyfries.json",
        r#"{ "defaultEnvironment": "dev", "timeout": 1 }"#,
    );
    let mut app = open(&dir);
    app.select_id("debug/slow");
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("timed out after 1s"), "{screen}");
    let _ = server.finish();
}

#[test]
fn broken_session_file_is_an_error() {
    let server = StubServer::start(|_| Vec::new());
    let dir = project_fixture(
        "tui-broken-session",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships" }"#,
        )],
    );
    dir.write(
        ".curlyfries/session.json",
        r#"{ "version": 4, "updatedAtUnix": 1, "varScopes": {} }"#,
    );
    let (project, entries) = build(&dir);
    let error = match App::new(project, entries) {
        Ok(_) => panic!("a broken session file must fail"),
        Err(error) => error,
    };
    let text = error.to_string();
    assert!(text.contains("unreadable session file"), "{text}");
    assert!(
        text.contains("unsupported version 4 (expected 3)"),
        "{text}"
    );

    // A file from the previous schema version is refused too.
    dir.write(
        ".curlyfries/session.json",
        r#"{ "version": 2, "updatedAtUnix": 1, "varScopes": { "dev": { "token": "a" } } }"#,
    );
    let (project, entries) = build(&dir);
    let error = match App::new(project, entries) {
        Ok(_) => panic!("a v2 session file must fail"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("unsupported version 2 (expected 3)"),
        "{error}"
    );
    let _ = server.finish();
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
    let mut app = open(&dir);
    app.on_key(key_mod(KeyCode::Char('?'), KeyModifiers::SHIFT));
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("Help"), "{screen}");
    assert!(screen.contains("run the selected request"), "{screen}");
    assert!(screen.contains("clear captures"), "{screen}");
    app.on_key(key(KeyCode::Esc));
    let closed = rendered(&mut app, 120, 40);
    assert!(!closed.contains("run the selected request"), "{closed}");
}

#[test]
fn long_request_values_wrap_in_the_resolved_request() {
    let server = StubServer::start(|_| Vec::new());
    let token = "x".repeat(120);
    let dir = project_fixture(
        "tui-wrap-request",
        &server,
        &[(
            "requests/ships/list.json",
            &format!(
                r#"{{ "method": "GET", "path": "/ships", "headers": {{ "Authorization": "Bearer {token}" }} }}"#
            ),
        )],
    );
    let mut app = open(&dir);
    app.select_id("ships/list");
    let screen = rendered(&mut app, 72, 50);
    // A JWT-length value no longer runs off the right edge: it cannot appear
    // whole on any single row of the wrapped pane...
    assert!(!screen.contains(&token), "{screen}");
    // ...but every character survives on its own wrapped row.
    assert!(screen.contains(&token[..10]), "{screen}");
    assert!(screen.contains(&token[token.len() - 10..]), "{screen}");
    let _ = server.finish();
}

#[test]
fn long_response_values_wrap_and_scroll_to_the_bottom() {
    let value = |key: &str| format!("{}-end-{key}", "y".repeat(140));
    let body = format!(
        r#"{{"aaa":"{aaa}","bbb":"{bbb}","ccc":"{ccc}","ddd":"{ddd}","eee":"{eee}"}}"#,
        aaa = value("aaa"),
        bbb = value("bbb"),
        ccc = value("ccc"),
        ddd = value("ddd"),
        eee = value("eee"),
    );
    let server = StubServer::start(move |_| vec![StubResponse::json(200, "OK", &body)]);
    let dir = project_fixture(
        "tui-wrap-response",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships" }"#,
        )],
    );
    let mut app = open(&dir);
    app.select_id("ships/list");
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    // The wrapped body is taller than the pane, so it must scroll; the top
    // shows the first key, not the last.
    let top = rendered(&mut app, 72, 50);
    assert!(top.contains("end-aaa"), "{top}");
    assert!(!top.contains("end-eee"), "{top}");

    // `End` reaches the true bottom: wrapping no longer understates the height
    // and pins the viewport at the first logical line. The scroll keys act on
    // the focused pane, so focus the response first (tree → request → response).
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::End));
    let bottom = rendered(&mut app, 72, 50);
    assert!(bottom.contains("end-eee"), "{bottom}");
    assert!(bottom.contains('}'), "{bottom}");
    assert!(!bottom.contains("\"aaa\""), "{bottom}");
    let _ = server.finish();
}

#[test]
fn toggle_modal_disables_headers_and_query_parameters() {
    let server = StubServer::start(|_| {
        vec![
            StubResponse::json(200, "OK", "{}"),
            StubResponse::json(200, "OK", "{}"),
        ]
    });
    let dir = project_fixture(
        "tui-toggle",
        &server,
        &[(
            "requests/ships/list.json",
            r#"{
              "method": "GET",
              "path": "/ships",
              "headers": { "X-Fingerprint": "abc" },
              "query": { "limit": "2" }
            }"#,
        )],
    );
    let mut app = open(&dir);
    app.select_id("ships/list");

    let before = rendered(&mut app, 120, 40);
    assert!(before.contains("[x] X-Fingerprint: abc"), "{before}");
    assert!(before.contains("[x] limit=2"), "{before}");

    // `t` opens the toggle modal listing every header and query slot.
    app.on_key(key(KeyCode::Char('t')));
    let modal = rendered(&mut app, 120, 40);
    assert!(modal.contains("Toggle — ships/list"), "{modal}");
    assert!(modal.contains("[x] header  X-Fingerprint: abc"), "{modal}");
    assert!(modal.contains("[x] query   limit=2"), "{modal}");

    // Space disables the highlighted header; closing shows it gone from the pane.
    app.on_key(key(KeyCode::Char(' ')));
    let toggled = rendered(&mut app, 120, 40);
    assert!(
        toggled.contains("[ ] header  X-Fingerprint: abc"),
        "{toggled}"
    );
    app.on_key(key(KeyCode::Esc));
    let closed = rendered(&mut app, 120, 40);
    assert!(closed.contains("[ ] X-Fingerprint: abc"), "{closed}");
    assert!(!closed.contains("[x] X-Fingerprint"), "{closed}");
    assert!(closed.contains("limit=2"), "{closed}");

    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    // Disable the query parameter too, then run again.
    app.on_key(key(KeyCode::Char('t')));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char(' ')));
    let query_off = rendered(&mut app, 120, 40);
    assert!(query_off.contains("[ ] query   limit=2"), "{query_off}");
    app.on_key(key(KeyCode::Esc));
    app.on_key(key(KeyCode::Enter));
    app.wait_for_run(Duration::from_secs(5));

    let received = server.finish();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].header("X-Fingerprint"), None);
    assert_eq!(received[0].target, "/ships?limit=2");
    assert_eq!(received[1].header("X-Fingerprint"), None);
    assert_eq!(received[1].target, "/ships");
}

#[test]
fn request_pane_columns_the_body_and_scrolls_when_focused() {
    let server = StubServer::start(|_| Vec::new());
    let fields: Vec<String> = (0..30).map(|i| format!("\"field{i:02}\": {i}")).collect();
    let body = format!("{{ {} }}", fields.join(", "));
    let dir = project_fixture(
        "tui-request-pane",
        &server,
        &[(
            "requests/ships/create.json",
            &format!(
                r#"{{ "method": "POST", "path": "/ships", "headers": {{ "X-Trace": "abc" }}, "body": {body} }}"#
            ),
        )],
    );
    let mut app = open(&dir);
    app.select_id("ships/create");

    // Wide enough to split: the `headers` and `body` sections share a row.
    let screen = rendered(&mut app, 120, 40);
    assert!(
        screen
            .lines()
            .any(|row| row.contains("headers") && row.contains("body")),
        "{screen}"
    );
    // The tall body is clipped at the top of the pane...
    assert!(!screen.contains("field29"), "{screen}");

    // ...and the tree-focused keys do not scroll it.
    app.on_key(key(KeyCode::End));
    assert!(!rendered(&mut app, 120, 40).contains("field29"));

    // Focus the request pane (tree → request): `End` now scrolls it.
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::End));
    let bottom = rendered(&mut app, 120, 40);
    assert!(bottom.contains("field29"), "{bottom}");
    assert!(!bottom.contains("field00"), "{bottom}");

    // Too narrow for columns: the sections stack into one scrollable column.
    let narrow = rendered(&mut app, 60, 40);
    assert!(
        !narrow
            .lines()
            .any(|row| row.contains("headers") && row.contains("body")),
        "{narrow}"
    );
    let _ = server.finish();
}
