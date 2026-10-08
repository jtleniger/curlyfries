//! End-to-end tests of the launcher surface: help, version and project errors.

mod support;

use support::{TempDir, exit_code, run_binary, stderr, stdout};

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
