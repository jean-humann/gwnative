//! Isolated launch profiles.
//!
//! A profile is an operating-system boundary, not merely a label in a launcher.
//! Its settings, window state, client generation, writable web root, WebKit
//! origin and Keychain account must move together. Immutable snapshot chunks
//! remain shared because they are addressed and verified by content hash.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::instance;

const FORMAT: u32 = 1;
const DEFAULT_PORT: u16 = 38112;
const PROFILE_PORT_FIRST: u16 = 38113;
const PROFILE_PORT_COUNT: u16 = 1_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub format_version: u32,
    pub id: String,
    pub display_name: String,
    pub color: String,
    pub created_at: u64,
    pub origin_port: u16,
}

impl Profile {
    pub fn default_profile() -> Self {
        Self {
            format_version: FORMAT,
            id: "default".into(),
            display_name: "Default".into(),
            color: "#d9b25c".into(),
            created_at: 0,
            origin_port: DEFAULT_PORT,
        }
    }

    pub fn support_dir(&self, base: &Path) -> PathBuf {
        if self.is_default() {
            base.to_owned()
        } else {
            base.join("profiles").join(&self.id)
        }
    }

    pub fn keychain_account(&self) -> String {
        if self.is_default() {
            "login".into()
        } else {
            format!("login:{}", self.id)
        }
    }

    /// A stable origin for IndexedDB, distinct between ordinary profiles.
    pub fn port(&self) -> u16 {
        self.origin_port
    }

    pub fn is_default(&self) -> bool {
        self.id == "default"
    }
}

/// Select a profile, creating its small descriptor on first use.
pub fn select(base: &Path, id: Option<&str>) -> Result<Profile, String> {
    let Some(id) = id else {
        return Ok(Profile::default_profile());
    };
    if id == "default" {
        return Ok(Profile::default_profile());
    }
    if !valid_id(id) {
        return Err(format!("profile id {id:?} is not a safe directory name"));
    }

    // Allocation is a catalog transaction: two first launches must not both
    // observe the same free origin and persist it for different profiles.
    let _catalog = instance::acquire(&base.join("profiles.lock"), Duration::from_secs(5))
        .map_err(|error| format!("could not lock the profile catalog: {error}"))?;
    let profiles = named_profiles(base)?;
    if let Some(profile) = profiles.iter().find(|profile| profile.id == id) {
        return Ok(profile.clone());
    }

    let dir = base.join("profiles").join(id);
    let path = dir.join("profile.json");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    let profile = Profile {
        format_version: FORMAT,
        id: id.to_owned(),
        display_name: id.to_owned(),
        color: color(id),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        origin_port: allocate_port(id, &profiles)?,
    };
    write(&path, &profile)?;
    Ok(profile)
}

/// List the implicit default profile and every named descriptor.
pub fn list(base: &Path) -> Result<Vec<Profile>, String> {
    let mut profiles = vec![Profile::default_profile()];
    profiles.extend(named_profiles(base)?);
    Ok(profiles)
}

fn named_profiles(base: &Path) -> Result<Vec<Profile>, String> {
    let root = base.join("profiles");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("could not list {}: {error}", root.display())),
    };
    let mut profiles = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read {}: {error}", root.display()))?;
        let path = entry.path().join("profile.json");
        if !path.exists() {
            continue;
        }
        let profile = read(&path)?;
        if entry.file_name().to_str() != Some(profile.id.as_str()) {
            return Err(format!(
                "{} names profile {:?}, which does not match its directory",
                path.display(),
                profile.id
            ));
        }
        if let Some(known) = profiles
            .iter()
            .find(|known: &&Profile| known.id == profile.id)
        {
            return Err(format!(
                "profiles {:?} and {:?} have the same id",
                known.id, profile.id
            ));
        }
        if let Some(known) = profiles
            .iter()
            .find(|known: &&Profile| known.origin_port == profile.origin_port)
        {
            return Err(format!(
                "profiles {:?} and {:?} both claim WebKit origin port {}",
                known.id, profile.id, profile.origin_port
            ));
        }
        profiles.push(profile);
    }
    profiles.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(profiles)
}

fn read(path: &Path) -> Result<Profile, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let profile: Profile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    if profile.format_version != FORMAT {
        return Err(format!(
            "{} uses unsupported profile format {}",
            path.display(),
            profile.format_version
        ));
    }
    if profile.id == "default" || !valid_id(&profile.id) {
        return Err(format!(
            "{} contains invalid named profile id {:?}",
            path.display(),
            profile.id
        ));
    }
    if !(PROFILE_PORT_FIRST..PROFILE_PORT_FIRST + PROFILE_PORT_COUNT).contains(&profile.origin_port)
    {
        return Err(format!(
            "{} assigns WebKit origin port {}, outside {}–{}",
            path.display(),
            profile.origin_port,
            PROFILE_PORT_FIRST,
            PROFILE_PORT_FIRST + PROFILE_PORT_COUNT - 1
        ));
    }
    Ok(profile)
}

