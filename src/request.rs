//! Loading and validating request files.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, TemplateProblem};
use crate::{Map, expression, template, variables};

/// Keys allowed at the top level of a request file.
pub const ALLOWED_KEYS: [&str; 6] = ["body", "headers", "method", "name", "outputs", "path"];

/// Methods a request may use, in the order they are listed in errors.
pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// One captured variable: the expression to evaluate and whether its result is secret.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// Variable name captured into the scope.
    pub name: String,
    /// Expression evaluated against the response.
    pub expression: String,
    /// Whether the captured value must not be echoed in the UI.
    pub secret: bool,
}

/// A validated request definition, with templates left unevaluated.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestDef {
    /// Request id, the path relative to the requests directory without `.json`.
    pub id: String,
    /// File this definition was read from.
    pub file: PathBuf,
    /// Display name, defaulting to the id.
    pub name: String,
    /// Upper-case HTTP method.
    pub method: String,
    /// Raw (possibly templated) path.
    pub path: String,
    /// Raw (possibly templated) header name/value pairs, in file order.
    pub headers: Vec<(String, String)>,
    /// Raw (possibly templated) body, `None` when absent or `null`.
    pub body: Option<Value>,
    /// Captured variables, in file order.
    pub outputs: Vec<Output>,
}

/// Reads and validates the request file `file` as request id `id`.
pub fn load_request(id: &str, file: &Path) -> Result<RequestDef, Error> {
    let text = fs::read_to_string(file).map_err(|source| Error::Io {
        path: Some(file.to_path_buf()),
        source,
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|source| Error::InvalidJson {
        path: file.to_path_buf(),
        source,
    })?;
    validate(id, file, &value)
}

/// Validates an already-parsed request document.
pub fn validate(id: &str, file: &Path, value: &Value) -> Result<RequestDef, Error> {
    let schema = |pointer: &str, message: String| Error::Schema {
        path: file.to_path_buf(),
        pointer: pointer.to_string(),
        message,
    };
    let Value::Object(map) = value else {
        return Err(schema("", "top-level value must be an object".to_string()));
    };

    let mut unknown: Vec<&str> = map
        .keys()
        .map(|k| k.as_str())
        .filter(|k| !ALLOWED_KEYS.contains(k))
        .collect();
    unknown.sort_unstable();
    if let Some(key) = unknown.first() {
        return Err(schema(
            &format!("/{}", variables::pointer_token(key)),
            format!("unknown key `{key}` (allowed: {})", ALLOWED_KEYS.join(", ")),
        ));
    }

    let method = required_string(file, map, "method")?;
    let method_upper = method.to_ascii_uppercase();
    if !METHODS.contains(&method_upper.as_str()) {
        return Err(schema(
            "/method",
            format!(
                "unknown method `{method}` (expected one of: {})",
                METHODS.join(", ")
            ),
        ));
    }
    let path = required_string(file, map, "path")?;

    let name = match map.get("name") {
        None | Some(Value::Null) => id.to_string(),
        Some(Value::String(name)) if name.is_empty() => {
            return Err(schema("/name", "`name` must not be empty".to_string()));
        }
        Some(Value::String(name)) => name.clone(),
        Some(_) => return Err(schema("/name", "`name` must be a string".to_string())),
    };

    let mut headers = Vec::new();
    if let Some(value) = map.get("headers") {
        let Value::Object(header_map) = value else {
            return Err(schema(
                "/headers",
                "`headers` must be an object".to_string(),
            ));
        };
        for (key, value) in header_map {
            let Some(value) = value.as_str() else {
                return Err(schema(
                    &format!("/headers/{}", variables::pointer_token(key)),
                    format!("`headers.{key}` must be a string"),
                ));
            };
            headers.push((key.clone(), value.to_string()));
        }
    }

    let body = match map.get("body") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    };

    let mut outputs: Vec<Output> = Vec::new();
    if let Some(value) = map.get("outputs") {
        let Value::Object(items) = value else {
            return Err(schema(
                "/outputs",
                "`outputs` must be an object".to_string(),
            ));
        };
        for (name, entry) in items {
            let pointer = format!("/outputs/{}", variables::pointer_token(name));
            let (raw, secret) = variables::split_entry(&pointer, entry)
                .map_err(|p| schema(&p.pointer, p.message))?;
            let Some(source) = raw.as_str() else {
                return Err(schema(
                    &pointer,
                    "output expression must be a string".to_string(),
                ));
            };
            expression::parse(source).map_err(|reason| schema(&pointer, reason))?;
            outputs.push(Output {
                name: name.clone(),
                expression: source.to_string(),
                secret,
            });
        }
    }

    let def = RequestDef {
        id: id.to_string(),
        file: file.to_path_buf(),
        name,
        method: method_upper,
        path,
        headers,
        body,
        outputs,
    };

    check_template(file, "/path", &def.path)?;
    check_template(file, "/name", &def.name)?;
    for (key, value) in &def.headers {
        check_template(
            file,
            &format!("/headers/{}", variables::pointer_token(key)),
            key,
        )?;
        check_template(
            file,
            &format!("/headers/{}", variables::pointer_token(key)),
            value,
        )?;
    }
    if let Some(body) = &def.body {
        check_templates_in_tree(file, "/body", body)?;
    }
    Ok(def)
}

