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
use serde_json::Value;

use crate::Map;
use crate::error::Error;

/// Directory holding the session file, relative to the project root.
pub const SESSION_DIR: &str = ".curlyfries";
/// Name of the session file.
pub const SESSION_FILE: &str = "session.json";
/// Format version written by this tool.
pub const SESSION_VERSION: u32 = 1;
/// Scope key used when no environment is selected.
pub const NO_ENV_KEY: &str = "(none)";

static EMPTY: LazyLock<Map> = LazyLock::new(Map::new);

/// In-memory view of the session file.
#[derive(Debug)]
pub struct Session {
    /// Path the session would be written to.
    pub path: PathBuf,
    scopes: BTreeMap<String, Map>,
    persist: bool,
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
    /// Loads the session for `root`. With `persist == false` (`--no-session`)
    /// nothing is read and nothing will ever be written.
    pub fn load(root: &Path, persist: bool) -> Result<Session, Error> {
        let path = root.join(SESSION_DIR).join(SESSION_FILE);
        if !persist {
            return Ok(Session {
                path,
                scopes: BTreeMap::new(),
                persist: false,
            });
        }
        let scopes = if path.is_file() {
            read_scopes(&path)?
        } else {
            BTreeMap::new()
        };
        Ok(Session {
            path,
            scopes,
            persist: true,
        })
    }

    /// Variables stored in `scope`, empty when the scope is unknown.
    pub fn vars(&self, scope: &str) -> &Map {
        self.scopes.get(scope).unwrap_or(&EMPTY)
    }

    /// Merges `vars` into `scope`, returning whether anything was stored.
    pub fn set(&mut self, scope: &str, vars: Map) -> bool {
        if vars.is_empty() {
            return false;
        }
        let entry = self.scopes.entry(scope.to_string()).or_default();
        for (name, value) in vars {
            entry.insert(name, value);
        }
        true
    }

    /// Removes one scope (or every scope), returning how many variables went.
    pub fn clear(&mut self, scope: Option<&str>) -> usize {
        match scope {
            Some(scope) => self
                .scopes
                .remove(scope)
                .map(|vars| vars.len())
                .unwrap_or(0),
            None => {
                let removed = self.scopes.values().map(|vars| vars.len()).sum();
                self.scopes.clear();
                removed
            }
        }
    }

    /// Writes the session file atomically (temp file plus rename).
    pub fn save(&self) -> Result<(), Error> {
        if !self.persist {
            return Ok(());
        }
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
                .map(|(scope, vars)| (scope.clone(), Value::Object(vars.clone())))
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

fn read_scopes(path: &Path) -> Result<BTreeMap<String, Map>, Error> {
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
                scopes.insert(scope.clone(), vars.clone());
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

    #[test]
    fn missing_file_loads_empty() {
        let dir = TempDir::new("session-empty");
        let session = Session::load(dir.path(), true).unwrap();
        assert!(session.vars("dev").is_empty());
        assert!(session.path.ends_with(".curlyfries/session.json"));
    }

    #[test]
    fn save_and_load_round_trip_per_scope() {
        let dir = TempDir::new("session-round");
        let mut session = Session::load(dir.path(), true).unwrap();
        assert!(session.set("dev", map(&[("token", json!("abc")), ("shipId", json!(6))])));
        assert!(session.set(NO_ENV_KEY, map(&[("loose", json!(true))])));
        session.save().unwrap();

        let text = fs::read_to_string(&session.path).unwrap();
        assert!(text.ends_with("\n"), "{text}");
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("\"varScopes\""), "{text}");
        assert!(text.contains("\"updatedAtUnix\""), "{text}");
        assert!(
            !session
                .path
                .with_file_name(format!("{SESSION_FILE}.tmp-{}", std::process::id()))
                .exists(),
            "temp file must be renamed away"
        );

        let reloaded = Session::load(dir.path(), true).unwrap();
        assert_eq!(reloaded.vars("dev")["token"], json!("abc"));
        assert_eq!(reloaded.vars("dev")["shipId"], json!(6));
        assert_eq!(reloaded.vars(NO_ENV_KEY)["loose"], json!(true));
        assert!(reloaded.vars("stage").is_empty());

        let stored: Value = serde_json::from_str(&text).unwrap();
        let updated = stored["updatedAtUnix"].as_u64().unwrap();
        assert!(updated > 1_700_000_000, "implausible timestamp {updated}");
    }

    #[test]
    fn no_session_never_reads_or_writes() {
        let dir = TempDir::new("session-none");
        let mut session = Session::load(dir.path(), true).unwrap();
        session.set("dev", map(&[("token", json!("abc"))]));
        session.save().unwrap();

        let mut disabled = Session::load(dir.path(), false).unwrap();
        assert!(disabled.vars("dev").is_empty());
        assert!(disabled.set("dev", map(&[("token", json!("other"))])));
        disabled.save().unwrap();
        let stored = Session::load(dir.path(), true).unwrap();
        assert_eq!(stored.vars("dev")["token"], json!("abc"));
    }

    #[test]
    fn unsupported_version_and_unknown_field_are_errors() {
        let dir = TempDir::new("session-version");
        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 2, "updatedAtUnix": 1, "varScopes": {} }"#,
        );
        let err = Session::load(dir.path(), true).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported version 2 (expected 1)"),
            "{err}"
        );
        assert!(err.to_string().contains("session clear --all"), "{err}");
        assert_eq!(err.exit_code(), 1);

        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 1, "updatedAtUnix": 1, "varScopes": {}, "extra": 1 }"#,
        );
        let err = Session::load(dir.path(), true).unwrap_err();
        assert!(err.to_string().contains("unknown key `extra`"), "{err}");

        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": 1, "updatedAtUnix": 1, "varScopes": { "dev": 3 } }"#,
        );
        let err = Session::load(dir.path(), true).unwrap_err();
        assert!(
            err.to_string()
                .contains("`varScopes.dev` must be an object"),
            "{err}"
        );

        dir.write(
            ".curlyfries/session.json",
            r#"{ "version": "1", "updatedAtUnix": 1, "varScopes": {} }"#,
        );
        let err = Session::load(dir.path(), true).unwrap_err();
        assert!(
            err.to_string().contains("missing or invalid `version`"),
            "{err}"
        );

        dir.write(".curlyfries/session.json", "{");
        let err = Session::load(dir.path(), true).unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "{err}");
        assert!(err.to_string().contains("session.json"), "{err}");
    }

    #[test]
    fn clear_removes_one_scope_or_all() {
        let dir = TempDir::new("session-clear");
        let mut session = Session::load(dir.path(), true).unwrap();
        session.set("dev", map(&[("a", json!(1)), ("b", json!(2))]));
        session.set("stage", map(&[("c", json!(3))]));
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
        let mut session = Session::load(dir.path(), true).unwrap();
        assert!(!session.set("dev", Map::new()));
        assert!(session.set("dev", map(&[("a", json!(1))])));
        assert!(session.set("dev", map(&[("a", json!(1))])));
    }
}
