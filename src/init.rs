//! `curlyfries init`: writes a starter project into an empty directory.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::Error;
use crate::project::{DEFAULT_ENVIRONMENTS_DIR, DEFAULT_REQUESTS_DIR, MANIFEST_FILE};

/// Starter manifest: a project root with the `dev` environment selected.
const MANIFEST: &str = "{\n  \"defaultEnvironment\": \"dev\"\n}\n";
/// Starter request: one `GET` against the environment's base URL.
const REQUEST: &str =
    "{\n  \"name\": \"Ping\",\n  \"method\": \"GET\",\n  \"path\": \"${baseUrl}/ping\"\n}\n";
/// Starter environment: the local sample API address.
const ENVIRONMENT: &str = "{\n  \"baseUrl\": \"http://127.0.0.1:4000\"\n}\n";
/// Starter ignore list: the capture store is machine-local state.
pub(crate) const GITIGNORE: &str = ".curlyfries/\n";

/// Writes a starter project into `root` and reports each created file on `out`.
///
/// Creates `curlyfries.json`, `<requestsDir>/ping.json`, `<environmentsDir>/dev.json`
/// and `.gitignore`. Fails with [`Error::InitConflict`] — without creating
/// anything — when any of those four paths already exists.
pub fn init(root: &Path, out: &mut impl Write) -> Result<(), Error> {
    let request_rel = format!("{DEFAULT_REQUESTS_DIR}/ping.json");
    let environment_rel = format!("{DEFAULT_ENVIRONMENTS_DIR}/dev.json");
    let targets: [(&str, &str); 4] = [
        (MANIFEST_FILE, MANIFEST),
        (&request_rel, REQUEST),
        (&environment_rel, ENVIRONMENT),
        (".gitignore", GITIGNORE),
    ];

    let existing: Vec<String> = targets
        .iter()
        .filter(|(relative, _)| fs::symlink_metadata(root.join(relative)).is_ok())
        .map(|(relative, _)| (*relative).to_string())
        .collect();
    if !existing.is_empty() {
        return Err(Error::InitConflict {
            root: root.to_path_buf(),
            existing,
        });
    }

    for (relative, contents) in targets {
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

    let _ = writeln!(out, "initialized curlyfries project in {}", root.display());
    Ok(())
}
