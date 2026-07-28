//! Which client build is on disk, whether it has ever reached a first frame,
//! and what to go back to when it has not.
//!
//! Two problems, one record.
//!
//! The first is that a file being present is not the same as a file being
//! right. A sync interrupted at the wrong moment, a disk that filled, a
//! bit that rotted over a year in Application Support — every one of them
//! leaves `Gw.jspi.wasm` sitting there looking exactly like a good one, and the
//! only symptom is a boot that fails in the client's own code, where nothing
//! this host says can help. So the size and a digest of each artifact are
//! recorded when it is written, and checked before the window opens.
//!
//! The second is that a *correctly downloaded* build can still be one this
//! harness cannot run. ArenaNet ships when they ship; the transform in `wasm`
//! is certified against a specific module, the JSPI glue changes shape, and the
//! failure arrives as an app that will not start any more — with the previous,
//! working client already overwritten. So a freshly synced set is not trusted
//! until the page reports a first frame. Until then the set it replaced is kept
//! beside it, and a launch that finds an unproven set restores what came
//! before and refuses that build by identity, so the next sync does not walk
//! straight back into it.
//!
//! The two identities are deliberately different things. A generation's `id`
//! says *which build* this is and comes from the manifest's chunk hashes, so it
//! is known before a byte is downloaded — that is what makes refusing one
//! possible. An artifact's `hash` says *what is on this disk* and is taken from
//! the bytes as written. Neither can stand in for the other.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::error::{Error, Result};
use crate::manifest::Manifest;

const FORMAT: u32 = 1;

/// How many refused builds to remember.
///
/// Long enough that a bad build stays refused for as long as it is the current
/// one, short enough that the list cannot grow without bound. A build that
/// falls off the end is simply tried again, which is the right outcome once it
/// is old enough that nobody is being offered it any more.
const REJECTED_KEPT: usize = 8;

/// The id given to a client that was on the disk before the record existed.
///
/// Not hex and not sixteen characters, so [`identify`] can never produce it and
/// no real build can ever be refused by matching it.
const ADOPTED: &str = "adopted";

/// One artifact, as it was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    size: u64,
    /// Lowercase SHA-256 of the whole file. Not the manifest's hash, which
    /// covers chunks rather than the assembled result.
    hash: String,
}

/// A set of client artifacts that belong together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    /// What the patch service called this build. See the module docs.
    id: String,
    artifacts: BTreeMap<String, Artifact>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    format_version: u32,
    current: Option<Generation>,
    /// Whether `current` has ever reported a first frame.
    proven: bool,
    /// The set `current` replaced, whose files are stashed in `previous/`.
    previous: Option<Generation>,
    rejected: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            format_version: FORMAT,
            current: None,
            proven: false,
            previous: None,
            rejected: Vec::new(),
        }
    }
}

/// The identity of the build this manifest is currently offering.
///
/// Taken from the chunk hashes of the artifacts themselves, so it changes when
/// and only when the bytes a sync would fetch change. Available before any of
/// them are downloaded, which is the whole point: refusing a build after
/// downloading it would refuse nothing.
pub fn identify(manifest: &Manifest, names: &[&str]) -> Result<String> {
    let mut digest = sha2::Sha256::new();
    for name in names {
        let path = manifest.require_unique(name)?;
        let entry = &manifest.files[path];
        digest.update(name.as_bytes());
        digest.update(b" ");
        digest.update(entry.size.to_le_bytes());
        for hash in &entry.chunk_hashes {
            digest.update(hash.bytes());
        }
        digest.update(b"\n");
    }
    // Sixteen hex characters is a build label, not a security boundary: it goes
    // in a log line a player might read out, and the full 64 buys nothing when
    // the alternative it must be told apart from is the handful of builds this
    // machine has ever seen.
    Ok(hex::encode(&digest.finalize()[..8]))
}

fn hash_file(path: &Path) -> Result<Artifact> {
    let bytes = fs::read(path)?;
    Ok(Artifact {
        size: bytes.len() as u64,
        hash: hex::encode(sha2::Sha256::digest(&bytes)),
    })
}

pub struct Store {
    dir: PathBuf,
    state: Mutex<State>,
}

