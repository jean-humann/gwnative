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
    shell_seed: PathBuf,
    shell_store: PathBuf,
    derived: PathBuf,
    cache: PathBuf,
    port: u16,
}

impl Layout {
    pub fn new(invocation: &cli::Invocation, profile: &profile::Profile) -> Self {
        let base_support = base_support_dir();
        let support = profile.support_dir(&base_support);
        let cache = invocation
            .cache_root
            .clone()
            .unwrap_or_else(crate::cache::default_cache_dir);
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
        let mut web = web_root_plan(&support, profile.is_default());
        if let Some(live) = web_override {
            web.live = live;
        }
        let derived = support.join("derived");
        let shell_store = support.join("shells");
        Self {
            support,
            web: web.live,
            shell_seed: web.seed,
            shell_store,
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

    /// Refresh the writable shell after this profile's instance lock is held.
    /// Computing a layout must remain read-only: a launch that is about to be
    /// rejected must never rewrite files underneath the process already using
    /// them.
    pub fn prepare_shell(&self) -> std::io::Result<PathBuf> {
        crate::shell::install(&self.shell_seed, &self.shell_store)
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
    if let Ok(root) = std::env::var("GWNATIVE_WEB_ROOT") {
        return PathBuf::from(root);
    }
    web_root_plan(&base_support_dir(), true).live
}

/// The client directory `patch::sync` fills and the separate reviewed shell seed.
///
/// Development runs straight out of the source tree. A packaged build does
/// *not* serve out of `Contents/Resources/web`, tempting as that is: the patch
/// client writes `Gw.jspi.wasm` into this directory, and writing into a bundle
/// invalidates its code signature — the same signature the keychain matches the
/// saved login against, so the cost of getting this wrong is an account that
/// silently stops appearing. The bundle's copy is a seed for a writable root
/// instead, refreshed on every launch so an upgraded app ships an upgraded
/// shell in an immutable sibling revision.
struct WebRoot {
    live: PathBuf,
    seed: PathBuf,
}

/// Decide where the shell lives without touching it. The selected instance
/// lock must be acquired before the shell installer acts on this plan.
fn web_root_plan(support: &Path, use_source_tree_directly: bool) -> WebRoot {
    let exe = std::env::current_exe().expect("a running process has a path on macOS");
    let bundled_seed = exe
        .parent()
        .and_then(Path::parent)
        .map(|contents| contents.join("Resources/web"))
        .filter(|seed| seed.is_dir());
    let source_seed = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    let (live, seed) = match bundled_seed {
        Some(seed) => (support.join("web"), seed),
        None if use_source_tree_directly => (source_seed.clone(), source_seed),
        None => (support.join("web"), source_seed),
    };
    WebRoot { live, seed }
}

/// Keep this identical in meaning to the allowlist in `scripts/bundle`.
pub(crate) fn is_shell_file(name: &str) -> bool {
    (name.ends_with(".js") || name.ends_with(".html"))
        && name != "Gw.js"
        && !name.starts_with("Gw.jspi.")
        && !name.ends_with(".test.js")
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
    fn command_line_locations_and_port_win() {
        let invocation = cli::parse([
            "--profile",
            "iron",
            "--cache",
            "/tmp/gw-cache",
            "--dir",
            "/tmp/gw-web",
            "--host-port",
            "39000",
        ])
        .unwrap();
        let profile = profile::Profile::default_profile();
        let layout = Layout::new(&invocation, &profile);
        assert_eq!(layout.cache_dir(), Path::new("/tmp/gw-cache"));
        assert_eq!(layout.web_root(), Path::new("/tmp/gw-web"));
        assert_ne!(layout.shell_seed, Path::new("/tmp/gw-web"));
        assert_eq!(layout.port(), 39000);
    }

    #[test]
    fn named_development_profiles_get_a_private_seeded_web_root() {
        let scratch = crate::scratch::TempDir::new("profile-web-root");
        let plan = web_root_plan(&scratch.0, false);
        assert_eq!(plan.live, scratch.0.join("web"));
        assert!(!plan.live.exists(), "planning must not mutate the root");
        let installed = crate::shell::install(&plan.seed, &scratch.0.join("shells")).unwrap();
        assert_eq!(
            std::fs::read(installed.join("index.html")).unwrap(),
            std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/index.html")).unwrap(),
        );
    }

    #[test]
    fn source_seeding_copies_shell_but_never_clients_or_tests() {
        let scratch = crate::scratch::TempDir::new("profile-web-allowlist");
        let seed = scratch.0.join("seed");
        let store = scratch.0.join("shells");
        std::fs::create_dir_all(&seed).unwrap();
        for (name, bytes) in [
            ("index.html", b"shell".as_slice()),
            ("harness.js", b"shell-js".as_slice()),
            ("Gw.js", b"client-js".as_slice()),
            ("Gw.jspi.js", b"client-jspi".as_slice()),
            ("Gw.wasm", b"client-wasm".as_slice()),
            ("version.json", b"client-version".as_slice()),
            ("page.test.js", b"test".as_slice()),
        ] {
            std::fs::write(seed.join(name), bytes).unwrap();
        }

        let live = crate::shell::install(&seed, &store).unwrap();

        assert_eq!(std::fs::read(live.join("index.html")).unwrap(), b"shell");
        assert_eq!(std::fs::read(live.join("harness.js")).unwrap(), b"shell-js");
        for excluded in [
            "Gw.js",
            "Gw.jspi.js",
            "Gw.wasm",
            "version.json",
            "page.test.js",
        ] {
            assert!(!live.join(excluded).exists(), "copied {excluded}");
        }
    }
}
