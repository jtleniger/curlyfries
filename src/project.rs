//! Project discovery, request enumeration and environment resolution.
//!
//! A project root is the current directory (or `-C <dir>`) when it holds
//! `curlyfries.json`; no parent directories are searched.
//! Request ids are the file path relative to `requestsDir` without the `.json`
//! suffix; environment names are env file stems matching
//! `[A-Za-z0-9][A-Za-z0-9_-]*`.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::error::Error;
use crate::variables::{self, Variables};

/// The manifest file whose presence marks a project root.
pub const MANIFEST_FILE: &str = "curlyfries.json";
/// Default requests directory.
pub const DEFAULT_REQUESTS_DIR: &str = "requests";
/// Default environments directory.
pub const DEFAULT_ENVIRONMENTS_DIR: &str = "environments";
/// Per-request timeout in seconds when the manifest does not say otherwise.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Manifest keys, in the order they are listed in errors.
pub const MANIFEST_KEYS: [&str; 6] = [
    "requestsDir",
    "environmentsDir",
    "defaultEnvironment",
    "followRedirects",
    "followSymlinks",
    "timeout",
];

/// Parsed `curlyfries.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Directory holding request files, relative to the project root.
    pub requests_dir: String,
    /// Directory holding environment files, relative to the project root.
    pub environments_dir: String,
    /// Environment the UI opens on when the manifest names one.
    pub default_environment: Option<String>,
    /// Follow `3xx` redirects that stay on the same origin.
    pub follow_redirects: bool,
    /// Follow symlinks inside the request and environment directories.
    pub follow_symlinks: bool,
    /// Per-request timeout in seconds; `0` disables timeouts.
    pub timeout_secs: u64,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            requests_dir: DEFAULT_REQUESTS_DIR.to_string(),
            environments_dir: DEFAULT_ENVIRONMENTS_DIR.to_string(),
            default_environment: None,
            follow_redirects: false,
            follow_symlinks: false,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }
}

