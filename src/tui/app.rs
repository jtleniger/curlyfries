//! TUI state machine: selection, environment switching and request runs.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::Map;
use crate::error::Error;
use crate::execute::{self, ExecResult, ResolvedRequest};
use crate::project::{self, Entry, Project};
use crate::render::{self, StatusClass};
use crate::request::{self, DisabledSlots, Input, RequestDef};
use crate::runner::{self, Runner};
use crate::scopes::Scopes;
use crate::session::{self, Session};
use crate::tui::json;
use crate::tui::theme::Theme;
use crate::tui::tree::{self, TreeRow};
use crate::tui::ui;
use crate::variables::Variables;

/// A destructive capture clear waiting for confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingClear {
    /// The active scope only.
    Scope,
    /// Every scope in the session file.
    All,
}

/// Which request tab the request pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestView {
    /// The request with every template resolved.
    Resolved,
    /// The request file as written.
    Definition,
}

/// Which pane the navigation keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The request tree: the keys move the selection.
    Tree,
    /// The request pane: the keys scroll it.
    Request,
    /// The response pane: the keys scroll it.
    Response,
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

/// A request waiting for `${name[]}` picks and `${name?}` prompts before it is sent.
struct PendingRun {
    /// Index into `entries` of the request to send.
    entry_index: usize,
    /// Inputs still to satisfy, in first-seen order.
    remaining: Vec<Input>,
    /// Chosen values, keyed `name[]`, `name?` or `name?json`.
    choices: Map,
}

/// The open picker for one array variable.
struct PickModal {
    /// Base name of the array variable, e.g. `shipClasses`.
    name: String,
    /// The array's elements, in order.
    options: Vec<Value>,
    /// Highlighted option.
    selected: usize,
}

/// The open text prompt for one `${name?}` / `${name?json}` token.
struct PromptModal {
    /// Label shown as the modal title.
    name: String,
    /// Whether the typed text is parsed as JSON before it is stored.
    json: bool,
    /// Text typed so far.
    buffer: String,
    /// Why the last `Enter` was refused, if it was.
    error: Option<String>,
}

/// Which list a toggle row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotKind {
    Header,
    Query,
}

/// One toggleable slot of a request.
struct ToggleRow {
    kind: SlotKind,
    key: String,
    value: String,
}

/// The open enable/disable modal for one request.
pub(crate) struct ToggleModal {
    entry_index: usize,
    rows: Vec<ToggleRow>,
    selected: usize,
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
    definition_files: Vec<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) rows: Vec<TreeRow>,
    pub(crate) filter: String,
    pub(crate) filter_active: bool,
    pub(crate) selected_id: Option<String>,
    pub(crate) env_name: Option<String>,
    env: Variables,
    timeout: Option<Duration>,
    session: Session,
    pub(crate) request_view: RequestView,
    /// Pane the navigation keys act on.
    pub(crate) focus: Focus,
    /// The `url` section of the resolved request, shown full width.
    resolved_url: Vec<Line<'static>>,
    /// The `headers` and `query` sections of the resolved request.
    resolved_meta: Vec<Line<'static>>,
    /// The `body` section of the resolved request.
    resolved_body: Vec<Line<'static>>,
    definition_view: Vec<Line<'static>>,
    pub(crate) request_scroll: u16,
    pub(crate) response_tab: ResponseTab,
    response_lines: Vec<Line<'static>>,
    pub(crate) response_scroll: u16,
    running: bool,
    rx: Option<Receiver<RunMsg>>,
    pub(crate) tick: u64,
    last: Option<LastRun>,
    env_choices: Vec<EnvChoice>,
    pub(crate) env_picker: Option<usize>,
    /// A request waiting for picks and prompts, if any.
    pending: Option<PendingRun>,
    /// The open picker for one array variable, if any.
    pick_modal: Option<PickModal>,
    /// The open text prompt for one `${name?}` / `${name?json}` token, if any.
    prompt_modal: Option<PromptModal>,
    /// The open enable/disable modal for one request, if any.
    pub(crate) toggle_modal: Option<ToggleModal>,
    /// Header and query slots switched off, keyed by request id.
    disabled: HashMap<String, DisabledSlots>,
    pub(crate) confirm: Option<PendingClear>,
    pub(crate) help: bool,
    pub(crate) status_message: Option<String>,
    quit: bool,
    pub(crate) theme: Theme,
}

