//! The persisted capture store: `.curlyfries/session.json`.
//!
//! Variables are keyed by scope: the environment name, or `(none)` when no
//! environment is selected. Reads and writes are strict: an unreadable or
//! unknown-version file is an error rather than a silent reset.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

use crate::Map;
use crate::error::Error;
use crate::variables::{self, Variables};

/// Directory holding the session file, relative to the project root.
pub const SESSION_DIR: &str = ".curlyfries";
/// Name of the session file.
pub const SESSION_FILE: &str = "session.json";
/// Format version written by this tool.
pub const SESSION_VERSION: u32 = 2;
/// Scope key used when no environment is selected.
pub const NO_ENV_KEY: &str = "(none)";

static EMPTY_VARIABLES: LazyLock<Variables> = LazyLock::new(Variables::default);

/// In-memory view of the session file.
#[derive(Debug)]
pub struct Session {
    /// Path the session would be written to.
    pub path: PathBuf,
    scopes: BTreeMap<String, Variables>,
}

/// On-disk shape of the session file.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionFile {
    version: u32,
    updated_at_unix: u64,
    var_scopes: BTreeMap<String, Value>,
}

impl Session {
    /// Loads the session for `root`, or an empty store when no file exists.
    pub fn load(root: &Path) -> Result<Session, Error> {
        let path = root.join(SESSION_DIR).join(SESSION_FILE);
        let scopes = if path.is_file() {
            read_scopes(&path)?
        } else {
            BTreeMap::new()
        };
        Ok(Session { path, scopes })
    }

    /// Variables stored in `scope`, empty when the scope is unknown.
    pub fn variables(&self, scope: &str) -> &Variables {
        self.scopes.get(scope).unwrap_or(&EMPTY_VARIABLES)
    }

    /// The variable values stored in `scope`, empty when the scope is unknown.
    pub fn vars(&self, scope: &str) -> &Map {
        &self.variables(scope).vars
    }

    /// Merges `vars` into `scope`, returning whether anything was stored.
    ///
    /// An overwrite updates the secret flag along with the value.
    pub fn set(&mut self, scope: &str, vars: &Variables) -> bool {
        if vars.is_empty() {
            return false;
        }
        let entry = self.scopes.entry(scope.to_string()).or_default();
        for (name, value) in &vars.vars {
            entry.insert(
                name.clone(),
                value.clone(),
                vars.secrets.contains(name.as_str()),
            );
        }
        true
    }

    /// Removes one scope (or every scope), returning how many variables went.
    pub fn clear(&mut self, scope: Option<&str>) -> usize {
        match scope {
            Some(scope) => self
                .scopes
                .remove(scope)
                .map(|vars| vars.vars.len())
                .unwrap_or(0),
            None => {
                let removed = self.scopes.values().map(|vars| vars.vars.len()).sum();
                self.scopes.clear();
                removed
            }
        }
    }

