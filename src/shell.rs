//! Crash-consistent installation of the browser shell, separate from clients.
use crate::{instance, paths};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Error, Result, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const CLEANUP_SCAN: usize = 32;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    format: u32,
    revision: String,
    files: BTreeMap<String, Entry>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    size: u64,
    sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    current: String,
    predecessor: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    FileCopied,
    FileSynced,
    InventorySynced,
    StageSynced,
    RevisionRenamed,
    RevisionsSynced,
    PointerSynced,
    PointerRenamed,
    StoreSynced,
}

#[derive(Default)]
struct Installer {
    failure: Option<Boundary>,
}

/// Install `seed` as one immutable reviewed revision and return that revision.
pub fn install(seed: &Path, store: &Path) -> Result<PathBuf> {
    Installer::default().install(seed, store)
}

impl Installer {
    fn install(&self, seed: &Path, store: &Path) -> Result<PathBuf> {
        ensure_directory(store)?;
        let lock = store.join("install.lock");
        if fs::symlink_metadata(&lock).is_ok_and(|meta| !meta.file_type().is_file()) {
            return Err(Error::other("shell install lock is not a regular file"));
        }
        let _lock = instance::acquire(&lock, Duration::ZERO).map_err(Error::other)?;
        let fallback = selected(store).ok();
        let result = match cleanup_temporary(store).and_then(|()| self.refresh(seed, store)) {
            Ok(path) => Ok(path),
            Err(error) => match selected(store).ok().or(fallback) {
                Some(path) => {
                    note!(
                        "[shell] refresh failed; retaining verified revision {} ({error})",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    Ok(path)
                }
                None => Err(error),
            },
        };
        defer_cleanup(store.join("trash"));
        result
    }

    fn refresh(&self, seed: &Path, store: &Path) -> Result<PathBuf> {
        let (inventory, files) = inventory(seed)?;
        let revisions = store.join("revisions");
        ensure_directory(&revisions)?;
        let target = revisions.join(&inventory.revision);
        if target.exists() {
            verify(&target, &inventory.revision)?;
        } else {
            let stage = revisions.join(format!(
                ".staging-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&stage)?;
            for (name, bytes) in &files {
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(stage.join(name))?;
                file.write_all(bytes)?;
                self.fail(Boundary::FileCopied)?;
                file.sync_all()?;
                self.fail(Boundary::FileSynced)?;
            }
            let mut manifest = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(stage.join("inventory.json"))?;
            manifest.write_all(&serde_json::to_vec_pretty(&inventory).map_err(Error::other)?)?;
            manifest.sync_all()?;
            self.fail(Boundary::InventorySynced)?;
            File::open(&stage)?.sync_all()?;
            self.fail(Boundary::StageSynced)?;
            verify(&stage, &inventory.revision)?;
            fs::rename(&stage, &target)?;
            self.fail(Boundary::RevisionRenamed)?;
            File::open(&revisions)?.sync_all()?;
            self.fail(Boundary::RevisionsSynced)?;
        }

        let pointer = read_pointer(store).ok();
        let current = pointer
            .as_ref()
            .and_then(|pointer| verified_revision(store, &pointer.current));
        if current.as_ref() != Some(&inventory.revision) {
            let predecessor = current
                .or_else(|| pointer.and_then(|pointer| pointer.predecessor))
                .filter(|id| id != &inventory.revision && verified_revision(store, id).is_some());
            self.switch(
                store,
                &Pointer {
                    current: inventory.revision.clone(),
                    predecessor,
                },
            )?;
        }
        cleanup_revisions(store)?;
        Ok(target)
    }

    fn switch(&self, store: &Path, pointer: &Pointer) -> Result<()> {
        let temporary = store.join(format!(
            ".pointer-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(pointer).map_err(Error::other)?)?;
        file.sync_all()?;
        self.fail(Boundary::PointerSynced)?;
        fs::rename(&temporary, store.join("current.json"))?;
        self.fail(Boundary::PointerRenamed)?;
        File::open(store)?.sync_all()?;
        self.fail(Boundary::StoreSynced)
    }

    fn fail(&self, boundary: Boundary) -> Result<()> {
        if self.failure == Some(boundary) {
            Err(Error::other(format!(
                "injected shell failure at {boundary:?}"
            )))
        } else {
            Ok(())
        }
    }
}

fn inventory(seed: &Path) -> Result<(Inventory, BTreeMap<String, Vec<u8>>)> {
    let mut files = BTreeMap::new();
    for item in fs::read_dir(seed)? {
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str().filter(|name| paths::is_shell_file(name)) else {
            continue;
        };
        if item.file_type()?.is_file() {
            files.insert(name.to_owned(), fs::read(item.path())?);
        }
    }
    if !files.contains_key("index.html") {
        return Err(Error::other("shell seed has no index.html"));
    }
    let entries = files
        .iter()
        .map(|(name, bytes)| {
            (
                name.clone(),
                Entry {
                    size: bytes.len() as u64,
                    sha256: hex::encode(sha2::Sha256::digest(bytes)),
                },
            )
        })
        .collect();
    let revision = revision(&entries);
    Ok((
        Inventory {
            format: 1,
            revision,
            files: entries,
        },
        files,
    ))
}

fn revision(files: &BTreeMap<String, Entry>) -> String {
    let mut digest = sha2::Sha256::new();
    for (name, entry) in files {
        digest.update(name.as_bytes());
        digest.update(b"\0");
        digest.update(entry.size.to_le_bytes());
        digest.update(entry.sha256.as_bytes());
        digest.update(b"\n");
    }
    hex::encode(digest.finalize())
}

fn verify(directory: &Path, expected: &str) -> Result<()> {
    if !valid_revision(expected) {
        return Err(Error::other("shell revision name is invalid"));
    }
    if !fs::symlink_metadata(directory)?.file_type().is_dir() {
        return Err(Error::other("shell revision is not a directory"));
    }
    let inventory: Inventory = serde_json::from_slice(&fs::read(directory.join("inventory.json"))?)
        .map_err(Error::other)?;
    if inventory.format != 1
        || inventory.revision != expected
        || revision(&inventory.files) != expected
        || !inventory.files.contains_key("index.html")
    {
        return Err(Error::other("shell inventory identity is invalid"));
    }
    let mut actual = BTreeSet::new();
    for item in fs::read_dir(directory)? {
        let item = item?;
        if !item.file_type()?.is_file() {
            return Err(Error::other("shell revision contains a non-file"));
        }
        actual.insert(item.file_name().to_string_lossy().into_owned());
    }
    let expected_files = inventory
        .files
        .keys()
        .cloned()
        .chain(["inventory.json".to_owned()])
        .collect::<BTreeSet<_>>();
    if actual != expected_files {
        return Err(Error::other(
            "shell inventory does not exactly match its directory",
        ));
    }
    for (name, entry) in inventory.files {
        let bytes = fs::read(directory.join(name))?;
        if bytes.len() as u64 != entry.size
            || hex::encode(sha2::Sha256::digest(&bytes)) != entry.sha256
        {
            return Err(Error::other(
                "shell file digest does not match its inventory",
            ));
        }
    }
    Ok(())
}

fn read_pointer(store: &Path) -> Result<Pointer> {
    serde_json::from_slice(&fs::read(store.join("current.json"))?).map_err(Error::other)
}

fn verified_revision(store: &Path, id: &str) -> Option<String> {
    let path = store.join("revisions").join(id);
    verify(&path, id).ok().map(|()| id.to_owned())
}

fn valid_revision(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn selected(store: &Path) -> Result<PathBuf> {
    let pointer = read_pointer(store)?;
    for id in [Some(pointer.current), pointer.predecessor]
        .into_iter()
        .flatten()
    {
        if verified_revision(store, &id).is_some() {
            return Ok(store.join("revisions").join(id));
        }
    }
    Err(Error::other("shell pointer names no verified revision"))
}

fn cleanup_temporary(store: &Path) -> Result<()> {
    let revisions = store.join("revisions");
    let trash = store.join("trash");
    ensure_directory(&revisions)?;
    ensure_directory(&trash)?;
    for item in fs::read_dir(&revisions)?.take(CLEANUP_SCAN) {
        let item = item?;
        if item.file_name().to_string_lossy().starts_with(".staging-") {
            quarantine(&item.path(), &trash)?;
        }
    }
    for item in fs::read_dir(store)?.take(CLEANUP_SCAN) {
        let item = item?;
        if item.file_name().to_string_lossy().starts_with(".pointer-") {
            quarantine(&item.path(), &trash)?;
        }
    }
    Ok(())
}

fn cleanup_revisions(store: &Path) -> Result<()> {
    let pointer = read_pointer(store)?;
    let keep = [Some(pointer.current), pointer.predecessor]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    let trash = store.join("trash");
    ensure_directory(&trash)?;
    for item in fs::read_dir(store.join("revisions"))?.take(CLEANUP_SCAN) {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && !keep.contains(&name) {
            quarantine(&item.path(), &trash)?;
        }
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path),
        Ok(_) => Err(Error::other("shell store entry is not a directory")),
        Err(error) => Err(error),
    }
}

fn quarantine(path: &Path, trash: &Path) -> Result<()> {
    fs::rename(
        path,
        trash.join(format!(
            "{}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            path.file_name().unwrap_or_default().to_string_lossy()
        )),
    )
}

fn defer_cleanup(trash: PathBuf) {
    let Ok(directory) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&trash)
    else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("gwnative-shell-cleanup".into())
        .spawn(move || {
            let held = format!("/dev/fd/{}", directory.as_raw_fd());
            for item in fs::read_dir(held).into_iter().flatten().flatten() {
                let path = item.path();
                if item.file_type().is_ok_and(|kind| kind.is_dir()) {
                    let _ = fs::remove_dir_all(path);
                } else {
                    let _ = fs::remove_file(path);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(root: &Path, flavour: &str) -> PathBuf {
        let path = root.join(format!("seed-{flavour}"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("index.html"), format!("{flavour}:index")).unwrap();
        fs::write(path.join("harness.js"), format!("{flavour}:harness")).unwrap();
        path
    }

    #[test]
    fn every_transaction_boundary_selects_one_complete_revision() {
        for boundary in [
            Boundary::FileCopied,
            Boundary::FileSynced,
            Boundary::InventorySynced,
            Boundary::StageSynced,
            Boundary::RevisionRenamed,
            Boundary::RevisionsSynced,
            Boundary::PointerSynced,
            Boundary::PointerRenamed,
            Boundary::StoreSynced,
        ] {
            let temp = crate::scratch::TempDir::new("shell-boundary");
            let store = temp.0.join("shells");
            let old = install(&seed(&temp.0, "old"), &store).unwrap();
            let selected = Installer {
                failure: Some(boundary),
            }
            .install(&seed(&temp.0, "new"), &store)
            .unwrap();
            verify(&selected, selected.file_name().unwrap().to_str().unwrap()).unwrap();
            let body = fs::read_to_string(selected.join("index.html")).unwrap();
            assert!(body == "old:index" || body == "new:index", "{boundary:?}");
            assert_eq!(fs::read(old.join("index.html")).unwrap(), b"old:index");
        }
    }

    #[test]
    fn abandoned_staging_is_cleaned_and_a_downgrade_swaps_predecessors() {
        let temp = crate::scratch::TempDir::new("shell-downgrade");
        let store = temp.0.join("shells");
        let old_seed = seed(&temp.0, "old");
        let new_seed = seed(&temp.0, "new");
        let old = install(&old_seed, &store).unwrap();
        let new = install(&new_seed, &store).unwrap();
        let abandoned = store.join("revisions/.staging-dead");
        fs::create_dir(&abandoned).unwrap();
        assert_eq!(install(&old_seed, &store).unwrap(), old);
        assert!(!abandoned.exists());
        let pointer = read_pointer(&store).unwrap();
        assert_eq!(pointer.current, old.file_name().unwrap().to_str().unwrap());
        assert_eq!(
            pointer.predecessor.as_deref(),
            new.file_name().unwrap().to_str()
        );
        fs::write(old.join("harness.js"), "corrupt").unwrap();
        assert_eq!(install(&new_seed, &store).unwrap(), new);
        let repaired = read_pointer(&store).unwrap();
        assert_eq!(repaired.current, pointer.predecessor.unwrap());
    }

    #[test]
    fn client_and_player_state_are_never_copied_or_modified() {
        let temp = crate::scratch::TempDir::new("shell-client-boundary");
        let seed = seed(&temp.0, "shell");
        let sentinels = [
            "Gw.js",
            "Gw.wasm",
            "Gw.jspi.js",
            "Gw.jspi.wasm",
            "Gw.snapshot",
            "version.json",
            "page.test.js",
        ];
        for name in sentinels {
            fs::write(seed.join(name), format!("sentinel:{name}")).unwrap();
        }
        let before = sentinels.map(|name| {
            let path = seed.join(name);
            (
                fs::read(&path).unwrap(),
                fs::metadata(path).unwrap().modified().unwrap(),
            )
        });
        for name in ["chunks", "player-data"] {
            fs::create_dir(seed.join(name)).unwrap();
            fs::write(seed.join(name).join("sentinel"), name).unwrap();
        }
        let directory_times = ["chunks", "player-data"]
            .map(|name| fs::metadata(seed.join(name)).unwrap().modified().unwrap());
        let revision = install(&seed, &temp.0.join("shells")).unwrap();
        for (index, name) in sentinels.into_iter().enumerate() {
            assert!(!revision.join(name).exists(), "copied {name}");
            let path = seed.join(name);
            assert_eq!(fs::read(&path).unwrap(), before[index].0);
            assert_eq!(
                fs::metadata(path).unwrap().modified().unwrap(),
                before[index].1
            );
        }
        for (index, name) in ["chunks", "player-data"].into_iter().enumerate() {
            assert!(!revision.join(name).exists(), "copied {name}");
            assert_eq!(
                fs::read_to_string(seed.join(name).join("sentinel")).unwrap(),
                name
            );
            assert_eq!(
                fs::metadata(seed.join(name)).unwrap().modified().unwrap(),
                directory_times[index]
            );
        }
    }

    #[test]
    fn an_install_lock_refuses_a_concurrent_writer() {
        let temp = crate::scratch::TempDir::new("shell-concurrent");
        let store = temp.0.join("shells");
        fs::create_dir_all(&store).unwrap();
        let held = instance::acquire(&store.join("install.lock"), Duration::ZERO).unwrap();
        assert!(install(&seed(&temp.0, "new"), &store).is_err());
        assert!(!store.join("current.json").exists());
        drop(held);
        assert!(install(&seed(&temp.0, "new"), &store).is_ok());
    }

    #[test]
    fn exact_inventory_rejects_extra_missing_and_changed_files() {
        for mutation in 0..3 {
            let temp = crate::scratch::TempDir::new("shell-inventory");
            let store = temp.0.join("shells");
            let revision = install(&seed(&temp.0, "old"), &store).unwrap();
            match mutation {
                0 => fs::write(revision.join("extra.js"), "extra").unwrap(),
                1 => fs::remove_file(revision.join("harness.js")).unwrap(),
                _ => fs::write(revision.join("harness.js"), "changed").unwrap(),
            }
            assert!(verify(&revision, revision.file_name().unwrap().to_str().unwrap()).is_err());
        }
    }
}
