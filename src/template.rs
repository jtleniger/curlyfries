//! The `${...}` templating engine.
//!
//! A template is scanned left to right. `${name}` is a placeholder, `$${` emits
//! the literal text `${` (consuming all three characters), and any other `$` is
//! a literal `$`. A lone `}` is a literal `}`.
//!
//! Two renderings exist. [`Template::render_native`] keeps JSON typing for a
//! *whole placeholder* (a string whose trimmed form is exactly one placeholder
//! and nothing else) and falls back to [`Template::render_string`] otherwise.
//! String rendering uses the *string form* of a value: strings as themselves,
//! numbers and booleans as JSON literals, arrays and objects as compact JSON and
//! `null` as an error, so a null can never silently become an empty URL segment,
//! header or key.

use serde_json::Value;

use crate::error::TemplateProblem;

/// Lookup used to resolve variable names while rendering.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<Value>;

/// One piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Literal text emitted as written.
    Literal(String),
    /// A `${name}` placeholder.
    Var(String),
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    segments: Vec<Segment>,
    whole: bool,
}

/// Failure while rendering a parsed template.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// No scope provided the variable.
    #[error("undefined variable `{0}`")]
    MissingVar(String),
    /// A `null` value was used where a string is required.
    #[error("variable `{0}` is null")]
    NullInStringSlot(String),
    /// A templated object key or header name rendered to the empty string.
    #[error("`{0}` templated to an empty string")]
    EmptyKey(String),
    /// The template did not parse; loading validates this up front.
    #[error("{0}")]
    Syntax(TemplateProblem),
}

/// Parses `input`, returning the segments and whether it is a whole placeholder.
pub fn parse(input: &str) -> Result<Template, TemplateProblem> {
    let chars: Vec<char> = input.chars().collect();
    let mut segments: Vec<Segment> = Vec::new();
    let mut literal = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let is_escape = i + 2 < chars.len() && chars[i + 1] == '$' && chars[i + 2] == '{';
            if is_escape {
                literal.push_str("${");
                i += 3;
                continue;
            }
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                let start = i + 2;
                let mut end = start;
                while end < chars.len() && chars[end] != '}' {
                    end += 1;
                }
                if end >= chars.len() {
                    return Err(TemplateProblem::Unterminated);
                }
                let raw: String = chars[start..end].iter().collect();
                let name = raw.trim().to_string();
                if name.is_empty() {
                    return Err(TemplateProblem::EmptyName);
                }
                if !is_valid_name(&name) {
                    return Err(TemplateProblem::InvalidName);
                }
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(Segment::Var(name));
                i = end + 1;
                continue;
            }
        }
        literal.push(chars[i]);
        i += 1;
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }

    let trimmed = input.trim();
    let whole = if trimmed.len() == input.len() {
        segments.len() == 1 && matches!(segments.first(), Some(Segment::Var(_)))
    } else if trimmed.is_empty() {
        false
    } else {
        // Trimming can only remove literal whitespace.
        parse(trimmed).map(|t| t.whole).unwrap_or(false)
    };
    if whole {
        let name = segments
            .iter()
            .find_map(|s| match s {
                Segment::Var(name) => Some(name.clone()),
                Segment::Literal(_) => None,
            })
            .unwrap_or_else(|| unreachable!("whole templates hold exactly one variable"));
        segments = vec![Segment::Var(name)];
    }

    Ok(Template { segments, whole })
}

/// Whether `name` matches `[A-Za-z_][A-Za-z0-9_.-]*`.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

impl Template {
    /// The parsed segments.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Whether the template is exactly one placeholder (after trimming).
    pub fn is_whole(&self) -> bool {
        self.whole
    }

    /// Whether the template contains no placeholder at all.
    pub fn is_literal(&self) -> bool {
        !self.segments.iter().any(|s| matches!(s, Segment::Var(_)))
    }

