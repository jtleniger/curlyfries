//! `curlyfries import`: builds a project from an OpenAPI 3.x or Swagger 2.0
//! document.
//!
//! One request file is written per operation, under
//! `requests/<first tag>/<operationId>.json`, plus the manifest, a `.gitignore`
//! and `environments/default.json` holding the document's `baseUrl`.
//!
//! What cannot be read off the document is left to the caller: no capture
//! definitions, no auth headers and no other environment variables. Operation
//! parameters become `${name}` templates, while request bodies are rendered
//! from the document's schemas so an operation without parameters runs as soon
//! as it is imported.
//!
//! Skipped silently because curlyfries cannot express them: `trace`
//! operations, non-JSON request bodies (`multipart/form-data`,
//! `application/x-www-form-urlencoded`, …) and `cookie` parameters.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::error::Error;
use crate::project::{DEFAULT_ENVIRONMENTS_DIR, DEFAULT_REQUESTS_DIR, MANIFEST_FILE};
use crate::{Map, request};

/// Manifest of an imported project: the single generated environment.
const MANIFEST: &str = "{\n  \"defaultEnvironment\": \"default\"\n}\n";
/// Environment selected by the generated manifest.
const ENVIRONMENT: &str = "default";
/// HTTP methods an operation key may name, upper case.
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
/// How deep `$ref` chains may nest before rendering gives up.
const MAX_REF_DEPTH: usize = 64;

/// The API document dialect being imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    /// `openapi: 3.x`.
    OpenApi3,
    /// `swagger: "2.0"`.
    Swagger2,
}

/// One operation to turn into a request file.
#[derive(Debug, Clone, PartialEq)]
struct Operation {
    /// Upper-case HTTP method.
    method: String,
    /// Path as written in the document, templates included.
    path_template: String,
    /// Operation `tags`, first one wins for the directory.
    tags: Vec<String>,
    /// `operationId`, when the document names one.
    operation_id: Option<String>,
    /// `summary`, when the document has one.
    summary: Option<String>,
    /// Resolved path-item and operation parameters, deduplicated.
    parameters: Vec<Value>,
    /// Schema of a JSON request body, when the document declares one.
    body_schema: Option<Value>,
}

/// Reads `spec` and writes an imported project into `root`.
///
/// `spec` is resolved against `root`; the document may be JSON or YAML.
/// Nothing is written unless every generated request file validates and no
/// target path exists yet.
pub fn import(root: &Path, spec: &Path, out: &mut impl Write) -> Result<(), Error> {
    let spec_path = root.join(spec);
    let text = fs::read_to_string(&spec_path).map_err(|source| Error::Io {
        path: Some(spec_path.clone()),
        source,
    })?;
    let doc = parse_document(&text, &spec_path)?;
    let version = document_version(&doc, &spec_path)?;
    let operations = operations(&doc, version, &spec_path)?;

    let mut count = 0usize;
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut requests: Vec<(String, String)> = Vec::new();
    for operation in &operations {
        let (relative, value) = request_file(operation, &doc, &mut used);
        let id = relative
            .strip_prefix(&format!("{DEFAULT_REQUESTS_DIR}/"))
            .unwrap_or(&relative)
            .strip_suffix(".json")
            .unwrap_or(&relative)
            .to_string();
        // The generator must only emit requests the loader accepts.
        request::validate(&id, Path::new(&relative), &value)?;
        requests.push((relative, serialize(&value)));
        count += 1;
    }
    if count == 0 {
        return Err(Error::InvalidSpec {
            path: spec_path,
            pointer: "/paths".to_string(),
            message: "no operations found to import".to_string(),
        });
    }
    requests.sort_by(|left, right| left.0.cmp(&right.0));

    let mut targets: Vec<(String, String)> = vec![
        (MANIFEST_FILE.to_string(), MANIFEST.to_string()),
        (".gitignore".to_string(), crate::init::GITIGNORE.to_string()),
        (
            format!("{DEFAULT_ENVIRONMENTS_DIR}/{ENVIRONMENT}.json"),
            serialize(&environment_value(&doc, version)),
        ),
    ];
    targets.extend(requests);

    let existing: Vec<String> = targets
        .iter()
        .filter(|(relative, _)| fs::symlink_metadata(root.join(relative)).is_ok())
        .map(|(relative, _)| relative.clone())
        .collect();
    if !existing.is_empty() {
        return Err(Error::ImportConflict {
            root: root.to_path_buf(),
            existing,
        });
    }

    for (relative, contents) in &targets {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: Some(parent.to_path_buf()),
                source,
            })?;
        }
        fs::write(&path, contents).map_err(|source| Error::Io {
            path: Some(path.clone()),
            source,
        })?;
        let _ = writeln!(out, "created {relative}");
    }
    let _ = writeln!(
        out,
        "imported {count} request files into {}",
        root.display()
    );
    Ok(())
}

