//! Variable scopes and their precedence.
//!
//! Lookup order for `${name}` is the session store (persisted captures plus
//! captures from this invocation), then the selected environment.

use serde_json::Value;

use crate::Map;
use crate::variables::{SECRET_MASK, Variables};

/// The variable scopes, borrowed for the duration of one render.
pub struct Scopes<'a> {
    /// Active environment name, if one is selected.
    pub env_name: Option<&'a str>,
    /// Variables from the selected environment.
    pub env: &'a Variables,
    /// Variables captured in this scope (this invocation sees its own writes).
    pub session: &'a Variables,
}

impl<'a> Scopes<'a> {
    /// Resolves a variable name across the scopes.
    pub fn lookup(&self, name: &str) -> Option<&'a Value> {
        self.session.get(name).or_else(|| self.env.get(name))
    }

    /// Whether the name that wins in [`Scopes::lookup`] is secret.
    ///
    /// A session capture of the same name wins over the environment entry,
    /// secret or not.
    pub fn is_secret(&self, name: &str) -> bool {
        if self.session.vars.contains_key(name) {
            self.session.secrets.contains(name)
        } else {
            self.env.secrets.contains(name)
        }
    }

    /// A closure version of [`Scopes::lookup`] for the templating engine.
    pub fn lookup_fn(&self) -> impl Fn(&str) -> Option<Value> + '_ {
        let this = self;
        move |name: &str| this.lookup(name).cloned()
    }

    /// A closure that resolves like [`Scopes::lookup_fn`] but replaces every
    /// secret value with [`SECRET_MASK`], for anything the UI echoes.
    pub fn display_lookup_fn(&self) -> impl Fn(&str) -> Option<Value> + '_ {
        let this = self;
        move |name: &str| match this.lookup(name) {
            Some(_) if this.is_secret(name) => Some(Value::String(SECRET_MASK.to_string())),
            other => other.cloned(),
        }
    }

    /// Sorted variable names from the environment scope.
    pub fn env_keys(&self) -> Vec<String> {
        sorted_keys(&self.env.vars)
    }

    /// Sorted variable names from the session scope.
    pub fn session_keys(&self) -> Vec<String> {
        sorted_keys(&self.session.vars)
    }
}

fn sorted_keys(map: &Map) -> Vec<String> {
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(pairs: &[(&str, Value)]) -> Variables {
        Variables::from_values(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn session_beats_environment() {
        let env = vars(&[("token", json!("env")), ("limit", json!(10))]);
        let session = vars(&[("token", json!("session")), ("shipId", json!(3))]);
        let scopes = Scopes {
            env_name: Some("dev"),
            env: &env,
            session: &session,
        };
        assert_eq!(scopes.lookup("token"), Some(&json!("session")));
        assert_eq!(scopes.lookup("limit"), Some(&json!(10)));
        assert_eq!(scopes.lookup("shipId"), Some(&json!(3)));
        assert_eq!(scopes.lookup("missing"), None);

        let lookup = scopes.lookup_fn();
        assert_eq!(lookup("token"), Some(json!("session")));
        assert_eq!(lookup("nope"), None);

        assert_eq!(scopes.env_keys(), vec!["limit", "token"]);
        assert_eq!(scopes.session_keys(), vec!["shipId", "token"]);
    }

    #[test]
    fn display_lookup_masks_secrets_and_keeps_json_types() {
        let mut env = vars(&[("limit", json!(10)), ("name", json!("dev"))]);
        env.insert("token".to_string(), json!("abc"), true);
        let mut session = vars(&[("captured", json!({ "a": 1 }))]);
        session.insert("apiKey".to_string(), json!("k"), true);
        let scopes = Scopes {
            env_name: Some("dev"),
            env: &env,
            session: &session,
        };

        let display = scopes.display_lookup_fn();
        // Secrets become the mask, whatever their type.
        assert_eq!(display("token"), Some(json!(SECRET_MASK)));
        assert_eq!(display("apiKey"), Some(json!(SECRET_MASK)));
        // Non-secrets pass through with their JSON type intact.
        assert_eq!(display("limit"), Some(json!(10)));
        assert_eq!(display("name"), Some(json!("dev")));
        assert_eq!(display("captured"), Some(json!({ "a": 1 })));
        assert_eq!(display("missing"), None);
        // The real lookup never masks.
        assert_eq!(scopes.lookup_fn()("token"), Some(json!("abc")));
    }

    #[test]
    fn secret_precedence_follows_the_winning_scope() {
        // A plain session capture of the same name hides a secret env entry.
        let mut env = vars(&[]);
        env.insert("token".to_string(), json!("env"), true);
        let session = vars(&[("token", json!("session"))]);
        let scopes = Scopes {
            env_name: Some("dev"),
            env: &env,
            session: &session,
        };
        assert!(!scopes.is_secret("token"));
        assert_eq!(scopes.display_lookup_fn()("token"), Some(json!("session")));

        // A secret session capture wins over a plain env entry.
        let env = vars(&[("token", json!("env"))]);
        let mut session = vars(&[]);
        session.insert("token".to_string(), json!("session"), true);
        let scopes = Scopes {
            env_name: Some("dev"),
            env: &env,
            session: &session,
        };
        assert!(scopes.is_secret("token"));
        assert_eq!(
            scopes.display_lookup_fn()("token"),
            Some(json!(SECRET_MASK))
        );

        // An env secret with no session entry still sticks.
        let mut env = vars(&[]);
        env.insert("baseUrl".to_string(), json!("http://h"), true);
        let session = vars(&[]);
        let scopes = Scopes {
            env_name: Some("dev"),
            env: &env,
            session: &session,
        };
        assert!(scopes.is_secret("baseUrl"));
    }
}
