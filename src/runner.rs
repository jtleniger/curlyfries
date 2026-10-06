//! The single execution path shared by `run` and interactive mode.

use std::time::Duration;

use crate::Map;
use crate::error::Error;
use crate::execute::{self, ExecResult, ResolvedRequest};
use crate::project::{Entry, Project};
use crate::request;
use crate::scopes::Scopes;
use crate::session::{self, Session};

/// Everything needed to execute requests against one project.
pub struct Runner<'a> {
    /// Project being executed against.
    pub project: &'a Project,
    /// Requests available in the project (sorted by id).
    pub entries: &'a [Entry],
    /// Selected environment name, if any.
    pub env_name: Option<String>,
    /// Variables from the selected environment.
    pub env: Map,
    /// `--var` overrides.
    pub overrides: Map,
    /// Capture store, persisted between invocations.
    pub session: &'a mut Session,
    /// HTTP agent.
    pub client: ureq::Agent,
    /// Per-request timeout; `None` disables timeouts.
    pub timeout: Option<Duration>,
    /// Non-fatal problems, drained by the caller and printed to stderr.
    pub warnings: Vec<String>,
}

/// A successful exchange plus everything captured from it.
#[derive(Debug)]
pub struct RunOutcome {
    /// The request as sent.
    pub request: ResolvedRequest,
    /// The response as received.
    pub response: ExecResult,
    /// Variables captured from this response.
    pub captured: Map,
    /// Why `outputs` evaluation failed, if it did. When set, `captured` is empty.
    pub capture_error: Option<Error>,
}

impl Runner<'_> {
    /// Resolves, sends and captures one request id.
    pub fn execute_id(&mut self, id: &str) -> Result<RunOutcome, Error> {
        let entry = find_entry(self.entries, id)?;
        let def = request::load_request(&entry.id, &entry.file)?;
        let scope = self
            .env_name
            .clone()
            .unwrap_or_else(|| session::NO_ENV_KEY.to_string());

        let resolved = {
            let scopes = Scopes {
                env_name: self.env_name.as_deref(),
                env: &self.env,
                session: self.session.vars(&scope),
                overrides: &self.overrides,
            };
            execute::render_request(&def, &scopes)?
        };

        let response = execute::execute(&self.client, &resolved, self.timeout)?;
        let (captured, capture_error) = match execute::capture(&def.outputs, &response, &def.file) {
            Ok(captured) => (captured, None),
            Err(error) => (Map::new(), Some(error)),
        };

        if !captured.is_empty() {
            self.session.set(&scope, captured.clone());
            if let Err(error) = self.session.save() {
                let path = self.session.path.display();
                let reason = match &error {
                    Error::Io { source, .. } => source.to_string(),
                    other => other.to_string(),
                };
                self.warnings.push(format!(
                    "warning: could not write session file {path}: {reason}"
                ));
            }
        }

        Ok(RunOutcome {
            request: resolved,
            response,
            captured,
            capture_error,
        })
    }
}

/// Finds a request by id, accepting an optional trailing `.json`.
///
/// An exact id always wins, so a request file named `a.json.json` (id `a.json`)
/// stays reachable by the id `list` prints.
pub fn find_entry<'a>(entries: &'a [Entry], id: &str) -> Result<&'a Entry, Error> {
    let by_id = |wanted: &str| entries.iter().find(|entry| entry.id == wanted);
    let found = by_id(id).or_else(|| id.strip_suffix(".json").and_then(by_id));
    found.ok_or_else(|| Error::UnknownRequest {
        id: id.to_string(),
        available: entries.iter().map(|entry| entry.id.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entries(ids: &[&str]) -> Vec<Entry> {
        ids.iter()
            .map(|id| Entry {
                id: (*id).to_string(),
                file: PathBuf::from(format!("requests/{id}.json")),
            })
            .collect()
    }

    #[test]
    fn ids_accept_an_optional_json_suffix() {
        let all = entries(&["auth/login", "ships/create"]);
        assert_eq!(find_entry(&all, "ships/create").unwrap().id, "ships/create");
        assert_eq!(
            find_entry(&all, "ships/create.json").unwrap().id,
            "ships/create"
        );
    }

    #[test]
    fn ids_ending_in_json_resolve_exactly() {
        let all = entries(&["a.json", "b"]);
        assert_eq!(find_entry(&all, "a.json").unwrap().id, "a.json");
        assert_eq!(find_entry(&all, "a.json.json").unwrap().id, "a.json");
        assert_eq!(find_entry(&all, "b.json").unwrap().id, "b");
        let err = find_entry(&all, "a").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("unknown request `a`"), "{text}");
    }

    #[test]
    fn unknown_ids_list_every_available_id() {
        let all = entries(&["auth/login", "ships/create"]);
        let err = find_entry(&all, "ships/crete").unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("error: unknown request `ships/crete`"),
            "{text}"
        );
        assert!(
            text.contains("available: auth/login, ships/create"),
            "{text}"
        );
        assert_eq!(err.exit_code(), 3);
    }
}
