//! End-to-end tests of the launcher surface: help, version and project errors.

mod support;

use std::fs;

use support::{
    StubResponse, StubServer, TempDir, environment, exit_code, load_project, run_binary, stderr,
    stdout,
};

/// A project with a single request and no environment, enough to reach the UI.
fn simple_project(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    dir.write("curlyfries.json", "{}");
    dir.write("requests/ping.json", r#"{"method":"GET","path":"/ping"}"#);
    dir
}

#[test]
fn help_and_version_exit_cleanly() {
    let dir = TempDir::new("cli-help");
    let help = run_binary(dir.path(), &["--help"]);
    assert_eq!(exit_code(&help), 0);
    let text = stdout(&help);
    assert!(text.contains("-C, --project"), "{text}");
    for absent in [
        "run",
        "list",
        "session",
        "--var",
        "--env",
        "--no-session",
        "--timeout",
    ] {
        assert!(
            !text.contains(absent),
            "help still mentions {absent}: {text}"
        );
    }

    let version = run_binary(dir.path(), &["--version"]);
    assert_eq!(exit_code(&version), 0);
    assert!(
        stdout(&version).contains(concat!("curlyfries ", env!("CARGO_PKG_VERSION"))),
        "{}",
        stdout(&version)
    );

    let unknown_flag = run_binary(dir.path(), &["--nope"]);
    assert_eq!(exit_code(&unknown_flag), 2);
    assert!(!stderr(&unknown_flag).is_empty());
}

#[test]
fn the_launcher_requires_a_terminal() {
    let dir = simple_project("cli-not-a-terminal");
    let output = run_binary(dir.path(), &[]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("needs a terminal"), "{text}");
    assert!(text.contains("not a terminal: stdin"), "{text}");
}

#[test]
fn a_project_without_requests_is_an_error() {
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
fn running_outside_a_project_explains_how_to_fix_it() {
    let dir = TempDir::new("cli-no-project");
    let output = run_binary(dir.path(), &[]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("no curlyfries.json found in"), "{text}");
    assert!(text.contains("-C <dir>"), "{text}");
}

#[test]
fn a_missing_requests_directory_names_the_manifest_key() {
    let dir = TempDir::new("cli-missing-dir");
    dir.write("curlyfries.json", r#"{ "requestsDir": "specs" }"#);
    let output = run_binary(dir.path(), &[]);
    assert_eq!(exit_code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("requests directory not found"), "{text}");
    assert!(text.contains("specs"), "{text}");
}

/// The four files `init` writes, in write order, with their exact contents.
const SCAFFOLD: [(&str, &str); 4] = [
    (
        "curlyfries.json",
        "{\n  \"defaultEnvironment\": \"dev\"\n}\n",
    ),
    (
        "requests/ping.json",
        "{\n  \"name\": \"Ping\",\n  \"method\": \"GET\",\n  \"path\": \"${baseUrl}/ping\"\n}\n",
    ),
    (
        "environments/dev.json",
        "{\n  \"baseUrl\": \"http://127.0.0.1:4000\"\n}\n",
    ),
    (".gitignore", ".curlyfries/\n"),
];

#[test]
fn init_writes_a_runnable_project() {
    let dir = TempDir::new("cli-init-writes");
    let output = run_binary(dir.path(), &["init"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));

    let text = stdout(&output);
    for (relative, contents) in SCAFFOLD {
        assert!(text.contains(&format!("created {relative}")), "{text}");
        assert_eq!(
            fs::read_to_string(dir.child(relative)).expect("created file is readable"),
            contents,
            "{relative} holds unexpected bytes"
        );
    }
    assert!(text.contains("initialized curlyfries project in"), "{text}");
    assert!(!dir.child(".curlyfries").exists(), "capture store is lazy");

    // The written files form a project curlyfries can start from.
    let (project, entries) = load_project(&dir);
    let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids, ["ping"]);
    assert_eq!(project.manifest.default_environment.as_deref(), Some("dev"));

    let (name, vars) = environment(&project);
    assert_eq!(name.as_deref(), Some("dev"));
    assert_eq!(
        vars.get("baseUrl").and_then(|value| value.as_str()),
        Some("http://127.0.0.1:4000")
    );

    let definition =
        curlyfries::request::load_request("ping", &dir.child("requests/ping.json")).expect("loads");
    assert_eq!(definition.method, "GET");
    let lookup = |key: &str| vars.get(key).cloned();
    let rendered = curlyfries::template::parse(&definition.path)
        .expect("path parses")
        .render_string(&lookup)
        .expect("path renders");
    assert_eq!(rendered, "http://127.0.0.1:4000/ping");

    // A second run refuses rather than overwriting the project it just wrote.
    let again = run_binary(dir.path(), &["init"]);
    assert_eq!(exit_code(&again), 1);
    let text = stderr(&again);
    assert!(
        text.contains("cannot initialize a curlyfries project"),
        "{text}"
    );
    assert!(text.contains("curlyfries.json"), "{text}");
}

#[test]
fn init_refuses_when_any_target_file_exists() {
    for (relative, _) in SCAFFOLD {
        let dir = TempDir::new(&format!(
            "cli-init-conflict-{}",
            relative.replace(['/', '.'], "-")
        ));
        dir.write(relative, "keep");
        let output = run_binary(dir.path(), &["init"]);
        assert_eq!(exit_code(&output), 1, "{relative} was overwritten");
        let text = stderr(&output);
        assert!(
            text.contains("cannot initialize a curlyfries project"),
            "{text}"
        );
        assert!(text.contains(relative), "{text}");
        assert_eq!(
            fs::read_to_string(dir.child(relative)).expect("file is readable"),
            "keep",
            "{relative} was modified"
        );
        for (other, _) in SCAFFOLD {
            if other != relative {
                assert!(
                    !dir.child(other).exists(),
                    "{other} was created despite the conflict on {relative}"
                );
            }
        }
    }
}

#[test]
fn init_is_documented_and_rejects_a_project_override() {
    let dir = TempDir::new("cli-init-surface");

    let help = run_binary(dir.path(), &["--help"]);
    assert_eq!(exit_code(&help), 0);
    assert!(stdout(&help).contains("init"), "{}", stdout(&help));

    let sub_help = run_binary(dir.path(), &["init", "--help"]);
    assert_eq!(exit_code(&sub_help), 0);
    assert!(
        stdout(&sub_help).contains("current directory"),
        "{}",
        stdout(&sub_help)
    );

    let conflict = run_binary(dir.path(), &["-C", ".", "init"]);
    assert_eq!(exit_code(&conflict), 2);
    assert!(!stderr(&conflict).is_empty());
    assert!(!dir.child("curlyfries.json").exists());
}

/// The OpenAPI 3.1 document the import tests share; `__BASE__` is the server URL.
const OPENAPI: &str = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Ships", "version": "1.0.0" },
  "servers": [{ "url": "__BASE__" }],
  "tags": [{ "name": "ships" }],
  "paths": {
    "/health": {
      "get": { "operationId": "health" }
    },
    "/ships": {
      "get": {
        "tags": ["ships"],
        "operationId": "listShips",
        "summary": "List ships",
        "parameters": [
          { "in": "query", "name": "limit", "schema": { "type": "integer", "default": 25 } },
          { "in": "query", "name": "sort", "schema": { "type": "string" } }
        ]
      },
      "post": {
        "tags": ["ships"],
        "operationId": "createShip",
        "parameters": [
          { "in": "header", "name": "X-Trace", "schema": { "type": "string" } }
        ],
        "requestBody": {
          "content": {
            "application/json": {
              "schema": {
                "type": "object",
                "properties": {
                  "id": { "type": "string", "readOnly": true },
                  "name": { "type": "string" },
                  "class": { "enum": ["frigate", "galleon"] },
                  "crewCapacity": { "type": "integer", "default": 12 },
                  "flags": { "type": "array", "items": { "type": "boolean" } },
                  "captain": { "$ref": "#/components/schemas/Captain" }
                }
              }
            }
          }
        }
      }
    },
    "/ships/{id}": {
      "delete": {
        "tags": ["ships"],
        "operationId": "deleteShip",
        "parameters": [
          { "in": "path", "name": "id", "required": true, "schema": { "type": "string" } }
        ]
      }
    }
  },
  "components": {
    "schemas": {
      "Captain": { "type": "object", "properties": { "name": { "type": "string" } } }
    }
  }
}
"##;

/// Every path `import` writes, in write order.
const IMPORT_TARGETS: [&str; 7] = [
    "curlyfries.json",
    ".gitignore",
    "environments/default.json",
    "requests/default/health.json",
    "requests/ships/createShip.json",
    "requests/ships/deleteShip.json",
    "requests/ships/listShips.json",
];

/// A generated file's expected contents, newline included.
fn file(relative: &str, contents: &str) -> (String, String) {
    (relative.to_string(), format!("{contents}\n"))
}

#[test]
fn import_writes_a_runnable_project() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", r#"{"status":"ok"}"#)]);
    let dir = TempDir::new("cli-import-writes");
    dir.write("spec.json", &OPENAPI.replace("__BASE__", &server.base_url));

    let output = run_binary(dir.path(), &["import", "spec.json"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));

    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    let expected: Vec<String> = IMPORT_TARGETS
        .iter()
        .map(|relative| format!("created {relative}"))
        .collect();
    assert_eq!(lines[..7], expected, "{text}");
    assert!(
        lines[7].starts_with("imported 4 request files into"),
        "{text}"
    );
    assert_eq!(lines.len(), 8, "{text}");

    let expected_files = [
        file(
            "curlyfries.json",
            "{\n  \"defaultEnvironment\": \"default\"\n}",
        ),
        file(".gitignore", ".curlyfries/"),
        file(
            "environments/default.json",
            &format!("{{\n  \"baseUrl\": \"{}\"\n}}", server.base_url),
        ),
        file(
            "requests/default/health.json",
            r#"{
  "name": "health",
  "method": "GET",
  "path": "/health"
}"#,
        ),
        file(
            "requests/ships/createShip.json",
            r#"{
  "name": "createShip",
  "method": "POST",
  "path": "/ships",
  "headers": {
    "X-Trace": "${X-Trace}"
  },
  "body": {
    "name": "",
    "class": "frigate",
    "crewCapacity": 12,
    "flags": [
      false
    ],
    "captain": {
      "name": ""
    }
  }
}"#,
        ),
        file(
            "requests/ships/deleteShip.json",
            r#"{
  "name": "deleteShip",
  "method": "DELETE",
  "path": "/ships/${id}"
}"#,
        ),
        file(
            "requests/ships/listShips.json",
            r#"{
  "name": "List ships",
  "method": "GET",
  "path": "/ships?limit=25&sort=${sort}"
}"#,
        ),
    ];
    for (relative, contents) in &expected_files {
        assert_eq!(
            fs::read_to_string(dir.child(relative)).expect("created file is readable"),
            *contents,
            "{relative} holds unexpected bytes"
        );
    }
    // `curlyfries.json` is the same manifest `init` writes, with the imported environment.
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(dir.child("curlyfries.json")).expect("manifest is readable")
        )
        .expect("manifest is JSON"),
        serde_json::json!({ "defaultEnvironment": "default" })
    );

    // The result is a project curlyfries can list and run.
    let (project, entries) = load_project(&dir);
    let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "default/health",
            "ships/createShip",
            "ships/deleteShip",
            "ships/listShips"
        ]
    );

    let (env_name, vars) = environment(&project);
    assert_eq!(env_name.as_deref(), Some("default"));
    assert_eq!(
        vars.get("baseUrl").and_then(|value| value.as_str()),
        Some(server.base_url.as_str())
    );

    let health = entries
        .iter()
        .find(|entry| entry.id == "default/health")
        .expect("the health request was imported");
    let definition =
        curlyfries::request::load_request(&health.id, &health.file).expect("health request loads");
    let session = curlyfries::variables::Variables::default();
    let scopes = curlyfries::scopes::Scopes {
        env_name: Some("default"),
        env: &vars,
        session: &session,
    };
    let resolved =
        curlyfries::execute::render_request(&definition, &scopes).expect("health request renders");
    assert_eq!(resolved.url, format!("{}/health", server.base_url));
    let result =
        curlyfries::execute::execute(&curlyfries::execute::client(None), &resolved, None, false)
            .expect("health request executes");
    assert_eq!(result.status, 200);
    let received = server.finish();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "GET");
    assert_eq!(received[0].target, "/health");
}

