//! Where this app keeps things, and why each one is where it is.
//!
//! Three directories and the rule that produces them. Gathered here because the
//! reasoning is shared — one of them must be writable, one must not be inside
//! the bundle, and one must not be in `~/Library/Caches` — and because a path
//! spelled out at its use site is a path that will eventually be spelled two
//! different ways.

use std::path::{Path, PathBuf};

use crate::{cli, profile};

/// All writable and selectable locations for one invocation.
#[derive(Clone, Debug)]
pub struct Layout {
    support: PathBuf,
    web: PathBuf,
    derived: PathBuf,
    cache: PathBuf,
    port: u16,
}

impl Layout {
    pub fn new(invocation: &cli::Invocation, profile: &profile::Profile) -> Self {
        let base_support = base_support_dir();
        let support = profile.support_dir(&base_support);
        let cache = crate::cache::default_cache_dir();
        let port = invocation
            .host_port
            .or_else(|| {
                std::env::var("GWNATIVE_PORT")
                    .ok()
                    .and_then(|value| value.parse().ok())
            })
            .unwrap_or_else(|| profile.port());

        let web_override = invocation
            .web_root
            .clone()
            .or_else(|| std::env::var("GWNATIVE_WEB_ROOT").ok().map(PathBuf::from));
        let web = web_override.unwrap_or_else(|| writable_web_root(&support, profile.is_default()));
        let derived = support.join("derived");
        Self {
            support,
            web,
            derived,
            cache,
            port,
        }
    }

    pub fn support_dir(&self) -> &Path {
        &self.support
    }

    pub fn web_root(&self) -> &Path {
        &self.web
    }

    pub fn derived_dir(&self) -> &Path {
        &self.derived
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// `~/Library/Application Support/gwnative`, shared profile metadata and
/// immutable game-image chunks.
///
/// The chunk cache is already a directory inside it — see
/// [`crate::cache::default_cache_dir`], which explains why it is here rather
/// than in `~/Library/Caches`.
pub fn base_support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Library/Application Support/gwnative")
}

/// Verified artifact-certificate updates, separate from derived modules so
/// clearing one never rolls the other back.
pub fn certificate_dir() -> PathBuf {
    base_support_dir().join("certificates")
}

/// The command-line certification root before a launch profile is selected.
pub fn web_root() -> PathBuf {
    std::env::var("GWNATIVE_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| writable_web_root(&base_support_dir(), true))
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
fn writable_web_root(support: &Path, use_source_tree_directly: bool) -> PathBuf {
    let exe = std::env::current_exe().expect("a running process has a path on macOS");
    let bundled_seed = exe
        .parent()
        .and_then(Path::parent)
        .map(|contents| contents.join("Resources/web"))
        .filter(|seed| seed.is_dir());
    let source_seed = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    let seed = match bundled_seed {
        Some(seed) => seed,
        None if use_source_tree_directly => return source_seed,
        None => source_seed,
    };
    let live = support.join("web");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli;

    #[test]
    fn named_profiles_isolate_mutable_state_and_share_chunks() {
        let invocation = cli::parse(["--profile", "iron", "--dir", "/tmp/gw-web"]).unwrap();
        let profile = profile::Profile {
            format_version: 1,
            id: "iron".into(),
            display_name: "Iron".into(),
            color: "#000000".into(),
            created_at: 0,
            origin_port: 38113,
            website_data_store_id: Some("00000000-0000-4000-8000-000000000001".into()),
        };
        let layout = Layout::new(&invocation, &profile);
        assert!(layout.support_dir().ends_with("gwnative/profiles/iron"));
        assert!(layout.cache_dir().ends_with("gwnative/chunks"));
        assert_ne!(layout.port(), 38112);
    }

    #[test]
    fn command_line_web_root_and_port_win() {
        let invocation = cli::parse([
            "--profile",
            "iron",
            "--dir",
            "/tmp/gw-web",
            "--host-port",
            "39000",
        ])
        .unwrap();
        let profile = profile::Profile::default_profile();
        let layout = Layout::new(&invocation, &profile);
        assert_eq!(layout.web_root(), Path::new("/tmp/gw-web"));
        assert_eq!(layout.port(), 39000);
    }

    #[test]
    fn named_development_profiles_get_a_private_seeded_web_root() {
        let scratch = crate::scratch::TempDir::new("profile-web-root");
        let root = writable_web_root(&scratch.0, false);
        assert_eq!(root, scratch.0.join("web"));
        assert_eq!(
            std::fs::read(root.join("index.html")).unwrap(),
            std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/index.html")).unwrap(),
        );
    }
}
