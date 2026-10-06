//! Project discovery, request enumeration and environment resolution.
//!
//! A project root is the current directory (or `-C <dir>`) when it holds
//! `curlyfries.json`; no parent directories are searched.
//! Request ids are the file path relative to `requestsDir` without the `.json`
//! suffix; environment names are env file stems matching
//! `[A-Za-z0-9][A-Za-z0-9_-]*`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::Map;
use crate::error::Error;

/// The manifest file whose presence marks a project root.
pub const MANIFEST_FILE: &str = "curlyfries.json";
/// Default requests directory.
pub const DEFAULT_REQUESTS_DIR: &str = "requests";
/// Default environments directory.
pub const DEFAULT_ENVIRONMENTS_DIR: &str = "environments";
/// Manifest keys, in the order they are listed in errors.
pub const MANIFEST_KEYS: [&str; 3] = ["requestsDir", "environmentsDir", "defaultEnvironment"];

/// Parsed `curlyfries.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Directory holding request files, relative to the project root.
    pub requests_dir: String,
    /// Directory holding environment files, relative to the project root.
    pub environments_dir: String,
    /// Environment used when `--env` is absent.
    pub default_environment: Option<String>,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            requests_dir: DEFAULT_REQUESTS_DIR.to_string(),
            environments_dir: DEFAULT_ENVIRONMENTS_DIR.to_string(),
            default_environment: None,
        }
    }
}

/// A discovered project root plus its manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Absolute project root.
    pub root: PathBuf,
    /// Parsed manifest.
    pub manifest: Manifest,
}

impl Project {
    /// Absolute requests directory.
    pub fn requests_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.requests_dir)
    }

    /// Absolute environments directory.
    pub fn environments_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.environments_dir)
    }

    /// Absolute path of the environment file for `name`.
    pub fn environment_file(&self, name: &str) -> PathBuf {
        self.environments_dir().join(format!("{name}.json"))
    }

    /// Path of a file relative to the project root, using `/` separators.
    pub fn relative(&self, path: &Path) -> String {
        relative_slash(path, &self.root)
    }
}

/// One request file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Request id.
    pub id: String,
    /// Absolute path of the request file.
    pub file: PathBuf,
}

/// Finds the project root in `start` only, or uses `override_dir` verbatim.
///
/// Parent directories are never searched.
pub fn discover(start: &Path, override_dir: Option<&Path>) -> Result<Project, Error> {
    if let Some(dir) = override_dir {
        let root = dir.to_path_buf();
        let manifest_path = root.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            return Err(Error::ProjectNotFound { start: root });
        }
        let manifest = load_manifest(&manifest_path)?;
        return Ok(Project { root, manifest });
    }
    let start = start.canonicalize().map_err(|source| Error::Io {
        path: Some(start.to_path_buf()),
        source,
    })?;
    let manifest_path = start.join(MANIFEST_FILE);
    if manifest_path.is_file() {
        let manifest = load_manifest(&manifest_path)?;
        return Ok(Project {
            root: start,
            manifest,
        });
    }
    Err(Error::ProjectNotFound { start })
}

/// Reads and validates a manifest file.
pub fn load_manifest(path: &Path) -> Result<Manifest, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::Io {
        path: Some(path.to_path_buf()),
        source,
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|source| Error::InvalidJson {
        path: path.to_path_buf(),
        source,
    })?;
    let schema = |pointer: &str, message: String| Error::Schema {
        path: path.to_path_buf(),
        pointer: pointer.to_string(),
        message,
    };
    let Value::Object(map) = &value else {
        return Err(schema("", "manifest must be a JSON object".to_string()));
    };
    let mut unknown: Vec<&str> = map
        .keys()
        .map(|k| k.as_str())
        .filter(|k| !MANIFEST_KEYS.contains(k))
        .collect();
    unknown.sort_unstable();
    if let Some(key) = unknown.first() {
        return Err(schema(
            "",
            format!(
                "unknown key `{key}` (allowed: {})",
                MANIFEST_KEYS.join(", ")
            ),
        ));
    }

    let mut manifest = Manifest::default();
    for (key, slot) in [
        ("requestsDir", &mut manifest.requests_dir),
        ("environmentsDir", &mut manifest.environments_dir),
    ] {
        if let Some(value) = map.get(key) {
            match value.as_str() {
                Some(text) if !text.trim().is_empty() => *slot = text.to_string(),
                Some(_) => {
                    return Err(schema(
                        &format!("/{key}"),
                        format!("`{key}` must be a non-empty string"),
                    ));
                }
                None => {
                    return Err(schema(
                        &format!("/{key}"),
                        format!("`{key}` must be a string"),
                    ));
                }
            }
        }
    }
    if let Some(value) = map.get("defaultEnvironment") {
        manifest.default_environment = Some(
            value
                .as_str()
                .ok_or_else(|| {
                    schema(
                        "/defaultEnvironment",
                        "`defaultEnvironment` must be a string".to_string(),
                    )
                })?
                .to_string(),
        );
    }
    Ok(manifest)
}