/// Pretty-printed JSON with the trailing newline every generated file carries.
fn serialize(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON values always serialize");
    text.push('\n');
    text
}

/// `environments/default.json`: the document's `baseUrl`, when it names one.
fn environment_value(doc: &Value, version: Version) -> Value {
    let mut map = Map::new();
    if let Some(url) = base_url(doc, version) {
        map.insert("baseUrl".to_string(), Value::String(url));
    }
    Value::Object(map)
}

/// Parses `text` as JSON when `path` ends in `.json`, as YAML otherwise.
fn parse_document(text: &str, path: &Path) -> Result<Value, Error> {
    let invalid = |message: String| Error::InvalidSpec {
        path: path.to_path_buf(),
        pointer: String::new(),
        message,
    };
    if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
        serde_json::from_str(text).map_err(|source| invalid(format!("invalid JSON: {source}")))
    } else {
        serde_yaml_ng::from_str(text).map_err(|source| invalid(format!("invalid YAML: {source}")))
    }
}

/// Detects the document dialect and checks the parts every import needs.
fn document_version(doc: &Value, path: &Path) -> Result<Version, Error> {
    let invalid = |pointer: &str, message: String| Error::InvalidSpec {
        path: path.to_path_buf(),
        pointer: pointer.to_string(),
        message,
    };
    let Some(map) = doc.as_object() else {
        return Err(invalid(
            "",
            "the document must be a JSON/YAML object".to_string(),
        ));
    };
    let version = match map.get("openapi") {
        Some(value) => match value.as_str() {
            Some(text) if text.starts_with("3.") => Version::OpenApi3,
            _ => {
                return Err(invalid(
                    "/openapi",
                    format!(
                        "unsupported `openapi` version `{}` (expected 3.x)",
                        value_text(value)
                    ),
                ));
            }
        },
        None => match map.get("swagger") {
            Some(Value::String(text)) if text == "2.0" => Version::Swagger2,
            Some(value) => {
                return Err(invalid(
                    "/swagger",
                    format!(
                        "unsupported `swagger` version `{}` (expected 2.0)",
                        value_text(value)
                    ),
                ));
            }
            None => {
                return Err(invalid(
                    "",
                    "unsupported API document: expected an `openapi` 3.x or `swagger` 2.0 field"
                        .to_string(),
                ));
            }
        },
    };
    if !map.get("paths").is_some_and(Value::is_object) {
        return Err(invalid("/paths", "`paths` must be an object".to_string()));
    }
    Ok(version)
}

