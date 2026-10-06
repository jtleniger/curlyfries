//! TUI state machine: selection, environment switching and request runs.

use std::fs;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::Map;
use crate::error::Error;
use crate::execute::{self, ExecResult, ResolvedRequest};
use crate::project::{self, Entry, Project};
use crate::render::{self, StatusClass};
use crate::request::{self, RequestDef};
use crate::runner::{self, Runner};
use crate::scopes::Scopes;
use crate::session::{self, Session};
use crate::tui::json;
use crate::tui::theme::Theme;
use crate::tui::tree::{self, TreeRow};
use crate::tui::ui;

/// Configuration captured from the CLI for one TUI session.
#[derive(Debug, Clone)]
pub struct TuiConfig {
    /// `--env`.
    pub env: Option<String>,
    /// `--var` overrides, already parsed to JSON values.
    pub overrides: Map,
    /// `--no-session`.
    pub no_session: bool,
    /// `--timeout`, `None` disables timeouts.
    pub timeout: Option<Duration>,
}

/// Which request tab the request pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestView {
    /// The request with every template resolved.
    Resolved,
    /// The request file as written.
    Raw,
}

/// Which tab the response pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseTab {
    /// The colorized body.
    Body,
    /// Response headers.
    Headers,
    /// Status line, headers and the raw body.
    Raw,
}

/// One entry of the environment picker.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EnvChoice {
    /// No environment: the `(none)` scope.
    NoneKey,
    /// A named environment from `environments/`.
    Named(String),
}

/// A finished run, sent from the worker thread.
struct RunMsg {
    /// Scope the run was started in.
    scope: String,
    /// The exchange, or why it failed.
    outcome: Result<runner::RunOutcome, Error>,
    /// Non-fatal problems collected while running.
    warnings: Vec<String>,
}

/// The last finished run, as shown in the response pane.
struct LastRun {
    /// Status class and the rendered status line, when the run succeeded.
    status: Option<(StatusClass, String)>,
    /// Resolved URL, used as the pane title.
    url: String,
    /// Failure message, when the run failed.
    error: Option<String>,
    /// The response, when the run succeeded.
    response: Option<ExecResult>,
}

/// State of the TUI. Constructible without a terminal so tests can drive it.
pub struct App {
    pub(crate) project: Project,
    pub(crate) entries: Vec<Entry>,
    defs: Vec<RequestDef>,
    raw_files: Vec<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) rows: Vec<TreeRow>,
    pub(crate) filter: String,
    pub(crate) filter_active: bool,
    pub(crate) selected_id: Option<String>,
    pub(crate) env_name: Option<String>,
    env: Map,
    pub(crate) overrides: Map,
    timeout: Option<Duration>,
    pub(crate) no_session: bool,
    session: Session,
    pub(crate) request_view: RequestView,
    resolved_view: Vec<Line<'static>>,
    raw_view: Vec<Line<'static>>,
    pub(crate) response_tab: ResponseTab,
    response_lines: Vec<Line<'static>>,
    pub(crate) response_scroll: u16,
    running: bool,
    rx: Option<Receiver<RunMsg>>,
    pub(crate) tick: u64,
    last: Option<LastRun>,
    env_choices: Vec<EnvChoice>,
    pub(crate) env_picker: Option<usize>,
    pub(crate) help: bool,
    pub(crate) status_message: Option<String>,
    quit: bool,
    pub(crate) theme: Theme,
}

impl App {
    /// Loads definitions, resolves the environment and reads the session.
    pub fn new(config: TuiConfig, project: Project, entries: Vec<Entry>) -> Result<App, Error> {
        let (env_name, env) = match project::resolve_environment(&project, config.env.as_deref())? {
            Some((name, vars)) => (Some(name), vars),
            None => (None, Map::new()),
        };
        let session = Session::load(&project.root, !config.no_session)?;

        let mut defs = Vec::with_capacity(entries.len());
        let mut raw_files = Vec::with_capacity(entries.len());
        for entry in &entries {
            defs.push(request::load_request(&entry.id, &entry.file)?);
            let text = fs::read_to_string(&entry.file).map_err(|source| Error::Io {
                path: Some(entry.file.clone()),
                source,
            })?;
            raw_files.push(text);
        }
        let labels: Vec<String> = defs.iter().map(render::request_label).collect();
        let rows = tree::build(&entries, &labels);
        let selected_id = entries.first().map(|entry| entry.id.clone());

        let mut env_choices = vec![EnvChoice::NoneKey];
        env_choices.extend(
            project::list_environments(&project)?
                .into_iter()
                .map(EnvChoice::Named),
        );

        let mut app = App {
            project,
            entries,
            defs,
            raw_files,
            labels,
            rows,
            filter: String::new(),
            filter_active: false,
            selected_id,
            env_name,
            env,
            overrides: config.overrides,
            timeout: config.timeout,
            no_session: config.no_session,
            session,
            request_view: RequestView::Resolved,
            resolved_view: Vec::new(),
            raw_view: Vec::new(),
            response_tab: ResponseTab::Body,
            response_lines: Vec::new(),
            response_scroll: 0,
            running: false,
            rx: None,
            tick: 0,
            last: None,
            env_choices,
            env_picker: None,
            help: false,
            status_message: None,
            quit: false,
            theme: Theme::from_env(),
        };
        app.refresh_request_views();
        app.refresh_response_view();
        Ok(app)
    }

