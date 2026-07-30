//! Where this app keeps things, and why each one is where it is.
//!
//! Three directories and the rule that produces them. Gathered here because the
//! reasoning is shared — one of them must be writable, one must not be inside
//! the bundle, and one must not be in `~/Library/Caches` — and because a path
//! spelled out at its use site is a path that will eventually be spelled two
//! different ways.

use std::path::{Path, PathBuf};

/// `~/Library/Application Support/gwnative`, the one place this app writes.
///
/// The chunk cache is already a directory inside it — see
/// [`crate::cache::default_cache_dir`], which explains why it is here rather
/// than in `~/Library/Caches`.
pub fn support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Library/Application Support/gwnative")
}

/// Where derived clients live. Owned outright by the transform, which empties it
/// whenever it cannot serve the input — an entry is ~8.2 MB, so keeping one per
/// build the machine has ever seen adds up quickly.
///
/// Deliberately not inside the web root: that directory is what the loopback
/// origin serves, and the derived module is reachable only through the one path
/// the server maps to it.
pub fn derived_dir() -> PathBuf {
    support_dir().join("derived")
}

/// Verified artifact-certificate updates, separate from derived modules so
/// clearing one never rolls the other back.
pub fn certificate_dir() -> PathBuf {
    support_dir().join("certificates")
}

/// The directory the loopback origin serves, and the one `patch::sync` fills.
///
/// Development runs straight out of the source tree. A packaged build does
/// *not* serve out of `Contents/Resources/web`, tempting as that is: the patch
/// client writes `Gw.jspi.wasm` into this directory, and writing into a bundle
/// invalidates its code signature — the same signature the keychain matches the
/// saved login against, so the cost of getting this wrong is an account that
/// silently stops appearing. The bundle's copy is a seed for a writable root
/// instead, refreshed on every launch so an upgraded app ships an upgraded
/// shell.
pub fn web_root() -> PathBuf {
    if let Ok(dir) = std::env::var("GWNATIVE_WEB_ROOT") {
        return PathBuf::from(dir);
    }
    let exe = std::env::current_exe().expect("a running process has a path on macOS");
    let seed = exe
        .parent()
        .and_then(Path::parent)
        .map(|contents| contents.join("Resources/web"))
        .filter(|seed| seed.is_dir());
    let Some(seed) = seed else {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    };
    let live = support_dir().join("web");
    if let Err(e) = seed_web(&seed, &live) {
        // Reported rather than fatal, and still the root we return: a partial
        // seed leaves the missing file to be noticed by whatever needed it,
        // whereas falling back to the bundle would put the patch sync inside
        // it, which is the one outcome this function exists to prevent.
        note!("[gwnative] could not lay out {}: {e}", live.display());
    }
    live
}

/// Copy the bundle's shell files over the live web root.
///
/// Only what the bundle carries: the client artifacts sit in the same directory
/// once fetched and must survive. Contents are compared rather than timestamps,
/// which a copy does not preserve — these are a few tens of kilobytes, so the
/// comparison costs less than being wrong about it would.
fn seed_web(seed: &Path, live: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(live)?;
    for entry in std::fs::read_dir(seed)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let fresh = std::fs::read(entry.path())?;
        let installed = live.join(entry.file_name());
        if std::fs::read(&installed).is_ok_and(|current| current == fresh) {
            continue;
        }
        std::fs::write(&installed, &fresh)?;
    }
    Ok(())
}