    /// Renders with JSON typing: a whole placeholder yields its value untouched,
    /// anything else yields an interpolated string.
    pub fn render_native(&self, lookup: Lookup<'_>) -> Result<Value, RenderError> {
        if self.whole {
            let Segment::Var(name) = &self.segments[0] else {
                unreachable!("whole templates hold exactly one variable")
            };
            return lookup(name).ok_or_else(|| RenderError::MissingVar(name.clone()));
        }
        Ok(Value::String(self.render_string(lookup)?))
    }

    /// Renders every segment using the string form of each value.
    pub fn render_string(&self, lookup: Lookup<'_>) -> Result<String, RenderError> {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Var(name) => match lookup(name) {
                    Some(value) => out.push_str(&string_form(name, &value)?),
                    None => return Err(RenderError::MissingVar(name.clone())),
                },
            }
        }
        Ok(out)
    }
}

/// The string form of a value (see the module docs).
pub fn string_form(name: &str, value: &Value) -> Result<String, RenderError> {
    Ok(match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).expect("JSON values always serialize")
        }
        Value::Null => return Err(RenderError::NullInStringSlot(name.to_string())),
    })
}

/// Renders a whole JSON value, templating strings, object keys and arrays.
pub fn render_tree(value: &Value, lookup: Lookup<'_>) -> Result<Value, RenderError> {
    Ok(match value {
        Value::String(text) => parse(text)
            .map_err(RenderError::Syntax)?
            .render_native(lookup)?,
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                let key = render_key(key, lookup)?;
                out.insert(key, render_tree(child, lookup)?);
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(render_tree(item, lookup)?);
            }
            Value::Array(out)
        }
        other => other.clone(),
    })
}

