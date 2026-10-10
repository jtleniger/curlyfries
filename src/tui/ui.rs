//! Rendering of the full-screen interface.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListState, Paragraph, Tabs, Widget, Wrap,
};

use crate::render::{self, StatusClass};
use crate::tui::app::{App, Focus, PendingClear, RequestView};
use crate::tui::theme::Theme;
use crate::tui::tree::TreeRow;

/// Spinner phases shown while a request is in flight.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Request-pane inner width below which the sections stack instead of splitting
/// into two columns too narrow to read.
const COLUMNS_MIN_WIDTH: u16 = 50;

/// Footer key legend.
const FOOTER: &str = "Tab focus · j/k move·scroll · Enter run · e env · t toggle · x clear · v view · [/] tab · / filter · ? help · q quit";

/// Help overlay contents.
const HELP: [&str; 15] = [
    "Tab / Shift-Tab   focus the tree, request or response pane",
    "↑ / k, ↓ / j      move the selection, or scroll the focused pane",
    "g / G, Home / End first / last request, or top / bottom of a pane",
    "PageUp/PageDown   scroll the focused pane by ten lines (Ctrl-u/d)",
    "Enter / r         run the selected request (picks array slots)",
    "e                 choose the environment",
    "x / X             clear captures (scope / every scope, y/n)",
    "/                 filter requests (Esc clears)",
    "[ / ]             previous / next response tab",
    "1 / 2 / 3         Body / Headers / Raw",
    "v                 resolved / definition request",
    "t                 enable/disable headers and query params",
    "?                 close this help",
    "q / Esc / Ctrl-C  quit",
    "NO_COLOR=1        run without colours",
];

/// Draws the whole interface into `frame`.
pub fn draw(app: &mut App, frame: &mut Frame) {
    let theme = app.theme;
    let area = frame.area();
    let [header, main, captures, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .areas(area);
    let [tree_area, right] =
        Layout::horizontal([Constraint::Length(40), Constraint::Min(20)]).areas(main);
    let [request_area, response_area] =
        Layout::vertical([Constraint::Percentage(42), Constraint::Min(5)]).areas(right);

    draw_header(app, frame, header, &theme);
    draw_tree(app, frame, tree_area, &theme);
    draw_request(app, frame, request_area, &theme);
    draw_response(app, frame, response_area, &theme);
    draw_captures(app, frame, captures, &theme);
    draw_footer(app, frame, footer, &theme);

    if app.help {
        draw_help(frame, area, &theme);
    }
    if app.env_picker.is_some() {
        draw_env_picker(app, frame, area, &theme);
    }
    if app.pick_prompt().is_some() {
        draw_pick_modal(app, frame, area, &theme);
    }
    if app.toggle_modal.is_some() {
        draw_toggle_modal(app, frame, area, &theme);
    }
    if app.confirm.is_some() {
        draw_confirm(app, frame, area, &theme);
    }
}

/// One-line header: project, environment and any message.
fn draw_header(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let text = format!(
        "curlyfries · {} · env: {}",
        app.project.root.display(),
        app.env_name.as_deref().unwrap_or("(none)"),
    );
    match app.status_message.as_deref() {
        None => {
            frame.render_widget(
                Paragraph::new(Line::styled(text, Style::default().fg(theme.title))),
                area,
            );
        }
        Some(message) => {
            let color = if message.starts_with("error:") {
                theme.method_delete
            } else {
                theme.dim
            };
            let width = u16::try_from(message.chars().count())
                .unwrap_or(u16::MAX)
                .min(area.width);
            let [left, right] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(area);
            frame.render_widget(
                Paragraph::new(Line::styled(text, Style::default().fg(theme.title))),
                left,
            );
            frame.render_widget(
                Paragraph::new(Line::styled(
                    message.to_string(),
                    Style::default().fg(color),
                )),
                right,
            );
        }
    }
}

/// Request tree, with the selected request reversed.
fn draw_tree(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let title = if app.filter_active || !app.filter.is_empty() {
        format!("Requests  /{}", app.filter)
    } else {
        "Requests".to_string()
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.focus == Focus::Tree {
            theme.accent
        } else {
            theme.dim
        }))
        .title(title);

    if app.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "no requests match",
                Style::default().fg(theme.dim),
            ))
            .block(block)
            .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.rows.len());
    let mut selected = None;
    for (position, row) in app.rows.iter().enumerate() {
        let indent = "  ".repeat(match row {
            TreeRow::Folder { depth, .. } | TreeRow::Request { depth, .. } => *depth,
        });
        match row {
            TreeRow::Folder { label, .. } => lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(label.clone(), Style::default().fg(theme.title)),
            ])),
            TreeRow::Request { index, .. } => {
                let label = app
                    .labels
                    .get(*index)
                    .cloned()
                    .unwrap_or_else(|| format!("request {index}"));
                let (method, rest) = match label.split_once(' ') {
                    Some((method, rest)) => (method.to_string(), rest.to_string()),
                    None => (String::new(), label),
                };
                if app.selected_id.as_deref() == app.entries.get(*index).map(|e| e.id.as_str()) {
                    selected = Some(position);
                }
                let color = theme.method(&method);
                lines.push(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(
                        method,
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(" {rest}")),
                ]));
            }
        }
    }

    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(
        List::new(lines)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}