/// Every request file under the configured requests directory, sorted by id.
pub fn collect_requests(project: &Project) -> Result<Vec<Entry>, Error> {
    let dir = project.requests_dir();
    if !dir.is_dir() {
        return Err(Error::MissingDir { path: dir });
    }
    let mut entries = Vec::new();
    walk(&dir, &dir, &mut entries)?;
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> Result<(), Error> {
    let io = |source: std::io::Error| Error::Io {
        path: Some(dir.to_path_buf()),
        source,
    };
    let mut items: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(io)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io)?
        .into_iter()
        .map(|item| item.path())
        .collect();
    items.sort();
    for path in items {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let metadata = fs::metadata(&path).map_err(io)?;
        if metadata.is_dir() {
            if !file_name.starts_with('.') {
                walk(root, &path, out)?;
            }
        } else if metadata.is_file()
            && file_name.ends_with(".json")
            && file_name.len() > ".json".len()
        {
            let mut id = relative_slash(&path, root);
            id.truncate(id.len() - ".json".len());
            out.push(Entry { id, file: path });
        }
    }
    Ok(())
}

/// `/`-separated path of `path` relative to `base`.
fn relative_slash(path: &Path, base: &Path) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    let mut out = String::new();
    for component in relative.components() {
        if let Component::Normal(part) = component {
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&part.to_string_lossy());
        }
    }
    out
}