impl Manifest {
    /// The per-request timeout; `None` disables timeouts.
    pub fn timeout(&self) -> Option<Duration> {
        (self.timeout_secs > 0).then(|| Duration::from_secs(self.timeout_secs))
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
    for (key, slot) in [
        ("followRedirects", &mut manifest.follow_redirects),
        ("followSymlinks", &mut manifest.follow_symlinks),
    ] {
        if let Some(value) = map.get(key) {
            match value.as_bool() {
                Some(flag) => *slot = flag,
                None => {
                    return Err(schema(
                        &format!("/{key}"),
                        format!("`{key}` must be a boolean"),
                    ));
                }
            }
        }
    }
    if let Some(value) = map.get("timeout") {
        match value.as_u64() {
            Some(seconds) => manifest.timeout_secs = seconds,
            None => {
                return Err(schema(
                    "/timeout",
                    "`timeout` must be a non-negative integer".to_string(),
                ));
            }
        }
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
    walk(&dir, &dir, &mut entries, project.manifest.follow_symlinks)?;
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Entry>, follow_symlinks: bool) -> Result<(), Error> {
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
        let metadata = fs::symlink_metadata(&path).map_err(io)?;
        let metadata = if metadata.file_type().is_symlink() {
            if !follow_symlinks {
                continue;
            }
            fs::metadata(&path).map_err(io)?
        } else {
            metadata
        };
        if metadata.is_dir() {
            if !file_name.starts_with('.') {
                walk(root, &path, out, follow_symlinks)?;
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
        let metadata = fs::symlink_metadata(&path).map_err(io)?;
        let metadata = if metadata.file_type().is_symlink() {
            if !project.manifest.follow_symlinks {
                continue;
            }
            fs::metadata(&path).map_err(io)?
        } else {
            metadata
        };
        if !metadata.is_file() {
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

/// Loads an environment as a variable map (values are literal).
pub fn load_environment(project: &Project, name: &str) -> Result<Variables, Error> {
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
        Value::Object(map) => variables::split_document("", &map).map_err(|p| Error::Schema {
            path: file.clone(),
            pointer: p.pointer,
            message: p.message,
        }),
        _ => Err(Error::Schema {
            path: file,
            pointer: String::new(),
            message: "environment file must be a JSON object".to_string(),
        }),
    }
}

/// Picks the environment to use at startup: the manifest `defaultEnvironment`,
/// else the only environment file, else none. Several candidates with no default
/// open the UI on the `(none)` scope, where `e` picks one.
pub fn resolve_environment(project: &Project) -> Result<Option<(String, Variables)>, Error> {
    if let Some(name) = project.manifest.default_environment.clone() {
        return Ok(Some((name.clone(), load_environment(project, &name)?)));
    }
    let available = list_environments(project)?;
    if available.len() == 1 {
        let name = available.into_iter().next().expect("one name");
        let vars = load_environment(project, &name)?;
        return Ok(Some((name, vars)));
    }
    Ok(None)
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
    fn manifest_flags_parse_and_default_to_false() {
        assert!(!Manifest::default().follow_redirects);
        assert!(!Manifest::default().follow_symlinks);

        let dir = project_with(&[(
            MANIFEST_FILE,
            r#"{ "followRedirects": true, "followSymlinks": true }"#,
        )]);
        let manifest = load_manifest(&dir.child(MANIFEST_FILE)).unwrap();
        assert!(manifest.follow_redirects);
        assert!(manifest.follow_symlinks);

        let bad = project_with(&[(MANIFEST_FILE, r#"{ "followRedirects": "yes" }"#)]);
        let err = load_manifest(&bad.child(MANIFEST_FILE)).unwrap_err();
        match err {
            Error::Schema {
                pointer, message, ..
            } => {
                assert_eq!(pointer, "/followRedirects");
                assert!(message.contains("must be a boolean"), "{message}");
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_unless_enabled() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("requests/a.json", "{}"),
            ("outside/b.json", "{}"),
            ("env/dev.json", r#"{ "baseUrl": "http://x" }"#),
        ]);
        dir.mkdir("environments");
        std::os::unix::fs::symlink(dir.child("outside"), dir.child("requests/extra"))
            .expect("symlink is creatable");
        std::os::unix::fs::symlink(
            dir.child("env/dev.json"),
            dir.child("environments/dev.json"),
        )
        .expect("symlink is creatable");

        let project = discover(dir.path(), None).unwrap();
        let ids = |project: &Project| -> Vec<String> {
            collect_requests(project)
                .unwrap()
                .into_iter()
                .map(|entry| entry.id)
                .collect()
        };
        assert_eq!(ids(&project), vec!["a"]);
        assert!(list_environments(&project).unwrap().is_empty());

        let project = Project {
            root: project.root,
            manifest: Manifest {
                follow_symlinks: true,
                ..Default::default()
            },
        };
        assert_eq!(ids(&project), vec!["a", "extra/b"]);
        assert_eq!(list_environments(&project).unwrap(), vec!["dev"]);
    }

    #[test]
    fn manifest_timeout_defaults_and_validates() {
        let default = project_with(&[(MANIFEST_FILE, "{}")]);
        let manifest = load_manifest(&default.child(MANIFEST_FILE)).unwrap();
        assert_eq!(manifest.timeout(), Some(Duration::from_secs(30)));
        assert_eq!(manifest.timeout_secs, DEFAULT_TIMEOUT_SECS);

        let off = project_with(&[(MANIFEST_FILE, r#"{ "timeout": 0 }"#)]);
        let manifest = load_manifest(&off.child(MANIFEST_FILE)).unwrap();
        assert_eq!(manifest.timeout(), None);

        let custom = project_with(&[(MANIFEST_FILE, r#"{ "timeout": 5 }"#)]);
        let manifest = load_manifest(&custom.child(MANIFEST_FILE)).unwrap();
        assert_eq!(manifest.timeout(), Some(Duration::from_secs(5)));

        for bad in [r#"{ "timeout": -1 }"#, r#"{ "timeout": "x" }"#] {
            let dir = project_with(&[(MANIFEST_FILE, bad)]);
            let err = load_manifest(&dir.child(MANIFEST_FILE)).unwrap_err();
            match err {
                Error::Schema {
                    pointer, message, ..
                } => {
                    assert_eq!(pointer, "/timeout");
                    assert!(message.contains("non-negative integer"), "{message}");
                }
                other => panic!("wrong error: {other}"),
            }
        }

        let unknown = project_with(&[(MANIFEST_FILE, r#"{ "timeouts": 5 }"#)]);
        let err = load_manifest(&unknown.child(MANIFEST_FILE)).unwrap_err();
        assert!(err.to_string().contains("unknown key `timeouts`"), "{err}");
        assert!(err.to_string().contains("timeout"), "{err}");
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
        assert!(
            entries[3]
                .file
                .ends_with("requests/nested/deep/deeper.json")
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
    fn environment_resolution_prefers_the_manifest_default() {
        // manifest default.
        let dir = project_with(&[
            (MANIFEST_FILE, r#"{ "defaultEnvironment": "stage" }"#),
            ("environments/dev.json", r#"{ "baseUrl": "http://dev" }"#),
            (
                "environments/stage.json",
                r#"{ "baseUrl": "http://stage" }"#,
            ),
        ]);
        let project = discover(dir.path(), None).unwrap();
        let (name, vars) = resolve_environment(&project).unwrap().unwrap();
        assert_eq!(name, "stage");
        assert_eq!(
            vars.get("baseUrl"),
            Some(&serde_json::json!("http://stage"))
        );

        // the only environment.
        let single = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("environments/only.json", r#"{ "a": 1 }"#),
        ]);
        let project = discover(single.path(), None).unwrap();
        let (name, vars) = resolve_environment(&project).unwrap().unwrap();
        assert_eq!(name, "only");
        assert_eq!(vars.get("a"), Some(&serde_json::json!(1)));

        // none at all.
        let none = project_with(&[(MANIFEST_FILE, "{}")]);
        let project = discover(none.path(), None).unwrap();
        assert_eq!(resolve_environment(&project).unwrap(), None);

        // ambiguous opens on the `(none)` scope rather than erroring.
        let ambiguous = project_with(&[
            (MANIFEST_FILE, "{}"),
            ("environments/b.json", "{}"),
            ("environments/a.json", "{}"),
        ]);
        let project = discover(ambiguous.path(), None).unwrap();
        assert_eq!(resolve_environment(&project).unwrap(), None);

        // unknown environment names still fail when loaded directly.
        let err = load_environment(&project, "nope").unwrap_err();
        assert!(
            err.to_string().contains("unknown environment `nope`"),
            "{err}"
        );
        assert!(err.to_string().contains("a, b"), "{err}");
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
        assert_eq!(
            vars.get("baseUrl"),
            Some(&serde_json::json!("http://${host}"))
        );
    }

    #[test]
    fn environment_secret_wrappers_unwrap_and_flag() {
        let dir = project_with(&[
            (MANIFEST_FILE, "{}"),
            (
                "environments/dev.json",
                r#"{
                  "baseUrl": "http://127.0.0.1:4000",
                  "limit": 2,
                  "pirate": { "secret": true, "value": { "username": "silver" } }
                }"#,
            ),
            (
                "environments/bad.json",
                r#"{ "pirate": { "secret": true } }"#,
            ),
        ]);
        let project = discover(dir.path(), None).unwrap();
        let vars = load_environment(&project, "dev").unwrap();
        assert_eq!(
            vars.get("pirate"),
            Some(&serde_json::json!({ "username": "silver" }))
        );
        assert_eq!(vars.get("limit"), Some(&serde_json::json!(2)));
        assert!(vars.is_secret("pirate"));
        assert!(!vars.is_secret("limit"));
        assert_eq!(vars.secrets, ["pirate".to_string()].into_iter().collect());

        // The `secret`-key rule makes a bare wrapper an error, located by pointer.
        let err = load_environment(&project, "bad").unwrap_err();
        match &err {
            Error::Schema {
                pointer, message, ..
            } => {
                assert_eq!(pointer, "/pirate");
                assert_eq!(message, "a secret value needs a `value` key");
            }
            other => panic!("wrong error: {other}"),
        }
        assert!(err.to_string().contains("environments/bad.json"), "{err}");
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
