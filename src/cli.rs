//! Command-line surface and dispatch.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::error::Error;
use crate::project::{self, Entry, Project};
use crate::render::{self, RenderOpts};
use crate::request;
use crate::runner::Runner;
use crate::session::{NO_ENV_KEY, Session};
use crate::{Map, execute, template};

/// Filesystem-driven CLI API client.
#[derive(Debug, Parser)]
#[command(
    name = "curlyfries",
    version,
    about = "Filesystem-driven CLI API client",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Project root (overrides upward discovery).
    #[arg(short = 'C', long = "project", global = true, value_name = "DIR")]
    pub project: Option<PathBuf>,

    /// Environment to load from `environments/<name>.json`.
    #[arg(short = 'e', long = "env", global = true, value_name = "NAME")]
    pub env: Option<String>,

    /// Override a variable; VALUE is parsed as JSON when possible.
    #[arg(long = "var", global = true, value_name = "NAME=VALUE", value_parser = parse_var)]
    pub var: Vec<(String, String)>,

    /// Ignore the session file: neither read nor written.
    #[arg(long = "no-session", global = true)]
    pub no_session: bool,

    /// Per-request timeout in seconds (0 disables timeouts).
    #[arg(
        long = "timeout",
        global = true,
        value_name = "SECS",
        default_value_t = 30
    )]
    pub timeout: u64,

    /// Subcommand; none means interactive mode.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run one or more requests in order, sharing captured variables.
    Run {
        /// Request ids, with or without a trailing `.json`.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
        /// Print one NDJSON object per request.
        #[arg(long)]
        json: bool,
        /// Print resolved request headers and body.
        #[arg(long)]
        verbose: bool,
        /// Stop with exit code 4 on the first status >= 400.
        #[arg(long = "fail-on-error")]
        fail_on_error: bool,
    },
    /// List every request in the project.
    List {
        /// Print a JSON array instead of one label per line.
        #[arg(long)]
        json: bool,
    },
    /// Inspect or clear captured variables.
    Session {
        /// Session subcommand.
        #[command(subcommand)]
        cmd: SessionCmd,
    },
}

/// `session` subcommands.
#[derive(Debug, Subcommand)]
pub enum SessionCmd {
    /// Print the variables of the active scope.
    Show {
        /// Print the scope as a JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Delete captured variables.
    Clear {
        /// Delete every scope instead of just the active one.
        #[arg(long)]
        all: bool,
    },
}

/// Parses `NAME=VALUE`, rejecting a missing `=` or an invalid name.
fn parse_var(raw: &str) -> Result<(String, String), String> {
    let (name, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=VALUE, got `{raw}`"))?;
    if !template::is_valid_name(name) {
        return Err(format!(
            "invalid variable name `{name}` (names match [A-Za-z_][A-Za-z0-9_.-]*)"
        ));
    }
    Ok((name.to_string(), value.to_string()))
}

/// Parses arguments and runs the requested command.
///
/// Returns the process exit code; argument-parser failures return `0` (help and
/// version) or `2` (usage) themselves.
pub fn parse_and_run<I, W, E>(args: I, out: &mut W, err: &mut E) -> Result<u8, Error>
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let text = error.render().to_string();
            if error.use_stderr() {
                let _ = write!(err, "{text}");
            } else {
                let _ = write!(out, "{text}");
            }
            return Ok(code);
        }
    };
    dispatch(cli, out, err)
}

fn dispatch<W: Write, E: Write>(cli: Cli, out: &mut W, err: &mut E) -> Result<u8, Error> {
    let cwd = std::env::current_dir().map_err(|source| Error::Io { path: None, source })?;
    let project =
        project::discover(&cwd, cli.project.as_deref()).map_err(|error| error.relativize(&cwd))?;
    let entries =
        project::collect_requests(&project).map_err(|error| error.relativize(&project.root))?;
    run_command(&cli, &project, &entries, out, err)
}

/// Runs the selected subcommand, reporting paths relative to the project root.
fn run_command<W: Write, E: Write>(
    cli: &Cli,
    project: &Project,
    entries: &[Entry],
    out: &mut W,
    err: &mut E,
) -> Result<u8, Error> {
    let root = project.root.clone();
    run_subcommand(cli, project, entries, out, err).map_err(|error| error.relativize(&root))
}

fn run_subcommand<W: Write, E: Write>(
    cli: &Cli,
    project: &Project,
    entries: &[Entry],
    out: &mut W,
    err: &mut E,
) -> Result<u8, Error> {
    match &cli.command {
        None => interactive_mode(cli, project, entries),
        Some(Command::Run {
            ids,
            json,
            verbose,
            fail_on_error,
        }) => {
            let opts = RunOpts {
                json: *json,
                render: RenderOpts {
                    verbose: *verbose,
                    color: color_enabled(),
                },
                fail_on_error: *fail_on_error,
            };
            run_requests(cli, project, entries, out, err, ids, opts)
        }
        Some(Command::List { json }) => list_requests(project, entries, out, *json),
        Some(Command::Session { cmd }) => session_command(cli, project, out, cmd),
    }
}

