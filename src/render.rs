//! Request labels and captured-value formatting for the terminal UI.

use std::borrow::Cow;

use serde_json::Value;

use crate::request::RequestDef;
use crate::variables::SECRET_MASK;

/// How long a captured string may be before it is truncated.
const CAPTURE_LIMIT: usize = 60;

/// Removes control characters a terminal would interpret as escape sequences,
/// keeping `\n` and `\t`. Borrows the input when there is nothing to strip.
pub fn sanitize_for_terminal(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_control) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().filter(|c| !is_control(*c)).collect())
}

/// C0 controls other than `\n` (0x0a) and `\t` (0x09), DEL, and the C1 range.
fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}

/// The list label of a request: `METHOD name`. The tree already nests requests
/// under their folders, so the id's path is dropped; an unnamed request (whose
/// name defaults to the full id) falls back to its last id segment.
pub fn request_label(def: &RequestDef) -> String {
    let name = if def.name == def.id {
        def.id.rsplit('/').next().unwrap_or(def.id.as_str())
    } else {
        def.name.as_str()
    };
    let label = format!("{} {}", def.method, name);
    sanitize_for_terminal(&label).into_owned()
}

/// Compact JSON for a value.
pub fn compact(value: &Value) -> String {
    serde_json::to_string(value).expect("JSON values always serialize")
}

/// A pick option as printed in the picker: strings bare, anything else compact JSON.
pub fn pick_label(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => compact(other),
    }
}

/// A captured value as printed: the mask when secret, else compact JSON with
/// strings truncated.
pub fn captured_value(value: &Value, secret: bool) -> String {
    if secret {
        return SECRET_MASK.to_string();
    }
    match value {
        Value::String(text) => {
            let mut chars = text.chars();
            let short: String = chars.by_ref().take(CAPTURE_LIMIT).collect();
            if chars.next().is_some() {
                compact(&Value::String(format!("{short}…")))
            } else {
                compact(value)
            }
        }
        other => compact(other),
    }
}

/// Broad class of an HTTP status, for callers that pick their own colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    /// 2xx.
    Success,
    /// 3xx.
    Redirect,
    /// 4xx.
    ClientError,
    /// 5xx.
    ServerError,
    /// Anything else (1xx, 0, or out of range).
    Other,
}

/// Classifies `status` by its hundreds digit.
pub fn status_class(status: u16) -> StatusClass {
    match status / 100 {
        2 => StatusClass::Success,
        3 => StatusClass::Redirect,
        4 => StatusClass::ClientError,
        5 => StatusClass::ServerError,
        _ => StatusClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn sanitize_for_terminal_strips_controls_and_borrows_clean_text() {
        assert_eq!(sanitize_for_terminal("\u{1b}]52;c;x\u{7}"), "]52;c;x");
        assert_eq!(sanitize_for_terminal("a\nb\tc\rd"), "a\nb\tcd");
        assert!(matches!(
            sanitize_for_terminal("plain text"),
            Cow::Borrowed("plain text")
        ));
    }

    #[test]
    fn request_labels_drop_the_folder_path() {
        let named = request::validate(
            "ships/create",
            Path::new("requests/ships/create.json"),
            &json!({ "method": "post", "path": "/ships", "name": "Create ship" }),
        )
        .unwrap();
        assert_eq!(request_label(&named), "POST Create ship");
        let unnamed = request::validate(
            "ships/list",
            Path::new("requests/ships/list.json"),
            &json!({ "method": "GET", "path": "/ships" }),
        )
        .unwrap();
        assert_eq!(request_label(&unnamed), "GET list");
        // A request at the tree root keeps its only segment.
        let root = request::validate(
            "health",
            Path::new("requests/health.json"),
            &json!({ "method": "GET", "path": "/health" }),
        )
        .unwrap();
        assert_eq!(request_label(&root), "GET health");
    }

    #[test]
    fn captured_values_are_compact_json() {
        assert_eq!(captured_value(&json!({ "a": 1 }), false), "{\"a\":1}");
        assert_eq!(captured_value(&json!(7), false), "7");
        // Exactly at the limit stays untouched; one more character truncates.
        let at_limit: String = "e".repeat(CAPTURE_LIMIT);
        assert_eq!(
            captured_value(&json!(at_limit.clone()), false),
            compact(&json!(at_limit))
        );
        let over: String = "e".repeat(CAPTURE_LIMIT + 1);
        let mut expected = "e".repeat(CAPTURE_LIMIT);
        expected.push('…');
        assert_eq!(
            captured_value(&json!(over), false),
            compact(&Value::String(expected))
        );
    }

    #[test]
    fn pick_labels_print_strings_bare_and_other_types_as_json() {
        assert_eq!(pick_label(&json!("frigate")), "frigate");
        assert_eq!(pick_label(&json!(7)), "7");
        assert_eq!(pick_label(&json!(true)), "true");
        assert_eq!(pick_label(&json!(null)), "null");
        assert_eq!(pick_label(&json!(["a", 1])), "[\"a\",1]");
        assert_eq!(pick_label(&json!({ "a": 1 })), "{\"a\":1}");
    }

    #[test]
    fn a_secret_value_is_masked_whatever_its_type() {
        assert_eq!(captured_value(&json!("s3cret"), true), SECRET_MASK);
        assert_eq!(captured_value(&json!({ "a": 1 }), true), SECRET_MASK);
        assert_eq!(captured_value(&json!(null), true), SECRET_MASK);
    }
}
