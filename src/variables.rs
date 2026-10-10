//! Variable entries: literal values plus the secret wrapper.
//!
//! A variable entry (an environment value, an `outputs` entry, or a session
//! capture) is either a bare JSON value or a *secret wrapper*
//! `{ "secret": true, "value": <any JSON> }`. A wrapper carries the value
//! unchanged; only the terminal UI withholds it, printing [`SECRET_MASK`]
//! instead. The detection rule is deliberately blunt: **an object with a
//! top-level `secret` key is a wrapper**, so a literal object value can never
//! itself carry a top-level `secret` key.

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
    let Value::Object(wrapper) = value else {
        return Ok((value.clone(), false));
    };
    let Some(secret) = wrapper.get("secret") else {
        return Ok((value.clone(), false));
    };
    if let Some(key) = wrapper
        .keys()
        .find(|key| *key != "secret" && *key != "value")
    {
        return Err(EntryProblem {
            pointer: format!("{pointer}/{}", pointer_token(key)),
            message: format!("unknown key `{key}` in a secret value (allowed: secret, value)"),
        });
    }
    let secret = match secret {
        Value::Bool(flag) => *flag,
        _ => {
            return Err(EntryProblem {
                pointer: format!("{pointer}/secret"),
                message: "`secret` must be a boolean".to_string(),
            });
        }
    };
    let Some(value) = wrapper.get("value") else {
        return Err(EntryProblem {
            pointer: pointer.to_string(),
            message: "a secret value needs a `value` key".to_string(),
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
    fn bare_values_stay_literal_and_plain() {
        for value in [
            json!("text"),
            json!(7),
            json!(true),
            json!(null),
            json!([1, 2]),
            json!({ "username": "silver" }),
        ] {
            let (split, secret) = split_entry("/x", &value).unwrap();
            assert_eq!(split, value);
            assert!(!secret, "{value} must not be secret");
        }
    }

    #[test]
    fn wrappers_unwrap_and_carry_the_flag() {
        let (value, secret) =
            split_entry("/pirate", &json!({ "secret": true, "value": { "a": 1 } })).unwrap();
        assert_eq!(value, json!({ "a": 1 }));
        assert!(secret);

        // `secret: false` still unwraps the value; it is just not secret.
        let (value, secret) =
            split_entry("/x", &json!({ "secret": false, "value": "abc" })).unwrap();
        assert_eq!(value, json!("abc"));
        assert!(!secret);

        // A null value is allowed.
        let (value, secret) = split_entry("/x", &json!({ "secret": true, "value": null })).unwrap();
        assert_eq!(value, Value::Null);
        assert!(secret);
    }

    #[test]
    fn wrapper_problems_name_the_pointer() {
        let unknown =
            split_entry("/pirate", &json!({ "secret": true, "value": 1, "nope": 2 })).unwrap_err();
        assert_eq!(unknown.pointer, "/pirate/nope");
        assert_eq!(
            unknown.message,
            "unknown key `nope` in a secret value (allowed: secret, value)"
        );

        let not_bool = split_entry("/x", &json!({ "secret": "yes", "value": 1 })).unwrap_err();
        assert_eq!(not_bool.pointer, "/x/secret");
        assert_eq!(not_bool.message, "`secret` must be a boolean");

        let no_value = split_entry("/x", &json!({ "secret": true })).unwrap_err();
        assert_eq!(no_value.pointer, "/x");
        assert_eq!(no_value.message, "a secret value needs a `value` key");
    }

    #[test]
    fn a_top_level_secret_key_is_a_wrapper_not_a_literal() {
        // `{"secret": true}` looks like a literal object but is read as a
        // (malformed) wrapper, by the blunt detection rule.
        assert!(split_entry("/x", &json!({ "secret": true })).is_err());
        assert!(split_entry("/x", &json!({ "secret": false, "other": 1 })).is_err());
    }

    #[test]
    fn split_document_keeps_file_order_and_flags() {
        let variables = split_document(
            "",
            &map(&[
                ("limit", json!(2)),
                (
                    "pirate",
                    json!({ "secret": true, "value": { "username": "silver" } }),
                ),
                ("admiral", json!({ "secret": true, "value": "flint" })),
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
