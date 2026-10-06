//! curlyfries — a filesystem-driven CLI API client.
//!
//! The project root is the nearest ancestor directory holding `curlyfries.json`.
//! Request definitions live under `requests/**/*.json`, environments under
//! `environments/<name>.json` and captured variables persist in
//! `.curlyfries/session.json`.
//!
//! See `docs/format.md` for the on-disk formats and `README.md` for usage.

#![warn(missing_docs)]
// Errors are built once per failure and printed, never on a hot path, so the
// size of the error type does not matter.
#![allow(clippy::result_large_err)]

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

pub mod cli;
pub mod error;
pub mod execute;
pub mod expression;
pub mod project;
pub mod render;
pub mod request;
pub mod runner;
pub mod scopes;
pub mod session;
pub mod template;
pub mod tui;

#[cfg(test)]
pub mod testutil;

/// The JSON object type used throughout this crate.
pub type Map = serde_json::Map<String, serde_json::Value>;

/// Runs the CLI with injected argument and output streams.
///
/// Errors are printed to `err` as `error: …` plus indented detail lines, and
/// the returned code is [`error::Error::exit_code`](error::Error::exit_code).
pub fn main_with<I, W, E>(args: I, out: &mut W, err: &mut E) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    match cli::parse_and_run(args, out, err) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(err, "{error}");
            ExitCode::from(error.exit_code())
        }
    }
}