#[test]
fn import_reads_yaml_and_swagger_2() {
    let server = StubServer::start(|_| vec![StubResponse::json(200, "OK", "{}")]);
    let host = server.base_url.trim_start_matches("http://");
    let dir = TempDir::new("cli-import-yaml");
    dir.write(
        "spec.yaml",
        &r#"swagger: "2.0"
info:
  title: Things
  version: "1.0.0"
host: __HOST__
schemes: [http]
basePath: /api
paths:
  /ping:
    get:
      operationId: ping
      tags: [system]
      parameters:
        - in: query
          name: verbose
          type: boolean
          default: true
  /things:
    post:
      operationId: addThing
      tags: [things]
      parameters:
        - in: body
          name: thing
          schema:
            type: object
            properties:
              label:
                type: string
"#
        .replace("__HOST__", host),
    );

    let output = run_binary(dir.path(), &["import", "spec.yaml"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    for (relative, contents) in [
        file(
            "environments/default.json",
            &format!("{{\n  \"baseUrl\": \"http://{host}/api\"\n}}"),
        ),
        file(
            "requests/system/ping.json",
            r#"{
  "name": "ping",
  "method": "GET",
  "path": "/ping?verbose=true"
}"#,
        ),
        file(
            "requests/things/addThing.json",
            r#"{
  "name": "addThing",
  "method": "POST",
  "path": "/things",
  "body": {
    "label": ""
  }
}"#,
        ),
    ] {
        assert_eq!(
            fs::read_to_string(dir.child(&relative)).expect("created file is readable"),
            contents,
            "{relative} holds unexpected bytes"
        );
    }

    // The imported `ping` request reaches the live stub through `baseUrl`.
    let (project, entries) = load_project(&dir);
    let ping = entries
        .iter()
        .find(|entry| entry.id == "system/ping")
        .expect("the ping request was imported");
    let definition =
        curlyfries::request::load_request(&ping.id, &ping.file).expect("ping request loads");
    let (env_name, vars) = environment(&project);
    let session = curlyfries::variables::Variables::default();
    let scopes = curlyfries::scopes::Scopes {
        env_name: env_name.as_deref(),
        env: &vars,
        session: &session,
    };
    let resolved =
        curlyfries::execute::render_request(&definition, &scopes).expect("ping request renders");
    let result =
        curlyfries::execute::execute(&curlyfries::execute::client(None), &resolved, None, false)
            .expect("ping request executes");
    assert_eq!(result.status, 200);
    assert_eq!(server.finish()[0].target, "/api/ping?verbose=true");
}

