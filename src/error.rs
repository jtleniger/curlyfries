//! The error type, its human-readable rendering, and the exit-code mapping.
//!
//! Every failure surfaces through [`Error`]; [`Error::exit_code`] is the single
//! place that decides which process exit code a failure produces.

use std::io;
use std::path::PathBuf;

/// Why a `${...}` template could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateProblem {
    /// A `${` with no later `}`.
    Unterminated,
    /// `${}` or `${ }`.
    EmptyName,
    /// A name that does not match `[A-Za-z_][A-Za-z0-9_.-]*`.
    InvalidName,
}

impl TemplateProblem {
    /// Short description used as the first words of the error message.
    pub fn message(self) -> &'static str {
        match self {
            TemplateProblem::Unterminated => "unterminated `${`",
            TemplateProblem::EmptyName => "empty variable name",
            TemplateProblem::InvalidName => "invalid variable name",
        }
    }

    /// Actionable hint shown on the `hint:` line.
    pub fn hint(self) -> &'static str {
        match self {
            TemplateProblem::Unterminated => {
                "write `$${` for a literal `$` followed by `{`, or close the placeholder with `}`"
            }
            TemplateProblem::EmptyName => "write `${name}` with a non-empty variable name",
            TemplateProblem::InvalidName => {
                "variable names match [A-Za-z_][A-Za-z0-9_.-]* (for example `${token}`)"
            }
        }
    }
}

impl std::fmt::Display for TemplateProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Every failure curlyfries can report.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed.
    #[error("error: i/o error: {source}{suffix}", suffix = opt_path(path))]
    Io {
        /// File involved, when known.
        path: Option<PathBuf>,
        /// Underlying failure.
        source: io::Error,
    },
    /// No `curlyfries.json` was found walking up from the start directory.
    #[error("error: no curlyfries.json found in {start} or any parent directory\n  hint: run inside a project, or pass -C <dir>", start = start.display())]
    ProjectNotFound {
        /// Directory discovery started from.
        start: PathBuf,
    },
    /// The configured requests directory does not exist.
    #[error("error: requests directory not found: {path}\n  hint: create it, or set \"requestsDir\" in curlyfries.json", path = path.display())]
    MissingDir {
        /// Missing directory.
        path: PathBuf,
    },
    /// The requests directory holds no request files.
    #[error("error: no request files found in {dir}\n  hint: add JSON request files (for example requests/ping.json) under that directory", dir = dir.display())]
    NoRequests {
        /// Requests directory that was searched.
        dir: PathBuf,
    },
    /// A JSON document could not be parsed.
    #[error("error: invalid JSON in {path}\n  reason: {source}", path = path.display())]
    InvalidJson {
        /// File that failed to parse.
        path: PathBuf,
        /// Parser failure.
        source: serde_json::Error,
    },
    /// A JSON document parsed but violated the documented schema.
    #[error("error: invalid request file {path}\n  at:    {pointer}\n  reason: {message}", path = path.display())]
    Schema {
        /// File that failed validation.
        path: PathBuf,
        /// JSON pointer of the offending value (empty for the document root).
        pointer: String,
        /// What is wrong.
        message: String,
    },
    /// A `${...}` template failed to parse.
    #[error("error: {message} in {path}\n  at:    {pointer}\n  value: {value}\n  hint:  {hint}", message = problem.message(), hint = problem.hint(), value = json_string(value), path = path.display())]
    Template {
        /// File holding the template.
        path: PathBuf,
        /// JSON pointer of the offending string.
        pointer: String,
        /// What is wrong with the template.
        problem: TemplateProblem,
        /// The raw string as written in the file.
        value: String,
    },
    /// A `${name}` placeholder had no value in any scope.
    #[error("error: undefined variable `{name}` in {path}\n  at:    {pointer}\n  value: {value}\n{known}", known = known_lines(env, env_keys, session_keys), value = json_string(value), path = path.display())]
    UndefinedVariable {
        /// File holding the template.
        path: PathBuf,
        /// JSON pointer of the offending string.
        pointer: String,
        /// The raw string as written in the file.
        value: String,
        /// Missing variable name.
        name: String,
        /// Active environment name, if any.
        env: Option<String>,
        /// Variables known from the selected environment (sorted).
        env_keys: Vec<String>,
        /// Variables known from the session store (sorted).
        session_keys: Vec<String>,
    },
    /// An `outputs` expression could not be evaluated.
    #[error("error: output `{key}` failed in {path}\n  expression: {expression}\n  reason: {reason}", path = path.display())]
    Output {
        /// File declaring the output.
        path: PathBuf,
        /// Variable being captured.
        key: String,
        /// Expression source text.
        expression: String,
        /// Why evaluation failed.
        reason: String,
    },
    /// A relative request path could not be joined to a base URL.
    #[error("error: no base URL for request `{request_id}`\n  path: {resolved}\n  hint: add \"baseUrl\" to {env_file}, or pass --var baseUrl=http://host:port, or use an absolute URL", resolved = json_string(resolved), env_file = env_file_name(env))]
    MissingBaseUrl {
        /// Request id.
        request_id: String,
        /// Request file the path came from.
        path: PathBuf,
        /// The rendered path.
        resolved: String,
        /// Active environment name, if any.
        env: Option<String>,
    },
    /// A request id did not match any request file.
    #[error("error: unknown request `{id}`\n  available: {available}", available = join_or_none(available))]
    UnknownRequest {
        /// Id the user asked for.
        id: String,
        /// Every known request id (sorted).
        available: Vec<String>,
    },
    /// The HTTP exchange failed below the response level.
    #[error("error: transport error: {message}\n  url: {url}")]
    Transport {
        /// Request URL.
        url: String,
        /// Underlying failure.
        message: String,
    },
    /// The request exceeded the configured timeout.
    #[error("error: request timed out after {seconds}s\n  url: {url}")]
    Timeout {
        /// Request URL.
        url: String,
        /// Configured timeout in seconds.
        seconds: u64,
    },
    /// `.curlyfries/session.json` could not be read or understood.
    #[error("error: unreadable session file {path}\n  reason: {reason}{hint}", path = path.display(), hint = session_hint(hint))]
    SessionFile {
        /// Path of the session file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
        /// Whether to suggest `session clear --all`.
        hint: bool,
    },
    /// Interactive mode was requested without a terminal.
    #[error(
        "error: interactive mode requires a terminal; use `curlyfries run <id>`\n  not a terminal: {stream}"
    )]
    NotATerminal {
        /// Which standard stream is not a terminal.
        stream: &'static str,
    },
    /// The terminal UI failed to start or run.
    #[error("error: {message}")]
    Interactive {
        /// The underlying failure.
        message: String,
    },
}

