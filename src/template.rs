//! The `${...}` templating engine.
//!
//! A template is scanned left to right. `${name}` is a placeholder, `$${` emits
//! the literal text `${` (consuming all three characters), and any other `$` is
//! a literal `$`. A lone `}` is a literal `}`.
//!
//! `${name[]}` is a *pick* token: it names an array variable whose chosen
//! element is substituted just before the request is sent. It resolves through
//! the lookup alone — never the `random.` built-ins — under the key `name[]`.
//!
//! `${name?}` and `${name?json}` are *prompt* tokens: the user types a value for
//! the label `name` before the request is sent. A text prompt's value is the
//! entered text as a JSON string, looked up under `name?`; a JSON prompt's value
//! is the entered text parsed as JSON, looked up under `name?json`. The parse
//! happens when the user submits the modal, so the render path only ever reads a
//! value that is already typed.
//!
//! Two renderings exist. [`Template::render_native`] keeps JSON typing for a
//! *whole placeholder* (a string whose trimmed form is exactly one placeholder
//! and nothing else) and falls back to [`Template::render_string`] otherwise.
//! String rendering uses the *string form* of a value: strings as themselves,
//! numbers and booleans as JSON literals, arrays and objects as compact JSON and
//! `null` as an error, so a null can never silently become an empty URL segment,
//! header or key.
//!
//! Names starting with `random.` are built-in generators rather than variables
//! (see [`crate::random`]). They are consulted only when every scope misses, so
//! the session store and the environment can shadow them.

use serde_json::Value;

use crate::error::TemplateProblem;
use crate::random::{self, RandomError};

/// Lookup used to resolve variable names while rendering.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<Value>;

/// One piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Literal text emitted as written.
    Literal(String),
    /// A `${name}` placeholder.
    Var(String),
    /// A `${name[]}` pick token; the string is the base name, e.g. `types`.
    Pick(String),
    /// A `${name?}` (text) or `${name?json}` (JSON) prompt token.
    Prompt {
        /// Label the value is entered under, e.g. `limit`.
        name: String,
        /// Whether the entered text is parsed as JSON before it is stored.
        json: bool,
    },
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
    /// A `${name[]}` pick slot had no chosen value.
    #[error("no value chosen for `{0}[]`")]
    MissingPick(String),
    /// A prompt slot had no entered value; the string is the token as written.
    #[error("no value entered for `{0}`")]
    MissingPrompt(String),
    /// A `null` value was used where a string is required.
    #[error("variable `{0}` is null")]
    NullInStringSlot(String),
    /// A templated object key or header name rendered to the empty string.
    #[error("`{0}` templated to an empty string")]
    EmptyKey(String),
    /// A `random.` built-in name could not be generated.
    #[error(transparent)]
    Random(#[from] RandomError),
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
                let segment = if let Some(base) = name.strip_suffix("[]") {
                    if !is_valid_name(base) {
                        return Err(TemplateProblem::InvalidName);
                    }
                    Segment::Pick(base.to_string())
                } else if let Some(base) = name.strip_suffix("?json") {
                    if !is_valid_name(base) {
                        return Err(TemplateProblem::InvalidName);
                    }
                    Segment::Prompt {
                        name: base.to_string(),
                        json: true,
                    }
                } else if let Some(base) = name.strip_suffix('?') {
                    if !is_valid_name(base) {
                        return Err(TemplateProblem::InvalidName);
                    }
                    Segment::Prompt {
                        name: base.to_string(),
                        json: false,
                    }
                } else if is_valid_name(&name) {
                    Segment::Var(name)
                } else {
                    return Err(TemplateProblem::InvalidName);
                };
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(segment);
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
        segments.len() == 1
            && matches!(
                segments.first(),
                Some(Segment::Var(_) | Segment::Pick(_) | Segment::Prompt { .. })
            )
    } else if trimmed.is_empty() {
        false
    } else {
        // Trimming can only remove literal whitespace.
        parse(trimmed).map(|t| t.whole).unwrap_or(false)
    };
    if whole {
        let single = segments
            .into_iter()
            .find(|s| {
                matches!(
                    s,
                    Segment::Var(_) | Segment::Pick(_) | Segment::Prompt { .. }
                )
            })
            .unwrap_or_else(|| unreachable!("whole templates hold exactly one placeholder"));
        segments = vec![single];
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
        !self.segments.iter().any(|s| {
            matches!(
                s,
                Segment::Var(_) | Segment::Pick(_) | Segment::Prompt { .. }
            )
        })
    }

    /// Renders with JSON typing: a whole placeholder yields its value untouched,
    /// anything else yields an interpolated string.
    pub fn render_native(&self, lookup: Lookup<'_>) -> Result<Value, RenderError> {
        if self.whole {
            return match &self.segments[0] {
                Segment::Var(name) => resolve(name, lookup),
                Segment::Pick(name) => resolve_pick(name, lookup),
                Segment::Prompt { name, json } => resolve_prompt(name, *json, lookup),
                Segment::Literal(_) => {
                    unreachable!("whole templates hold exactly one placeholder")
                }
            };
        }
        Ok(Value::String(self.render_string(lookup)?))
    }

    /// Renders every segment using the string form of each value.
    pub fn render_string(&self, lookup: Lookup<'_>) -> Result<String, RenderError> {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Var(name) => {
                    let value = resolve(name, lookup)?;
                    out.push_str(&string_form(name, &value)?);
                }
                Segment::Pick(name) => {
                    let value = resolve_pick(name, lookup)?;
                    out.push_str(&string_form(name, &value)?);
                }
                Segment::Prompt { name, json } => {
                    let value = resolve_prompt(name, *json, lookup)?;
                    out.push_str(&string_form(name, &value)?);
                }
            }
        }
        Ok(out)
    }
}

