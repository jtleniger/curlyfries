//! Command-line surface and dispatch.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::error::Error;
use crate::project::{self, Entry, Project};
use crate::render;

/// Subcommands accepted in place of opening the terminal UI.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write a starter curlyfries project into the current directory.
    Init,
    /// Create a project from an OpenAPI 3.x or Swagger 2.0 document.
    Import {
        /// Path of a JSON or YAML API document.
        #[arg(value_name = "SPEC")]
        spec: PathBuf,
    },
}

/// Filesystem-driven CLI API client.
#[derive(Debug, Parser)]
#[command(
    name = "curlyfries",
    version,
    about = "Filesystem-driven CLI API client",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Project root (overrides the current directory).
    #[arg(short = 'C', long = "project", value_name = "DIR")]
    pub project: Option<PathBuf>,
    /// Subcommand to run; absent opens the terminal UI.
    #[command(subcommand)]
    pub command: Option<Command>,
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
                let _ = write!(err, "{}", render::sanitize_for_terminal(&text));
            } else {
                let _ = write!(out, "{text}");
            }
            return Ok(code);
        }
    };
    dispatch(cli, out)
}

fn dispatch<W: Write>(cli: Cli, out: &mut W) -> Result<u8, Error> {
    let cwd = std::env::current_dir().map_err(|source| Error::Io { path: None, source })?;
    match cli.command {
        Some(Command::Init) => return crate::init::init(&cwd, out).map(|()| 0),
        Some(Command::Import { spec }) => {
            return crate::import::import(&cwd, &spec, out)
                .map_err(|error| error.relativize(&cwd))
                .map(|()| 0);
        }
        None => {}
    }
    let project =
        project::discover(&cwd, cli.project.as_deref()).map_err(|error| error.relativize(&cwd))?;
    let entries =
        project::collect_requests(&project).map_err(|error| error.relativize(&project.root))?;
    let root = project.root.clone();
    run_tui(project, entries).map_err(|error| error.relativize(&root))
}

/// Opens the terminal UI, rejecting a project with no request files.
fn run_tui(project: Project, entries: Vec<Entry>) -> Result<u8, Error> {
    if entries.is_empty() {
        return Err(Error::NoRequests {
            dir: project.requests_dir(),
        });
    }
    crate::tui::run(project, entries)
}
