//! Variable scopes and their precedence.
//!
//! Lookup order for `${name}` is `--var` overrides, then the session store
//! (persisted captures plus captures from this invocation), then the selected
//! environment.

use serde_json::Value;

use crate::Map;

/// The three variable scopes, borrowed for the duration of one render.
pub struct Scopes<'a> {
    /// Active environment name, if one is selected.
    pub env_name: Option<&'a str>,
    /// Variables from the selected environment.
    pub env: &'a Map,
    /// Variables captured in this scope (this invocation sees its own writes).
    pub session: &'a Map,
    /// `--var NAME=VALUE` overrides.
    pub overrides: &'a Map,
}

impl<'a> Scopes<'a> {
    /// Resolves a variable name across the three scopes.
    pub fn lookup(&self, name: &str) -> Option<&'a Value> {
        self.overrides
            .get(name)
            .or_else(|| self.session.get(name))
            .or_else(|| self.env.get(name))
    }

    /// A closure version of [`Scopes::lookup`] for the templating engine.
    pub fn lookup_fn(&self) -> impl Fn(&str) -> Option<Value> + '_ {
        let this = self;
        move |name: &str| this.lookup(name).cloned()
    }

    /// Sorted variable names from the environment scope.
    pub fn env_keys(&self) -> Vec<String> {
        sorted_keys(self.env)
    }

    /// Sorted variable names from the session scope.
    pub fn session_keys(&self) -> Vec<String> {
        sorted_keys(self.session)
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

    fn map(pairs: &[(&str, Value)]) -> Map {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn overrides_beat_session_which_beats_environment() {
        let env = map(&[("token", json!("env")), ("limit", json!(10))]);
        let session = map(&[("token", json!("session")), ("shipId", json!(3))]);
        let overrides = map(&[("token", json!("override"))]);
        let scopes = Scopes {
            env_name: Some("dev"),
            env: &env,
            session: &session,
            overrides: &overrides,
        };
        assert_eq!(scopes.lookup("token"), Some(&json!("override")));
        assert_eq!(scopes.lookup("limit"), Some(&json!(10)));
        assert_eq!(scopes.lookup("shipId"), Some(&json!(3)));
        assert_eq!(scopes.lookup("missing"), None);

        let lookup = scopes.lookup_fn();
        assert_eq!(lookup("token"), Some(json!("override")));
        assert_eq!(lookup("nope"), None);

        assert_eq!(scopes.env_keys(), vec!["limit", "token"]);
        assert_eq!(scopes.session_keys(), vec!["shipId", "token"]);
    }
}
