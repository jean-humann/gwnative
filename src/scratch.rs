//! A temporary directory that removes itself.
//!
//! Test-only, and shared rather than copied into each test module: eight of
//! them want the same thing, and a helper that exists in eight places is one
//! that gets fixed in one of them.

use std::fs;
use std::path::PathBuf;

/// Named for the test that made it, so a directory left behind by a crash says
/// which one to look at.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "gwnative-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