impl App {
    /// Loads definitions, resolves the environment and reads the session.
    pub fn new(project: Project, entries: Vec<Entry>) -> Result<App, Error> {
        let (env_name, env) = match project::resolve_environment(&project)? {
            Some((name, vars)) => (Some(name), vars),
            None => (None, Variables::default()),
        };
        let timeout = project.manifest.timeout();
        let session = Session::load(&project.root)?;

        let mut defs = Vec::with_capacity(entries.len());
        let mut definition_files = Vec::with_capacity(entries.len());
        for entry in &entries {
            defs.push(request::load_request(&entry.id, &entry.file)?);
            let text = fs::read_to_string(&entry.file).map_err(|source| Error::Io {
                path: Some(entry.file.clone()),
                source,
            })?;
            definition_files.push(text);
        }
        let labels: Vec<String> = defs.iter().map(render::request_label).collect();
        let rows = tree::build(&entries);
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
            definition_files,
            labels,
            rows,
            filter: String::new(),
            filter_active: false,
            selected_id,
            env_name,
            env,
            timeout,
            session,
            request_view: RequestView::Resolved,
            focus: Focus::Tree,
            resolved_url: Vec::new(),
            resolved_meta: Vec::new(),
            resolved_body: Vec::new(),
            definition_view: Vec::new(),
            request_scroll: 0,
            response_tab: ResponseTab::Body,
            response_lines: Vec::new(),
            response_scroll: 0,
            running: false,
            rx: None,
            tick: 0,
            last: None,
            env_choices,
            env_picker: None,
            pending: None,
            pick_modal: None,
            prompt_modal: None,
            toggle_modal: None,
            disabled: HashMap::new(),
            confirm: None,
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
        if self.confirm.is_some() {
            self.confirm_key(key);
            return;
        }
        if self.pick_modal.is_some() {
            self.pick_key(key);
            return;
        }
        if self.prompt_modal.is_some() {
            self.prompt_key(key);
            return;
        }
        if self.toggle_modal.is_some() {
            self.toggle_key(key);
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
        if let KeyCode::Char(character @ ('x' | 'X')) = key.code
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            self.request_clear(character == 'X');
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
                            self.session.set(&message.scope, &outcome.captured);
                        }
                        let status = format!(
                            "← {} {}  {}ms",
                            outcome.response.status,
                            outcome.response.reason,
                            outcome.response.duration_ms
                        );
                        self.last = Some(LastRun {
                            status: Some((render::status_class(outcome.response.status), status)),
                            url: outcome.display_url.clone(),
                            error: outcome.capture_error.map(|error| error.to_string()),
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

    /// The `url` section of the resolved request: shown full width above the
    /// two columns. Empty in the definition view.
    pub(crate) fn request_url(&self) -> &[Line<'static>] {
        match self.request_view {
            RequestView::Resolved => &self.resolved_url,
            RequestView::Definition => &[],
        }
    }

    /// Lines shown on the left of the request pane: `headers` and `query`.
    /// Empty in the definition view, where the file fills the pane.
    pub(crate) fn request_meta(&self) -> &[Line<'static>] {
        match self.request_view {
            RequestView::Resolved => &self.resolved_meta,
            RequestView::Definition => &[],
        }
    }

    /// Lines shown on the right of the request pane: the `body` section, or the
    /// whole request file in the definition view.
    pub(crate) fn request_body(&self) -> &[Line<'static>] {
        match self.request_view {
            RequestView::Resolved => &self.resolved_body,
            RequestView::Definition => &self.definition_view,
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

    /// Names among the active scope's captures that are secret.
    pub(crate) fn captured_secrets(&self) -> &BTreeSet<String> {
        &self.session.variables(self.scope()).secrets
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

    /// Title, options and selection of the open picker, if any.
    pub(crate) fn pick_prompt(&self) -> Option<(String, Vec<String>, usize)> {
        self.pick_modal.as_ref().map(|modal| {
            (
                format!("Pick {}", modal.name),
                modal.options.iter().map(render::pick_label).collect(),
                modal.selected,
            )
        })
    }

    /// Title, typed text and error of the open prompt modal, if any.
    pub(crate) fn prompt_prompt(&self) -> Option<(String, String, Option<String>)> {
        self.prompt_modal.as_ref().map(|modal| {
            let title = if modal.json {
                format!("Input {} (JSON)", modal.name)
            } else {
                format!("Input {}", modal.name)
            };
            (title, modal.buffer.clone(), modal.error.clone())
        })
    }

    /// Request id, `(enabled, label)` rows and selection of the open toggle modal.
    pub(crate) fn toggle_prompt(&self) -> Option<(String, Vec<(bool, String)>, usize)> {
        self.toggle_modal.as_ref().map(|modal| {
            let id = self.entries[modal.entry_index].id.clone();
            let slots = self.disabled.get(&id);
            let rows = modal
                .rows
                .iter()
                .map(|row| {
                    let enabled = match (row.kind, slots) {
                        (SlotKind::Header, Some(slots)) => !slots.headers.contains(&row.key),
                        (SlotKind::Query, Some(slots)) => !slots.query.contains(&row.key),
                        (_, None) => true,
                    };
                    let label = match row.kind {
                        SlotKind::Header => format!("header  {}: {}", row.key, row.value),
                        SlotKind::Query => format!("query   {}={}", row.key, row.value),
                    };
                    (enabled, label)
                })
                .collect();
            (id, rows, modal.selected)
        })
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

    /// Handles a key while the pick modal is up.
    fn pick_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.pick_modal.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up => modal.selected = modal.selected.saturating_sub(1),
            KeyCode::Down => {
                let last = modal.options.len().saturating_sub(1);
                modal.selected = (modal.selected + 1).min(last);
            }
            KeyCode::Enter => {
                let name = modal.name.clone();
                let chosen = modal.options.get(modal.selected).cloned();
                self.pick_modal = None;
                if let (Some(chosen), Some(pending)) = (chosen, self.pending.as_mut()) {
                    pending.choices.insert(format!("{name}[]"), chosen);
                }
                self.advance_input();
            }
            KeyCode::Esc => {
                self.pick_modal = None;
                self.pending = None;
            }
            _ => {}
        }
    }

    /// Handles a key while the prompt modal is up.
    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.prompt_modal.as_ref() else {
            return;
        };
        let (json, name, text) = (modal.json, modal.name.clone(), modal.buffer.clone());
        match key.code {
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(modal) = self.prompt_modal.as_mut() {
                    modal.buffer.push(character);
                    modal.error = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(modal) = self.prompt_modal.as_mut() {
                    modal.buffer.pop();
                    modal.error = None;
                }
            }
            KeyCode::Enter => {
                let value = if json {
                    match serde_json::from_str::<Value>(&text) {
                        Ok(value) => value,
                        Err(error) => {
                            if let Some(modal) = self.prompt_modal.as_mut() {
                                modal.error = Some(format!("not valid JSON: {error}"));
                            }
                            return;
                        }
                    }
                } else {
                    Value::String(text)
                };
                let key = if json {
                    format!("{name}?json")
                } else {
                    format!("{name}?")
                };
                self.prompt_modal = None;
                if let Some(pending) = self.pending.as_mut() {
                    pending.choices.insert(key, value);
                }
                self.advance_input();
            }
            KeyCode::Esc => {
                self.prompt_modal = None;
                self.pending = None;
            }
            _ => {}
        }
    }

    /// Opens the toggle modal for the selected request.
    fn open_toggle_modal(&mut self) {
        let Some(index) = self.selected_entry() else {
            return;
        };
        let def = &self.defs[index];
        let mut rows = Vec::with_capacity(def.headers.len() + def.query.len());
        rows.extend(def.headers.iter().map(|(key, value)| ToggleRow {
            kind: SlotKind::Header,
            key: key.clone(),
            value: value.clone(),
        }));
        rows.extend(def.query.iter().map(|(key, value)| ToggleRow {
            kind: SlotKind::Query,
            key: key.clone(),
            value: value.clone(),
        }));
        self.toggle_modal = Some(ToggleModal {
            entry_index: index,
            rows,
            selected: 0,
        });
    }

    /// Handles a key while the toggle modal is up.
    fn toggle_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.toggle_modal.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                modal.selected = modal.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = modal.rows.len().saturating_sub(1);
                modal.selected = (modal.selected + 1).min(last);
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle_selected(),
            KeyCode::Esc | KeyCode::Char('q') => self.toggle_modal = None,
            _ => {}
        }
    }

    /// Flips the highlighted slot on/off and re-renders the request pane.
    fn toggle_selected(&mut self) {
        let Some(modal) = self.toggle_modal.as_ref() else {
            return;
        };
        let Some(row) = modal.rows.get(modal.selected) else {
            return;
        };
        let id = self.entries[modal.entry_index].id.clone();
        let slots = self.disabled.entry(id).or_default();
        let set = match row.kind {
            SlotKind::Header => &mut slots.headers,
            SlotKind::Query => &mut slots.query,
        };
        if !set.remove(&row.key) {
            set.insert(row.key.clone());
        }
        self.refresh_request_views();
    }

    /// Opens the next picker or prompt, or sends the request once every input is set.
    fn advance_input(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let next = if pending.remaining.is_empty() {
            None
        } else {
            Some(pending.remaining.remove(0))
        };
        let Some(input) = next else {
            let pending = self.pending.take().expect("pending is set");
            self.spawn_run(pending.entry_index, pending.choices);
            return;
        };
        match input {
            Input::Pick(name) => match self.pick_options(&name) {
                Ok(options) => {
                    self.pick_modal = Some(PickModal {
                        name,
                        options,
                        selected: 0,
                    });
                }
                Err(message) => {
                    self.status_message = Some(message);
                    self.pending = None;
                }
            },
            Input::Prompt { name, json } => {
                self.prompt_modal = Some(PromptModal {
                    name,
                    json,
                    buffer: String::new(),
                    error: None,
                });
            }
        }
    }

    /// The elements to offer for the array variable `name`.
    fn pick_options(&self, name: &str) -> Result<Vec<Value>, String> {
        let scopes = Scopes {
            env_name: self.env_name.as_deref(),
            env: &self.env,
            session: self.session.variables(self.scope()),
            picks: None,
        };
        if scopes.is_secret(name) {
            return Err(format!("cannot pick from secret variable `{name}`"));
        }
        match scopes.lookup(name) {
            Some(Value::Array(items)) if !items.is_empty() => Ok(items.clone()),
            Some(Value::Array(_)) => Err(format!("array `{name}` is empty")),
            Some(_) => Err(format!("variable `{name}` is not an array")),
            None => Err(format!("no array `{name}` for `${{{name}[]}}`")),
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
            KeyCode::Up | KeyCode::Char('k') => self.navigate(-1),
            KeyCode::Down | KeyCode::Char('j') => self.navigate(1),
            KeyCode::Char('g') | KeyCode::Home => self.navigate_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.navigate_edge(true),
            KeyCode::PageUp => self.navigate(-10),
            KeyCode::PageDown => self.navigate(10),
            KeyCode::Char('u') if control => self.navigate(-10),
            KeyCode::Char('d') if control => self.navigate(10),
            KeyCode::Enter | KeyCode::Char('r') => self.run_selected(),
            KeyCode::Char('e') => self.open_env_picker(),
            KeyCode::Char('t') => self.open_toggle_modal(),
            KeyCode::Tab if shift => self.cycle_focus(false),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::Char('[') => self.cycle_response_tab(false),
            KeyCode::Char(']') => self.cycle_response_tab(true),
            KeyCode::Char('1') => self.set_response_tab(ResponseTab::Body),
            KeyCode::Char('2') => self.set_response_tab(ResponseTab::Headers),
            KeyCode::Char('3') => self.set_response_tab(ResponseTab::Raw),
            KeyCode::Char('v') => {
                self.request_view = match self.request_view {
                    RequestView::Resolved => RequestView::Definition,
                    RequestView::Definition => RequestView::Resolved,
                };
                self.request_scroll = 0;
                self.refresh_request_views();
            }
            _ => {}
        }
    }

    /// Asks to clear captures, or refuses while a request is in flight.
    fn request_clear(&mut self, all: bool) {
        if self.running {
            self.status_message = Some("wait for the running request to finish".to_string());
            return;
        }
        self.confirm = Some(if all {
            PendingClear::All
        } else {
            PendingClear::Scope
        });
    }

    /// Handles a key while the clear confirmation is up.
    fn confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                let all = matches!(self.confirm, Some(PendingClear::All));
                self.confirm = None;
                self.apply_clear(all);
            }
            KeyCode::Esc | KeyCode::Char('n' | 'N') => self.confirm = None,
            _ => {}
        }
    }

    /// Clears the active scope, or every scope, and reports the outcome.
    fn apply_clear(&mut self, all: bool) {
        let scope = self.scope().to_string();
        let removed = if all {
            self.session.clear(None)
        } else {
            self.session.clear(Some(&scope))
        };
        self.status_message = Some(if removed == 0 {
            "no captured variables to clear".to_string()
        } else {
            match self.session.save() {
                Ok(()) => format!("cleared {removed} captured variable(s)"),
                Err(error) => format!("error: {error}"),
            }
        });
        self.refresh_request_views();
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
        self.request_scroll = 0;
        self.refresh_request_views();
    }

    /// Moves the focused pane: the selection in the tree, a scroll offset
    /// elsewhere.
    fn navigate(&mut self, delta: i32) {
        match self.focus {
            Focus::Tree => self.move_selection(delta as isize),
            Focus::Request => self.request_scroll = shifted(self.request_scroll, delta),
            Focus::Response => self.response_scroll = shifted(self.response_scroll, delta),
        }
    }

    /// Jumps the focused pane to its first row (`last` = false) or last. Both
    /// scroll offsets use `u16::MAX` for "bottom", clamped when drawn.
    fn navigate_edge(&mut self, last: bool) {
        match self.focus {
            Focus::Tree => self.select_edge(last),
            Focus::Request => self.request_scroll = if last { u16::MAX } else { 0 },
            Focus::Response => self.response_scroll = if last { u16::MAX } else { 0 },
        }
    }

    /// Cycles the focus: Tree → Request → Response → Tree.
    fn cycle_focus(&mut self, forward: bool) {
        self.focus = match (self.focus, forward) {
            (Focus::Tree, true) => Focus::Request,
            (Focus::Request, true) => Focus::Response,
            (Focus::Response, true) => Focus::Tree,
            (Focus::Tree, false) => Focus::Response,
            (Focus::Response, false) => Focus::Request,
            (Focus::Request, false) => Focus::Tree,
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
                    self.env = Variables::default();
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
        let all = tree::build(&self.entries);
        self.rows = tree::filter(&all, &self.entries, &self.labels, &self.filter);
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
            self.resolved_url = Vec::new();
            self.resolved_meta = Vec::new();
            self.resolved_body = Vec::new();
            self.definition_view = Vec::new();
            return;
        };
        let theme = self.theme;
        let (url, meta, body) = {
            let scopes = Scopes {
                env_name: self.env_name.as_deref(),
                env: &self.env,
                session: self.session.variables(self.scope()),
                picks: None,
            };
            let disabled = self
                .disabled
                .get(&self.defs[index].id)
                .cloned()
                .unwrap_or_default();
            let def = self.defs[index].without_disabled(&disabled);
            match execute::render_request_display(&def, &scopes) {
                Ok(request) => resolved_lines(&self.defs[index], &disabled, &request, &theme),
                Err(error) => (
                    Vec::new(),
                    vec![Line::styled(
                        format!("{error}"),
                        Style::default().fg(theme.method_delete),
                    )],
                    Vec::new(),
                ),
            }
        };
        self.resolved_url = url;
        self.resolved_meta = meta;
        self.resolved_body = body;
        self.definition_view = json::json_lines(&self.definition_files[index], &theme);
    }

    fn refresh_response_view(&mut self) {
        let theme = self.theme;
        let lines = match &self.last {
            None => Vec::new(),
            Some(last) => match &last.response {
                None => Vec::new(),
                Some(response) => match self.response_tab {
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
        let inputs = request::input_names(&self.defs[index]);
        if inputs.is_empty() {
            self.spawn_run(index, Map::new());
            return;
        }
        self.pending = Some(PendingRun {
            entry_index: index,
            remaining: inputs,
            choices: Map::new(),
        });
        self.advance_input();
    }

    /// Starts the selected request on a worker thread with `choices` as picks.
    fn spawn_run(&mut self, index: usize, choices: Map) {
        let id = self.entries[index].id.clone();
        let scope = self.scope().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let (project, entries) = (self.project.clone(), self.entries.clone());
        let (env_name, env) = (self.env_name.clone(), self.env.clone());
        let picks = Variables::from_values(choices);
        let disabled = self.disabled.get(&id).cloned().unwrap_or_default();
        let timeout = self.timeout;
        std::thread::spawn(move || {
            let mut session = match Session::load(&project.root) {
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
                picks,
                disabled,
                session: &mut session,
                client: execute::client(timeout),
                timeout,
                follow_redirects: project.manifest.follow_redirects,
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

/// Renders a resolved request for the request pane, labelling each part.
///
/// Returns the `url` section (drawn full width), then the `headers` and `query`
/// sections (the left column) and the `body` (the right column).
///
/// `def` is the request as written and `disabled` the slots switched off, so
/// the `headers` and `query` sections list every slot in file order with its
/// toggle state (`[x]` enabled, `[ ]` off). Switched-off slots keep their raw
/// text, since their templates are never evaluated.
fn resolved_lines(
    def: &RequestDef,
    disabled: &DisabledSlots,
    request: &ResolvedRequest,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Line<'static>>, Vec<Line<'static>>) {
    let section = |title: &str| Line::styled(title.to_string(), Style::default().fg(theme.title));
    let empty = || Line::styled("(none)", Style::default().fg(theme.dim));

    let url = vec![
        section("url"),
        Line::from(vec![
            Span::styled(
                format!("{} ", request.method),
                Style::default()
                    .fg(theme.method(&request.method))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(request.url.clone()),
        ]),
        Line::raw(String::new()),
    ];

    let mut meta = vec![section("headers")];

    // Sent values line up with the enabled slots in file order; headers added
    // at render time (a default `Content-Type`) follow them.
    let mut sent = request.headers.iter();
    let mut headers: Vec<String> = Vec::new();
    for (key, value) in &def.headers {
        if disabled.headers.contains(key) {
            headers.push(format!("[ ] {key}: {value}"));
        } else if let Some((name, value)) = sent.next() {
            headers.push(format!("[x] {name}: {value}"));
        }
    }
    headers.extend(sent.map(|(name, value)| format!("[x] {name}: {value}")));

    if headers.is_empty() {
        meta.push(empty());
    } else {
        meta.extend(headers.into_iter().map(Line::from));
    }

    let mut sent = request.query.iter();
    let mut query: Vec<String> = Vec::new();
    for (key, value) in &def.query {
        if disabled.query.contains(key) {
            query.push(format!("[ ] {key}={value}"));
        } else if let Some((name, value)) = sent.next() {
            query.push(format!("[x] {name}={value}"));
        }
    }
    query.extend(sent.map(|(name, value)| format!("[x] {name}={value}")));

    meta.push(Line::raw(String::new()));
    meta.push(section("query"));
    if query.is_empty() {
        meta.push(empty());
    } else {
        meta.extend(query.into_iter().map(Line::from));
    }

    let mut body = vec![section("body")];
    match &request.body {
        Some(text) => body.extend(json::json_lines(text, theme)),
        None => body.push(empty()),
    }

    (url, meta, body)
}

/// Euclidean modulo of `value` into `0..modulo`.
fn wrap(value: isize, modulo: usize) -> usize {
    let modulo = modulo as isize;
    (((value % modulo) + modulo) % modulo) as usize
}

/// Shifts a scroll offset by `delta`, saturating at both ends.
fn shifted(value: u16, delta: i32) -> u16 {
    if delta < 0 {
        value.saturating_sub(delta.unsigned_abs() as u16)
    } else {
        value.saturating_add(delta as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use serde_json::json;

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
        let app = App::new(project, entries).unwrap();
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
    fn tab_cycles_focus_and_brackets_cycle_the_response_tab() {
        let (_dir, mut app) = app();
        assert_eq!(app.focus, Focus::Tree);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Request);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Tree);
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Response);
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Request);

        // Tab no longer touches the response tabs; `[`/`]` cycle them.
        assert_eq!(app.response_tab, ResponseTab::Body);
        app.on_key(key(KeyCode::Char(']')));
        assert_eq!(app.response_tab, ResponseTab::Headers);
        app.on_key(key(KeyCode::Char('[')));
        assert_eq!(app.response_tab, ResponseTab::Body);
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.response_tab, ResponseTab::Body);
    }

    #[test]
    fn navigation_keys_follow_the_focused_pane() {
        let (_dir, mut app) = app();

        // Tree focus: the keys move the selection.
        let first = app.selected_id.clone();
        app.on_key(key(KeyCode::Char('j')));
        assert_ne!(app.selected_id, first);
        assert_eq!(app.request_scroll, 0);

        // Request focus: the same keys scroll the request, not the tree.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Request);
        let selected = app.selected_id.clone();
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_id, selected, "the tree must not move");
        assert_eq!(app.request_scroll, 1);
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.request_scroll, 11);
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.request_scroll, 0);

        // Response focus: the same keys scroll the response instead.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Response);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.response_scroll, 1);
        assert_eq!(app.request_scroll, 0);
    }

    #[test]
    fn pick_tokens_open_a_picker_before_sending() {
        let dir = TempDir::new("app-pick");
        dir.write("curlyfries.json", r#"{ "defaultEnvironment": "dev" }"#);
        dir.write(
            "environments/dev.json",
            r#"{ "baseUrl": { "value": "http://127.0.0.1:1" }, "shipClasses": { "value": ["frigate", "sloop"] } }"#,
        );
        dir.write(
            "requests/ships/list.json",
            r#"{ "method": "GET", "path": "/ships?class=${shipClasses[]}" }"#,
        );
        let project = project::discover(dir.path(), None).unwrap();
        let entries = project::collect_requests(&project).unwrap();
        let mut app = App::new(project, entries).unwrap();

        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.pick_prompt(),
            Some((
                "Pick shipClasses".to_string(),
                vec!["frigate".to_string(), "sloop".to_string()],
                0
            ))
        );
        assert!(!app.is_running(), "nothing is sent until a pick is chosen");

        // `Esc` closes the picker without running anything.
        app.on_key(key(KeyCode::Esc));
        assert!(app.pick_prompt().is_none());
        assert!(!app.is_running());
    }

    #[test]
    fn pick_options_explain_every_refusal() {
        let dir = TempDir::new("app-pick-errors");
        dir.write("curlyfries.json", r#"{ "defaultEnvironment": "dev" }"#);
        dir.write(
            "environments/dev.json",
            r#"{
              "baseUrl": { "value": "http://127.0.0.1:1" },
              "classes": { "value": ["frigate", "sloop"] },
              "empty": { "value": [] },
              "scalar": { "value": 3 },
              "hidden": { "secret": true, "value": ["a"] }
            }"#,
        );
        dir.write(
            "requests/ping.json",
            r#"{ "method": "GET", "path": "/ping" }"#,
        );
        let project = project::discover(dir.path(), None).unwrap();
        let entries = project::collect_requests(&project).unwrap();
        let app = App::new(project, entries).unwrap();

        assert_eq!(
            app.pick_options("classes").unwrap(),
            vec![json!("frigate"), json!("sloop")]
        );
        assert_eq!(
            app.pick_options("empty").unwrap_err(),
            "array `empty` is empty"
        );
        assert_eq!(
            app.pick_options("scalar").unwrap_err(),
            "variable `scalar` is not an array"
        );
        assert_eq!(
            app.pick_options("hidden").unwrap_err(),
            "cannot pick from secret variable `hidden`"
        );
        assert_eq!(
            app.pick_options("missing").unwrap_err(),
            "no array `missing` for `${missing[]}`"
        );
    }

    /// Flattens the columns of [`resolved_lines`] into one list.
    fn resolved_text(
        def: &RequestDef,
        disabled: &DisabledSlots,
        request: &ResolvedRequest,
        theme: &Theme,
    ) -> Vec<String> {
        let (url, meta, body) = resolved_lines(def, disabled, request, theme);
        url.into_iter()
            .chain(meta)
            .chain(body)
            .map(|line| line.to_string())
            .collect()
    }

    #[test]
    fn resolved_lines_label_each_part() {
        let theme = Theme::plain();
        let def = request::validate(
            "ships/list",
            std::path::Path::new("requests/ships/list.json"),
            &json!({
                "method": "POST",
                "path": "/ships",
                "headers": { "X-Trace": "abc" },
                "query": { "limit": "2" }
            }),
        )
        .expect("test request is valid");
        let request = ResolvedRequest {
            id: "ships/list".to_string(),
            def_file: std::path::PathBuf::from("requests/ships/list.json"),
            name: "list ships".to_string(),
            method: "POST".to_string(),
            url: "http://127.0.0.1:4000/ships?limit=2".to_string(),
            headers: vec![
                ("X-Trace".to_string(), "abc".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            query: vec![("limit".to_string(), "2".to_string())],
            body: Some(r#"{"name":"x"}"#.to_string()),
        };
        let text = resolved_text(&def, &DisabledSlots::default(), &request, &theme);
        assert_eq!(text[0], "url");
        assert!(
            text[1].contains("POST http://127.0.0.1:4000/ships?limit=2"),
            "{text:?}"
        );
        assert!(text.contains(&"headers".to_string()), "{text:?}");
        // Every defined header is listed with its state; the default
        // `Content-Type` is shown too, without a file slot of its own.
        assert!(text.contains(&"[x] X-Trace: abc".to_string()), "{text:?}");
        assert!(
            text.contains(&"[x] Content-Type: application/json".to_string()),
            "{text:?}"
        );
        assert!(text.contains(&"query".to_string()), "{text:?}");
        assert!(text.contains(&"[x] limit=2".to_string()), "{text:?}");
        assert!(text.contains(&"body".to_string()), "{text:?}");

        // The pane keeps `url` full width, then `headers`/`query` beside `body`.
        let (url, meta, body) = resolved_lines(&def, &DisabledSlots::default(), &request, &theme);
        let url_text: Vec<String> = url.iter().map(|line| line.to_string()).collect();
        let meta_text: Vec<String> = meta.iter().map(|line| line.to_string()).collect();
        let body_text: Vec<String> = body.iter().map(|line| line.to_string()).collect();
        assert_eq!(url_text[0], "url");
        assert!(url_text[1].contains("POST http://127.0.0.1:4000/ships?limit=2"));
        assert!(!meta_text.contains(&"url".to_string()), "{meta_text:?}");
        assert_eq!(meta_text[0], "headers", "{meta_text:?}");
        assert!(meta_text.contains(&"query".to_string()), "{meta_text:?}");
        assert!(!meta_text.contains(&"body".to_string()), "{meta_text:?}");
        assert_eq!(body_text[0], "body", "{body_text:?}");
        assert!(
            body_text.iter().any(|line| line.contains("\"name\"")),
            "{body_text:?}"
        );

        // A switched-off header keeps its raw text and shows `[ ]`; the render
        // that produced `request` for the enabled case would have dropped it.
        let mut disabled = DisabledSlots::default();
        disabled.headers.insert("X-Trace".to_string());
        let stripped = ResolvedRequest {
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            ..request.clone()
        };
        let text = resolved_text(&def, &disabled, &stripped, &theme);
        assert!(text.contains(&"[ ] X-Trace: abc".to_string()), "{text:?}");
        assert!(
            text.contains(&"[x] Content-Type: application/json".to_string()),
            "{text:?}"
        );
        assert!(
            !text.iter().any(|line| line == "[x] X-Trace: abc"),
            "{text:?}"
        );

        // A request with no slots shows `(none)` under all three sections.
        let bare = request::validate(
            "x",
            std::path::Path::new("requests/x.json"),
            &json!({ "method": "GET", "path": "/x" }),
        )
        .expect("test request is valid");
        let bodyless = ResolvedRequest {
            headers: Vec::new(),
            query: Vec::new(),
            body: None,
            ..request
        };
        let text = resolved_text(&bare, &DisabledSlots::default(), &bodyless, &theme);
        assert_eq!(
            text.iter().filter(|line| *line == "(none)").count(),
            3,
            "{text:?}"
        );
    }
}
