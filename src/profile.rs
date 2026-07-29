//! Isolated launch profiles.
//!
//! A profile is an operating-system boundary, not merely a label in a launcher.
//! Its settings, window state, client generation, writable web root, WebKit
//! origin and Keychain account must move together. Immutable snapshot chunks
//! remain shared because they are addressed and verified by content hash.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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
}

impl Profile {
    pub fn default_profile() -> Self {
        Self {
            format_version: FORMAT,
            id: "default".into(),
            display_name: "Default".into(),
            color: "#d9b25c".into(),
            created_at: 0,
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
        if self.is_default() {
            return DEFAULT_PORT;
        }
        let hash = self.id.bytes().fold(2_166_136_261u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
        });
        PROFILE_PORT_FIRST + (hash % u32::from(PROFILE_PORT_COUNT)) as u16
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

    let dir = base.join("profiles").join(id);
    let path = dir.join("profile.json");
    if path.exists() {
        return read(&path).and_then(|profile| {
            if profile.id == id {
                Ok(profile)
            } else {
                Err(format!(
                    "{} names profile {:?}, expected {id:?}",
                    path.display(),
                    profile.id
                ))
            }
        });
    }

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
    };
    write(&path, &profile)?;
    Ok(profile)
}

/// List the implicit default profile and every valid named descriptor.
pub fn list(base: &Path) -> Vec<Profile> {
    let mut profiles = vec![Profile::default_profile()];
    let Ok(entries) = fs::read_dir(base.join("profiles")) else {
        return profiles;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("profile.json");
        let Ok(profile) = read(&path) else { continue };
        if profile.id != "default"
            && entry.file_name().to_str() == Some(profile.id.as_str())
            && !profiles.iter().any(|known| known.id == profile.id)
        {
            profiles.push(profile);
        }
    }
    profiles[1..].sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    profiles
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
    Ok(profile)
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
    fn listing_is_stable_and_ignores_malformed_descriptors() {
        let scratch = TempDir::new("profile-list");
        select(&scratch.0, Some("zeta")).unwrap();
        select(&scratch.0, Some("alpha")).unwrap();
        fs::create_dir_all(scratch.0.join("profiles/broken")).unwrap();
        fs::write(scratch.0.join("profiles/broken/profile.json"), b"not json").unwrap();
        assert_eq!(
            list(&scratch.0)
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            ["default", "alpha", "zeta"]
        );
    }

    #[test]
    fn ports_are_stable_and_in_the_reserved_profile_range() {
        let one = Profile {
            id: "one".into(),
            ..Profile::default_profile()
        };
        assert_eq!(one.port(), one.port());
        assert!(
            (PROFILE_PORT_FIRST..PROFILE_PORT_FIRST + PROFILE_PORT_COUNT).contains(&one.port())
        );
    }
}