/// A token that needs a value chosen or entered before the request is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A `${name[]}` pick: choose one element of the array variable `name`.
    Pick(String),
    /// A `${name?}` / `${name?json}` prompt: type a value under the label `name`.
    Prompt {
        /// Label the value is entered under.
        name: String,
        /// Whether the entered text is parsed as JSON.
        json: bool,
    },
}

/// The inputs in `text`, in first-seen order.
///
/// A parse failure yields an empty list: the file was already validated at load.
pub fn input_names(text: &str) -> Vec<Input> {
    let Ok(parsed) = parse(text) else {
        return Vec::new();
    };
    parsed
        .segments
        .iter()
        .filter_map(|segment| match segment {
            Segment::Pick(name) => Some(Input::Pick(name.clone())),
            Segment::Prompt { name, json } => Some(Input::Prompt {
                name: name.clone(),
                json: *json,
            }),
            Segment::Literal(_) | Segment::Var(_) => None,
        })
        .collect()
}

/// Resolves one variable name: the scopes first, then the `random.` built-ins.
fn resolve(name: &str, lookup: Lookup<'_>) -> Result<Value, RenderError> {
    if let Some(value) = lookup(name) {
        return Ok(value);
    }
    match random::generate(name) {
        Some(result) => result.map_err(RenderError::from),
        None => Err(RenderError::MissingVar(name.to_string())),
    }
}

/// Resolves a pick token: the chosen element is looked up under `name[]`.
fn resolve_pick(name: &str, lookup: Lookup<'_>) -> Result<Value, RenderError> {
    lookup(&format!("{name}[]")).ok_or_else(|| RenderError::MissingPick(name.to_string()))
}