fn allocate_port(id: &str, profiles: &[Profile]) -> Result<u16, String> {
    let start = usize::from(preferred_port(id) - PROFILE_PORT_FIRST);
    (0..usize::from(PROFILE_PORT_COUNT))
        .map(|offset| (start + offset) % usize::from(PROFILE_PORT_COUNT))
        .map(|offset| PROFILE_PORT_FIRST + offset as u16)
        .find(|candidate| {
            profiles
                .iter()
                .all(|profile| profile.origin_port != *candidate)
        })
        .ok_or_else(|| "all reserved profile origin ports are assigned".to_owned())
}

fn preferred_port(id: &str) -> u16 {
    let hash = id.bytes().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    PROFILE_PORT_FIRST + (hash % u32::from(PROFILE_PORT_COUNT)) as u16
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn write(path: &Path, profile: &Profile) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(profile).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("could not save {}: {error}", path.display()))
}

fn color(id: &str) -> String {
    const COLORS: [&str; 8] = [
        "#d9b25c", "#89b4fa", "#a6e3a1", "#cba6f7", "#f38ba8", "#94e2d5", "#fab387", "#74c7ec",
    ];
    let hash = id.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(33).wrapping_add(usize::from(byte))
    });
    COLORS[hash % COLORS.len()].into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;

    #[test]
    fn default_profile_preserves_legacy_locations_and_origin() {
        let base = Path::new("/tmp/gwnative");
        let profile = Profile::default_profile();
        assert_eq!(profile.support_dir(base), base);
        assert_eq!(profile.keychain_account(), "login");
        assert_eq!(profile.port(), 38112);
    }

    #[test]
    fn named_profile_is_persisted_and_isolated() {
        let scratch = TempDir::new("profile-isolation");
        let profile = select(&scratch.0, Some("iron")).unwrap();
        assert_eq!(
            profile.support_dir(&scratch.0),
            scratch.0.join("profiles/iron")
        );
        assert_eq!(profile.keychain_account(), "login:iron");
        assert_ne!(profile.port(), 38112);
        assert_eq!(select(&scratch.0, Some("iron")).unwrap(), profile);
    }

    #[test]
    fn listing_is_stable() {
        let scratch = TempDir::new("profile-list");
        select(&scratch.0, Some("zeta")).unwrap();
        select(&scratch.0, Some("alpha")).unwrap();
        assert_eq!(
            list(&scratch.0)
                .unwrap()
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            ["default", "alpha", "zeta"]
        );
    }

    #[test]
    fn colliding_preferences_probe_to_distinct_persisted_origins() {
        let scratch = TempDir::new("profile-port-collision");
        let mut first_by_port = vec![None; usize::from(PROFILE_PORT_COUNT)];
        let (first_id, second_id) = (0..10_000)
            .find_map(|index| {
                let id = format!("collision-{index}");
                let slot = usize::from(preferred_port(&id) - PROFILE_PORT_FIRST);
                first_by_port[slot]
                    .replace(id.clone())
                    .map(|first| (first, id))
            })
            .expect("the finite preferred-port range must collide");
        assert_eq!(preferred_port(&first_id), preferred_port(&second_id));

        let first = select(&scratch.0, Some(&first_id)).unwrap();
        let second = select(&scratch.0, Some(&second_id)).unwrap();
        assert_ne!(first.port(), second.port());
        assert_eq!(select(&scratch.0, Some(&first_id)).unwrap(), first);
        assert_eq!(select(&scratch.0, Some(&second_id)).unwrap(), second);
        assert!(
            (PROFILE_PORT_FIRST..PROFILE_PORT_FIRST + PROFILE_PORT_COUNT).contains(&first.port())
        );
    }

    #[test]
    fn malformed_duplicate_and_out_of_range_descriptors_fail_closed() {
        let scratch = TempDir::new("profile-invalid-catalog");
        let first = select(&scratch.0, Some("first")).unwrap();
        let mut second = select(&scratch.0, Some("second")).unwrap();
        second.origin_port = first.origin_port;
        write(&scratch.0.join("profiles/second/profile.json"), &second).unwrap();
        assert!(list(&scratch.0).unwrap_err().contains("both claim"));
        assert!(
            select(&scratch.0, Some("first"))
                .unwrap_err()
                .contains("both claim")
        );

        second.origin_port = PROFILE_PORT_FIRST - 1;
        write(&scratch.0.join("profiles/second/profile.json"), &second).unwrap();
        assert!(list(&scratch.0).unwrap_err().contains("outside"));

        fs::write(scratch.0.join("profiles/second/profile.json"), b"not json").unwrap();
        assert!(list(&scratch.0).unwrap_err().contains("could not parse"));
    }
}
