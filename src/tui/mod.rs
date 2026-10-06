//! Full-screen terminal UI for browsing and running requests.

use std::io::{self, IsTerminal};
use std::time::Duration;

use ratatui::crossterm::event::{self, Event};

use crate::error::Error;
use crate::project::{Entry, Project};

pub mod app;
pub mod json;
pub mod theme;
pub mod tree;
pub mod ui;

pub use app::TuiConfig;

/// How long the event loop waits for a key before redrawing.
const POLL: Duration = Duration::from_millis(50);

/// Runs the TUI until the user quits. Enters the alternate screen.
///
/// Errors with [`Error::NotATerminal`] when stdin or stdout is not a terminal,
/// and [`Error::Interactive`] when the terminal cannot be initialised or read.
pub fn run(config: TuiConfig, project: Project, entries: Vec<Entry>) -> Result<u8, Error> {
    if !io::stdin().is_terminal() {
        return Err(Error::NotATerminal { stream: "stdin" });
    }
    if !io::stdout().is_terminal() {
        return Err(Error::NotATerminal { stream: "stdout" });
    }

    let mut app = app::App::new(config, project, entries)?;
    let mut guard = TerminalGuard::enter()?;
    while !app.should_quit() {
        guard
            .terminal
            .draw(|frame| app.draw(frame))
            .map_err(tui_io)?;
        app.on_tick();
        if event::poll(POLL).map_err(tui_io)?
            && let Event::Key(key) = event::read().map_err(tui_io)?
        {
            app.on_key(key);
        }
    }
    Ok(0)
}

/// Turns an I/O failure into [`Error::Interactive`].
fn tui_io(source: io::Error) -> Error {
    Error::Interactive {
        message: source.to_string(),
    }
}

/// Owns the terminal; restores it even when the loop returns early or panics.
struct TerminalGuard {
    terminal: ratatui::DefaultTerminal,
}

impl TerminalGuard {
    fn enter() -> Result<TerminalGuard, Error> {
        let terminal = ratatui::try_init().map_err(tui_io)?;
        Ok(TerminalGuard { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