#[test]
fn import_refuses_when_a_target_exists() {
    for relative in ["curlyfries.json", "requests/ships/listShips.json"] {
        let dir = TempDir::new(&format!(
            "cli-import-conflict-{}",
            relative.replace(['/', '.'], "-")
        ));
        dir.write(
            "spec.json",
            &OPENAPI.replace("__BASE__", "http://127.0.0.1:1"),
        );
        dir.write(relative, "keep");

        let output = run_binary(dir.path(), &["import", "spec.json"]);
        assert_eq!(exit_code(&output), 1, "{relative} was overwritten");
        let text = stderr(&output);
        assert!(text.contains("cannot import into"), "{text}");
        assert!(text.contains(relative), "{text}");
        assert_eq!(
            fs::read_to_string(dir.child(relative)).expect("file is readable"),
            "keep",
            "{relative} was modified"
        );
        for other in IMPORT_TARGETS {
            if other != relative {
                assert!(
                    !dir.child(other).exists(),
                    "{other} was created despite the conflict on {relative}"
                );
            }
        }
    }
}

#[test]
fn import_reports_an_invalid_document() {
    let dir = TempDir::new("cli-import-invalid");

    dir.write("empty.json", "{}");
    let output = run_binary(dir.path(), &["import", "empty.json"]);
    assert_eq!(exit_code(&output), 3);
    let text = stderr(&output);
    assert!(text.contains("invalid API document empty.json"), "{text}");
    assert!(text.contains("unsupported API document"), "{text}");

    dir.write("v2.json", r#"{"openapi": "2.0", "paths": {}}"#);
    let output = run_binary(dir.path(), &["import", "v2.json"]);
    assert_eq!(exit_code(&output), 3);
    let text = stderr(&output);
    assert!(
        text.contains("unsupported `openapi` version `2.0`"),
        "{text}"
    );
    assert!(text.contains("at:    /openapi"), "{text}");

    dir.write(
        "trace.json",
        r#"{"openapi": "3.1.0", "paths": {"trace": {"trace": {}}}}"#,
    );
    let output = run_binary(dir.path(), &["import", "trace.json"]);
    assert_eq!(exit_code(&output), 3);
    let text = stderr(&output);
    assert!(text.contains("no operations found to import"), "{text}");
    assert!(text.contains("at:    /paths"), "{text}");

    dir.write("broken.yaml", "openapi: [");
    let output = run_binary(dir.path(), &["import", "broken.yaml"]);
    assert_eq!(exit_code(&output), 3);
    assert!(
        stderr(&output).contains("reason: invalid YAML: "),
        "{}",
        stderr(&output)
    );

    let output = run_binary(dir.path(), &["import", "nope.json"]);
    assert_eq!(exit_code(&output), 1);
    assert!(stderr(&output).contains("i/o error"), "{}", stderr(&output));

    for relative in IMPORT_TARGETS {
        assert!(
            !dir.child(relative).exists(),
            "{relative} was written for an invalid document"
        );
    }
}

#[test]
fn import_is_documented_and_rejects_a_project_override() {
    let dir = TempDir::new("cli-import-surface");

    let help = run_binary(dir.path(), &["--help"]);
    assert_eq!(exit_code(&help), 0);
    assert!(stdout(&help).contains("import"), "{}", stdout(&help));

    let sub_help = run_binary(dir.path(), &["import", "--help"]);
    assert_eq!(exit_code(&sub_help), 0);
    let text = stdout(&sub_help);
    assert!(text.contains("OpenAPI"), "{text}");
    assert!(text.contains("SPEC"), "{text}");

    let missing = run_binary(dir.path(), &["import"]);
    assert_eq!(exit_code(&missing), 2);
    assert!(!stderr(&missing).is_empty());

    dir.write("spec.json", "{}");
    let conflict = run_binary(dir.path(), &["-C", ".", "import", "spec.json"]);
    assert_eq!(exit_code(&conflict), 2);
    assert!(!stderr(&conflict).is_empty());
    assert!(!dir.child("curlyfries.json").exists());
}
