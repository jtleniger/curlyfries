//! End-to-end tests of the launcher surface: help, version and project errors.

mod support;

use std::fs;

use support::{TempDir, environment, exit_code, load_project, run_binary, stderr, stdout};

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
    for absent in ["run", "list", "session", "--var", "--env", "--no-session", "--timeout"] {
        assert!(!text.contains(absent), "help still mentions {absent}: {text}");
    }

    let version = run_binary(dir.path(), &["--version"]);
    assert_eq!(exit_code(&version), 0);
    assert!(
        stdout(&version).contains("curlyfries 0.1.0"),
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