    /// Writes the session file atomically (temp file plus rename).
    pub fn save(&self) -> Result<(), Error> {
        if let Some(dir) = self.path.parent()
            && !dir.as_os_str().is_empty()
        {
            fs::create_dir_all(dir).map_err(|source| Error::Io {
                path: Some(dir.to_path_buf()),
                source,
            })?;
        }
        let file = SessionFile {
            version: SESSION_VERSION,
            updated_at_unix: now_unix(),
            var_scopes: self
                .scopes
                .iter()
                .map(|(scope, vars)| (scope.clone(), Value::Object(encode_scope(vars))))
                .collect(),
        };
        let mut text = serde_json::to_string_pretty(&file).expect("session JSON serializes");
        text.push('\n');
        let temp = self
            .path
            .with_file_name(format!("{SESSION_FILE}.tmp-{}", std::process::id()));
        fs::write(&temp, text).map_err(|source| Error::Io {
            path: Some(temp.clone()),
            source,
        })?;
        fs::rename(&temp, &self.path).map_err(|source| Error::Io {
            path: Some(self.path.clone()),
            source,
        })
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Serializes one scope: a secret name is written as a `secret` wrapper.
fn encode_scope(vars: &Variables) -> Map {
    vars.vars
        .iter()
        .map(|(name, value)| {
            let value = if vars.secrets.contains(name) {
                json!({ "secret": true, "value": value })
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect()
}

fn read_scopes(path: &Path) -> Result<BTreeMap<String, Variables>, Error> {
    let unreadable = |reason: String| Error::SessionFile {
        path: path.to_path_buf(),
        reason,
        hint: true,
    };
    let text = fs::read_to_string(path).map_err(|source| Error::Io {
        path: Some(path.to_path_buf()),
        source,
    })?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|source| unreadable(format!("invalid JSON: {source}")))?;
    let Value::Object(map) = &value else {
        return Err(unreadable("top-level value must be an object".to_string()));
    };
    for key in map.keys() {
        if !["version", "updatedAtUnix", "varScopes"].contains(&key.as_str()) {
            return Err(unreadable(format!(
                "unknown key `{key}` (allowed: version, updatedAtUnix, varScopes)"
            )));
        }
    }
    match map.get("version") {
        Some(Value::Number(version)) if version.as_u64() == Some(SESSION_VERSION as u64) => {}
        Some(Value::Number(version)) => {
            return Err(unreadable(format!(
                "unsupported version {} (expected {SESSION_VERSION})",
                version
            )));
        }
        _ => {
            return Err(unreadable(format!(
                "missing or invalid `version` (expected {SESSION_VERSION})"
            )));
        }
    }
    match map.get("updatedAtUnix") {
        Some(Value::Number(value)) if value.as_u64().is_some() => {}
        _ => {
            return Err(unreadable("missing or invalid `updatedAtUnix`".to_string()));
        }
    }
    let mut scopes = BTreeMap::new();
    match map.get("varScopes") {
        Some(Value::Object(var_scopes)) => {
            for (scope, vars) in var_scopes {
                let Some(vars) = vars.as_object() else {
                    return Err(unreadable(format!("`varScopes.{scope}` must be an object")));
                };
                let split = variables::split_document(&format!("/varScopes/{scope}"), vars)
                    .map_err(|p| unreadable(format!("{}: {}", p.pointer, p.message)))?;
                scopes.insert(scope.clone(), split);
            }
        }
        _ => return Err(unreadable("missing or invalid `varScopes`".to_string())),
    }
    Ok(scopes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use serde_json::json;

    fn map(pairs: &[(&str, Value)]) -> Map {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn vars(pairs: &[(&str, Value)]) -> Variables {
        Variables::from_values(map(pairs))
    }

    /// A scope with one secret value among the plain ones.
    fn vars_with_secret(pairs: &[(&str, Value)], secret: &str, secret_value: Value) -> Variables {
        let mut variables = vars(pairs);
        variables.insert(secret.to_string(), secret_value, true);
        variables
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = TempDir::new("session-empty");
        let session = Session::load(dir.path()).unwrap();
        assert!(session.vars("dev").is_empty());
        assert!(session.path.ends_with(".curlyfries/session.json"));
    }

    #[test]
    fn save_and_load_round_trip_per_scope() {
        let dir = TempDir::new("session-round");
        let mut session = Session::load(dir.path()).unwrap();
        assert!(session.set(
            "dev",
            &vars_with_secret(&[("shipId", json!(6))], "token", json!("abc"))
        ));
        assert!(session.set(NO_ENV_KEY, &vars(&[("loose", json!(true))])));
        session.save().unwrap();

        let text = fs::read_to_string(&session.path).unwrap();
        assert!(text.ends_with("\n"), "{text}");
        assert!(text.contains("\"version\": 2"), "{text}");
        assert!(text.contains("\"varScopes\""), "{text}");
        assert!(text.contains("\"updatedAtUnix\""), "{text}");
        // A secret capture is stored wrapped; a plain one stays bare.
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed["varScopes"]["dev"]["token"],
            json!({ "secret": true, "value": "abc" })
        );
        assert_eq!(parsed["varScopes"]["dev"]["shipId"], json!(6));
        assert!(
            !session
                .path
                .with_file_name(format!("{SESSION_FILE}.tmp-{}", std::process::id()))
                .exists(),
            "temp file must be renamed away"
        );

        let reloaded = Session::load(dir.path()).unwrap();
        assert_eq!(reloaded.vars("dev")["token"], json!("abc"));
        assert_eq!(reloaded.vars("dev")["shipId"], json!(6));
        assert!(reloaded.variables("dev").is_secret("token"));
        assert!(!reloaded.variables("dev").is_secret("shipId"));
        assert_eq!(reloaded.vars(NO_ENV_KEY)["loose"], json!(true));
        assert!(!reloaded.variables(NO_ENV_KEY).is_secret("loose"));
        assert!(reloaded.vars("stage").is_empty());

        let stored: Value = serde_json::from_str(&text).unwrap();
        let updated = stored["updatedAtUnix"].as_u64().unwrap();
        assert!(updated > 1_700_000_000, "implausible timestamp {updated}");
    }

    #[test]
    fn a_secret_capture_wrapper_is_read_and_a_bad_one_is_reported() {
        let dir = TempDir::new("session-secret-parse");
        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 2, "updatedAtUnix": 1, "varScopes": {
              "dev": { "token": { "secret": true, "value": "tok-123" }, "role": "admin" }
            } }"#,
        );
        let session = Session::load(dir.path()).unwrap();
        assert_eq!(session.vars("dev")["token"], json!("tok-123"));
        assert!(session.variables("dev").is_secret("token"));
        assert!(!session.variables("dev").is_secret("role"));

        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 2, "updatedAtUnix": 1, "varScopes": {
              "dev": { "token": { "secret": true } }
            } }"#,
        );
        let err = Session::load(dir.path()).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("/varScopes/dev/token: a secret value needs a `value` key"),
            "{text}"
        );
    }

