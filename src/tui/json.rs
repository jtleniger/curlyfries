//! Syntax colouring for JSON bodies.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::theme::Theme;

/// Splits a JSON document into one colored `Line` per line.
///
/// `text` is pretty-printed JSON; on parse failure the raw lines are returned
/// unstyled.
pub fn json_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return text
            .lines()
            .map(|line| Line::from(line.to_string()))
            .collect();
    };
    let pretty = serde_json::to_string_pretty(&value).expect("JSON values always serialize");
    pretty
        .lines()
        .map(|line| styled_line(line, theme))
        .collect()
}

/// One line of pretty JSON, tokenized into styled spans.
fn styled_line(line: &str, theme: &Theme) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == '"' {
            let start = index;
            index += 1;
            while index < chars.len() {
                match chars[index] {
                    '\\' => index += 2,
                    '"' => {
                        index += 1;
                        break;
                    }
                    _ => index += 1,
                }
            }
            let end = index.min(chars.len());
            let token: String = chars[start..end].iter().collect();
            let is_key = chars[end..].iter().find(|c| !c.is_whitespace()) == Some(&':');
            let color = if is_key {
                theme.json_key
            } else {
                theme.json_string
            };
            push_plain(&mut plain, &mut spans);
            spans.push(Span::styled(token, Style::default().fg(color)));
        } else if current == '-' || current.is_ascii_digit() {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_digit() || "+-.eE".contains(chars[index]))
            {
                index += 1;
            }
            let token: String = chars[start..index].iter().collect();
            push_plain(&mut plain, &mut spans);
            spans.push(Span::styled(token, Style::default().fg(theme.json_number)));
        } else if current.is_ascii_alphabetic() {
            let start = index;
            while index < chars.len() && chars[index].is_ascii_alphabetic() {
                index += 1;
            }
            let token: String = chars[start..index].iter().collect();
            if matches!(token.as_str(), "true" | "false" | "null") {
                push_plain(&mut plain, &mut spans);
                spans.push(Span::styled(token, Style::default().fg(theme.json_literal)));
            } else {
                plain.push_str(&token);
            }
        } else {
            plain.push(current);
            index += 1;
        }
    }
    push_plain(&mut plain, &mut spans);
    Line::from(spans)
}

/// Moves the buffered punctuation/whitespace into `spans`.
fn push_plain(plain: &mut String, spans: &mut Vec<Span<'static>>) {
    if !plain.is_empty() {
        spans.push(Span::raw(std::mem::take(plain)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn theme() -> Theme {
        Theme {
            json_key: Color::Cyan,
            json_string: Color::Green,
            json_number: Color::Yellow,
            json_literal: Color::Magenta,
            ..Theme::plain()
        }
    }

    fn fg(line: &Line<'_>, needle: &str) -> Option<Color> {
        line.spans
            .iter()
            .find(|span| span.content == needle)
            .and_then(|span| span.style.fg)
    }

    #[test]
    fn keys_strings_numbers_and_literals_are_colored() {
        let lines = json_lines(
            r#"{"name":"silver","ships":3,"nested":{"ok":true},"none":null}"#,
            &theme(),
        );
        let all = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| (span.content.to_string(), span.style.fg))
            .collect::<Vec<_>>();
        assert!(
            all.contains(&("\"name\"".to_string(), Some(Color::Cyan))),
            "{all:?}"
        );
        assert!(
            all.contains(&("\"silver\"".to_string(), Some(Color::Green))),
            "{all:?}"
        );
        assert!(
            all.contains(&("3".to_string(), Some(Color::Yellow))),
            "{all:?}"
        );
        assert!(
            all.contains(&("true".to_string(), Some(Color::Magenta))),
            "{all:?}"
        );
        assert!(
            all.contains(&("null".to_string(), Some(Color::Magenta))),
            "{all:?}"
        );
        assert!(
            all.contains(&("\"ok\"".to_string(), Some(Color::Cyan))),
            "{all:?}"
        );
    }

    #[test]
    fn nested_string_values_are_colored_as_strings() {
        let lines = json_lines(r#"{"inner":{"token":"tok-123"}}"#, &theme());
        let token = lines
            .iter()
            .find(|line| line.spans.iter().any(|s| s.content == "\"tok-123\""))
            .expect("value line is present");
        assert_eq!(fg(token, "\"tok-123\""), Some(Color::Green));
    }

    #[test]
    fn non_json_input_renders_verbatim() {
        let text = "not json\nat all";
        let lines = json_lines(text, &theme());
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect()
            })
            .collect();
        assert_eq!(rendered, vec!["not json", "at all"]);
        assert!(lines.iter().all(|line| line.style.fg.is_none()));
        assert!(
            lines
                .iter()
                .all(|line| line.spans.iter().all(|span| span.style.fg.is_none()))
        );
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(json_lines("", &theme()).is_empty());
    }
}