impl Store {
    /// Read the record, or start without one.
    ///
    /// Unlike settings, this file is not the player's — every field in it can
    /// be rebuilt by syncing again. So an unreadable record is discarded rather
    /// than set aside: keeping a copy of something nobody can act on is just
    /// litter in Application Support.
    pub fn open(dir: PathBuf) -> Self {
        let path = dir.join("state.json");
        let state = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(state) if state.format_version == FORMAT => state,
                Ok(state) => {
                    note!(
                        "[generation] a record written by format {} cannot be read here; \
                         starting over",
                        state.format_version
                    );
                    State::default()
                }
                Err(e) => {
                    note!("[generation] the record is unreadable ({e}); starting over");
                    State::default()
                }
            },
            Err(_) => State::default(),
        };
        Self {
            dir,
            state: Mutex::new(state),
        }
    }

    fn save(&self, state: &State) {
        let path = self.dir.join("state.json");
        let write = || -> Result<()> {
            fs::create_dir_all(&self.dir)?;
            let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
            let bytes =
                serde_json::to_vec_pretty(state).map_err(|e| Error::Decode(e.to_string()))?;
            fs::write(&tmp, &bytes)?;
            fs::rename(&tmp, &path).inspect_err(|_| {
                let _ = fs::remove_file(&tmp);
            })?;
            Ok(())
        };
        if let Err(e) = write() {
            // Not fatal, and deliberately not retried. The cost of losing the
            // record is one redundant sync and one build that could have been
            // refused not being; the cost of treating it as fatal is an app
            // that will not start because of a bookkeeping file.
            note!("[generation] could not write the record: {e}");
        }
    }

    /// Artifacts that are absent, the wrong length, or the wrong content.
    ///
    /// Names with no record are only checked for existence — that is every name
    /// on a first launch, and on any launch after a record was lost, where
    /// re-downloading a good client because we forgot what it looked like would
    /// be the worse answer.
    pub fn unsound(&self, root: &Path, names: &[&'static str]) -> Vec<&'static str> {
        // A copy, so the lock is released before the hashing starts. `check`
        // reads whole artifacts — 9 MB for the wasm — and this runs on the
        // launch path, where holding the state lock for the length of a hash
        // stalls every other caller behind it. The record is a handful of
        // names and digests, so copying it costs nothing worth measuring.
        let recorded = {
            let state = self.state.lock().unwrap();
            state.current.as_ref().map(|g| g.artifacts.clone())
        };
        names
            .iter()
            .copied()
            .filter(|name| {
                let path = root.join(name);
                let Some(expected) = recorded.as_ref().and_then(|a| a.get(*name)) else {
                    return !path.is_file();
                };
                match check(&path, expected) {
                    Ok(()) => false,
                    Err(reason) => {
                        note!("[generation] {name}: {reason}");
                        true
                    }
                }
            })
            .collect()
    }

    /// Whether the build on offer is one this Mac has not installed.
    ///
    /// The patch check, and the only one that notices a build shipping rather
    /// than a file rotting: [`Store::unsound`] compares the disk against the
    /// record, so both agreeing on a client six patches old is a clean answer.
    /// This compares the record against the service.
    ///
    /// True when there is no record at all, and true for the client [adopted]
    /// from the disk — `ADOPTED` is not a build id, so that client is known to
    /// be intact and not known to be current, and the way to make it current is
    /// to install what is on offer. That is one download of three small files,
    /// once, after which the record names a real build and this answers false
    /// until the service publishes another.
    ///
    /// [adopted]: Store::adopt
    pub fn stale(&self, offered: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_none_or(|current| current.id != offered)
    }

    /// Whether this build has already failed to reach a first frame here.
    pub fn rejected(&self, id: &str) -> bool {
        self.state.lock().unwrap().rejected.iter().any(|r| r == id)
    }

    /// Undo a build that was installed but never reported a first frame.
    ///
    /// Returns the id it refused, if it refused one. Does nothing when there is
    /// no previous set to go back to: on a first install, retrying is the only
    /// move there is, and refusing the one client on the disk would turn a boot
    /// that failed for an unrelated reason into an app with no client at all.
    pub fn roll_back(&self, root: &Path) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        if state.proven {
            return None;
        }
        let (current, previous) = (state.current.clone()?, state.previous.clone()?);

        // Verified before it is trusted, for the same reason the current set is:
        // restoring a stash that rotted would replace a build that does not work
        // with one that does not work either, and lose the record of both.
        for (name, expected) in &previous.artifacts {
            if let Err(reason) = check(&self.dir.join("previous").join(name), expected) {
                note!("[generation] cannot roll back — the stashed {name}: {reason}");
                return None;
            }
        }
        for name in previous.artifacts.keys() {
            if let Err(e) = fs::copy(self.dir.join("previous").join(name), root.join(name)) {
                note!("[generation] cannot roll back — restoring {name}: {e}");
                return None;
            }
        }

        state.rejected.push(current.id.clone());
        let excess = state.rejected.len().saturating_sub(REJECTED_KEPT);
        state.rejected.drain(..excess);
        state.current = Some(previous);
        // The set being restored is one that booted here before, which is what
        // being proven means. Nothing is left to fall back to, and nothing needs
        // to be: the next sync stashes this one before replacing it.
        state.proven = true;
        state.previous = None;
        self.save(&state);
        let _ = fs::remove_dir_all(self.dir.join("previous"));
        Some(current.id)
    }

    /// Copy the current artifacts aside so a sync can be undone.
    ///
    /// Called before the download, because afterwards there is nothing left to
    /// copy. A set that has never been proven is not stashed — it is not a
    /// rollback target, and overwriting the stash with it would throw away the
    /// last set that did work.
    pub fn stash(&self, root: &Path, names: &[&'static str]) {
        let state = self.state.lock().unwrap();
        if !state.proven {
            return;
        }
        let Some(current) = state.current.clone() else {
            return;
        };
        let stash = self.dir.join("previous");
        let copy = || -> Result<()> {
            fs::create_dir_all(&stash)?;
            for name in names {
                fs::copy(root.join(name), stash.join(name))?;
            }
            Ok(())
        };
        if let Err(e) = copy() {
            note!(
                "[generation] could not stash the current client ({e}); a bad sync will not be undoable"
            );
            let _ = fs::remove_dir_all(&stash);
            return;
        }
        drop(state);
        let mut state = self.state.lock().unwrap();
        state.previous = Some(current);
        self.save(&state);
    }

    /// Drop the stash, after a sync that replaced nothing.
    ///
    /// [`Store::stash`] runs before the download, because afterwards there is
    /// nothing left to copy — so a download that fails leaves a stashed copy of
    /// the set that is still installed. Nothing can ever use it: [`Store::record`]
    /// was not reached, so the current set is still proven, and [`Store::roll_back`]
    /// only ever undoes an unproven one. What is left is the size of a client in
    /// Application Support and a record claiming a rollback target that would
    /// restore what is already there.
    pub fn forget_stash(&self) {
        let mut state = self.state.lock().unwrap();
        if state.previous.is_none() {
            return;
        }
        state.previous = None;
        self.save(&state);
        let _ = fs::remove_dir_all(self.dir.join("previous"));
    }

    /// Record a freshly written set as current, unproven — unless it is the set
    /// that was already here.
    ///
    /// Writing the same build again is not a new build, and two ordinary things
    /// do it: the `sync` command, and repairing an artifact that rotted. Calling
    /// either of those unproven arms a rollback whose target is a copy of the
    /// same set, so an app that then dies before its first frame restores what
    /// was already on disk and refuses, by name, the build it just restored —
    /// after which the service goes on offering it and nothing will install it.
    /// A set that has booted here has not stopped having booted here.
    pub fn record(&self, id: &str, root: &Path, names: &[&'static str]) {
        let Some(artifacts) = weigh(root, names) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        let same = state
            .current
            .as_ref()
            .is_some_and(|current| current.id == id);
        state.current = Some(Generation {
            id: id.to_owned(),
            artifacts,
        });
        state.proven = same && state.proven;
        if state.proven {
            // And with nothing to undo, nothing to undo it with. See
            // [`Store::forget_stash`], which this is the other half of.
            state.previous = None;
        }
        self.save(&state);
        if state.proven {
            note!("[generation] client build {id} reinstalled; it had already booted here");
        } else {
            note!("[generation] client build {id} installed, not yet proven");
        }
        if state.previous.is_none() {
            let _ = fs::remove_dir_all(self.dir.join("previous"));
        }
    }

    /// Take an installation that predates the record as the current set.
    ///
    /// Without this, a client that was already on the disk is only ever
    /// existence-checked — `unsound` has nothing to compare it against — so the
    /// install most likely to have rotted is the one nothing is watching, until
    /// some future patch happens to replace it. Hashing it once is a single pass
    /// over nine megabytes and makes every later launch a real check.
    ///
    /// Adopted proven, which is not a fiction: this set was here before the
    /// process started, so either it has booted or the only thing to roll back
    /// to is nothing at all. Its id is deliberately not a build id — no manifest
    /// can produce this string — because what is on the disk is known but which
    /// build it came from is not.
    pub fn adopt(&self, root: &Path, names: &[&'static str]) {
        if self.state.lock().unwrap().current.is_some() {
            return;
        }
        let Some(artifacts) = weigh(root, names) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        state.current = Some(Generation {
            id: ADOPTED.to_owned(),
            artifacts,
        });
        state.proven = true;
        self.save(&state);
        note!("[generation] adopted the client already on disk; it is checked from now on");
    }

    /// The page reached a first frame, so the current set works here.
    ///
    /// Idempotent, and cheap when there is nothing to do: this is called from a
    /// request handler on a route the page hits once per launch, and every
    /// launch after the first finds the set already proven.
    pub fn prove(&self) {
        let mut state = self.state.lock().unwrap();
        if state.proven {
            return;
        }
        state.proven = true;
        state.previous = None;
        self.save(&state);
        let id = state.current.as_ref().map_or("", |g| g.id.as_str());
        note!("[generation] client build {id} reached a first frame; keeping it");
        let _ = fs::remove_dir_all(self.dir.join("previous"));
    }
}

/// Measure every named artifact under `root`, or nothing.
///
/// All-or-nothing on purpose: a half-recorded set would make [`Store::unsound`]
/// report the unrecorded half as fine and the recorded half as checkable, which
/// is a worse answer than falling back to existence checks until the next sync.
fn weigh(root: &Path, names: &[&'static str]) -> Option<BTreeMap<String, Artifact>> {
    let mut artifacts = BTreeMap::new();
    for name in names {
        match hash_file(&root.join(name)) {
            Ok(artifact) => {
                artifacts.insert((*name).to_owned(), artifact);
            }
            Err(e) => {
                note!("[generation] could not record {name} ({e}); the set is not recorded");
                return None;
            }
        }
    }
    Some(artifacts)
}

/// Whether the file at `path` is the artifact that was recorded.
fn check(path: &Path, expected: &Artifact) -> std::result::Result<(), String> {
    // Length first, and separately: it is a `stat` rather than a read of the
    // whole file, and truncation — a sync cut short, a disk that filled — is
    // both the likeliest corruption and the one this catches for free.
    let actual = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if actual != expected.size {
        return Err(format!(
            "{actual} bytes on disk, {} recorded",
            expected.size
        ));
    }
    let found = hash_file(path).map_err(|e| e.to_string())?;
    if found.hash != expected.hash {
        return Err(format!(
            "content is {}…, {}… recorded",
            short(&found.hash),
            short(&expected.hash)
        ));
    }
    Ok(())
}

/// Enough of a digest to tell two apart in a log line.
///
/// Written to be total rather than to slice, because one of these two hashes
/// did not come from `hash_file` — it came out of `state.json`, which is a file
/// in Application Support that this module's own opening paragraph says can rot.
/// A record that still parses as JSON but holds a truncated digest is exactly
/// the corruption `check` exists to catch, and `&hash[..16]` would panic
/// reporting it. Hex is ASCII, so there is no character boundary to miss.
fn short(hash: &str) -> &str {
    hash.get(..16).unwrap_or(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;

    const NAMES: [&str; 2] = ["Gw.jspi.js", "Gw.jspi.wasm"];

    fn write_client(root: &Path, flavour: &str) {
        fs::create_dir_all(root).unwrap();
        for name in NAMES {
            fs::write(root.join(name), format!("{flavour}:{name}")).unwrap();
        }
    }

    /// A manifest offering [`NAMES`], each one chunk long.
    ///
    /// The two knobs are the two things [`identify`] reads besides the name: how
    /// long each artifact is, and what its chunks hash to.
    fn offering(sizes: [u64; 2], chunks: [char; 2]) -> Manifest {
        let files: Vec<String> = NAMES
            .iter()
            .zip(sizes)
            .zip(chunks)
            .map(|((name, size), chunk)| {
                let hash: String = std::iter::repeat_n(chunk, 64).collect();
                format!(r#"{{"name":"{name}","size":{size},"chunkHashes":["{hash}"]}}"#)
            })
            .collect();
        Manifest::parse(
            format!(
                r#"{{"compressionMode":"none","chunkSize":64,"files":[{}]}}"#,
                files.join(",")
            )
            .as_bytes(),
        )
        .expect("a well-formed manifest")
    }

    /// A store with `flavour` installed and proven, ready to be replaced.
    fn proven(dir: PathBuf, root: &Path, flavour: &str) -> Store {
        write_client(root, flavour);
        let store = Store::open(dir);
        store.record(flavour, root, &NAMES);
        store.prove();
        store
    }

    #[test]
    fn presence_is_not_soundness() {
        let temp = TempDir::new("generation-soundness");
        let root = temp.0.join("web");
        let store = proven(temp.0.join("state"), &root, "good");

        assert!(store.unsound(&root, &NAMES).is_empty());

        // Truncated: the length check alone catches this, and it is what a sync
        // that ran out of disk leaves behind.
        fs::write(root.join("Gw.jspi.js"), "goo").unwrap();
        assert_eq!(store.unsound(&root, &NAMES), vec!["Gw.jspi.js"]);

        // The same length, different bytes — invisible to every check this
        // repository made before the record existed.
        fs::write(root.join("Gw.jspi.js"), "gxxd:Gw.jspi.js").unwrap();
        assert_eq!(store.unsound(&root, &NAMES), vec!["Gw.jspi.js"]);

        fs::remove_file(root.join("Gw.jspi.wasm")).unwrap();
        assert_eq!(
            store.unsound(&root, &NAMES),
            vec!["Gw.jspi.js", "Gw.jspi.wasm"]
        );
    }

    #[test]
    fn with_no_record_a_file_only_has_to_be_there() {
        let temp = TempDir::new("generation-unrecorded");
        let root = temp.0.join("web");
        write_client(&root, "whatever");
        let store = Store::open(temp.0.join("state"));

        // The case that matters: a working client and a lost record must not
        // send the app back to the patch service for 8 MB it already has.
        assert!(store.unsound(&root, &NAMES).is_empty());
        fs::remove_file(root.join("Gw.jspi.js")).unwrap();
        assert_eq!(store.unsound(&root, &NAMES), vec!["Gw.jspi.js"]);
    }

    #[test]
    fn a_client_that_predates_the_record_is_taken_at_its_word_once() {
        let temp = TempDir::new("generation-adopt");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        write_client(&root, "installed-long-ago");
        let store = Store::open(state.clone());

        store.adopt(&root, &NAMES);

        // What adoption buys: same-length corruption, which existence checks
        // cannot see, is caught from the very next launch.
        drop(store);
        let store = Store::open(state);
        fs::write(root.join("Gw.jspi.js"), "xxxxxxxxxxxxxxxxx:Gw.jspi.js").unwrap();
        assert_eq!(store.unsound(&root, &NAMES), vec!["Gw.jspi.js"]);

        // And it is a rollback target, not a build waiting to be judged: a
        // launch that never reaches a frame must not undo the client the user
        // has been playing for months.
        assert_eq!(store.roll_back(&root), None);

        // Adoption happens once. A real build recorded later owns the slot.
        write_client(&root, "installed-long-ago");
        store.stash(&root, &NAMES);
        write_client(&root, "patched");
        store.record("0123456789abcdef", &root, &NAMES);
        store.adopt(&root, &NAMES);
        assert_eq!(
            store.roll_back(&root).as_deref(),
            Some("0123456789abcdef"),
            "adopt must not have overwritten the recorded build"
        );
    }

    #[test]
    fn a_build_that_never_booted_is_undone_and_refused_by_name() {
        let temp = TempDir::new("generation-rollback");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        // A sync: stash, overwrite, record — and then the app dies before the
        // page ever reports a frame.
        store.stash(&root, &NAMES);
        write_client(&root, "new");
        store.record("new", &root, &NAMES);
        drop(store);

        let store = Store::open(state.clone());
        assert_eq!(store.roll_back(&root).as_deref(), Some("new"));
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "old:Gw.jspi.js",
            "the working client should be back on disk"
        );
        assert!(store.rejected("new"));
        assert!(!store.rejected("old"));
        assert!(store.unsound(&root, &NAMES).is_empty());

        // And it stays refused across launches, which is the whole point: the
        // service is still offering the same broken build.
        let store = Store::open(state);
        assert!(store.rejected("new"));
        assert_eq!(
            store.roll_back(&root),
            None,
            "there is nothing left to undo"
        );
    }

    /// The other half of that sequence: the sync that never gets past the
    /// stash. A download can fail on any of the artifacts, and what it leaves
    /// behind must not look like a build worth going back to.
    #[test]
    fn a_sync_that_downloads_nothing_leaves_nothing_stashed() {
        let temp = TempDir::new("generation-failed-sync");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        store.stash(&root, &NAMES);
        assert!(state.join("previous").is_dir());

        // The download failed here — nothing was overwritten and nothing was
        // recorded, so the set on disk is still the proven one.
        store.forget_stash();
        assert!(
            !state.join("previous").exists(),
            "and nothing on disk either"
        );

        let store = Store::open(state);
        assert_eq!(
            store.roll_back(&root),
            None,
            "there was never anything to undo"
        );
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    /// Installing the build that is already here is not a new build. The `sync`
    /// command does it, and so does repairing an artifact that rotted — and
    /// both used to arm a rollback against a copy of the same set, which on the
    /// next bad launch refused the build it had just restored.
    #[test]
    fn reinstalling_the_current_build_does_not_unprove_it() {
        let temp = TempDir::new("generation-reinstall");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "shipped");

        // Stash, write, record — with the id that is already current, because
        // the service is offering exactly what is installed.
        store.stash(&root, &NAMES);
        write_client(&root, "shipped");
        store.record("shipped", &root, &NAMES);
        assert!(
            !state.join("previous").exists(),
            "a stash of the current set is not a rollback target"
        );

        // And now the app dies before ever reaching a frame.
        let store = Store::open(state);
        assert_eq!(store.roll_back(&root), None, "there is nothing to undo");
        assert!(
            !store.rejected("shipped"),
            "this build booted here and has not stopped having booted here"
        );
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn a_first_frame_settles_it() {
        let temp = TempDir::new("generation-prove");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        store.stash(&root, &NAMES);
        write_client(&root, "new");
        store.record("new", &root, &NAMES);
        store.prove();
        assert!(
            !state.join("previous").exists(),
            "a proven build has nothing to fall back to and should not hold 8 MB open"
        );

        let store = Store::open(state);
        assert_eq!(store.roll_back(&root), None);
        assert!(!store.rejected("new"));
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn the_only_client_on_the_disk_is_never_taken_away() {
        let temp = TempDir::new("generation-first-install");
        let root = temp.0.join("web");
        write_client(&root, "first");
        let store = Store::open(temp.0.join("state"));
        store.record("first", &root, &NAMES);

        // Unproven, but there is no previous set — a crash for an unrelated
        // reason must not leave the app with nothing to run.
        assert_eq!(store.roll_back(&root), None);
        assert!(store.unsound(&root, &NAMES).is_empty());
        assert!(!store.rejected("first"));
    }

    /// The record is a file in Application Support, and this module opens by
    /// saying what a year in Application Support does to files. One that still
    /// parses as JSON but holds a truncated digest has to come back as a
    /// mismatch — the thing `check` is for — rather than as a panic on the
    /// launch path.
    #[test]
    fn a_record_holding_a_truncated_digest_is_a_mismatch_not_a_crash() {
        let temp = TempDir::new("generation-short-digest");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "good");
        drop(store);

        let path = state.join("state.json");
        let record = fs::read_to_string(&path).unwrap();
        let corrupt = regex_free_replace(&record);
        fs::write(&path, corrupt).unwrap();

        let store = Store::open(state);
        assert_eq!(store.unsound(&root, &NAMES), NAMES.to_vec());
    }

    /// Cut every recorded digest down to four characters, leaving the JSON
    /// valid and every length intact — so the only thing wrong with the record
    /// is the field `check` reads last.
    fn regex_free_replace(record: &str) -> String {
        let mut out = String::new();
        for (i, part) in record.split(r#""hash": ""#).enumerate() {
            if i > 0 {
                out.push_str(r#""hash": ""#);
                let end = part.find('"').expect("a hash is a quoted string");
                out.push_str(&part[..4]);
                out.push_str(&part[end..]);
                continue;
            }
            out.push_str(part);
        }
        out
    }

    #[test]
    fn refusals_do_not_accumulate_without_bound() {
        let temp = TempDir::new("generation-refusals");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state, &root, "keeper");

        for round in 0..REJECTED_KEPT + 3 {
            store.stash(&root, &NAMES);
            write_client(&root, &format!("bad{round}"));
            store.record(&format!("bad{round}"), &root, &NAMES);
            assert_eq!(
                store.roll_back(&root).as_deref(),
                Some(&*format!("bad{round}"))
            );
        }

        assert_eq!(store.state.lock().unwrap().rejected.len(), REJECTED_KEPT);
        assert!(
            !store.rejected("bad0"),
            "the oldest refusal should have aged out"
        );
        assert!(store.rejected(&format!("bad{}", REJECTED_KEPT + 2)));
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "keeper:Gw.jspi.js",
            "every rollback should land on the one build that ever worked"
        );
    }

    /// A manifest names the build it offers from what it already carries, so a
    /// patch is visible before a byte of it is downloaded. This is the whole
    /// basis of the check: if two manifests offering different bytes could name
    /// the same build, a patch would ship and nothing would install it.
    #[test]
    fn a_manifest_offering_different_bytes_offers_a_different_build() {
        let shipped = identify(&offering([16, 16], ['a', 'b']), &NAMES).unwrap();

        // The same manifest again — the ordinary launch, where nothing has
        // changed and nothing should be fetched.
        assert_eq!(
            identify(&offering([16, 16], ['a', 'b']), &NAMES).unwrap(),
            shipped
        );

        // Different content at the same length, which is what a patch to one
        // artifact usually looks like and what a size check cannot see.
        assert_ne!(
            identify(&offering([16, 16], ['a', 'c']), &NAMES).unwrap(),
            shipped
        );

        // And a different length behind the same chunk hash, which no service
        // would publish but which must not collide either — the size is in the
        // digest for exactly this reason.
        assert_ne!(
            identify(&offering([16, 32], ['a', 'b']), &NAMES).unwrap(),
            shipped
        );

        // Sixteen hex characters, so it can never be mistaken for `ADOPTED`.
        assert_eq!(shipped.len(), 16);
        assert!(shipped.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// What turns that id into a download. Until a build has been recorded here,
    /// the answer is that this Mac has not installed it — including for a client
    /// that is on the disk and intact, because intact is not the same question.
    #[test]
    fn a_build_is_stale_until_it_is_installed() {
        let temp = TempDir::new("generation-stale");
        let root = temp.0.join("web");
        write_client(&root, "shipped");
        let store = Store::open(temp.0.join("state"));

        // No record at all: the disk is fine and says nothing about provenance.
        assert!(store.stale("shipped"));

        // Adopting it does not say so either — that records the bytes that are
        // there, not the build they came from.
        store.adopt(&root, &NAMES);
        assert!(
            store.stale("shipped"),
            "an adopted client is intact, not identified"
        );
        assert!(store.unsound(&root, &NAMES).is_empty());

        store.record("shipped", &root, &NAMES);
        assert!(!store.stale("shipped"));
        assert!(
            store.stale("patched"),
            "the service published something new"
        );
    }
}
