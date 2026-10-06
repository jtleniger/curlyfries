//! Scratch directories for unit tests.
//!
//! Kept in the test build only; integration tests carry their own copy.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp dir that removes itself on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a uniquely named directory for `label`.
    pub fn new(label: &str) -> TempDir {
        let unique = format!(
            "curlyfries-{}-{}-{label}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir is creatable");
        TempDir { path }
    }

    /// The directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Joins a relative path onto the directory.
    pub fn child(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }

    /// Writes `contents` to `relative`, creating parent directories.
    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.child(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory is creatable");
        }
        fs::write(&path, contents).expect("file is writable");
        path
    }

    /// Creates a directory at `relative`.
    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.child(relative);
        fs::create_dir_all(&path).expect("directory is creatable");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