/// The JSON text of `value`, strings unquoted.
fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Every operation in `doc`, in document order.
fn operations(doc: &Value, version: Version, path: &Path) -> Result<Vec<Operation>, Error> {
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return Err(Error::InvalidSpec {
            path: path.to_path_buf(),
            pointer: "/paths".to_string(),
            message: "`paths` must be an object".to_string(),
        });
    };
    let mut operations = Vec::new();
    for (path_template, path_item) in paths {
        let Some(item) = path_item.as_object() else {
            continue;
        };
        for (key, operation) in item {
            if !METHODS.contains(&key.to_ascii_uppercase().as_str()) {
                continue;
            }
            let Some(operation) = operation.as_object() else {
                continue;
            };
            let parameters = merge_parameters(path_item, operation, doc);
            operations.push(Operation {
                method: key.to_ascii_uppercase(),
                path_template: path_template.clone(),
                tags: operation
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|tags| {
                        tags.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                operation_id: operation
                    .get("operationId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                summary: operation
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                body_schema: body_schema(doc, version, operation, &parameters),
                parameters,
            });
        }
    }
    Ok(operations)
}

/// Path-item parameters followed by operation parameters, `(in, name)`
/// deduplicated with the operation-level entry winning.
fn merge_parameters(path_item: &Value, operation: &Map, doc: &Value) -> Vec<Value> {
    let mut merged: Vec<((String, String), Value)> = Vec::new();
    for source in [path_item, &Value::Object(operation.clone())] {
        let Some(items) = source.get("parameters").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let resolved = resolve_ref(item, doc);
            let key = (
                resolved
                    .get("in")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                resolved
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
            match merged.iter().position(|(seen, _)| *seen == key) {
                Some(index) => merged[index].1 = resolved.clone(),
                None => merged.push((key, resolved.clone())),
            }
        }
    }
    merged.into_iter().map(|(_, value)| value).collect()
}

/// The schema of the operation's JSON request body, when it declares one.
fn body_schema(
    doc: &Value,
    version: Version,
    operation: &Map,
    parameters: &[Value],
) -> Option<Value> {
    match version {
        Version::OpenApi3 => {
            let body = resolve_ref(operation.get("requestBody")?, doc);
            let content = body.get("content")?.as_object()?;
            let media = match content.get("application/json") {
                Some(media) => media,
                None => content
                    .iter()
                    .find(|(media_type, _)| media_type.ends_with("+json"))
                    .map(|(_, media)| media)?,
            };
            media.get("schema").cloned()
        }
        Version::Swagger2 => parameters
            .iter()
            .find(|parameter| parameter.get("in").and_then(Value::as_str) == Some("body"))
            .and_then(|parameter| parameter.get("schema"))
            .cloned(),
    }
}

/// Resolves a local `#/…` JSON pointer reference against `doc`.
///
/// A value that is not a `$ref`, or one that does not resolve inside `doc`, is
/// returned unchanged.
fn resolve_ref<'a>(value: &'a Value, doc: &'a Value) -> &'a Value {
    resolve_pointer(value, doc).unwrap_or(value)
}

/// Resolves `value` when it is a `$ref` that points inside `doc`.
fn resolve_pointer<'a>(value: &'a Value, doc: &'a Value) -> Option<&'a Value> {
    let reference = value.get("$ref").and_then(Value::as_str)?;
    let pointer = reference.strip_prefix("#/")?;
    let mut current = doc;
    for token in pointer.split('/') {
        let token = token.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(map) => map.get(&token)?,
            Value::Array(items) => items.get(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Generates the request file for `op`, plus its path relative to `root`.
fn request_file(op: &Operation, doc: &Value, used: &mut BTreeSet<String>) -> (String, Value) {
    let dir = match request_segment(op.tags.first().map(String::as_str).unwrap_or_default()) {
        segment if segment.is_empty() => "default".to_string(),
        segment => segment,
    };
    let stem = match request_segment(op.operation_id.as_deref().unwrap_or_default()) {
        segment if segment.is_empty() => method_path_slug(op),
        segment => segment,
    };
    let mut relative = format!("{DEFAULT_REQUESTS_DIR}/{dir}/{stem}.json");
    let mut suffix = 2;
    while used.contains(&relative) {
        relative = format!("{DEFAULT_REQUESTS_DIR}/{dir}/{stem}-{suffix}.json");
        suffix += 1;
    }
    used.insert(relative.clone());

    let name = non_empty(op.summary.as_deref())
        .or_else(|| non_empty(op.operation_id.as_deref()))
        .unwrap_or_else(|| format!("{} {}", op.method, op.path_template));
    let (path, headers) = parameter_slots(op, doc);

    let mut file = Map::new();
    file.insert("name".to_string(), Value::String(name));
    file.insert("method".to_string(), Value::String(op.method.clone()));
    file.insert("path".to_string(), Value::String(path));
    if !headers.is_empty() {
        file.insert("headers".to_string(), Value::Object(headers));
    }
    if let Some(schema) = &op.body_schema {
        let body = render_example(schema, doc, &mut Vec::new())
            .unwrap_or_else(|| Value::Object(Map::new()));
        file.insert("body".to_string(), body);
    }
    (relative, Value::Object(file))
}

/// `text` trimmed and non-empty, or `None`.
fn non_empty(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// A filesystem- and request-id-safe path segment.
fn request_segment(text: &str) -> String {
    let mapped: String = text
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    mapped.trim_matches(['-', '.']).to_string()
}

/// `"{method-lower}-{path with / → -, {} stripped}"`, e.g. `delete-ships-id-crew`.
fn method_path_slug(op: &Operation) -> String {
    let path: String = op
        .path_template
        .chars()
        .filter(|character| !matches!(character, '{' | '}'))
        .map(|character| if character == '/' { '-' } else { character })
        .collect();
    let segment = request_segment(&path);
    let method = op.method.to_ascii_lowercase();
    if segment.is_empty() {
        method
    } else {
        format!("{method}-{segment}")
    }
}

/// A `${…}`-safe variable name, deduplicated against `used`.
fn template_name(name: &str, used: &mut BTreeSet<String>) -> String {
    let mut base: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if base.is_empty() {
        base.push('p');
    }
    if !base.starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
        || base.starts_with("random.")
    {
        base.insert(0, '_');
    }
    let mut candidate = base.clone();
    let mut suffix = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    used.insert(candidate.clone());
    candidate
}

/// The request `path` and `headers` for `op`'s parameters.
fn parameter_slots(op: &Operation, doc: &Value) -> (String, Map) {
    let mut used = BTreeSet::new();
    let mut path = op.path_template.clone();
    let mut headers = Map::new();
    let mut query: Vec<String> = Vec::new();
    for parameter in &op.parameters {
        let parameter = resolve_ref(parameter, doc);
        let (Some(location), Some(name)) = (
            parameter.get("in").and_then(Value::as_str),
            parameter.get("name").and_then(Value::as_str),
        ) else {
            continue;
        };
        match location {
            "path" => {
                let variable = template_name(name, &mut used);
                path = path.replace(&format!("{{{name}}}"), &format!("${{{variable}}}"));
            }
            "query" => {
                let schema = match parameter.get("schema") {
                    Some(schema) => resolve_ref(schema, doc),
                    None => parameter,
                };
                let text = render_example(schema, doc, &mut Vec::new())
                    .as_ref()
                    .and_then(inline_query_text);
                match text {
                    Some(text) => query.push(format!("{name}={text}")),
                    None => {
                        let variable = template_name(name, &mut used);
                        query.push(format!("{name}=${{{variable}}}"));
                    }
                }
            }
            "header" => {
                let variable = template_name(name, &mut used);
                headers.insert(name.to_string(), Value::String(format!("${{{variable}}}")));
            }
            _ => {}
        }
    }
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query.join("&"));
    }
    (path, headers)
}

/// The URL-safe text form of `value`, when it can be inlined into a query.
fn inline_query_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text)
            if !text.is_empty()
                && text.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '_' | '~' | '%' | '-')
                }) =>
        {
            Some(text.clone())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// A concrete value for `schema`, or `None` when nothing can be derived.
fn render_example(schema: &Value, doc: &Value, stack: &mut Vec<String>) -> Option<Value> {
    if stack.len() > MAX_REF_DEPTH {
        return None;
    }
    let map = schema.as_object()?;
    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        let target = resolve_pointer(schema, doc)?;
        if !reference.starts_with("#/") || stack.iter().any(|seen| seen == reference) {
            return None;
        }
        stack.push(reference.to_string());
        let rendered = render_example(target, doc, stack);
        stack.pop();
        return rendered;
    }
    for key in ["const", "example", "default"] {
        if let Some(value) = map.get(key) {
            return Some(value.clone());
        }
    }
    if let Some(first) = map
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    {
        return Some(first.clone());
    }
    if let Some(first) = map
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    {
        return Some(first.clone());
    }
    if let Some(members) = map.get("allOf").and_then(Value::as_array) {
        let mut merged: Option<Value> = None;
        for member in members {
            let Some(value) = render_example(member, doc, stack) else {
                continue;
            };
            merged = Some(match (merged, value) {
                (Some(Value::Object(mut base)), Value::Object(extra)) => {
                    for (key, value) in extra {
                        base.insert(key, value);
                    }
                    Value::Object(base)
                }
                (_, value) => value,
            });
        }
        if merged.is_some() {
            return merged;
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(members) = map.get(key).and_then(Value::as_array) {
            for member in members {
                if let Some(value) = render_example(member, doc, stack) {
                    return Some(value);
                }
            }
        }
    }
    if let Some(Value::Array(names)) = map.get("type") {
        let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() && names.iter().all(|name| *name == "null") {
            return Some(Value::Null);
        }
    }
    match type_name(map) {
        Some("object") => Some(object_example(map, doc, stack)),
        Some("array") => {
            let items = match map.get("items") {
                Some(Value::Array(items)) => items.first(),
                other => other,
            };
            let item = items.and_then(|item| render_example(item, doc, stack));
            Some(Value::Array(item.into_iter().collect()))
        }
        Some("string") => Some(Value::String(String::new())),
        Some("integer") | Some("number") => Some(Value::Number(0.into())),
        Some("boolean") => Some(Value::Bool(false)),
        Some("null") => Some(Value::Null),
        None if map.contains_key("properties") => Some(object_example(map, doc, stack)),
        _ => None,
    }
}

/// The `type` of a schema, treating `["null", "string"]` as `string` and an
/// all-`null` list as `null`.
fn type_name(map: &Map) -> Option<&str> {
    match map.get("type") {
        Some(Value::String(name)) => Some(name.as_str()),
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .find(|name| *name != "null"),
        _ => None,
    }
}

/// An object with one rendered value per writable property.
fn object_example(map: &Map, doc: &Value, stack: &mut Vec<String>) -> Value {
    let mut object = Map::new();
    if let Some(properties) = map.get("properties").and_then(Value::as_object) {
        for (name, schema) in properties {
            if resolve_ref(schema, doc)
                .get("readOnly")
                .and_then(Value::as_bool)
                == Some(true)
            {
                continue;
            }
            if let Some(value) = render_example(schema, doc, stack) {
                object.insert(name.clone(), value);
            }
        }
    }
    Value::Object(object)
}

/// The `baseUrl` for `environments/default.json`, when the document names one.
fn base_url(doc: &Value, version: Version) -> Option<String> {
    match version {
        Version::OpenApi3 => {
            let server = doc.get("servers")?.as_array()?.first()?;
            let url = server.get("url")?.as_str()?;
            Some(
                substitute_server_variables(url, server)
                    .trim_end_matches('/')
                    .to_string(),
            )
        }
        Version::Swagger2 => {
            let scheme = doc
                .get("schemes")
                .and_then(Value::as_array)
                .and_then(|schemes| schemes.first())
                .and_then(Value::as_str)
                .unwrap_or("https");
            let host = doc.get("host")?.as_str()?;
            let base_path = doc.get("basePath").and_then(Value::as_str).unwrap_or("");
            Some(
                format!("{scheme}://{host}{base_path}")
                    .trim_end_matches('/')
                    .to_string(),
            )
        }
    }
}

/// Replaces every `{var}` in a server URL with the variable's default.
///
/// A variable without a usable default is replaced with nothing; an unclosed
/// `{` is kept verbatim.
fn substitute_server_variables(url: &str, server: &Value) -> String {
    let variables = server.get("variables").and_then(Value::as_object);
    let mut out = String::new();
    let mut rest = url;
    while let Some(start) = rest.find('{') {
        let (before, after) = rest.split_at(start);
        out.push_str(before);
        let Some(offset) = after[1..].find('}') else {
            out.push_str(after);
            return out;
        };
        out.push_str(&variable_default(variables, &after[1..1 + offset]));
        rest = &after[offset + 2..];
    }
    out.push_str(rest);
    out
}

/// The default (or first enum entry) of a server variable, as text.
fn variable_default(variables: Option<&Map>, name: &str) -> String {
    let Some(variable) = variables.and_then(|variables| variables.get(name)) else {
        return String::new();
    };
    let value = variable.get("default").or_else(|| {
        variable
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
    });
    value.map(value_text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    /// Parses a JSON test document into its operations.
    fn parse(text: &str) -> (Value, Vec<Operation>) {
        let doc: Value = serde_json::from_str(text).expect("test document is JSON");
        let path = Path::new("spec.json");
        let version = document_version(&doc, path).expect("test document names a version");
        let operations = operations(&doc, version, path).expect("test document has paths");
        (doc, operations)
    }

    /// Parses a JSON test document holding exactly one operation.
    fn only(text: &str) -> (Value, Operation) {
        let (doc, mut operations) = parse(text);
        assert_eq!(operations.len(), 1, "expected exactly one operation");
        (doc, operations.remove(0))
    }

    #[test]
    fn segments_are_filesystem_safe() {
        assert_eq!(request_segment("Pirate Ships!"), "Pirate-Ships");
        assert_eq!(request_segment("???"), "");
        assert_eq!(request_segment(".hidden."), "hidden");
        assert_eq!(request_segment("v1.2/ships"), "v1.2-ships");
    }

    #[test]
    fn template_names_are_safe_and_deduplicated() {
        let mut used = BTreeSet::new();
        assert_eq!(template_name("1x", &mut used), "_1x");
        assert_eq!(template_name("random.string", &mut used), "_random.string");
        assert_eq!(template_name("X-Trace", &mut used), "X-Trace");
        assert_eq!(template_name("a b", &mut used), "a_b");
        assert_eq!(template_name("", &mut used), "p");
        assert_eq!(template_name("token", &mut used), "token");
        assert_eq!(template_name("token", &mut used), "token_2");
        assert_eq!(template_name("token", &mut used), "token_3");
    }

    #[test]
    fn render_example_prefers_the_most_specific_value() {
        let doc = json!({});
        let schema = json!({ "type": "string", "const": "c", "example": "x", "default": "d", "enum": ["e"] });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!("c"))
        );
        let schema = json!({ "type": "string", "example": "x", "default": "d", "enum": ["e"] });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!("x"))
        );
        let schema = json!({ "type": "string", "default": "d", "enum": ["e"] });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!("d"))
        );
        let schema = json!({ "type": "string", "enum": ["e", "f"] });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!("e"))
        );
        let schema = json!({ "type": "string", "examples": ["one", "two"] });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!("one"))
        );
    }

    #[test]
    fn render_example_fills_objects_arrays_and_scalars() {
        let doc = json!({});
        let schema = json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "readOnly": true },
                "name": { "type": "string" },
                "crew": { "type": "integer" },
                "flags": { "type": "array", "items": { "type": "boolean" } },
                "nothing": { "type": "unsupported" },
                "maybe": { "type": ["null", "string"] }
            }
        });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!({ "name": "", "crew": 0, "flags": [false], "maybe": "" }))
        );
        assert_eq!(
            render_example(&json!({ "type": "object" }), &doc, &mut Vec::new()),
            Some(json!({}))
        );
        assert_eq!(
            render_example(
                &json!({ "properties": { "name": { "type": "string" } } }),
                &doc,
                &mut Vec::new()
            ),
            Some(json!({ "name": "" }))
        );
        assert_eq!(
            render_example(&json!({ "type": ["null"] }), &doc, &mut Vec::new()),
            Some(Value::Null)
        );
        assert_eq!(
            render_example(&json!({ "type": "unsupported" }), &doc, &mut Vec::new()),
            None
        );
        assert_eq!(
            render_example(&json!("string"), &doc, &mut Vec::new()),
            None
        );
    }

    #[test]
    fn render_example_composes_alternatives() {
        let doc = json!({});
        let all_of = json!({ "allOf": [
            { "type": "object", "properties": { "id": { "type": "integer" }, "name": { "type": "string" } } },
            { "type": "object", "properties": { "id": { "type": "integer", "default": 7 } } }
        ] });
        assert_eq!(
            render_example(&all_of, &doc, &mut Vec::new()),
            Some(json!({ "id": 7, "name": "" }))
        );
        let one_of = json!({ "oneOf": [
            { "type": "boolean" },
            { "type": "string" }
        ] });
        assert_eq!(
            render_example(&one_of, &doc, &mut Vec::new()),
            Some(json!(false))
        );
    }

    #[test]
    fn render_example_stops_at_reference_cycles() {
        let doc = json!({ "components": { "schemas": { "Node": {
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "next": { "$ref": "#/components/schemas/Node" }
            }
        } } } });
        let schema = json!({ "$ref": "#/components/schemas/Node" });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!({ "name": "" }))
        );
    }

    #[test]
    fn sibling_references_expand_independently() {
        let doc = json!({ "components": { "schemas": { "Captain": {
            "type": "object",
            "properties": { "name": { "type": "string" } }
        } } } });
        let schema = json!({ "type": "object", "properties": {
            "captain": { "$ref": "#/components/schemas/Captain" },
            "firstMate": { "$ref": "#/components/schemas/Captain" }
        } });
        assert_eq!(
            render_example(&schema, &doc, &mut Vec::new()),
            Some(json!({ "captain": { "name": "" }, "firstMate": { "name": "" } }))
        );
    }

    #[test]
    fn unresolvable_references_render_nothing() {
        let doc = json!({});
        assert_eq!(
            render_example(&json!({ "$ref": "#/nope" }), &doc, &mut Vec::new()),
            None
        );
        assert_eq!(
            render_example(
                &json!({ "$ref": "external.yaml#/Ship" }),
                &doc,
                &mut Vec::new()
            ),
            None
        );
    }

    #[test]
    fn parameters_merge_with_the_operation_winning() {
        let (doc, operation) = only(
            r#"{
                "openapi": "3.1.0",
                "paths": { "/ships/{id}": {
                    "parameters": [
                        { "in": "path", "name": "id", "required": true, "schema": { "type": "string" } },
                        { "in": "query", "name": "limit", "schema": { "type": "integer", "default": 1 } },
                        { "in": "header", "name": "X-Tenant", "schema": { "type": "string" } }
                    ],
                    "get": {
                        "operationId": "getShip",
                        "parameters": [
                            { "in": "query", "name": "limit", "schema": { "type": "integer", "default": 25 } },
                            { "in": "query", "name": "sort", "schema": { "type": "string" } },
                            { "in": "query", "name": "query", "schema": { "type": "string", "default": "a b&c" } },
                            { "in": "cookie", "name": "session", "schema": { "type": "string" } },
                            { "in": "formData", "name": "upload", "type": "string" },
                            { "name": "nameless-location", "schema": { "type": "string" } },
                            { "in": "query" }
                        ]
                    }
                } }
            }"#,
        );
        let (path, headers) = parameter_slots(&operation, &doc);
        assert_eq!(path, "/ships/${id}?limit=25&sort=${sort}&query=${query}");
        assert_eq!(Value::Object(headers), json!({ "X-Tenant": "${X-Tenant}" }));
    }

    #[test]
    fn swagger_inline_query_parameters_are_read_in_place() {
        let (doc, operation) = only(
            r#"{
                "swagger": "2.0",
                "paths": { "/ping": { "get": {
                    "operationId": "ping",
                    "parameters": [
                        { "in": "query", "name": "verbose", "type": "boolean", "default": true },
                        { "in": "query", "name": "ratio", "type": "number", "default": 0.5 },
                        { "in": "query", "name": "filter", "type": "string" }
                    ]
                } } }
            }"#,
        );
        let (path, headers) = parameter_slots(&operation, &doc);
        assert_eq!(path, "/ping?verbose=true&ratio=0.5&filter=${filter}");
        assert!(headers.is_empty());
    }

    #[test]
    fn non_operation_keys_are_ignored() {
        let (_, operations) = parse(
            r##"{
                "openapi": "3.1.0",
                "paths": { "/ping": {
                    "summary": "ignored",
                    "description": "ignored",
                    "$ref": "#/components/pathItems/Ping",
                    "servers": [],
                    "trace": { "operationId": "traced" },
                    "head": { "operationId": "ping" }
                } }
            }"##,
        );
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].method, "HEAD");
        assert_eq!(operations[0].operation_id.as_deref(), Some("ping"));
    }

    #[test]
    fn json_media_types_supply_the_request_body() {
        let (_, operations) = parse(
            r#"{
                "openapi": "3.1.0",
                "paths": {
                    "/json": { "post": { "requestBody": { "content": {
                        "application/json": { "schema": { "type": "string" } } } } } },
                    "/vendor": { "post": { "requestBody": { "content": {
                        "application/vnd.api+json": { "schema": { "type": "boolean" } } } } } },
                    "/form": { "post": { "requestBody": { "content": {
                        "application/x-www-form-urlencoded": { "schema": { "type": "string" } } } } } }
                }
            }"#,
        );
        assert_eq!(operations[0].body_schema, Some(json!({ "type": "string" })));
        assert_eq!(
            operations[1].body_schema,
            Some(json!({ "type": "boolean" }))
        );
        assert_eq!(operations[2].body_schema, None);

        let (_, operations) = parse(
            r#"{
                "swagger": "2.0",
                "paths": { "/things": { "post": { "parameters": [
                    { "in": "body", "name": "thing", "schema": { "type": "object", "properties": { "label": { "type": "string" } } } }
                ] } } }
            }"#,
        );
        assert_eq!(
            operations[0].body_schema,
            Some(json!({ "type": "object", "properties": { "label": { "type": "string" } } }))
        );
    }

    #[test]
    fn openapi_servers_supply_the_base_url() {
        let doc = json!({ "servers": [{
            "url": "https://{tenant}.example.com/{version}/",
            "variables": {
                "tenant": { "default": "acme" },
                "version": { "enum": ["v2", "v3"] },
                "unset": { "description": "no default" }
            }
        }] });
        assert_eq!(
            base_url(&doc, Version::OpenApi3).as_deref(),
            Some("https://acme.example.com/v2")
        );
        assert_eq!(
            base_url(
                &json!({ "servers": [{ "url": "/api" }] }),
                Version::OpenApi3
            )
            .as_deref(),
            Some("/api")
        );
        assert_eq!(base_url(&json!({ "servers": [] }), Version::OpenApi3), None);
        assert_eq!(base_url(&json!({}), Version::OpenApi3), None);
    }

    #[test]
    fn swagger_host_and_base_path_supply_the_base_url() {
        let doc = json!({ "schemes": ["http"], "host": "127.0.0.1:4000", "basePath": "/api/" });
        assert_eq!(
            base_url(&doc, Version::Swagger2).as_deref(),
            Some("http://127.0.0.1:4000/api")
        );
        assert_eq!(
            base_url(&json!({ "host": "example.com" }), Version::Swagger2).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            base_url(&json!({ "basePath": "/api" }), Version::Swagger2),
            None
        );
    }

    #[test]
    fn request_paths_never_collide() {
        let (doc, operations) = parse(
            r#"{
                "openapi": "3.1.0",
                "paths": {
                    "/ships": { "get": { "operationId": "listShips", "tags": ["ships"] } },
                    "/galleons": { "get": { "operationId": "listShips", "tags": ["ships"] } }
                }
            }"#,
        );
        let mut used = BTreeSet::new();
        assert_eq!(
            request_file(&operations[0], &doc, &mut used).0,
            "requests/ships/listShips.json"
        );
        assert_eq!(
            request_file(&operations[1], &doc, &mut used).0,
            "requests/ships/listShips-2.json"
        );
    }

    #[test]
    fn request_file_falls_back_to_the_method_and_path() {
        let (doc, operation) = only(
            r#"{
                "openapi": "3.1.0",
                "paths": { "/ships/{id}/crew": { "delete": { "parameters": [
                    { "in": "path", "name": "id", "required": true, "schema": { "type": "string" } }
                ] } } }
            }"#,
        );
        let (relative, value) = request_file(&operation, &doc, &mut BTreeSet::new());
        assert_eq!(relative, "requests/default/delete-ships-id-crew.json");
        assert_eq!(
            value,
            json!({
                "name": "DELETE /ships/{id}/crew",
                "method": "DELETE",
                "path": "/ships/${id}/crew"
            })
        );
    }

    #[test]
    fn operations_cover_every_method_and_skip_non_objects() {
        let (_, operations) = parse(
            r#"{
                "openapi": "3.1.0",
                "paths": {
                    "/one": {
                        "get": {}, "post": {}, "put": {}, "patch": {},
                        "delete": {}, "head": {}, "options": {},
                        "trace": {}, "parameters": []
                    },
                    "/two": "not an object"
                }
            }"#,
        );
        let methods: Vec<&str> = operations
            .iter()
            .map(|operation| operation.method.as_str())
            .collect();
        assert_eq!(
            methods,
            ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
        );
    }

    #[test]
    fn documents_are_validated_before_anything_is_generated() {
        let path = Path::new("spec.json");
        let cases: [(&str, &str, &str); 6] = [
            ("[]", "", "the document must be a JSON/YAML object"),
            (
                r#"{"openapi": "2.0", "paths": {}}"#,
                "/openapi",
                "unsupported `openapi` version `2.0` (expected 3.x)",
            ),
            (
                r#"{"swagger": "1.2", "paths": {}}"#,
                "/swagger",
                "unsupported `swagger` version `1.2` (expected 2.0)",
            ),
            (
                r#"{"info": {}}"#,
                "",
                "unsupported API document: expected an `openapi` 3.x or `swagger` 2.0 field",
            ),
            (
                r#"{"openapi": "3.1.0"}"#,
                "/paths",
                "`paths` must be an object",
            ),
            (
                r#"{"openapi": "3.1.0", "paths": []}"#,
                "/paths",
                "`paths` must be an object",
            ),
        ];
        for (text, pointer, message) in cases {
            let doc: Value = serde_json::from_str(text).expect("test document is JSON");
            let error = document_version(&doc, path).expect_err("document is rejected");
            match error {
                Error::InvalidSpec {
                    path: reported,
                    pointer: reported_pointer,
                    message: reported_message,
                } => {
                    assert_eq!(reported, PathBuf::from("spec.json"));
                    assert_eq!(reported_pointer, pointer, "{text}");
                    assert_eq!(reported_message, message, "{text}");
                }
                other => panic!("expected InvalidSpec, got {other:?}"),
            }
        }
        assert!(matches!(
            document_version(&json!({ "openapi": "3.0.0", "paths": {} }), path),
            Ok(Version::OpenApi3)
        ));
    }

    #[test]
    fn documents_parse_as_json_or_yaml() {
        let doc = parse_document(r#"{"openapi": "3.1.0"}"#, Path::new("spec.json"))
            .expect("JSON files parse as JSON");
        assert_eq!(doc, json!({ "openapi": "3.1.0" }));
        let doc = parse_document("openapi: 3.1.0\npaths: {}\n", Path::new("spec.yaml"))
            .expect("YAML files parse as YAML");
        assert_eq!(doc, json!({ "openapi": "3.1.0", "paths": {} }));
        let doc = parse_document("{\"openapi\": \"3.1.0\"}", Path::new("spec"))
            .expect("extension-less files parse as YAML");
        assert_eq!(doc, json!({ "openapi": "3.1.0" }));

        let error =
            parse_document("{", Path::new("spec.json")).expect_err("broken JSON is rejected");
        assert!(error.to_string().contains("invalid JSON: "), "{error}");
        let error =
            parse_document("openapi: [", Path::new("spec.yaml")).expect_err("broken YAML fails");
        assert!(error.to_string().contains("invalid YAML: "), "{error}");
    }

    #[test]
    fn every_generated_request_validates() {
        let doc: Value = serde_json::from_str(
            r#"{
                "openapi": "3.1.0",
                "servers": [{ "url": "http://127.0.0.1:4000" }],
                "paths": { "/ships/{id}": {
                    "get": {
                        "operationId": "getShip",
                        "tags": ["ships", "extra"],
                        "summary": "  Get a ship  ",
                        "parameters": [
                            { "in": "path", "name": "id", "required": true, "schema": { "type": "string" } },
                            { "in": "query", "name": "sort", "schema": { "type": "string" } },
                            { "in": "header", "name": "X-Trace", "schema": { "type": "string" } }
                        ],
                        "requestBody": { "content": { "application/json": { "schema": {
                            "type": "object",
                            "properties": { "flags": { "type": "array" } }
                        } } } }
                    }
                } }
            }"#,
        )
        .expect("test document is JSON");
        let version = document_version(&doc, Path::new("spec.json")).expect("version");
        let operations =
            operations(&doc, version, Path::new("spec.json")).expect("operations parse");
        let mut used = BTreeSet::new();
        let (relative, value) = request_file(&operations[0], &doc, &mut used);
        assert_eq!(relative, "requests/ships/getShip.json");
        assert_eq!(
            value,
            json!({
                "name": "Get a ship",
                "method": "GET",
                "path": "/ships/${id}?sort=${sort}",
                "headers": { "X-Trace": "${X-Trace}" },
                "body": { "flags": [] }
            })
        );
        let id = relative
            .strip_prefix("requests/")
            .and_then(|rest| rest.strip_suffix(".json"))
            .expect("generated path has the documented shape");
        request::validate(id, Path::new(&relative), &value).expect("generated request is valid");
    }
}
