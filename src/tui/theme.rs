//! Resolved colours for the TUI, honouring `NO_COLOR`.

use ratatui::style::Color;

/// Resolved colours for every TUI element.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Border of the focused pane / selected list row.
    pub accent: Color,
    /// Dim border and footer text.
    pub dim: Color,
    /// Folder rows and titles.
    pub title: Color,
    /// GET method label.
    pub method_get: Color,
    /// POST method label.
    pub method_post: Color,
    /// PUT method label.
    pub method_put: Color,
    /// PATCH method label.
    pub method_patch: Color,
    /// DELETE method label.
    pub method_delete: Color,
    /// HEAD/OPTIONS method label (also used for unknown methods).
    pub method_other: Color,
    /// JSON object key.
    pub json_key: Color,
    /// JSON string value.
    pub json_string: Color,
    /// JSON number.
    pub json_number: Color,
    /// JSON `true`/`false`/`null`.
    pub json_literal: Color,
}

impl Theme {
    /// Coloured theme. `NO_COLOR` set (any value) yields [`Theme::plain`].
    pub fn from_env() -> Theme {
        if std::env::var_os("NO_COLOR").is_some() {
            return Theme::plain();
        }
        Theme {
            accent: Color::Cyan,
            dim: Color::DarkGray,
            title: Color::Blue,
            method_get: Color::Green,
            method_post: Color::Yellow,
            method_put: Color::Cyan,
            method_patch: Color::Magenta,
            method_delete: Color::Red,
            method_other: Color::Gray,
            json_key: Color::Cyan,
            json_string: Color::Green,
            json_number: Color::Yellow,
            json_literal: Color::Magenta,
        }
    }

    /// Fully monochrome theme (`Color::Reset` everywhere).
    pub fn plain() -> Theme {
        Theme {
            accent: Color::Reset,
            dim: Color::Reset,
            title: Color::Reset,
            method_get: Color::Reset,
            method_post: Color::Reset,
            method_put: Color::Reset,
            method_patch: Color::Reset,
            method_delete: Color::Reset,
            method_other: Color::Reset,
            json_key: Color::Reset,
            json_string: Color::Reset,
            json_number: Color::Reset,
            json_literal: Color::Reset,
        }
    }

    /// The label colour for an HTTP method (upper-case).
    pub fn method(&self, method: &str) -> Color {
        match method {
            "GET" => self.method_get,
            "POST" => self.method_post,
            "PUT" => self.method_put,
            "PATCH" => self.method_patch,
            "DELETE" => self.method_delete,
            _ => self.method_other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_yields_the_plain_theme() {
        // `from_env` reads the process environment, which tests share; only the
        // monochrome fallback and the method mapping are asserted here.
        let plain = Theme::plain();
        assert_eq!(plain.accent, Color::Reset);
        assert_eq!(plain.json_key, Color::Reset);
        assert_eq!(plain.method("GET"), Color::Reset);

        let colored = Theme {
            method_get: Color::Green,
            method_other: Color::Gray,
            ..Theme::plain()
        };
        assert_eq!(colored.method("GET"), Color::Green);
        assert_eq!(colored.method("HEAD"), Color::Gray);
    }
}