impl Error {
    /// Process exit code for this failure.
    ///
    /// `1` filesystem/environment, `3` project/request definition. Usage errors
    /// (exit `2`) come from the argument parser, `4` from `--fail-on-error`.
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Io { .. }
            | Error::ProjectNotFound { .. }
            | Error::MissingDir { .. }
            | Error::NoRequests { .. }
            | Error::Transport { .. }
            | Error::Timeout { .. }
            | Error::SessionFile { .. }
            | Error::NotATerminal { .. }
            | Error::Interactive { .. } => 1,
            Error::InvalidJson { .. }
            | Error::Schema { .. }
            | Error::Template { .. }
            | Error::UndefinedVariable { .. }
            | Error::Output { .. }
            | Error::MissingBaseUrl { .. }
            | Error::UnknownRequest { .. } => 3,
        }
    }

    /// Rewrites path fields relative to `root` when possible.
    ///
    /// Request files are discovered through an absolute project root but are
    /// always reported relative to it, so messages match the layout on disk
    /// regardless of where the CLI was started.
    pub fn relativize(self, root: &std::path::Path) -> Error {
        let short = |path: PathBuf| path.strip_prefix(root).map(PathBuf::from).unwrap_or(path);
        match self {
            Error::Io { path, source } => Error::Io {
                path: path.map(short),
                source,
            },
            Error::MissingDir { path } => Error::MissingDir { path: short(path) },
            Error::NoRequests { dir } => Error::NoRequests { dir: short(dir) },
            Error::InvalidJson { path, source } => Error::InvalidJson {
                path: short(path),
                source,
            },
            Error::Schema {
                path,
                pointer,
                message,
            } => Error::Schema {
                path: short(path),
                pointer,
                message,
            },
            Error::Template {
                path,
                pointer,
                problem,
                value,
            } => Error::Template {
                path: short(path),
                pointer,
                problem,
                value,
            },
            Error::UndefinedVariable {
                path,
                pointer,
                value,
                name,
                env,
                env_keys,
                session_keys,
            } => Error::UndefinedVariable {
                path: short(path),
                pointer,
                value,
                name,
                env,
                env_keys,
                session_keys,
            },
            Error::Output {
                path,
                key,
                expression,
                reason,
            } => Error::Output {
                path: short(path),
                key,
                expression,
                reason,
            },
            Error::MissingBaseUrl {
                request_id,
                path,
                resolved,
                env,
            } => Error::MissingBaseUrl {
                request_id,
                path: short(path),
                resolved,
                env,
            },
            Error::SessionFile { path, reason, hint } => Error::SessionFile {
                path: short(path),
                reason,
                hint,
            },
            other => other,
        }
    }
}