    /// Renders one frame.
    pub fn draw(&mut self, frame: &mut Frame) {
        ui::draw(self, frame);
    }

    /// Applies one key press.
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.env_picker.is_some() {
            self.picker_key(key);
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if self.filter_active {
            self.filter_key(key);
            return;
        }
        self.normal_key(key);
        self.status_message = None;
    }

    /// Drains finished runs and advances the spinner. Call once per event-loop tick.
    pub fn on_tick(&mut self) {
        if self.rx.is_none() {
            return;
        }
        self.tick = self.tick.wrapping_add(1);
        let received = match &self.rx {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match received {
            Ok(message) => {
                self.running = false;
                self.rx = None;
                if !message.warnings.is_empty() {
                    self.status_message = Some(message.warnings.join("; "));
                }
                match message.outcome {
                    Ok(outcome) => {
                        if message.scope == self.scope() {
                            self.session.set(&message.scope, outcome.captured.clone());
                        }
                        let status = format!(
                            "← {} {}  {}ms",
                            outcome.response.status,
                            outcome.response.reason,
                            outcome.response.duration_ms
                        );
                        self.last = Some(LastRun {
                            status: Some((render::status_class(outcome.response.status), status)),
                            url: outcome.request.url.clone(),
                            error: None,
                            response: Some(outcome.response),
                        });
                    }
                    Err(error) => {
                        self.last = Some(LastRun {
                            status: None,
                            url: String::new(),
                            error: Some(error.to_string()),
                            response: None,
                        });
                    }
                }
                self.refresh_response_view();
                self.response_scroll = 0;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.running = false;
                self.rx = None;
                self.status_message =
                    Some("error: request worker stopped unexpectedly".to_string());
            }
        }
    }

    /// True while a request is in flight.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// True after `q`/`Esc`/`Ctrl-C` was handled.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Selects the request with id `id` in the tree, if present.
    pub fn select_id(&mut self, id: &str) {
        if self.entries.iter().any(|entry| entry.id == id) {
            self.selected_id = Some(id.to_string());
            self.refresh_request_views();
        }
    }

    /// Pumps [`App::on_tick`] until no run is in flight or `timeout` elapses,
    /// then asserts the run finished. Used by tests.
    pub fn wait_for_run(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while self.running && Instant::now() < deadline {
            self.on_tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.on_tick();
        assert!(!self.running, "run did not finish within {timeout:?}");
    }

    // --- views ------------------------------------------------------------

    /// Lines shown by the request pane.
    pub(crate) fn request_lines(&self) -> &[Line<'static>] {
        match self.request_view {
            RequestView::Resolved => &self.resolved_view,
            RequestView::Raw => &self.raw_view,
        }
    }

    /// Lines shown by the response pane, clamped to the last drawn viewport.
    pub(crate) fn response_body(&self) -> &[Line<'static>] {
        &self.response_lines
    }

    /// Last response status class and line, if a run succeeded.
    pub(crate) fn status(&self) -> Option<(StatusClass, &str)> {
        self.last
            .as_ref()
            .and_then(|last| last.status.as_ref())
            .map(|(class, line)| (*class, line.as_str()))
    }

    /// Last run URL, used as the response pane title.
    pub(crate) fn response_url(&self) -> &str {
        self.last
            .as_ref()
            .map(|last| last.url.as_str())
            .unwrap_or("")
    }

    /// Last run failure message, if any.
    pub(crate) fn error(&self) -> Option<&str> {
        self.last.as_ref().and_then(|last| last.error.as_deref())
    }

    /// Whether a response (or failure) is available.
    pub(crate) fn has_run(&self) -> bool {
        self.last.is_some()
    }

    /// Captured variables of the active scope.
    pub(crate) fn captured(&self) -> &Map {
        self.session.vars(self.scope())
    }

    /// The active capture scope key.
    pub(crate) fn scope(&self) -> &str {
        self.env_name.as_deref().unwrap_or(session::NO_ENV_KEY)
    }

    /// Picker rows: `(none)` first, then every environment name.
    pub(crate) fn env_choices(&self) -> Vec<String> {
        self.env_choices
            .iter()
            .map(|choice| match choice {
                EnvChoice::NoneKey => session::NO_ENV_KEY.to_string(),
                EnvChoice::Named(name) => name.clone(),
            })
            .collect()
    }

    // --- key handling ------------------------------------------------------

    fn picker_key(&mut self, key: KeyEvent) {
        let index = self.env_picker.unwrap_or(0);
        match key.code {
            KeyCode::Up => self.env_picker = Some(index.saturating_sub(1)),
            KeyCode::Down => {
                let last = self.env_choices.len().saturating_sub(1);
                self.env_picker = Some((index + 1).min(last));
            }
            KeyCode::Enter => self.apply_env_choice(),
            KeyCode::Esc => self.env_picker = None,
            _ => {}
        }
    }

    fn filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.filter_active = false;
                self.rebuild_rows();
            }
            KeyCode::Enter => self.filter_active = false,
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild_rows();
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.filter.push(character);
                self.rebuild_rows();
            }
            _ => {}
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('c') if control => self.quit = true,
            KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') => {
                self.filter.clear();
                self.filter_active = true;
                self.rebuild_rows();
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char('g') => self.select_edge(false),
            KeyCode::Char('G') => self.select_edge(true),
            KeyCode::Enter | KeyCode::Char('r') => self.run_selected(),
            KeyCode::Char('e') => self.open_env_picker(),
            KeyCode::Tab if shift => self.cycle_response_tab(false),
            KeyCode::BackTab => self.cycle_response_tab(false),
            KeyCode::Tab => self.cycle_response_tab(true),
            KeyCode::Char('1') => self.set_response_tab(ResponseTab::Body),
            KeyCode::Char('2') => self.set_response_tab(ResponseTab::Headers),
            KeyCode::Char('3') => self.set_response_tab(ResponseTab::Raw),
            KeyCode::Char('v') => {
                self.request_view = match self.request_view {
                    RequestView::Resolved => RequestView::Raw,
                    RequestView::Raw => RequestView::Resolved,
                };
                self.refresh_request_views();
            }
            KeyCode::Char('d') if control => self.scroll_response(10),
            KeyCode::Char('u') if control => self.scroll_response(-10),
            KeyCode::PageDown => self.scroll_response(10),
            KeyCode::PageUp => self.scroll_response(-10),
            KeyCode::Char('J') => self.scroll_response(1),
            KeyCode::Char('K') => self.scroll_response(-1),
            KeyCode::Home => self.response_scroll = 0,
            KeyCode::End => self.response_scroll = u16::MAX,
            _ => {}
        }
    }

    // --- state transitions -------------------------------------------------

    /// Moves the selection over selectable rows, wrapping at both ends.
    fn move_selection(&mut self, delta: isize) {
        let positions = self.visible_entries();
        if positions.is_empty() {
            return;
        }
        let current = self.selected_entry();
        let next = match current.and_then(|entry| positions.iter().position(|i| *i == entry)) {
            Some(at) => wrap(at as isize + delta, positions.len()),
            None if delta < 0 => positions.len() - 1,
            None => 0,
        };
        self.select_entry(positions[next]);
    }

    /// Selects the first or last visible request.
    fn select_edge(&mut self, last: bool) {
        let positions = self.visible_entries();
        let pick = if last {
            positions.last()
        } else {
            positions.first()
        };
        if let Some(index) = pick {
            self.select_entry(*index);
        }
    }

    /// Entry indices of the selectable rows, in display order.
    fn visible_entries(&self) -> Vec<usize> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                TreeRow::Request { index, .. } => Some(*index),
                TreeRow::Folder { .. } => None,
            })
            .collect()
    }

    fn select_entry(&mut self, index: usize) {
        self.selected_id = Some(self.entries[index].id.clone());
        self.refresh_request_views();
    }

    fn scroll_response(&mut self, delta: i32) {
        self.response_scroll = if delta < 0 {
            self.response_scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        } else {
            self.response_scroll.saturating_add(delta as u16)
        };
    }

    fn cycle_response_tab(&mut self, forward: bool) {
        let tab = match (self.response_tab, forward) {
            (ResponseTab::Body, true) => ResponseTab::Headers,
            (ResponseTab::Headers, true) => ResponseTab::Raw,
            (ResponseTab::Raw, true) => ResponseTab::Body,
            (ResponseTab::Body, false) => ResponseTab::Raw,
            (ResponseTab::Headers, false) => ResponseTab::Body,
            (ResponseTab::Raw, false) => ResponseTab::Headers,
        };
        self.set_response_tab(tab);
    }

    fn set_response_tab(&mut self, tab: ResponseTab) {
        self.response_tab = tab;
        self.refresh_response_view();
        self.response_scroll = 0;
    }

    fn open_env_picker(&mut self) {
        let current = match &self.env_name {
            Some(name) => self
                .env_choices
                .iter()
                .position(|choice| matches!(choice, EnvChoice::Named(other) if other == name)),
            None => self
                .env_choices
                .iter()
                .position(|choice| matches!(choice, EnvChoice::NoneKey)),
        };
        self.env_picker = Some(current.unwrap_or(0));
    }

    fn apply_env_choice(&mut self) {
        let Some(index) = self.env_picker else {
            return;
        };
        if let Some(choice) = self.env_choices.get(index).cloned() {
            match choice {
                EnvChoice::NoneKey => {
                    self.env_name = None;
                    self.env = Map::new();
                }
                EnvChoice::Named(name) => match project::load_environment(&self.project, &name) {
                    Ok(vars) => {
                        self.env = vars;
                        self.env_name = Some(name);
                    }
                    Err(error) => self.status_message = Some(error.to_string()),
                },
            }
        }
        self.env_picker = None;
        self.rebuild_rows();
        self.refresh_request_views();
        self.refresh_response_view();
    }

    fn rebuild_rows(&mut self) {
        let all = tree::build(&self.entries, &self.labels);
        self.rows = tree::filter(&all, &self.labels, &self.filter);
        let visible = self.visible_entries();
        if !self
            .selected_entry()
            .is_some_and(|entry| visible.contains(&entry))
        {
            self.selected_id = visible.first().map(|index| self.entries[*index].id.clone());
        }
    }

    fn selected_entry(&self) -> Option<usize> {
        let id = self.selected_id.as_deref()?;
        self.entries.iter().position(|entry| entry.id == id)
    }

    fn refresh_request_views(&mut self) {
        let Some(index) = self.selected_entry() else {
            self.resolved_view = Vec::new();
            self.raw_view = Vec::new();
            return;
        };
        let theme = self.theme;
        let resolved = {
            let scopes = Scopes {
                env_name: self.env_name.as_deref(),
                env: &self.env,
                session: self.session.vars(self.scope()),
                overrides: &self.overrides,
            };
            match execute::render_request(&self.defs[index], &scopes) {
                Ok(request) => resolved_lines(&request, &theme),
                Err(error) => vec![Line::styled(
                    format!("{error}"),
                    Style::default().fg(theme.method_delete),
                )],
            }
        };
        self.resolved_view = resolved;
        self.raw_view = json::json_lines(&self.raw_files[index], &theme);
    }

    fn refresh_response_view(&mut self) {
        let theme = self.theme;
        let lines = match (&self.last, self.response_tab) {
            (None, _) => Vec::new(),
            (Some(last), _) if last.error.is_some() => vec![Line::styled(
                last.error.clone().unwrap_or_default(),
                Style::default().fg(theme.method_delete),
            )],
            (Some(last), tab) => match &last.response {
                None => Vec::new(),
                Some(response) => match tab {
                    ResponseTab::Body => {
                        if response.body_text.is_empty() {
                            vec![Line::styled("(empty body)", Style::default().fg(theme.dim))]
                        } else if response.body_json.is_some() {
                            json::json_lines(&response.body_text, &theme)
                        } else {
                            plain_lines(&response.body_text)
                        }
                    }
                    ResponseTab::Headers => response
                        .headers
                        .iter()
                        .map(|(name, value)| Line::from(format!("{name}: {value}")))
                        .collect(),
                    ResponseTab::Raw => {
                        let mut lines: Vec<Line<'static>> = response
                            .headers
                            .iter()
                            .map(|(name, value)| Line::from(format!("{name}: {value}")))
                            .collect();
                        lines.push(Line::raw(String::new()));
                        lines.extend(plain_lines(&response.body_text));
                        lines
                    }
                },
            },
        };
        self.response_lines = lines;
    }

    fn run_selected(&mut self) {
        if self.running {
            return;
        }
        let Some(index) = self.selected_entry() else {
            return;
        };
        let id = self.entries[index].id.clone();
        let scope = self.scope().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let (project, entries) = (self.project.clone(), self.entries.clone());
        let (env_name, env, overrides) = (
            self.env_name.clone(),
            self.env.clone(),
            self.overrides.clone(),
        );
        let (timeout, no_session) = (self.timeout, self.no_session);
        std::thread::spawn(move || {
            let mut session = match Session::load(&project.root, !no_session) {
                Ok(session) => session,
                Err(error) => {
                    let _ = tx.send(RunMsg {
                        scope,
                        outcome: Err(error),
                        warnings: Vec::new(),
                    });
                    return;
                }
            };
            let mut runner = Runner {
                project: &project,
                entries: &entries,
                env_name: env_name.clone(),
                env,
                overrides,
                session: &mut session,
                client: execute::client(timeout),
                timeout,
                warnings: Vec::new(),
            };
            let outcome = runner.execute_id(&id);
            let _ = tx.send(RunMsg {
                scope,
                outcome,
                warnings: runner.warnings,
            });
        });
        self.running = true;
        self.rx = Some(rx);
    }
}