struct RunOpts {
    json: bool,
    render: RenderOpts,
    fail_on_error: bool,
}

fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn io_error(source: io::Error) -> Error {
    Error::Io { path: None, source }
}

/// Resolves the environment, `--var` overrides, timeout and session into a runner.
fn build_runner<'a>(
    cli: &Cli,
    project: &'a Project,
    entries: &'a [Entry],
    session: &'a mut Session,
) -> Result<Runner<'a>, Error> {
    let (env_name, env) = match project::resolve_environment(project, cli.env.as_deref())? {
        Some((name, vars)) => (Some(name), vars),
        None => (None, Map::new()),
    };
    let mut overrides = Map::new();
    for (name, raw) in &cli.var {
        overrides.insert(name.clone(), var_value(raw));
    }
    let timeout = timeout_of(cli);
    Ok(Runner {
        project,
        entries,
        env_name,
        env,
        overrides,
        session,
        client: execute::client(timeout),
        timeout,
        warnings: Vec::new(),
    })
}

fn timeout_of(cli: &Cli) -> Option<Duration> {
    if cli.timeout == 0 {
        None
    } else {
        Some(Duration::from_secs(cli.timeout))
    }
}

/// A `--var` value: JSON when it parses, otherwise the raw string.
fn var_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn active_scope(cli: &Cli, project: &Project) -> Result<String, Error> {
    Ok(
        match project::resolve_environment(project, cli.env.as_deref())? {
            Some((name, _)) => name,
            None => NO_ENV_KEY.to_string(),
        },
    )
}

fn run_requests<W: Write, E: Write>(
    cli: &Cli,
    project: &Project,
    entries: &[Entry],
    out: &mut W,
    err: &mut E,
    ids: &[String],
    opts: RunOpts,
) -> Result<u8, Error> {
    let mut session = Session::load(&project.root, !cli.no_session)?;
    let mut runner = build_runner(cli, project, entries, &mut session)?;
    for id in ids {
        let outcome = runner.execute_id(id)?;
        if let Some(error) = outcome.capture_error {
            return Err(error);
        }
        if opts.json {
            let line = render::json_line(&outcome.request, &outcome.response, &outcome.captured);
            writeln!(out, "{line}").map_err(io_error)?;
        } else {
            render::human(
                out,
                &outcome.request,
                &outcome.response,
                &outcome.captured,
                opts.render,
            )
            .map_err(io_error)?;
        }
        for warning in runner.warnings.drain(..) {
            writeln!(err, "{warning}").map_err(io_error)?;
        }
        if opts.fail_on_error && outcome.response.status >= 400 {
            return Ok(4);
        }
    }
    Ok(0)
}

fn list_requests<W: Write>(
    project: &Project,
    entries: &[Entry],
    out: &mut W,
    json: bool,
) -> Result<u8, Error> {
    let mut defs = Vec::with_capacity(entries.len());
    for entry in entries {
        defs.push(request::load_request(&entry.id, &entry.file)?);
    }
    if json {
        let listed: Vec<Value> = defs
            .iter()
            .map(|def| {
                serde_json::json!({
                    "id": def.id,
                    "method": def.method,
                    "name": def.name,
                    "path": project.relative(&def.file),
                })
            })
            .collect();
        writeln!(out, "{}", render::pretty(&Value::Array(listed))).map_err(io_error)?;
    } else {
        for def in &defs {
            writeln!(out, "{}", render::request_label(def)).map_err(io_error)?;
        }
    }
    Ok(0)
}

fn session_command<W: Write>(
    cli: &Cli,
    project: &Project,
    out: &mut W,
    cmd: &SessionCmd,
) -> Result<u8, Error> {
    match cmd {
        SessionCmd::Show { json } => {
            let scope = active_scope(cli, project)?;
            let session = Session::load(&project.root, !cli.no_session)?;
            let vars = session.vars(&scope);
            if *json {
                writeln!(out, "{}", render::pretty(&Value::Object(vars.clone())))
                    .map_err(io_error)?;
            } else {
                writeln!(out, "{}", render::scope_line(&scope, vars)).map_err(io_error)?;
            }
            Ok(0)
        }
        SessionCmd::Clear { all } => {
            let mut session = Session::load(&project.root, !cli.no_session)?;
            let removed = if *all {
                session.clear(None)
            } else {
                let scope = active_scope(cli, project)?;
                session.clear(Some(&scope))
            };
            if removed > 0 {
                session.save()?;
            }
            Ok(0)
        }
    }
}

fn interactive_mode(cli: &Cli, project: &Project, entries: &[Entry]) -> Result<u8, Error> {
    if entries.is_empty() {
        return Err(Error::NoRequests {
            dir: project.requests_dir(),
        });
    }
    let mut overrides = Map::new();
    for (name, raw) in &cli.var {
        overrides.insert(name.clone(), var_value(raw));
    }
    let config = crate::tui::TuiConfig {
        env: cli.env.clone(),
        overrides,
        no_session: cli.no_session,
        timeout: timeout_of(cli),
    };
    crate::tui::run(config, project.clone(), entries.to_vec())
}