fn opt_path(path: &Option<PathBuf>) -> String {
    match path {
        Some(p) => format!("\n  path: {}", p.display()),
        None => String::new(),
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn join_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

fn env_file_name(env: &Option<String>) -> String {
    match env {
        Some(name) => format!("environments/{name}.json"),
        None => "an environment file".to_string(),
    }
}

fn session_hint(hint: &bool) -> String {
    if *hint {
        "\n  hint:  delete the file or run `curlyfries session clear --all`".to_string()
    } else {
        String::new()
    }
}

/// The `known:` block of an undefined-variable error: one line per scope.
fn known_lines(env: &Option<String>, env_keys: &[String], session_keys: &[String]) -> String {
    let env_label = match env {
        Some(name) => format!("environment `{name}`"),
        None => "environment".to_string(),
    };
    let session_label = "session".to_string();
    let width = env_label.chars().count().max(session_label.chars().count());
    format!(
        "  known: {env_label:width$} → {env_names}\n         {session_label:width$} → {session_names}",
        env_names = join_or_none(env_keys),
        session_names = join_or_none(session_keys),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undefined_variable_lists_both_scopes() {
        let err = Error::UndefinedVariable {
            path: PathBuf::from("requests/ships/create.json"),
            pointer: "/headers/Authorization".to_string(),
            value: "Bearer ${token}".to_string(),
            name: "token".to_string(),
            env: Some("dev".to_string()),
            env_keys: vec!["apiKey".to_string(), "baseUrl".to_string()],
            session_keys: vec![],
        };
        let text = err.to_string();
        assert!(
            text.starts_with(
                "error: undefined variable `token` in requests/ships/create.json\n  at:    /headers/Authorization"
            ),
            "{text}"
        );
        assert!(text.contains("value: \"Bearer ${token}\""), "{text}");
        assert!(
            text.contains("known: environment `dev` → apiKey, baseUrl"),
            "{text}"
        );
        assert!(text.contains("→ (none)"), "{text}");
    }

    #[test]
    fn exit_codes_follow_the_table() {
        let io = Error::Io {
            path: Some(PathBuf::from("x")),
            source: io::Error::new(io::ErrorKind::NotFound, "nope"),
        };
        assert_eq!(io.exit_code(), 1);
        assert!(io.to_string().contains("path: x"), "{io}");
        let schema = Error::Schema {
            path: PathBuf::from("a.json"),
            pointer: "/ouputs".to_string(),
            message: "unknown key `ouputs`".to_string(),
        };
        assert_eq!(schema.exit_code(), 3);
        let timeout = Error::Timeout {
            url: "http://127.0.0.1:1/x".to_string(),
            seconds: 30,
        };
        assert_eq!(timeout.exit_code(), 1);
        assert!(timeout.to_string().contains("timed out after 30s"));
    }

    #[test]
    fn session_hint_is_optional() {
        let hinted = Error::SessionFile {
            path: PathBuf::from(".curlyfries/session.json"),
            reason: "unsupported version 2 (expected 1)".to_string(),
            hint: true,
        };
        let text = hinted.to_string();
        assert!(
            text.contains("unreadable session file .curlyfries/session.json"),
            "{text}"
        );
        assert!(
            text.contains("unsupported version 2 (expected 1)"),
            "{text}"
        );
        assert!(text.contains("session clear --all"), "{text}");
        let plain = Error::SessionFile {
            path: PathBuf::from("s.json"),
            reason: "boom".to_string(),
            hint: false,
        };
        assert!(!plain.to_string().contains("hint:"), "{plain}");
    }

    #[test]
    fn template_error_renders_problem_and_hint() {
        let err = Error::Template {
            path: PathBuf::from("requests/ships/create.json"),
            pointer: "/path".to_string(),
            problem: TemplateProblem::Unterminated,
            value: "/ships/${".to_string(),
        };
        let text = err.to_string();
        assert!(
            text.starts_with("error: unterminated `${` in requests/ships/create.json"),
            "{text}"
        );
        assert!(text.contains("value: \"/ships/${\""), "{text}");
        assert!(text.contains("$${"), "{text}");
    }
}