/// Splits plain text into unstyled lines.
fn plain_lines(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| Line::from(line.to_string()))
        .collect()
}

/// Renders a resolved request for the request pane.
fn resolved_lines(request: &ResolvedRequest, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{} ", request.method),
            Style::default()
                .fg(theme.method(&request.method))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(request.url.clone()),
    ])];
    if !request.headers.is_empty() {
        lines.push(Line::raw(String::new()));
        for (name, value) in &request.headers {
            lines.push(Line::from(format!("{name}: {value}")));
        }
    }
    if let Some(body) = &request.body {
        lines.push(Line::raw(String::new()));
        lines.extend(json::json_lines(body, theme));
    }
    lines
}

/// Euclidean modulo of `value` into `0..modulo`.
fn wrap(value: isize, modulo: usize) -> usize {
    let modulo = modulo as isize;
    (((value % modulo) + modulo) % modulo) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn fixture() -> (TempDir, Project, Vec<Entry>) {
        let dir = TempDir::new("app");
        dir.write("curlyfries.json", "{}");
        dir.write(
            "requests/auth/login.json",
            r#"{ "method": "POST", "path": "/auth/login" }"#,
        );
        dir.write(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships" }"#,
        );
        dir.write(
            "requests/health.json",
            r#"{ "method": "GET", "path": "/health" }"#,
        );
        let project = project::discover(dir.path(), None).unwrap();
        let entries = project::collect_requests(&project).unwrap();
        (dir, project, entries)
    }

    fn app() -> (TempDir, App) {
        let (dir, project, entries) = fixture();
        let config = TuiConfig {
            env: None,
            overrides: Map::new(),
            no_session: true,
            timeout: None,
        };
        let app = App::new(config, project, entries).unwrap();
        (dir, app)
    }

    #[test]
    fn selection_skips_folders_and_wraps() {
        let (_dir, mut app) = app();
        assert_eq!(app.selected_id.as_deref(), Some("auth/login"));
        // Ids sort as auth/login, health, ships/list.
        app.move_selection(1);
        assert_eq!(app.selected_id.as_deref(), Some("health"));
        // health -> ships/list crosses the `ships/` folder row.
        app.move_selection(1);
        assert_eq!(app.selected_id.as_deref(), Some("ships/list"));
        app.move_selection(1);
        assert_eq!(app.selected_id.as_deref(), Some("auth/login"));
        app.move_selection(-1);
        assert_eq!(app.selected_id.as_deref(), Some("ships/list"));
    }

    #[test]
    fn filter_narrows_the_tree_and_escape_restores_it() {
        let (_dir, mut app) = app();
        let before = app.rows.len();
        app.on_key(key(KeyCode::Char('/')));
        assert!(app.filter_active);
        for character in "ships".chars() {
            app.on_key(key(KeyCode::Char(character)));
        }
        assert_eq!(app.filter, "ships");
        assert_eq!(app.selected_id.as_deref(), Some("ships/list"));
        assert!(app.rows.len() < before);
        app.on_key(key(KeyCode::Esc));
        assert!(!app.filter_active);
        assert!(app.filter.is_empty());
        assert_eq!(app.rows.len(), before);
    }

    #[test]
    fn tab_keys_cycle_the_response_tab() {
        let (_dir, mut app) = app();
        assert_eq!(app.response_tab, ResponseTab::Body);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.response_tab, ResponseTab::Headers);
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.response_tab, ResponseTab::Body);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(app.response_tab, ResponseTab::Raw);
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.response_tab, ResponseTab::Body);
    }
}