/// Request pane: resolved or raw, toggled with `v`.
///
/// The resolved view keeps the `url` on its own full-width rows and lays
/// `headers`/`query` beside the `body` below it, so a tall body uses the pane's
/// height instead of pushing the metadata off the bottom. Under
/// [`COLUMNS_MIN_WIDTH`] the sections stack into a single column instead. The
/// definition view is one full-width JSON block. Every layout scrolls.
fn draw_request(app: &mut App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let label = match app.request_view {
        RequestView::Resolved => "resolved",
        RequestView::Definition => "definition",
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.focus == Focus::Request {
            theme.accent
        } else {
            theme.dim
        }))
        .title(format!("Request — {label} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let url = app.request_url().to_vec();
    let meta = app.request_meta().to_vec();
    let body = app.request_body().to_vec();

    // The definition view, and a resolved request that failed to render, have
    // metadata on one side only: stack whatever there is.
    if url.is_empty() && meta.is_empty() {
        draw_request_column(app, frame, inner, body);
        return;
    }
    if body.is_empty() {
        let mut lines = url;
        lines.extend(meta);
        draw_request_column(app, frame, inner, lines);
        return;
    }
    if inner.width < COLUMNS_MIN_WIDTH {
        let mut lines = url;
        lines.extend(meta);
        lines.extend(body);
        draw_request_column(app, frame, inner, lines);
        return;
    }

    // `url` spans the pane — it is the longest piece and one that must not wrap
    // mid-token — with `headers`/`query` and `body` side by side below it.
    let url_height = wrapped_rows(&url, inner.width).min(inner.height.saturating_sub(1));
    let [url_area, columns] =
        Layout::vertical([Constraint::Length(url_height), Constraint::Min(0)]).areas(inner);
    frame.render_widget(Paragraph::new(url).wrap(Wrap { trim: false }), url_area);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(columns);
    // The divider claims the left column's last cell.
    let left_width = left.width.saturating_sub(1);
    let maximum = wrapped_rows(&meta, left_width)
        .max(wrapped_rows(&body, right.width))
        .saturating_sub(columns.height);
    let scroll = app.request_scroll.min(maximum);
    app.request_scroll = scroll;
    frame.render_widget(
        Paragraph::new(meta)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(theme.dim)),
            ),
        left,
    );
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        right,
    );
}

/// One wrapped, scrollable column filling `area`, clamped to the shared request
/// scroll.
fn draw_request_column(app: &mut App, frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let maximum = wrapped_rows(&lines, area.width).saturating_sub(area.height);
    let scroll = app.request_scroll.min(maximum);
    app.request_scroll = scroll;
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

/// Response pane: status line, tab bar and the selected view.
fn draw_response(app: &mut App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.focus == Focus::Response {
            theme.accent
        } else {
            theme.dim
        }))
        .title(format!("Response — {}", app.response_url()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve a bordered section for a failure, sized to the wrapped message.
    let error_height = app
        .error()
        .map(|message| {
            let text_width = inner.width.saturating_sub(2).max(1);
            let needed = wrapped_height(message, text_width) + 2;
            needed.min(inner.height.saturating_sub(2))
        })
        .unwrap_or(0);

    let [status_area, tabs_area, error_area, body_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(error_height),
        Constraint::Min(1),
    ])
    .areas(inner);

    if let Some((class, line)) = app.status() {
        let color = match class {
            StatusClass::Success => theme.method_get,
            StatusClass::ClientError => theme.method_post,
            StatusClass::ServerError => theme.method_delete,
            StatusClass::Redirect | StatusClass::Other => theme.dim,
        };
        frame.render_widget(
            Paragraph::new(Line::styled(line.to_string(), Style::default().fg(color))),
            status_area,
        );
    }

    frame.render_widget(
        Tabs::new(["1 Body", "2 Headers", "3 Raw"]).select(app.response_tab as usize),
        tabs_area,
    );

    if let Some(message) = app.error() {
        let title = if app.status().is_some() {
            "Output error"
        } else {
            "Error"
        };
        frame.render_widget(
            Paragraph::new(message.to_string())
                .style(Style::default().fg(theme.method_delete))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(theme.method_delete))
                        .title(title),
                ),
            error_area,
        );
    }

    if !app.has_run() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "press Enter to run",
                Style::default().fg(theme.dim),
            ))
            .alignment(Alignment::Center),
            body_area,
        );
        return;
    }

    // Scroll is in wrapped rows, so bound it by the wrapped height, not the
    // logical line count.
    let maximum =
        wrapped_rows(app.response_body(), body_area.width).saturating_sub(body_area.height);
    let scroll = app.response_scroll.min(maximum);
    app.response_scroll = scroll;
    frame.render_widget(
        Paragraph::new(app.response_body().to_vec())
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        body_area,
    );
}

