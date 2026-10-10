//! Variable entries: a value wrapped in an object.
//!
//! A variable entry (an environment value, an `outputs` entry, or a session
//! capture) is always an object holding a required `value` key and an optional
//! boolean `secret` key: `{ "value": <any JSON> }` or
//! `{ "value": <any JSON>, "secret": true }`. The value is used unchanged; only
//! the terminal UI withholds a secret, printing [`SECRET_MASK`] instead.
//!
//! Because `value` is required and unknown keys are refused, a literal object
//! value is nested under `value` — `{ "value": { "secret": 1 } }` stores the
//! object `{ "secret": 1 }` rather than being read as a wrapper.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::Map;

/// Mask shown in place of a secret value in the TUI.
pub const SECRET_MASK: &str = "••••••";

/// Variables plus the names among them that are secret.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Variables {
    /// The variable values.
    pub vars: Map,
    /// Names whose value is secret and must not be echoed.
    pub secrets: BTreeSet<String>,
}

/// A malformed variable entry, located by a JSON pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryProblem {
    /// JSON pointer of the offending entry or key.
    pub pointer: String,
    /// What is wrong with it.
    pub message: String,
}

impl Variables {
    /// Wraps a plain variable map, with no secret names.
    pub fn from_values(vars: Map) -> Variables {
        Variables {
            vars,
            secrets: BTreeSet::new(),
        }
    }

    /// The value stored under `name`.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.vars.get(name)
    }

    /// Whether `name` is present and secret.
    pub fn is_secret(&self, name: &str) -> bool {
        self.secrets.contains(name)
    }

    /// Whether no variables are present.
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// Stores `value` under `name`, setting or clearing its secret flag.
    pub fn insert(&mut self, name: String, value: Value, secret: bool) {
        if secret {
            self.secrets.insert(name.clone());
        } else {
            self.secrets.remove(&name);
        }
        self.vars.insert(name, value);
    }
}

/// Escapes a JSON pointer token (`~` → `~0`, `/` → `~1`).
pub fn pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Splits one entry into its value and whether it is secret.
///
/// `pointer` is the entry's JSON pointer (e.g. `/pirate`).
pub fn split_entry(pointer: &str, value: &Value) -> Result<(Value, bool), EntryProblem> {
    let Value::Object(obj) = value else {
        return Err(EntryProblem {
            pointer: pointer.to_string(),
            message: "must be an object with a `value` key (allowed: secret, value)".to_string(),
        });
    };
    if let Some(key) = obj.keys().find(|k| *k != "secret" && *k != "value") {
        return Err(EntryProblem {
            pointer: format!("{pointer}/{}", pointer_token(key)),
            message: format!("unknown key `{key}` (allowed: secret, value)"),
        });
    }
    let secret = match obj.get("secret") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => {
            return Err(EntryProblem {
                pointer: format!("{pointer}/secret"),
                message: "`secret` must be a boolean".to_string(),
            });
        }
    };
    let Some(value) = obj.get("value") else {
        return Err(EntryProblem {
            pointer: pointer.to_string(),
            message: "missing `value` key".to_string(),
        });
    };
    Ok((value.clone(), secret))
}