/// Environment names available in the project, sorted.
pub fn list_environments(project: &Project) -> Result<Vec<String>, Error> {
    let dir = project.environments_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let io = |source: std::io::Error| Error::Io {
        path: Some(dir.to_path_buf()),
        source,
    };
    let mut names = Vec::new();
    for item in fs::read_dir(&dir).map_err(io)? {
        let path = item.map_err(io)?.path();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if file_name.starts_with('.') || !file_name.ends_with(".json") {
            continue;
        }
        if !fs::metadata(&path).map_err(io)?.is_file() {
            continue;
        }
        let stem = &file_name[..file_name.len() - ".json".len()];
        if !valid_env_name(stem) {
            return Err(Error::Schema {
                path: path.clone(),
                pointer: String::new(),
                message: format!(
                    "invalid environment file name `{file_name}`: environment names match [A-Za-z0-9][A-Za-z0-9_-]*"
                ),
            });
        }
        names.push(stem.to_string());
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Whether `name` matches `[A-Za-z0-9][A-Za-z0-9_-]*`.
pub fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Loads an environment as a flat variable map (values are literal).
pub fn load_environment(project: &Project, name: &str) -> Result<Map, Error> {
    let file = project.environment_file(name);
    if !valid_env_name(name) {
        return Err(Error::Schema {
            path: file,
            pointer: String::new(),
            message: format!(
                "invalid environment name `{name}`: environment names match [A-Za-z0-9][A-Za-z0-9_-]*"
            ),
        });
    }
    if !file.is_file() {
        let available = list_environments(project)?;
        return Err(Error::Schema {
            path: file,
            pointer: String::new(),
            message: format!(
                "unknown environment `{name}` (available: {})",
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            ),
        });
    }
    let text = fs::read_to_string(&file).map_err(|source| Error::Io {
        path: Some(file.clone()),
        source,
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|source| Error::InvalidJson {
        path: file.clone(),
        source,
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(Error::Schema {
            path: file,
            pointer: String::new(),
            message: "environment file must be a JSON object".to_string(),
        }),
    }
}

/// Picks the environment to use: `--env`, then the manifest default, then the
/// only available environment, then none.
pub fn resolve_environment(
    project: &Project,
    cli: Option<&str>,
) -> Result<Option<(String, Map)>, Error> {
    if let Some(name) = cli {
        return Ok(Some((name.to_string(), load_environment(project, name)?)));
    }
    if let Some(name) = project.manifest.default_environment.clone() {
        return Ok(Some((name.clone(), load_environment(project, &name)?)));
    }
    let available = list_environments(project)?;
    match available.len() {
        0 => Ok(None),
        1 => {
            let name = available.into_iter().next().expect("one name");
            let vars = load_environment(project, &name)?;
            Ok(Some((name, vars)))
        }
        _ => Err(Error::Schema {
            path: project.environments_dir(),
            pointer: String::new(),
            message: format!(
                "multiple environments available; choose one with --env or set \"defaultEnvironment\" (available: {})",
                available.join(", ")
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn project_with(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new("project");
        for (relative, contents) in files {
            dir.write(relative, contents);
        }
        dir
    }

    #[test]
    fn discovery_does_not_walk_up_to_the_manifest() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("requests/pets/inventory.json", "{}"),
        ]);
        let deep = dir.mkdir("requests/pets");
        let err = discover(&deep, None).unwrap_err();
        assert!(matches!(err, Error::ProjectNotFound { .. }), "{err}");
        assert_eq!(err.exit_code(), 1);

        let project = discover(dir.path(), None).unwrap();
        assert_eq!(project.root, dir.path().canonicalize().unwrap());
        assert_eq!(project.manifest, Manifest::default());
        assert!(project.requests_dir().ends_with("requests"));
    }

    #[test]
    fn discovery_uses_override_dir_verbatim() {
        let dir = project_with(&[(MANIFEST_FILE, r#"{ "requestsDir": "specs" }"#)]);
        let project = discover(Path::new("/nonexistent"), Some(dir.path())).unwrap();
        assert_eq!(project.root, dir.path());
        assert_eq!(project.manifest.requests_dir, "specs");
        let missing = discover(Path::new("/"), Some(&dir.child("nope"))).unwrap_err();
        assert!(matches!(missing, Error::ProjectNotFound { .. }));
    }

    #[test]
    fn discovery_reports_the_start_directory() {
        let dir = TempDir::new("empty");
        let err = discover(dir.path(), None).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("no curlyfries.json found in"), "{text}");
        assert!(text.contains("-C <dir>"), "{text}");
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn manifest_validation() {
        let dir = project_with(&[(
            MANIFEST_FILE,
            r#"{ "requestsDir": "r", "environmentsDir": "e", "defaultEnvironment": "dev" }"#,
        )]);
        let manifest = load_manifest(&dir.child(MANIFEST_FILE)).unwrap();
        assert_eq!(manifest.requests_dir, "r");
        assert_eq!(manifest.environments_dir, "e");
        assert_eq!(manifest.default_environment.as_deref(), Some("dev"));

        let bad = project_with(&[(MANIFEST_FILE, r#"{ "requstsDir": "r" }"#)]);
        let err = load_manifest(&bad.child(MANIFEST_FILE)).unwrap_err();
        assert!(
            err.to_string().contains("unknown key `requstsDir`"),
            "{err}"
        );

        let wrong_type = project_with(&[(MANIFEST_FILE, r#"{ "requestsDir": 3 }"#)]);
        let err = load_manifest(&wrong_type.child(MANIFEST_FILE)).unwrap_err();
        assert!(
            err.to_string().contains("`requestsDir` must be a string"),
            "{err}"
        );

        let not_object = project_with(&[(MANIFEST_FILE, "[]")]);
        let err = load_manifest(&not_object.child(MANIFEST_FILE)).unwrap_err();
        assert!(
            err.to_string().contains("manifest must be a JSON object"),
            "{err}"
        );

        let invalid = project_with(&[(MANIFEST_FILE, "{")]);
        let err = load_manifest(&invalid.child(MANIFEST_FILE)).unwrap_err();
        assert!(matches!(err, Error::InvalidJson { .. }), "{err}");
    }

    #[test]
    fn requests_are_collected_recursively_sorted_and_filtered() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("requests/b/zeta.json", "{}"),
            ("requests/b/alpha.json", "{}"),
            ("requests/a.json", "{}"),
            ("requests/notes.txt", "ignore me"),
            ("requests/.hidden/hidden.json", "{}"),
            ("requests/nested/deep/deeper.json", "{}"),
        ]);
        let project = discover(dir.path(), None).unwrap();
        let entries = collect_requests(&project).unwrap();
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b/alpha", "b/zeta", "nested/deep/deeper"]);
        assert!(entries[0].file.ends_with("requests/a.json"));
        assert_eq!(
            project.relative(&entries[3].file),
            "requests/nested/deep/deeper.json"
        );
    }

    #[test]
    fn missing_requests_dir_is_an_error() {
        let dir = project_with(&[(MANIFEST_FILE, r#"{ "requestsDir": "specs" }"#)]);
        let project = discover(dir.path(), None).unwrap();
        let err = collect_requests(&project).unwrap_err();
        assert!(matches!(err, Error::MissingDir { .. }), "{err}");
        assert!(err.to_string().contains("specs"), "{err}");
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn environment_names_are_validated() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("environments/dev.json", "{}"),
            ("environments/stage-1.json", "{}"),
        ]);
        let project = discover(dir.path(), None).unwrap();
        assert_eq!(list_environments(&project).unwrap(), vec!["dev", "stage-1"]);

        let bad = project_with(&[(MANIFEST_FILE, "{}"), ("environments/dev env.json", "{}")]);
        let project = discover(bad.path(), None).unwrap();
        let err = list_environments(&project).unwrap_err();
        assert!(
            err.to_string().contains("invalid environment file name"),
            "{err}"
        );
        assert!(err.to_string().contains("dev env.json"), "{err}");
    }

    #[test]
    fn environment_resolution_covers_every_branch() {
        // --env wins.
        let dir = project_with(&[
            (MANIFEST_FILE, r#"{ "defaultEnvironment": "stage" }"#),
            ("environments/dev.json", r#"{ "baseUrl": "http://dev" }"#),
            (
                "environments/stage.json",
                r#"{ "baseUrl": "http://stage" }"#,
            ),
        ]);
        let project = discover(dir.path(), None).unwrap();
        let (name, vars) = resolve_environment(&project, Some("dev")).unwrap().unwrap();
        assert_eq!(name, "dev");
        assert_eq!(vars["baseUrl"], serde_json::json!("http://dev"));

        // manifest default.
        let (name, _) = resolve_environment(&project, None).unwrap().unwrap();
        assert_eq!(name, "stage");

        // the only environment.
        let single = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("environments/only.json", r#"{ "a": 1 }"#),
        ]);
        let project = discover(single.path(), None).unwrap();
        let (name, vars) = resolve_environment(&project, None).unwrap().unwrap();
        assert_eq!(name, "only");
        assert_eq!(vars["a"], serde_json::json!(1));

        // none at all.
        let none = project_with(&[(MANIFEST_FILE, "{}")]);
        let project = discover(none.path(), None).unwrap();
        assert_eq!(resolve_environment(&project, None).unwrap(), None);

        // ambiguous.
        let ambiguous = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("environments/b.json", "{}"),
            ("environments/a.json", "{}"),
        ]);
        let project = discover(ambiguous.path(), None).unwrap();
        let err = resolve_environment(&project, None).unwrap_err();
        assert!(
            err.to_string().contains("multiple environments available"),
            "{err}"
        );
        assert!(err.to_string().contains("a, b"), "{err}");

        // unknown --env.
        let project = discover(dir.path(), None).unwrap();
        let err = resolve_environment(&project, Some("nope")).unwrap_err();
        assert!(
            err.to_string().contains("unknown environment `nope`"),
            "{err}"
        );
        assert!(err.to_string().contains("dev, stage"), "{err}");
    }

    #[test]
    fn environment_names_are_validated_on_load() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("environments/my env.json", r#"{ "baseUrl": "http://x" }"#),
            ("environments/dev.json", "{}"),
        ]);
        let project = discover(dir.path(), None).unwrap();
        let err = load_environment(&project, "my env").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("invalid environment name `my env`"), "{text}");
        assert!(text.contains("my env.json"), "{text}");
        assert_eq!(err.exit_code(), 3);

        let traversal = load_environment(&project, "../../etc/passwd").unwrap_err();
        assert!(
            traversal.to_string().contains("invalid environment name"),
            "{traversal}"
        );
        assert!(load_environment(&project, "dev").is_ok());
    }

    #[test]
    fn environment_files_must_parse_as_objects() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("environments/list.json", "[]"),
            ("environments/bad.json", "{"),
        ]);
        let project = discover(dir.path(), None).unwrap();
        let err = load_environment(&project, "list").unwrap_err();
        assert!(err.to_string().contains("must be a JSON object"), "{err}");
        let err = load_environment(&project, "bad").unwrap_err();
        assert!(matches!(err, Error::InvalidJson { .. }), "{err}");
    }

    #[test]
    fn environment_values_are_not_templated() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            (
                "environments/dev.json",
                r#"{ "baseUrl": "http://${host}" }"#,
            ),
        ]);
        let project = discover(dir.path(), None).unwrap();
        let vars = load_environment(&project, "dev").unwrap();
        assert_eq!(vars["baseUrl"], serde_json::json!("http://${host}"));
    }

    #[test]
    fn env_name_pattern() {
        assert!(valid_env_name("dev"));
        assert!(valid_env_name("stage-1_2"));
        assert!(!valid_env_name("(none)"));
        assert!(!valid_env_name("dev env"));
        assert!(!valid_env_name(""));
    }
}