/// Resolves a prompt token: the entered value is looked up under `name?`
/// (text) or `name?json` (JSON).
fn resolve_prompt(name: &str, json: bool, lookup: Lookup<'_>) -> Result<Value, RenderError> {
    let key = if json {
        format!("{name}?json")
    } else {
        format!("{name}?")
    };
    lookup(&key).ok_or(RenderError::MissingPrompt(key))
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
    fn random_builtins_keep_their_type_and_yield_to_scopes() {
        assert!(matches!(
            render("${random.bool}", &vars(&[])),
            Ok(Value::Bool(_))
        ));
        assert_eq!(render("${random.int.1.1}", &vars(&[])).unwrap(), json!(1));
        let text = render("${random.string.7}", &vars(&[])).unwrap();
        assert_eq!(text.as_str().map(str::len), Some(7));
        assert_eq!(
            parse("x-${random.string.5}")
                .unwrap()
                .render_string(&vars(&[]))
                .unwrap()
                .len(),
            7
        );
        // A scope value shadows the built-in, which keeps runs reproducible.
        assert_eq!(
            render("${random.int.6}", &vars(&[("random.int.6", json!(4))])).unwrap(),
            json!(4)
        );
        assert_eq!(
            parse("${random.bool}")
                .unwrap()
                .render_string(&vars(&[("random.bool", json!("fixed"))]))
                .unwrap(),
            "fixed"
        );
    }

    #[test]
    fn bad_random_names_are_render_errors() {
        assert!(matches!(
            render("${random.nope}", &vars(&[])),
            Err(RenderError::Random(RandomError::Unknown(name))) if name == "random.nope"
        ));
        assert!(matches!(
            render("${random.int.9.2}", &vars(&[])),
            Err(RenderError::Random(RandomError::Argument { .. }))
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
    fn pick_tokens_parse_as_picks() {
        let t = parse("${types[]}").unwrap();
        assert_eq!(t.segments(), &[Segment::Pick("types".into())]);
        assert!(t.is_whole());
        assert!(!t.is_literal());
        // Surrounding whitespace is trimmed like any other placeholder.
        let spaced = parse(" ${types[]} ").unwrap();
        assert_eq!(spaced.segments(), &[Segment::Pick("types".into())]);
        assert!(spaced.is_whole());
        // Inside a longer string the pick is one segment among many.
        let mixed = parse("/ships?class=${types[]}&x=1").unwrap();
        assert_eq!(
            mixed.segments(),
            &[
                Segment::Literal("/ships?class=".into()),
                Segment::Pick("types".into()),
                Segment::Literal("&x=1".into()),
            ]
        );
        assert!(!mixed.is_whole());

        // `[]` alone and a `[]` in the middle are still invalid names.
        assert_eq!(parse("${[]}").unwrap_err(), TemplateProblem::InvalidName);
        assert_eq!(parse("${a[]b}").unwrap_err(), TemplateProblem::InvalidName);
        assert_eq!(parse("${[]x}").unwrap_err(), TemplateProblem::InvalidName);
    }

    #[test]
    fn pick_tokens_resolve_under_the_bracketed_key() {
        let lookup = vars(&[("types[]", json!("frigate")), ("types", json!("ignored"))]);
        // The chosen element is looked up under `name[]`, not `name`.
        assert_eq!(render("${types[]}", &lookup).unwrap(), json!("frigate"));
        assert_eq!(
            parse("class=${types[]}")
                .unwrap()
                .render_string(&lookup)
                .unwrap(),
            "class=frigate"
        );
        // A whole pick keeps native typing.
        let typed = vars(&[("types[]", json!(["a", 1]))]);
        assert_eq!(render("${types[]}", &typed).unwrap(), json!(["a", 1]));
        // No chosen value is a dedicated error.
        assert_eq!(
            render("${types[]}", &vars(&[("types", json!("ignored"))])).unwrap_err(),
            RenderError::MissingPick("types".to_string())
        );
    }

    #[test]
    fn input_names_are_collected_in_order() {
        assert_eq!(
            input_names("/ships?class=${types[]}"),
            vec![Input::Pick("types".to_string())]
        );
        assert_eq!(input_names("plain"), Vec::<Input>::new());
        assert_eq!(
            input_names("${a[]}-${b[]}-${a[]}"),
            vec![
                Input::Pick("a".to_string()),
                Input::Pick("b".to_string()),
                Input::Pick("a".to_string()),
            ],
            "duplicates are kept; the caller deduplicates"
        );
        assert_eq!(
            input_names("${limit?}/${n?json}/${limit?}"),
            vec![
                Input::Prompt {
                    name: "limit".to_string(),
                    json: false,
                },
                Input::Prompt {
                    name: "n".to_string(),
                    json: true,
                },
                Input::Prompt {
                    name: "limit".to_string(),
                    json: false,
                },
            ]
        );
        assert_eq!(input_names("${"), Vec::<Input>::new());
    }

    #[test]
    fn prompt_tokens_parse_as_prompts() {
        assert_eq!(
            parse("${limit?}").unwrap().segments(),
            &[Segment::Prompt {
                name: "limit".into(),
                json: false,
            }]
        );
        assert_eq!(
            parse("${n?json}").unwrap().segments(),
            &[Segment::Prompt {
                name: "n".into(),
                json: true,
            }]
        );
        // Whitespace is trimmed like any other placeholder.
        assert_eq!(
            parse(" ${limit?} ").unwrap().segments(),
            &[Segment::Prompt {
                name: "limit".into(),
                json: false,
            }]
        );
        assert!(parse("${limit?}").unwrap().is_whole());
        assert!(!parse("${limit?}").unwrap().is_literal());
        assert!(!parse("limit=${limit?}").unwrap().is_whole());
        // `json` before a trailing `?` does not make a JSON prompt.
        assert_eq!(
            parse("${json?}").unwrap().segments(),
            &[Segment::Prompt {
                name: "json".into(),
                json: false,
            }]
        );
        // `?json5`, a spaced `?`, an empty base and a trailing `?` alone are invalid.
        assert_eq!(
            parse("${x?json5}").unwrap_err(),
            TemplateProblem::InvalidName
        );
        assert_eq!(parse("${x ?}").unwrap_err(), TemplateProblem::InvalidName);
        assert_eq!(parse("${?}").unwrap_err(), TemplateProblem::InvalidName);
        assert_eq!(parse("${a?b}").unwrap_err(), TemplateProblem::InvalidName);
    }

    #[test]
    fn prompt_tokens_resolve_under_their_marked_key() {
        // A text prompt keeps the typed value as-is.
        let text = vars(&[("n?", json!("3"))]);
        assert_eq!(render("${n?}", &text).unwrap(), json!("3"));
        // A JSON prompt keeps the parsed value's type.
        let json_value = vars(&[("n?json", json!(3))]);
        assert_eq!(render("${n?json}", &json_value).unwrap(), json!(3));
        assert_eq!(
            parse("limit=${n?json}")
                .unwrap()
                .render_string(&json_value)
                .unwrap(),
            "limit=3"
        );
        // Both forms are distinct keys: the text form ignores the JSON entry.
        assert_eq!(
            render("${n?}", &json_value).unwrap_err(),
            RenderError::MissingPrompt("n?".to_string())
        );
        assert_eq!(
            render("${n?json}", &vars(&[("n?", json!("3"))])).unwrap_err(),
            RenderError::MissingPrompt("n?json".to_string())
        );
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