/// Renders an object key or header name, rejecting a result that is empty.
pub fn render_key(key: &str, lookup: Lookup<'_>) -> Result<String, RenderError> {
    let rendered = parse(key)
        .map_err(RenderError::Syntax)?
        .render_string(lookup)?;
    if rendered.is_empty() {
        return Err(RenderError::EmptyKey(key.to_string()));
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(pairs: &[(&str, Value)]) -> impl Fn(&str) -> Option<Value> + use<> {
        let pairs: Vec<(String, Value)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    fn render(input: &str, lookup: Lookup<'_>) -> Result<Value, RenderError> {
        parse(input).expect("parses").render_native(lookup)
    }

    #[test]
    fn plain_text_is_returned_verbatim() {
        let t = parse("plain text").unwrap();
        assert!(t.is_literal());
        assert_eq!(t.segments(), &[Segment::Literal("plain text".into())]);
        assert_eq!(
            render("plain text", &vars(&[])).unwrap(),
            json!("plain text")
        );
    }

    #[test]
    fn whole_placeholder_uses_native_typing() {
        for (input, value) in [
            ("${x}", json!([1, 2])),
            (" ${x} ", json!(true)),
            ("${x}", json!(null)),
            ("${x}", json!({ "a": 1 })),
            ("${ x\t}", json!(3.5)),
        ] {
            let t = parse(input).unwrap();
            assert!(t.is_whole(), "{input}");
            assert_eq!(
                t.render_native(&vars(&[("x", value.clone())])).unwrap(),
                value
            );
        }
    }

    #[test]
    fn whole_placeholder_in_string_slots_uses_string_form() {
        let t = parse(" ${x} ").unwrap();
        assert!(t.is_whole());
        assert_eq!(t.render_string(&vars(&[("x", json!(7))])).unwrap(), "7");
        assert_eq!(
            t.render_string(&vars(&[("x", json!(["a", 1]))])).unwrap(),
            "[\"a\",1]"
        );
        assert_eq!(
            t.render_string(&vars(&[("x", json!({"k": true}))]))
                .unwrap(),
            "{\"k\":true}"
        );
        assert_eq!(t.render_string(&vars(&[("x", json!("v"))])).unwrap(), "v");
        assert!(matches!(
            t.render_string(&vars(&[("x", Value::Null)])),
            Err(RenderError::NullInStringSlot(name)) if name == "x"
        ));
    }

    #[test]
    fn escapes_and_lone_dollars() {
        let empty = &vars(&[]);
        assert_eq!(
            parse("$$${x}").unwrap().render_string(empty).unwrap(),
            "$${x}"
        );
        assert_eq!(
            parse("$${x}").unwrap().render_string(empty).unwrap(),
            "${x}"
        );
        for (input, expected) in [("$$", "$$"), ("$", "$"), ("$5", "$5"), ("$$x", "$$x")] {
            let t = parse(input).unwrap();
            assert!(t.is_literal(), "{input}");
            assert_eq!(t.render_string(empty).unwrap(), expected);
        }
        let resolved = vars(&[("x", json!(1))]);
        assert_eq!(
            parse("${x}}").unwrap().render_string(&resolved).unwrap(),
            "1}"
        );
        assert_eq!(
            parse("a${x}b").unwrap().render_string(&resolved).unwrap(),
            "a1b"
        );
        assert!(!parse("a${x}").unwrap().is_whole());
        assert!(!parse("${x}${x}").unwrap().is_whole());
    }

    #[test]
    fn parse_errors() {
        assert_eq!(parse("${x").unwrap_err(), TemplateProblem::Unterminated);
        assert_eq!(parse("a${").unwrap_err(), TemplateProblem::Unterminated);
        assert_eq!(parse("${}").unwrap_err(), TemplateProblem::EmptyName);
        assert_eq!(parse("${ }").unwrap_err(), TemplateProblem::EmptyName);
        assert_eq!(parse("${1x}").unwrap_err(), TemplateProblem::InvalidName);
        assert_eq!(parse("${a b}").unwrap_err(), TemplateProblem::InvalidName);
        assert_eq!(parse("${a/b}").unwrap_err(), TemplateProblem::InvalidName);
        assert_eq!(parse("${a-b_c.d1}").unwrap().segments().len(), 1);
    }

    #[test]
    fn interpolation_uses_string_forms() {
        let lookup = vars(&[
            ("s", json!("str")),
            ("n", json!(42)),
            ("b", json!(false)),
            ("o", json!({ "k": [1] })),
        ]);
        assert_eq!(
            parse("${s}-${n}-${b}-${o}")
                .unwrap()
                .render_string(&lookup)
                .unwrap(),
            "str-42-false-{\"k\":[1]}"
        );
        let missing = parse("x${nope}")
            .unwrap()
            .render_native(&lookup)
            .unwrap_err();
        assert_eq!(missing, RenderError::MissingVar("nope".to_string()));
    }

    #[test]
    fn render_tree_walks_containers_and_keys() {
        let value = json!({
            "name": "${shipName}",
            "nested": { "count": "${limit}", "flags": [true, null, "${shipName}"] },
            "typed": "${limit}"
        });
        let lookup = vars(&[("shipName", json!("Curlyfries")), ("limit", json!(7))]);
        let rendered = render_tree(&value, &lookup).unwrap();
        assert_eq!(rendered["name"], json!("Curlyfries"));
        assert_eq!(rendered["nested"]["count"], json!(7));
        assert_eq!(
            rendered["nested"]["flags"],
            json!([true, null, "Curlyfries"])
        );
        assert_eq!(rendered["typed"], json!(7));
    }

    #[test]
    fn render_tree_templates_object_keys() {
        let value = json!({ "${id}": "v", "plain": 1 });
        let rendered = render_tree(&value, &vars(&[("id", json!(7))])).unwrap();
        assert_eq!(rendered, json!({ "7": "v", "plain": 1 }));
    }

    #[test]
    fn empty_rendered_key_is_an_error() {
        let value = json!({ "${empty}": 1 });
        let lookup = vars(&[("empty", json!(""))]);
        assert!(matches!(
            render_tree(&value, &lookup),
            Err(RenderError::EmptyKey(key)) if key == "${empty}"
        ));
        assert!(matches!(
            render_key("", &lookup),
            Err(RenderError::EmptyKey(key)) if key.is_empty()
        ));
        assert_eq!(render_key("X-Trace", &lookup).unwrap(), "X-Trace");
    }
}