/// Terminal rows `text` needs when word-wrapped to `width` columns.
///
/// Renders into a throwaway [`Buffer`] so the count matches what the pane will
/// actually draw, including ratatui's word-boundary wrapping.
fn wrapped_height(text: &str, width: u16) -> u16 {
    // No wrapping can need more rows than one per character plus one per newline.
    let limit = (2 * text.chars().count() + 2).clamp(1, 2048) as u16;
    let area = Rect::new(0, 0, width.max(1), limit);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(text.to_string())
        .wrap(Wrap { trim: false })
        .render(area, &mut buffer);
    let stride = area.width as usize;
    let mut rows = 1;
    for row in 0..area.height {
        let start = row as usize * stride;
        if buffer.content[start..start + stride]
            .iter()
            .any(|cell| cell.symbol() != " ")
        {
            rows = row + 1;
        }
    }
    rows
}

/// Terminal rows `lines` need when word-wrapped to `width` columns.
///
/// Each [`Line`] is wrapped independently — the same as a paragraph rendering
/// them as `\n`-separated rows — so summing their heights matches the pane.
fn wrapped_rows(lines: &[Line<'static>], width: u16) -> u16 {
    lines.iter().fold(0u16, |rows, line| {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        rows.saturating_add(wrapped_height(&text, width))
    })
}

/// Captures strip for the active scope.
fn draw_captures(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim))
        .title(format!("Captures — {}", app.scope()));
    let captured = app.captured();
    let secrets = app.captured_secrets();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if captured.is_empty() {
        lines.push(Line::styled(
            "no captured variables",
            Style::default().fg(theme.dim),
        ));
    } else {
        for (name, value) in captured.iter().take(3) {
            lines.push(Line::from(format!(
                "{name} = {}",
                render::captured_value(value, secrets.contains(name))
            )));
        }
        if captured.len() > 3 {
            lines.push(Line::styled(
                format!("… (+{} more)", captured.len() - 3),
                Style::default().fg(theme.dim),
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// One-line key legend.
fn draw_footer(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let text = if app.is_running() {
        let phase = SPINNER[(app.tick as usize) % SPINNER.len()];
        format!("{phase} running…  {FOOTER}")
    } else {
        FOOTER.to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::styled(text, Style::default().fg(theme.dim))),
        area,
    );
}

/// Centred help overlay listing every binding.
fn draw_help(frame: &mut Frame, area: Rect, theme: &Theme) {
    let popup = centered(60, 70, area);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title("Help");
    let lines: Vec<Line<'static>> = HELP
        .iter()
        .map(|row| Line::styled((*row).to_string(), Style::default().fg(theme.dim)))
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// Centred environment picker.
fn draw_env_picker(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let popup = centered(40, 50, area);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title("Environment");
    let items: Vec<Line<'static>> = app.env_choices().into_iter().map(Line::from).collect();
    let mut state = ListState::default().with_selected(app.env_picker);
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        popup,
        &mut state,
    );
}

/// Centred picker over one array variable's elements.
fn draw_pick_modal(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let Some((title, options, selected)) = app.pick_prompt() else {
        return;
    };
    let popup = centered(40, 50, area);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(title);
    let items: Vec<Line<'static>> = options.into_iter().map(Line::from).collect();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        popup,
        &mut state,
    );
}

/// Centred enable/disable modal for a request's headers and query parameters.
fn draw_toggle_modal(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let Some((id, rows, selected)) = app.toggle_prompt() else {
        return;
    };
    let popup = centered(50, 60, area);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(format!("Toggle — {id}"));
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "no headers or query parameters",
                Style::default().fg(theme.dim),
            ))
            .block(block),
            popup,
        );
        return;
    }
    let items: Vec<Line<'static>> = rows
        .into_iter()
        .map(|(enabled, label)| {
            Line::from(format!("[{}] {label}", if enabled { 'x' } else { ' ' }))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        popup,
        &mut state,
    );
}

/// Centred capture-clear confirmation.
fn draw_confirm(app: &App, frame: &mut Frame, area: Rect, theme: &Theme) {
    let popup = centered(50, 30, area);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title("Clear captures");
    let message = match app.confirm {
        Some(PendingClear::All) => {
            "delete every captured variable in every scope?  y / n".to_string()
        }
        _ => format!(
            "delete every captured variable in scope `{}`?  y / n",
            app.scope()
        ),
    };
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .block(block),
        popup,
    );
}

/// A rectangle `percent_x` × `percent_y` of `area`, centred.
fn centered(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, middle, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);
    let [_, popup, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(middle);
    popup
}