/// Splits a whole object of entries, keys in file order.
///
/// Entry pointers are `{prefix}/{escaped_key}`.
pub fn split_document(prefix: &str, map: &Map) -> Result<Variables, EntryProblem> {
    let mut variables = Variables::default();
    for (name, value) in map {
        let pointer = format!("{prefix}/{}", pointer_token(name));
        let (value, secret) = split_entry(&pointer, value)?;
        variables.insert(name.clone(), value, secret);
    }
    Ok(variables)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(pairs: &[(&str, Value)]) -> Map {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn entries_are_objects_holding_a_value() {
        let (value, secret) = split_entry("/x", &json!({ "value": "text" })).unwrap();
        assert_eq!(value, json!("text"));
        assert!(!secret);

        let (value, secret) =
            split_entry("/pirate", &json!({ "value": { "a": 1 }, "secret": true })).unwrap();
        assert_eq!(value, json!({ "a": 1 }));
        assert!(secret);

        // `secret: false` is allowed and behaves like the absent flag.
        let (value, secret) =
            split_entry("/x", &json!({ "secret": false, "value": "abc" })).unwrap();
        assert_eq!(value, json!("abc"));
        assert!(!secret);

        // A null value is allowed.
        let (value, secret) = split_entry("/x", &json!({ "value": null, "secret": true })).unwrap();
        assert_eq!(value, Value::Null);
        assert!(secret);

        // Arrays and nested objects survive the unwrap.
        let (value, _) = split_entry("/x", &json!({ "value": [1, { "a": true }] })).unwrap();
        assert_eq!(value, json!([1, { "a": true }]));
    }

    #[test]
    fn a_literal_object_value_nests_under_value() {
        // `{"secret": 1}` is data, not a wrapper: the wrapper detection is gone.
        let (value, secret) = split_entry("/x", &json!({ "value": { "secret": 1 } })).unwrap();
        assert_eq!(value, json!({ "secret": 1 }));
        assert!(!secret);

        // ...and an object with a top-level `secret` key is now just a bad entry.
        let err = split_entry("/x", &json!({ "secret": true })).unwrap_err();
        assert_eq!(err.pointer, "/x");
        assert_eq!(err.message, "missing `value` key");
        let err = split_entry("/x", &json!({ "secret": false, "other": 1 })).unwrap_err();
        assert_eq!(err.pointer, "/x/other");
        assert_eq!(err.message, "unknown key `other` (allowed: secret, value)");
    }

    #[test]
    fn non_objects_are_rejected_by_pointer() {
        for value in [
            json!("text"),
            json!(7),
            json!(true),
            json!(null),
            json!([1, 2]),
        ] {
            let err = split_entry("/dev/x", &value).unwrap_err();
            assert_eq!(err.pointer, "/dev/x", "{value}");
            assert_eq!(
                err.message, "must be an object with a `value` key (allowed: secret, value)",
                "{value}"
            );
        }
    }

    #[test]
    fn entry_problems_name_the_pointer() {
        let unknown =
            split_entry("/pirate", &json!({ "value": 1, "secret": true, "nope": 2 })).unwrap_err();
        assert_eq!(unknown.pointer, "/pirate/nope");
        assert_eq!(
            unknown.message,
            "unknown key `nope` (allowed: secret, value)"
        );

        let not_bool = split_entry("/x", &json!({ "value": 1, "secret": "yes" })).unwrap_err();
        assert_eq!(not_bool.pointer, "/x/secret");
        assert_eq!(not_bool.message, "`secret` must be a boolean");

        let no_value = split_entry("/x", &json!({ "secret": true })).unwrap_err();
        assert_eq!(no_value.pointer, "/x");
        assert_eq!(no_value.message, "missing `value` key");
    }

    #[test]
    fn split_document_keeps_file_order_and_flags() {
        let variables = split_document(
            "",
            &map(&[
                ("limit", json!({ "value": 2 })),
                (
                    "pirate",
                    json!({ "secret": true, "value": { "username": "silver" } }),
                ),
                ("admiral", json!({ "value": "flint", "secret": true })),
            ]),
        )
        .unwrap();
        let names: Vec<&str> = variables.vars.keys().map(String::as_str).collect();
        assert_eq!(names, ["limit", "pirate", "admiral"]);
        assert_eq!(
            variables.get("pirate"),
            Some(&json!({ "username": "silver" }))
        );
        assert!(variables.is_secret("pirate"));
        assert!(variables.is_secret("admiral"));
        assert!(!variables.is_secret("limit"));
        assert_eq!(variables.secrets.len(), 2);
    }

    #[test]
    fn insert_sets_and_clears_the_secret_flag() {
        let mut variables = Variables::default();
        variables.insert("token".to_string(), json!("a"), true);
        assert!(variables.is_secret("token"));
        variables.insert("token".to_string(), json!("b"), false);
        assert!(!variables.is_secret("token"));
        assert_eq!(variables.get("token"), Some(&json!("b")));
        assert!(!variables.is_empty());
        assert!(Variables::default().is_empty());
    }

    #[test]
    fn pointer_tokens_escape_tildes_and_slashes() {
        assert_eq!(pointer_token("a/b"), "a~1b");
        assert_eq!(pointer_token("a~b"), "a~0b");
    }
}