fn required_string(file: &Path, map: &Map, key: &str) -> Result<String, Error> {
    let schema = |message: String| Error::Schema {
        path: file.to_path_buf(),
        pointer: format!("/{key}"),
        message,
    };
    match map.get(key) {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(schema(format!("`{key}` must be a string"))),
        None => Err(schema(format!("missing required key `{key}`"))),
    }
}

fn check_template(file: &Path, pointer: &str, text: &str) -> Result<(), Error> {
    template::parse(text)
        .map(|_| ())
        .map_err(|problem| Error::Template {
            path: file.to_path_buf(),
            pointer: pointer.to_string(),
            problem,
            value: text.to_string(),
        })
}

/// Validates every template inside a JSON value, object keys included.
fn check_templates_in_tree(file: &Path, pointer: &str, value: &Value) -> Result<(), Error> {
    match value {
        Value::String(text) => check_template(file, pointer, text),
        Value::Object(map) => {
            for (key, child) in map {
                let child_pointer = format!("{pointer}/{}", variables::pointer_token(key));
                check_template(file, &child_pointer, key)?;
                check_templates_in_tree(file, &child_pointer, child)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                check_templates_in_tree(file, &format!("{pointer}/{index}"), child)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Turns a template parse failure into [`Error::Template`].
pub fn template_error(file: &Path, pointer: &str, problem: TemplateProblem, value: &str) -> Error {
    Error::Template {
        path: file.to_path_buf(),
        pointer: pointer.to_string(),
        problem,
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::variables::pointer_token;
    use serde_json::json;

    fn load(id: &str, text: &str) -> Result<RequestDef, Error> {
        let value: Value = serde_json::from_str(text).expect("test JSON is valid");
        validate(id, Path::new("requests/x.json"), &value)
    }

    fn reason(err: &Error) -> String {
        match err {
            Error::Schema { message, .. } => message.clone(),
            Error::Template { problem, .. } => problem.message().to_string(),
            other => other.to_string(),
        }
    }

    #[test]
    fn minimal_file_defaults_the_name_to_the_id() {
        let def = load("pets/inventory", r#"{ "method": "get", "path": "/pets" }"#).unwrap();
        assert_eq!(def.id, "pets/inventory");
        assert_eq!(def.name, "pets/inventory");
        assert_eq!(def.method, "GET");
        assert_eq!(def.path, "/pets");
        assert!(def.headers.is_empty());
        assert_eq!(def.body, None);
        assert!(def.outputs.is_empty());
    }

    #[test]
    fn full_file_preserves_order_and_values() {
        let def = load(
            "ships/create",
            r#"{
              "name": "Create ship",
              "method": "POST",
              "path": "/ships",
              "headers": { "Authorization": "Bearer ${token}", "X-Trace": "curlyfries" },
              "body": { "name": "${shipName}", "typed": 42 },
              "outputs": { "shipId": "response.body.id", "shipName": "response.body.name" }
            }"#,
        )
        .unwrap();
        assert_eq!(def.name, "Create ship");
        assert_eq!(
            def.headers,
            vec![
                ("Authorization".to_string(), "Bearer ${token}".to_string()),
                ("X-Trace".to_string(), "curlyfries".to_string()),
            ]
        );
        assert_eq!(
            def.body,
            Some(json!({ "name": "${shipName}", "typed": 42 }))
        );
        assert_eq!(
            def.outputs,
            vec![
                Output {
                    name: "shipId".to_string(),
                    expression: "response.body.id".to_string(),
                    secret: false,
                },
                Output {
                    name: "shipName".to_string(),
                    expression: "response.body.name".to_string(),
                    secret: false,
                },
            ]
        );
    }

    #[test]
    fn outputs_preserve_file_order_and_the_secret_flag() {
        let def = load(
            "auth/login",
            r#"{
              "method": "POST",
              "path": "/auth/login",
              "outputs": {
                "token": { "secret": true, "value": "response.body.access_token" },
                "role": "response.body.user.role",
                "plain": { "secret": false, "value": "response.status" }
              }
            }"#,
        )
        .unwrap();
        assert_eq!(
            def.outputs,
            vec![
                Output {
                    name: "token".to_string(),
                    expression: "response.body.access_token".to_string(),
                    secret: true,
                },
                Output {
                    name: "role".to_string(),
                    expression: "response.body.user.role".to_string(),
                    secret: false,
                },
                Output {
                    name: "plain".to_string(),
                    // A `secret: false` wrapper is unwrapped too.
                    expression: "response.status".to_string(),
                    secret: false,
                },
            ]
        );
    }

    #[test]
    fn null_body_means_no_body() {
        let def = load("x", r#"{ "method": "POST", "path": "/x", "body": null }"#).unwrap();
        assert_eq!(def.body, None);
    }

    #[test]
    fn structural_errors() {
        assert!(reason(&load("x", "[]").unwrap_err()).contains("must be an object"));
        assert!(
            reason(&load("x", r#"{ "path": "/x" }"#).unwrap_err())
                .contains("missing required key `method`")
        );
        assert!(
            reason(&load("x", r#"{ "method": "GET" }"#).unwrap_err())
                .contains("missing required key `path`")
        );
        assert!(
            reason(&load("x", r#"{ "method": 1, "path": "/x" }"#).unwrap_err())
                .contains("`method` must be a string")
        );

        let unknown = load("x", r#"{ "method": "GET", "path": "/x", "ouputs": [] }"#).unwrap_err();
        match &unknown {
            Error::Schema {
                pointer, message, ..
            } => {
                assert_eq!(pointer, "/ouputs");
                assert!(message.contains("unknown key `ouputs`"), "{message}");
                assert!(
                    message.contains("allowed: body, headers, method, name, outputs, path"),
                    "{message}"
                );
            }
            other => panic!("wrong error: {other}"),
        }

        let bad_method = load("x", r#"{ "method": "FLY", "path": "/x" }"#).unwrap_err();
        let text = reason(&bad_method);
        for method in METHODS {
            assert!(text.contains(method), "{text}");
        }
        assert!(text.contains("unknown method `FLY`"), "{text}");

        assert!(
            reason(&load("x", r#"{ "method": "GET", "path": "/x", "headers": [] }"#).unwrap_err())
                .contains("`headers` must be an object")
        );
        assert!(
            reason(
                &load(
                    "x",
                    r#"{ "method": "GET", "path": "/x", "headers": { "A": 1 } }"#
                )
                .unwrap_err()
            )
            .contains("`headers.A` must be a string")
        );
        // `outputs` is an object keyed by capture name; anything else is refused.
        for text in [
            r#"{ "method": "GET", "path": "/x", "outputs": [] }"#,
            r#"{ "method": "GET", "path": "/x", "outputs": null }"#,
            r#"{ "method": "GET", "path": "/x", "outputs": ["a"] }"#,
        ] {
            let err = load("x", text).unwrap_err();
            match &err {
                Error::Schema {
                    pointer, message, ..
                } => {
                    assert_eq!(pointer, "/outputs", "{text}");
                    assert_eq!(message, "`outputs` must be an object", "{text}");
                }
                other => panic!("wrong error for {text}: {other}"),
            }
        }
        assert!(
            reason(
                &load(
                    "x",
                    r#"{ "method": "GET", "path": "/x", "outputs": { "a": 1 } }"#
                )
                .unwrap_err()
            )
            .contains("output expression must be a string")
        );
        assert!(
            reason(&load("x", r#"{ "method": "GET", "path": "/x", "name": 3 }"#).unwrap_err())
                .contains("`name` must be a string")
        );
        assert!(
            reason(&load("x", r#"{ "method": "GET", "path": "/x", "name": "" }"#).unwrap_err())
                .contains("`name` must not be empty")
        );
    }

    #[test]
    fn templates_and_expressions_are_validated_at_load() {
        let err = load("x", r#"{ "method": "GET", "path": "/ships/${" }"#).unwrap_err();
        match &err {
            Error::Template {
                pointer, problem, ..
            } => {
                assert_eq!(pointer, "/path");
                assert_eq!(*problem, TemplateProblem::Unterminated);
            }
            other => panic!("wrong error: {other}"),
        }

        let nested = load(
            "x",
            r#"{ "method": "POST", "path": "/x", "body": { "deep": { "list": ["${ok", 1] } } }"#,
        )
        .unwrap_err();
        match &nested {
            Error::Template { pointer, .. } => assert_eq!(pointer, "/body/deep/list/0"),
            other => panic!("wrong error: {other}"),
        }

        let bad_expression = load(
            "x",
            r#"{ "method": "GET", "path": "/x", "outputs": { "id": "response.nope" } }"#,
        )
        .unwrap_err();
        match &bad_expression {
            Error::Schema {
                pointer, message, ..
            } => {
                assert_eq!(pointer, "/outputs/id");
                assert!(message.contains("unknown response member"), "{message}");
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn pointer_tokens_are_escaped() {
        assert_eq!(pointer_token("a/b"), "a~1b");
        assert_eq!(pointer_token("a~b"), "a~0b");
        let err = load(
            "x",
            r#"{ "method": "GET", "path": "/x", "headers": { "a/b": "${" } }"#,
        )
        .unwrap_err();
        match err {
            Error::Template { pointer, .. } => assert_eq!(pointer, "/headers/a~1b"),
            other => panic!("wrong error: {other}"),
        }
    }
}