    #[test]
    fn unsupported_version_and_unknown_field_are_errors() {
        let dir = TempDir::new("session-version");
        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 1, "updatedAtUnix": 1, "varScopes": {} }"#,
        );
        let err = Session::load(dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported version 1 (expected 2)"),
            "{err}"
        );
        assert!(err.to_string().contains("press x"), "{err}");
        assert_eq!(err.exit_code(), 1);

        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 2, "updatedAtUnix": 1, "varScopes": {}, "extra": 1 }"#,
        );
        let err = Session::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("unknown key `extra`"), "{err}");

        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 2, "updatedAtUnix": 1, "varScopes": { "dev": 3 } }"#,
        );
        let err = Session::load(dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("`varScopes.dev` must be an object"),
            "{err}"
        );

        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": "2", "updatedAtUnix": 1, "varScopes": {} }"#,
        );
        let err = Session::load(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("missing or invalid `version`"),
            "{err}"
        );

        dir.write(".curlyfries/session.json", "{");
        let err = Session::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "{err}");
        assert!(err.to_string().contains("session.json"), "{err}");
    }

    #[test]
    fn clear_removes_one_scope_or_all() {
        let dir = TempDir::new("session-clear");
        let mut session = Session::load(dir.path()).unwrap();
        session.set("dev", &vars(&[("a", json!(1)), ("b", json!(2))]));
        session.set("stage", &vars(&[("c", json!(3))]));
        assert_eq!(session.clear(Some("dev")), 2);
        assert!(session.vars("dev").is_empty());
        assert_eq!(session.vars("stage")["c"], json!(3));
        assert_eq!(session.clear(Some("nope")), 0);
        assert_eq!(session.clear(None), 1);
        assert!(session.vars("stage").is_empty());
    }

    #[test]
    fn set_reports_whether_anything_changed() {
        let dir = TempDir::new("session-set");
        let mut session = Session::load(dir.path()).unwrap();
        assert!(!session.set("dev", &Variables::default()));
        assert!(session.set("dev", &vars(&[("a", json!(1))])));
        assert!(session.set("dev", &vars(&[("a", json!(1))])));
    }

    #[test]
    fn set_overwrites_the_secret_flag() {
        let dir = TempDir::new("session-secret-overwrite");
        let mut session = Session::load(dir.path()).unwrap();
        session.set("dev", &vars_with_secret(&[], "token", json!("a")));
        assert!(session.variables("dev").is_secret("token"));
        session.set("dev", &vars(&[("token", json!("b"))]));
        assert!(!session.variables("dev").is_secret("token"));
        assert_eq!(session.vars("dev")["token"], json!("b"));
    }
}
