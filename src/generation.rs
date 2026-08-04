//! Which client generation is on disk, whether it has ever reached a first frame,
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
//! The second is that a *correctly downloaded* generation can still fail before
//! a first frame. The page records whether it attempted gwnative's optional
//! transform or ArenaNet's exact module. A transformed failure disables only
//! that runtime/artifact transform. A freshly synced official set is not trusted
//! until an unmodified attempt reports a first frame; until then the set and
//! manifest it replaced are kept beside it. Only a failed unmodified attempt
//! restores that pair and refuses the offered generation by identity.
//!
//! The two identities are deliberately different things. A generation's `id`
//! says *which patch generation* this is and comes from the manifest's chunk
//! hashes, so it is known before a byte is downloaded — that is what makes
//! refusing one possible. An artifact's `hash` says *what is on this disk* and
//! is taken from the bytes as written. Neither can stand in for the other.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::error::{Error, Result};
#[cfg(test)]
use crate::generation_state::REJECTED_KEPT;
use crate::generation_state::{
    DISABLED_TRANSFORMS_KEPT, DisabledTransform, LaunchState, ProofState, State, bound_history,
    read_state, runtime_artifacts, same_runtime_tuple, valid_digest,
};
pub use crate::generation_state::{LaunchIdentity, RuntimeMode};
use crate::manifest::Manifest;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The id given to a client that was on the disk before the record existed.
///
/// Not hex and not sixteen characters, so [`identify`] can never produce it and
/// no real build can ever be refused by matching it.
const ADOPTED: &str = "adopted";

/// One artifact, as it was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub(crate) size: u64,
    /// Lowercase SHA-256 of the whole file. Not the manifest's hash, which
    /// covers chunks rather than the assembled result.
    pub(crate) hash: String,
}

/// A set of client artifacts that belong together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    /// Manifest-derived patch-generation identity. See the module docs.
    pub(crate) id: String,
    pub(crate) artifacts: BTreeMap<String, Artifact>,
    /// The active manifest that describes this generation's snapshot.
    #[serde(default)]
    pub(crate) manifest: Option<Artifact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    None,
    InstallationRestored,
    TransformDisabled { runtime: String, build: String },
    RuntimeFailed(LaunchIdentity),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeStateError {
    Invalid(String),
    NotSaved,
}

impl std::fmt::Display for RuntimeStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => formatter.write_str(reason),
            Self::NotSaved => formatter.write_str("the runtime state could not be recorded"),
        }
    }
}

/// The identity of the patch generation this manifest is currently offering.
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
    // Sixteen hex characters is a generation label, not a security boundary: it goes
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
    active_manifest: PathBuf,
    state_path: PathBuf,
    state: Mutex<State>,
    future_format: Option<u64>,
    #[cfg(test)]
    write_failure: Mutex<Option<WriteBoundary>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteBoundary {
    BeforeTemporary,
    AfterWrite,
    AfterFileSync,
    AfterRename,
    AfterDirectorySync,
    StorageFull,
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
        let (mut state, future_format) = match fs::read(&path) {
            Ok(bytes) => read_state(&bytes),
            Err(_) => (State::default(), None),
        };
        let state_path = if future_format.is_some() {
            dir.join("state.compat-v2.json")
        } else {
            path
        };
        if future_format.is_some()
            && let Ok(bytes) = fs::read(&state_path)
        {
            state = read_state(&bytes).0;
        }
        let active_manifest = dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("manifest.cache");
        Self {
            dir,
            active_manifest,
            state_path,
            state: Mutex::new(state),
            future_format,
            #[cfg(test)]
            write_failure: Mutex::new(None),
        }
    }

    fn save(&self, state: &State) -> bool {
        let path = &self.state_path;
        let write = || -> Result<()> {
            fs::create_dir_all(&self.dir)?;
            let bytes =
                serde_json::to_vec_pretty(state).map_err(|e| Error::Decode(e.to_string()))?;
            self.fail_write(WriteBoundary::BeforeTemporary)?;
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let tmp = self
                .dir
                .join(format!("state.{}.{sequence}.tmp", std::process::id()));
            let result = (|| -> Result<()> {
                let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
                self.fail_write(WriteBoundary::StorageFull)?;
                file.write_all(&bytes)?;
                self.fail_write(WriteBoundary::AfterWrite)?;
                file.sync_all()?;
                self.fail_write(WriteBoundary::AfterFileSync)?;
                fs::rename(&tmp, path)?;
                self.fail_write(WriteBoundary::AfterRename)?;
                fs::File::open(&self.dir)?.sync_all()?;
                self.fail_write(WriteBoundary::AfterDirectorySync)
            })();
            let _ = fs::remove_file(&tmp);
            result?;
            Ok(())
        };
        if let Err(e) = write() {
            note!("[generation] could not write the record: {e}");
            return false;
        }
        true
    }

    #[cfg(not(test))]
    fn fail_write(&self, _boundary: WriteBoundary) -> Result<()> {
        Ok(())
    }

    #[cfg(test)]
    fn fail_write(&self, boundary: WriteBoundary) -> Result<()> {
        let mut failure = self.write_failure.lock().unwrap();
        if *failure == Some(boundary) {
            *failure = None;
            return Err(Error::Io(std::io::Error::other(format!(
                "injected state failure at {boundary:?}"
            ))));
        }
        Ok(())
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

    /// Whether the generation on offer is one this Mac has not installed.
    ///
    /// The patch check, and the only one that notices a build shipping rather
    /// than a file rotting: [`Store::unsound`] compares the disk against the
    /// record, so both agreeing on a client six patches old is a clean answer.
    /// This compares the record against the service.
    ///
    /// True when there is no record at all, and true for the client [adopted]
    /// from the disk — `ADOPTED` is not a generation id, so that client is known to
    /// be intact and not known to be current, and the way to make it current is
    /// to install what is on offer. That is one download of the four runtime
    /// artifacts plus `version.json`, once, after which the record names a real
    /// generation and this answers false
    /// until the service publishes another.
    ///
    /// [adopted]: Store::adopt
    pub fn stale(&self, offered: &str) -> bool {
        if self.future_format.is_some() {
            return false;
        }
        self.state
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_none_or(|current| current.id != offered)
    }

    /// Reconcile the recorded generation with its active manifest.
    ///
    /// Snapshot metadata can change without changing any of the five client
    /// artifacts, and activation is necessarily a separate atomic rename from
    /// this state-file update. Repeating this on the next launch closes that
    /// crash window without rehashing the much larger client modules.
    pub fn refresh_manifest(&self, id: &str) -> bool {
        let manifest = match hash_file(&self.active_manifest) {
            Ok(manifest) => manifest,
            Err(error) => {
                note!("[generation] could not record the active manifest ({error})");
                return false;
            }
        };
        let mut state = self.state.lock().unwrap();
        let Some(current) = state.current.as_mut().filter(|current| current.id == id) else {
            return false;
        };
        if current.manifest.as_ref() == Some(&manifest) {
            return true;
        }
        let previous = current.manifest.replace(manifest);
        if !self.save(&state) {
            state.current.as_mut().unwrap().manifest = previous;
            return false;
        }
        true
    }

    /// Whether this generation has already failed to reach a first frame here.
    pub fn rejected(&self, id: &str) -> bool {
        self.state.lock().unwrap().rejected.iter().any(|r| r == id)
    }

    /// Whether this exact runtime/glue/Wasm transform has failed on this Mac.
    pub fn transform_disabled(&self, runtime: &str, build: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .disabled_transforms
            .iter()
            .any(|disabled| disabled.runtime == runtime && disabled.build == build)
    }
    /// Record the runtime that is about to execute.
    ///
    /// Called immediately before the page appends ArenaNet's glue. A launch
    /// closed before this point attempted no client and must not cause a
    /// rollback on the next launch.
    pub fn record_attempt(
        &self,
        runtime: &str,
        build: Option<&str>,
        transformed: bool,
        nonce: &str,
    ) -> std::result::Result<LaunchIdentity, RuntimeStateError> {
        validate_runtime_attempt(runtime, build, transformed)?;
        validate_launch_nonce(nonce)?;
        let mut state = self.state.lock().unwrap();
        let current = state.current.as_ref().ok_or_else(|| {
            RuntimeStateError::Invalid("no installed generation can be launched".to_owned())
        })?;
        let (glue, wasm) = runtime_artifacts(runtime);
        let artifact = |name: &str| {
            current.artifacts.get(name).ok_or_else(|| {
                RuntimeStateError::Invalid(format!("the installed generation has no {name}"))
            })
        };
        let launch = LaunchIdentity {
            generation_id: current.id.clone(),
            runtime: runtime.to_owned(),
            official_glue_sha256: artifact(glue)?.hash.clone(),
            official_wasm_sha256: artifact(wasm)?.hash.clone(),
            mode: if transformed {
                RuntimeMode::Derived
            } else {
                RuntimeMode::Original
            },
            transform_abi: transformed.then_some(crate::wasm::TRANSFORM_ABI),
            compatibility_id: transformed.then(|| build.unwrap().to_owned()),
            nonce: nonce.to_owned(),
        };
        if state
            .failed_runtimes
            .iter()
            .any(|failed| same_runtime_tuple(failed, &launch))
        {
            return Err(RuntimeStateError::Invalid(
                "this exact runtime already failed".to_owned(),
            ));
        }
        let previous = std::mem::replace(
            &mut state.launch_state,
            LaunchState::AttemptingRuntime(launch.clone()),
        );
        let previous_frame = state.last_first_frame.take();
        if !self.save(&state) {
            state.launch_state = previous;
            state.last_first_frame = previous_frame;
            return Err(RuntimeStateError::NotSaved);
        }
        Ok(launch)
    }

    /// The derived module failed before gameplay, so remember to serve the
    /// exact official module for this runtime/artifact from now on.
    pub fn disable_launch_transform(
        &self,
        failed: &LaunchIdentity,
    ) -> std::result::Result<(), RuntimeStateError> {
        let mut state = self.state.lock().unwrap();
        let LaunchState::AttemptingRuntime(launch) = &state.launch_state else {
            return Err(RuntimeStateError::Invalid(
                "no derived runtime is being attempted".to_owned(),
            ));
        };
        if launch != failed || launch.mode != RuntimeMode::Derived {
            return Err(RuntimeStateError::Invalid(
                "the transform failure does not match the active launch".to_owned(),
            ));
        }
        let build = failed.compatibility_id.as_deref().ok_or_else(|| {
            RuntimeStateError::Invalid("a derived launch has no compatibility identity".to_owned())
        })?;
        let previous_disabled = state.disabled_transforms.clone();
        remember_disabled(&mut state, &failed.runtime, build);
        state.launch_state = LaunchState::Idle;
        if !self.save(&state) {
            state.disabled_transforms = previous_disabled;
            state.launch_state = LaunchState::AttemptingRuntime(failed.clone());
            return Err(RuntimeStateError::NotSaved);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn disable_transform(
        &self,
        runtime: &str,
        build: &str,
    ) -> std::result::Result<(), RuntimeStateError> {
        let mut state = self.state.lock().unwrap();
        let before = state.clone();
        remember_disabled(&mut state, runtime, build);
        if !self.save(&state) {
            *state = before;
            return Err(RuntimeStateError::NotSaved);
        }
        Ok(())
    }

    /// Recover an unproven launch without blaming an optional transform on the
    /// official ArenaNet generation.
    ///
    /// A transformed attempt disables only that exact transform and retries the
    /// same generation. A launch that never reached the client is also retried.
    /// Only an attempted unmodified runtime may restore and reject a generation.
    pub fn recover(&self, root: &Path) -> Recovery {
        let mut state = self.state.lock().unwrap();

        // `stash` commits `previous` before download or live-file promotion.
        // Settle an interrupted transaction before interpreting a page attempt.
        //
        // For a proven current set, `previous` is the entry generation and
        // `record` was never reached. For an unproven current set, `previous`
        // predates this sync and remains a rollback target; keep it while the
        // current files are still exact, restore it only when they are not.
        if let Some(previous) = state.previous.clone() {
            let gameplay_proven = matches!(state.proof_state, Some(ProofState::GameplayProven(_)));
            let live_is_expected = state.current.as_ref().is_some_and(|current| {
                generation_matches(root, &self.active_manifest, current).is_ok()
            });
            if gameplay_proven && live_is_expected {
                let saved_previous = state.previous.take();
                let saved_proof = state.previous_proof.take();
                if !self.save(&state) {
                    state.previous = saved_previous;
                    state.previous_proof = saved_proof;
                    return Recovery::None;
                }
                let _ = fs::remove_dir_all(self.dir.join("previous"));
            } else if !live_is_expected {
                match self.restore_recorded(root, &previous) {
                    Ok(()) => {
                        let before = state.clone();
                        state.current = Some(previous);
                        state.proof_state = state.previous_proof.take();
                        state.last_first_frame = None;
                        state.launch_state = LaunchState::Idle;
                        state.previous = None;
                        if !self.save(&state) {
                            *state = before;
                            return Recovery::None;
                        }
                        let _ = fs::remove_dir_all(self.dir.join("previous"));
                        return Recovery::InstallationRestored;
                    }
                    Err(reason) => {
                        note!(
                            "[generation] cannot recover the interrupted installation — {reason}"
                        );
                        return Recovery::None;
                    }
                }
            }
        }

        let LaunchState::AttemptingRuntime(launch) = state.launch_state.clone() else {
            return Recovery::None;
        };
        // A transform can arrive later than the official client generation
        // through the signed feed. The generation may therefore already be
        // proven when this exact transform fails. Judge the transform before
        // consulting the generation proof so it cannot become permanently
        // crash-looped behind an older successful first frame.
        if launch.mode == RuntimeMode::Derived {
            let Some(build) = launch.compatibility_id.clone() else {
                return Recovery::None;
            };
            let before = state.clone();
            remember_disabled(&mut state, &launch.runtime, &build);
            state.launch_state = LaunchState::Idle;
            if !self.save(&state) {
                *state = before;
                return Recovery::None;
            }
            return Recovery::TransformDisabled {
                runtime: launch.runtime,
                build,
            };
        }
        let before = state.clone();
        state.failed_runtimes.push(launch.clone());
        bound_history(&mut state);
        state.launch_state = LaunchState::FailedRuntime(launch.clone());
        if !self.save(&state) {
            *state = before;
            return Recovery::None;
        }
        Recovery::RuntimeFailed(launch)
    }

    /// Copy the current artifacts aside so a sync can be undone.
    ///
    /// Called before the download, because afterwards there is nothing left to
    /// copy. A set that has never been proven is not stashed — it is not a
    /// rollback target, and overwriting the stash with it would throw away the
    /// last set that did work.
    /// Returns false only when a proven generation needed protection and could
    /// not be copied. A caller replacing a complete client should then keep it.
    pub fn stash(&self, root: &Path, names: &[&'static str]) -> bool {
        if self.future_format.is_some() {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        if state.previous.is_some() {
            return false;
        }
        if !matches!(state.proof_state, Some(ProofState::GameplayProven(_))) {
            return state.current.is_none();
        }
        let Some(mut current) = state.current.clone() else {
            return true;
        };
        // Older state records did not include the active-manifest digest.
        // Validate every recorded client artifact here, then measure and attach
        // the manifest while copying it below. That keeps the format migration
        // safe without ever accepting changed client bytes as a rollback target.
        let verified = if current.manifest.is_some() {
            generation_matches(root, &self.active_manifest, &current)
        } else {
            // Records written before the active manifest joined the
            // generation format can still migrate. Their artifact digests are
            // the only durable evidence they carry.
            artifacts_match(root, &current)
        };
        if let Err(reason) = verified {
            note!(
                "[generation] cannot stash the current client because it no longer matches its \
                 proven record ({reason})"
            );
            return false;
        }
        let stash = self.dir.join("previous");
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let staging = self.dir.join(format!("previous.{sequence}.tmp"));
        let mut copy = || -> Result<()> {
            fs::create_dir_all(&staging)?;
            for name in names {
                fs::copy(root.join(name), staging.join(name))?;
                fs::File::open(staging.join(name))?.sync_all()?;
            }
            let manifest = hash_file(&self.active_manifest)?;
            fs::copy(&self.active_manifest, staging.join("manifest.cache"))?;
            fs::File::open(staging.join("manifest.cache"))?.sync_all()?;
            fs::File::open(&staging)?.sync_all()?;
            let _ = fs::remove_dir_all(&stash);
            fs::rename(&staging, &stash)?;
            fs::File::open(&self.dir)?.sync_all()?;
            current.manifest = Some(manifest);
            Ok(())
        };
        if let Err(e) = copy() {
            note!(
                "[generation] could not stash the current client ({e}); a bad sync will not be undoable"
            );
            let _ = fs::remove_dir_all(&staging);
            return false;
        }
        let prior_previous = state.previous.replace(current);
        let proof = state.proof_state.clone().unwrap();
        let prior_previous_proof = state.previous_proof.replace(proof);
        if !self.save(&state) {
            state.previous = prior_previous;
            state.previous_proof = prior_previous_proof;
            return false;
        }
        true
    }

    /// Verify and restore the exact generation in `previous/`.
    fn restore_recorded(
        &self,
        root: &Path,
        generation: &Generation,
    ) -> std::result::Result<(), String> {
        let stash = self.dir.join("previous");
        for (name, expected) in &generation.artifacts {
            check(&stash.join(name), expected)
                .map_err(|reason| format!("the stashed {name}: {reason}"))?;
        }
        let expected_manifest = generation
            .manifest
            .as_ref()
            .ok_or_else(|| "the previous manifest was not recorded".to_owned())?;
        let stashed_manifest = stash.join("manifest.cache");
        check(&stashed_manifest, expected_manifest)
            .map_err(|reason| format!("the stashed manifest: {reason}"))?;
        for name in generation.artifacts.keys() {
            copy_atomic(&stash.join(name), &root.join(name))
                .map_err(|error| format!("restoring {name}: {error}"))?;
        }
        copy_atomic(&stashed_manifest, &self.active_manifest)
            .map_err(|error| format!("restoring the manifest: {error}"))
    }

    /// Record a freshly written set as current, unproven — unless it is the set
    /// that was already here.
    ///
    /// Writing the same generation again is not a new generation. Both the
    /// `sync` command and a repair can do it. Calling either of those unproven
    /// arms a rollback whose target is a copy of the same set, so an app that
    /// then dies before its first frame restores what was already on disk and
    /// refuses, by name, the generation it just restored. A set that has booted
    /// here has not stopped having booted here.
    pub fn record(&self, id: &str, root: &Path, names: &[&'static str]) -> bool {
        let Some((artifacts, manifest)) = weigh(root, names, &self.active_manifest) else {
            return false;
        };
        let mut state = self.state.lock().unwrap();
        let generation = Generation {
            id: id.to_owned(),
            artifacts,
            manifest: Some(manifest),
        };
        let same = state
            .current
            .as_ref()
            .is_some_and(|current| current.id == id && current.artifacts == generation.artifacts);
        let before = state.clone();
        state.current = Some(generation);
        if !same {
            state.proof_state = Some(ProofState::InstalledUnproven);
            state.last_first_frame = None;
            state.failed_runtimes.clear();
        }
        state.launch_state = LaunchState::Idle;
        let proven = same && matches!(state.proof_state, Some(ProofState::GameplayProven(_)));
        if proven {
            state.previous = None;
            state.previous_proof = None;
        }
        if !self.save(&state) {
            *state = before;
            return false;
        }
        if proven {
            note!("[generation] client generation {id} reinstalled; it had already booted here");
        } else {
            note!("[generation] client generation {id} installed, not yet proven");
        }
        if state.previous.is_none() {
            let _ = fs::remove_dir_all(self.dir.join("previous"));
        }
        true
    }

    /// Take an installation that predates the record as the current set.
    ///
    /// Without this, a client that was already on the disk is only ever
    /// existence-checked — `unsound` has nothing to compare it against — so the
    /// install most likely to have rotted is the one nothing is watching, until
    /// some future patch happens to replace it. Hashing it once is a single pass
    /// over nine megabytes and makes every later launch a real check.
    ///
    /// Its id is deliberately not a generation id — no manifest
    /// can produce this string — because what is on the disk is known but which
    /// build it came from is not.
    pub fn adopt(&self, root: &Path, names: &[&'static str]) {
        if self.state.lock().unwrap().current.is_some() {
            return;
        }
        let Some((artifacts, manifest)) = weigh(root, names, &self.active_manifest) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        let before = state.clone();
        state.current = Some(Generation {
            id: ADOPTED.to_owned(),
            artifacts,
            manifest: Some(manifest),
        });
        state.proof_state = Some(ProofState::InstalledUnproven);
        state.launch_state = LaunchState::Idle;
        if !self.save(&state) {
            *state = before;
            return;
        }
        note!("[generation] adopted the client already on disk; it is checked from now on");
    }

    /// Acknowledge the exact launch whose renderer presented a first frame.
    ///
    /// Idempotent, and cheap when there is nothing to do: this is called from a
    /// request handler on a route the page hits once per launch, and every
    /// launch after the first finds the set already proven.
    pub fn prove_first_frame(
        &self,
        claimed: &LaunchIdentity,
    ) -> std::result::Result<(), RuntimeStateError> {
        let mut state = self.state.lock().unwrap();
        if state.launch_state == LaunchState::Idle
            && state.last_first_frame.as_ref() == Some(claimed)
        {
            return Ok(());
        }
        let LaunchState::AttemptingRuntime(launch) = state.launch_state.clone() else {
            return Err(RuntimeStateError::Invalid(
                "no matching runtime is awaiting first-frame proof".to_owned(),
            ));
        };
        if &launch != claimed {
            return Err(RuntimeStateError::Invalid(
                "first-frame proof does not match the active launch".to_owned(),
            ));
        }
        let proof = state.proof_state.clone();
        let first_frame = state.last_first_frame.clone();
        state.proof_state = Some(match proof {
            Some(ProofState::GameplayProven(ref proven)) => {
                ProofState::GameplayProven(proven.clone())
            }
            _ => ProofState::FirstFrameProven(launch.clone()),
        });
        state.launch_state = LaunchState::Idle;
        state.last_first_frame = Some(launch.clone());
        if !self.save(&state) {
            state.proof_state = proof;
            state.launch_state = LaunchState::AttemptingRuntime(launch);
            state.last_first_frame = first_frame;
            return Err(RuntimeStateError::NotSaved);
        }
        let id = state.current.as_ref().map_or("", |g| g.id.as_str());
        note!("[generation] client generation {id} reached a first frame");
        Ok(())
    }

    /// Promote the latest first-frame proof after this session opens an
    /// allowed ArenaNet TCP connection. The socket is host-owned and tied to
    /// this loopback server's capability token and exact launch identity, so a
    /// page from an older session cannot nominate a newer launch.
    pub fn launch_for_gameplay(&self) -> Option<LaunchIdentity> {
        let state = self.state.lock().unwrap();
        match &state.launch_state {
            LaunchState::AttemptingRuntime(launch) => Some(launch.clone()),
            LaunchState::Idle => state.last_first_frame.clone(),
            LaunchState::FailedRuntime(_) => None,
        }
    }

    pub fn prove_gameplay(
        &self,
        observed: &LaunchIdentity,
    ) -> std::result::Result<bool, RuntimeStateError> {
        let mut state = self.state.lock().unwrap();
        let Some(launch) = state.last_first_frame.clone() else {
            return Ok(false);
        };
        if &launch != observed {
            return Ok(false);
        }
        let eligible = match &state.proof_state {
            Some(ProofState::FirstFrameProven(proven)) => proven == &launch,
            Some(ProofState::GameplayProven(_)) => true,
            _ => false,
        };
        if !eligible {
            return Ok(false);
        }
        if state.proof_state == Some(ProofState::GameplayProven(launch.clone())) {
            return Ok(true);
        }
        let proof = state
            .proof_state
            .replace(ProofState::GameplayProven(launch));
        if !self.save(&state) {
            state.proof_state = proof;
            return Err(RuntimeStateError::NotSaved);
        }
        note!("[generation] the current client reached an ArenaNet game connection");
        Ok(true)
    }
}

/// Measure every named artifact under `root`, or nothing.
///
/// All-or-nothing on purpose: a half-recorded set would make [`Store::unsound`]
/// report the unrecorded half as fine and the recorded half as checkable, which
/// is a worse answer than falling back to existence checks until the next sync.
fn weigh(
    root: &Path,
    names: &[&'static str],
    active_manifest: &Path,
) -> Option<(BTreeMap<String, Artifact>, Artifact)> {
    let synced = |path: &Path| {
        fs::File::open(path)
            .and_then(|file| file.sync_all())
            .is_ok()
    };
    if !synced(root) || !synced(active_manifest.parent().unwrap_or(root)) {
        return None;
    }
    let mut artifacts = BTreeMap::new();
    for name in names {
        if !synced(&root.join(name)) {
            return None;
        }
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
    if !synced(active_manifest) {
        return None;
    }
    let manifest = match hash_file(active_manifest) {
        Ok(manifest) => manifest,
        Err(e) => {
            note!("[generation] could not record the active manifest ({e})");
            return None;
        }
    };
    Some((artifacts, manifest))
}

fn validate_runtime_attempt(
    runtime: &str,
    build: Option<&str>,
    transformed: bool,
) -> std::result::Result<(), RuntimeStateError> {
    if runtime != "jspi" && runtime != "asyncify" {
        return Err(RuntimeStateError::Invalid(
            "runtime must be jspi or asyncify".to_owned(),
        ));
    }
    if transformed && build.is_none() {
        return Err(RuntimeStateError::Invalid(
            "a transformed runtime must name its artifact".to_owned(),
        ));
    }
    if let Some(build) = build
        && !valid_digest(build)
    {
        return Err(RuntimeStateError::Invalid(
            "runtime artifact must be a lowercase SHA-256".to_owned(),
        ));
    }
    Ok(())
}

fn validate_launch_nonce(nonce: &str) -> std::result::Result<(), RuntimeStateError> {
    if !valid_digest(nonce) {
        return Err(RuntimeStateError::Invalid(
            "launch nonce must be 32 random bytes in lowercase hex".to_owned(),
        ));
    }
    Ok(())
}

fn remember_disabled(state: &mut State, runtime: &str, build: &str) {
    if let Some(position) = state
        .disabled_transforms
        .iter()
        .position(|disabled| disabled.runtime == runtime && disabled.build == build)
    {
        let disabled = state.disabled_transforms.remove(position);
        state.disabled_transforms.push(disabled);
    } else {
        state.disabled_transforms.push(DisabledTransform {
            runtime: runtime.to_owned(),
            build: build.to_owned(),
        });
    }
    let excess = state
        .disabled_transforms
        .len()
        .saturating_sub(DISABLED_TRANSFORMS_KEPT);
    state.disabled_transforms.drain(..excess);
}

fn copy_atomic(source: &Path, destination: &Path) -> Result<()> {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = destination.with_extension(format!("{}.{sequence}.tmp", std::process::id()));
    fs::copy(source, &temporary)?;
    fs::File::open(&temporary)?.sync_all()?;
    fs::rename(&temporary, destination).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    fs::File::open(destination.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()?;
    Ok(())
}

/// Whether every client artifact still is the generation that was recorded.
fn artifacts_match(root: &Path, generation: &Generation) -> std::result::Result<(), String> {
    for (name, expected) in &generation.artifacts {
        check(&root.join(name), expected).map_err(|reason| format!("{name}: {reason}"))?;
    }
    Ok(())
}

/// Whether the live client and active manifest are the exact recorded pair.
fn generation_matches(
    root: &Path,
    active_manifest: &Path,
    generation: &Generation,
) -> std::result::Result<(), String> {
    artifacts_match(root, generation)?;
    let expected = generation
        .manifest
        .as_ref()
        .ok_or_else(|| "the active manifest was not recorded".to_owned())?;
    check(active_manifest, expected).map_err(|reason| format!("manifest.cache: {reason}"))
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

    const NAMES: [&str; 5] = [
        "Gw.jspi.js",
        "Gw.jspi.wasm",
        "Gw.js",
        "Gw.wasm",
        "version.json",
    ];
    const OFFERING_NAMES: [&str; 2] = ["Gw.jspi.js", "Gw.jspi.wasm"];
    const BUILD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NONCE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn write_client(root: &Path, flavour: &str) {
        fs::create_dir_all(root).unwrap();
        for name in NAMES {
            fs::write(root.join(name), format!("{flavour}:{name}")).unwrap();
        }
        fs::write(
            root.parent().unwrap().join("manifest.cache"),
            format!("{flavour}:manifest"),
        )
        .unwrap();
    }

    /// A manifest offering [`NAMES`], each one chunk long.
    ///
    /// The two knobs are the two things [`identify`] reads besides the name: how
    /// long each artifact is, and what its chunks hash to.
    fn offering(sizes: [u64; 2], chunks: [char; 2]) -> Manifest {
        let files: Vec<String> = OFFERING_NAMES
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
        let launch = attempt(&store, false);
        {
            let mut state = store.state.lock().unwrap();
            state.proof_state = Some(ProofState::GameplayProven(launch));
            state.launch_state = LaunchState::Idle;
        }
        let saved = store.state.lock().unwrap().clone();
        store.save(&saved);
        store
    }

    fn attempt(store: &Store, transformed: bool) -> LaunchIdentity {
        store
            .record_attempt("jspi", Some(BUILD), transformed, NONCE)
            .unwrap()
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
        assert_eq!(store.recover(&root), Recovery::None);

        // Adoption happens once. A real build recorded later owns the slot.
        write_client(&root, "installed-long-ago");
        store.stash(&root, &NAMES);
        write_client(&root, "patched");
        store.record("0123456789abcdef", &root, &NAMES);
        store.adopt(&root, &NAMES);
        assert_eq!(
            store.state.lock().unwrap().current.as_ref().unwrap().id,
            "0123456789abcdef",
            "adopt must not overwrite the recorded build"
        );
    }

    #[test]
    fn a_generation_never_durably_attempted_is_not_rejected() {
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
        assert_eq!(store.recover(&root), Recovery::None);
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "new:Gw.jspi.js",
            "an installed generation with no launch record stays available"
        );
        assert!(!store.rejected("new"));
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn a_failed_transform_is_disabled_without_rolling_back_official_files() {
        let temp = TempDir::new("generation-transform-fallback");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        store.stash(&root, &NAMES);
        write_client(&root, "new");
        store.record("new", &root, &NAMES);
        attempt(&store, true);

        assert_eq!(
            store.recover(&root),
            Recovery::TransformDisabled {
                runtime: "jspi".to_owned(),
                build: BUILD.to_owned(),
            }
        );
        assert!(store.transform_disabled("jspi", BUILD));
        assert!(!store.rejected("new"));
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "new:Gw.jspi.js",
            "a derived-module failure must keep ArenaNet's exact generation"
        );
        assert_eq!(
            fs::read_to_string(temp.0.join("manifest.cache")).unwrap(),
            "new:manifest",
            "the active manifest must stay paired with that generation"
        );

        // The next launch serves the official module. Only if that attempt also
        // fails is the official generation itself eligible for rollback.
        let official = attempt(&store, false);
        assert_eq!(store.recover(&root), Recovery::RuntimeFailed(official));
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "new:Gw.jspi.js"
        );
        assert_eq!(
            fs::read_to_string(temp.0.join("manifest.cache")).unwrap(),
            "new:manifest"
        );
    }

    #[test]
    fn runtime_state_is_not_acknowledged_without_a_durable_record() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new("generation-undurable-runtime-state");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");
        let launch = attempt(&store, true);
        fs::set_permissions(&state, fs::Permissions::from_mode(0o500)).unwrap();

        assert_eq!(
            store.record_attempt("jspi", Some(BUILD), true, NONCE),
            Err(RuntimeStateError::NotSaved),
        );
        assert_eq!(
            store.disable_launch_transform(&launch),
            Err(RuntimeStateError::NotSaved),
        );
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_post_rename_transform_error_cannot_invent_an_official_attempt() {
        const BUILD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let temp = TempDir::new("generation-transform-post-rename");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");
        store.stash(&root, &NAMES);
        write_client(&root, "new");
        store.record("new", &root, &NAMES);
        let launch = store
            .record_attempt("jspi", Some(BUILD), true, NONCE)
            .unwrap();

        *store.write_failure.lock().unwrap() = Some(WriteBoundary::AfterRename);
        assert_eq!(
            store.disable_launch_transform(&launch),
            Err(RuntimeStateError::NotSaved)
        );
        drop(store);

        let reopened = Store::open(state);
        assert_eq!(reopened.recover(&root), Recovery::None);
        assert!(!reopened.rejected("new"));
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "new:Gw.jspi.js"
        );
    }

    #[test]
    fn state_writes_fail_closed_at_every_durable_boundary() {
        for boundary in [
            WriteBoundary::BeforeTemporary,
            WriteBoundary::StorageFull,
            WriteBoundary::AfterWrite,
            WriteBoundary::AfterFileSync,
            WriteBoundary::AfterRename,
            WriteBoundary::AfterDirectorySync,
        ] {
            let temp = TempDir::new("generation-state-boundary");
            let root = temp.0.join("web");
            let state = temp.0.join("state");
            let store = proven(state.clone(), &root, "working");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let reader = std::thread::spawn({
                let state = state.clone();
                let barrier = barrier.clone();
                move || {
                    barrier.wait();
                    (0..16).for_each(|_| drop(Store::open(state.clone())));
                }
            });
            *store.write_failure.lock().unwrap() = Some(boundary);
            barrier.wait();
            assert_eq!(
                store.record_attempt("jspi", Some(BUILD), true, NONCE),
                Err(RuntimeStateError::NotSaved),
            );
            drop(store);
            reader.join().unwrap();
            assert!(
                serde_json::from_slice::<State>(&fs::read(state.join("state.json")).unwrap())
                    .is_ok()
            );
            let reopened = Store::open(state);
            let durable = matches!(
                boundary,
                WriteBoundary::AfterRename | WriteBoundary::AfterDirectorySync
            );
            assert_eq!(
                matches!(
                    reopened.state.lock().unwrap().launch_state,
                    LaunchState::AttemptingRuntime(_)
                ),
                durable
            );
        }
    }

    #[test]
    fn post_rename_failures_never_delete_or_invent_runtime_evidence() {
        let temp = TempDir::new("generation-post-rename");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");
        *store.write_failure.lock().unwrap() = Some(WriteBoundary::AfterRename);
        assert!(!store.stash(&root, &NAMES));
        drop(store);
        let store = Store::open(state.clone());
        assert!(state.join("previous").is_dir());
        assert!(store.state.lock().unwrap().previous.is_some());

        assert_eq!(store.recover(&root), Recovery::None);
        let launch = attempt(&store, true);
        *store.write_failure.lock().unwrap() = Some(WriteBoundary::AfterRename);
        assert_eq!(
            store.disable_launch_transform(&launch),
            Err(RuntimeStateError::NotSaved)
        );
        drop(store);
        let store = Store::open(state);
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(store.transform_disabled("jspi", BUILD));
        assert!(matches!(
            store.state.lock().unwrap().launch_state,
            LaunchState::Idle
        ));
    }

    #[test]
    fn failed_generation_record_restores_in_memory_authority() {
        let temp = TempDir::new("generation-record-failure");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state, &root, "old");
        assert!(store.stash(&root, &NAMES));
        write_client(&root, "new");
        *store.write_failure.lock().unwrap() = Some(WriteBoundary::StorageFull);
        assert!(!store.record("new", &root, &NAMES));
        assert_eq!(
            store.state.lock().unwrap().current.as_ref().unwrap().id,
            "old"
        );
        assert_eq!(store.recover(&root), Recovery::InstallationRestored);
    }

    #[test]
    fn proof_survives_attempts_and_exact_failures_are_bounded() {
        let temp = TempDir::new("generation-proof-preserved");
        let root = temp.0.join("web");
        let state_dir = temp.0.join("state");
        let store = proven(state_dir.clone(), &root, "working");
        fs::write(root.join("Gw.js"), "official asyncify glue").unwrap();
        fs::write(root.join("Gw.wasm"), "official asyncify wasm").unwrap();
        {
            let mut state = store.state.lock().unwrap();
            let current = state.current.as_mut().unwrap();
            current
                .artifacts
                .insert("Gw.js".to_owned(), hash_file(&root.join("Gw.js")).unwrap());
            current.artifacts.insert(
                "Gw.wasm".to_owned(),
                hash_file(&root.join("Gw.wasm")).unwrap(),
            );
            assert!(store.save(&state));
        }
        let failed = attempt(&store, false);
        assert!(matches!(
            store.state.lock().unwrap().proof_state,
            Some(ProofState::GameplayProven(_))
        ));
        assert_eq!(store.recover(&root), Recovery::RuntimeFailed(failed));
        let asyncify = store
            .record_attempt("asyncify", None, false, NONCE)
            .unwrap();
        assert_eq!(store.recover(&root), Recovery::RuntimeFailed(asyncify));
        drop(store);
        let store = Store::open(state_dir);
        assert!(matches!(
            store.record_attempt("jspi", Some(BUILD), false, NONCE),
            Err(RuntimeStateError::Invalid(_))
        ));
        assert_eq!(store.state.lock().unwrap().failed_runtimes.len(), 2);
    }

    #[test]
    fn future_state_keeps_unknown_official_launch_available_without_rewrite() {
        let temp = TempDir::new("generation-format-migration");
        let state = temp.0.join("state");
        fs::create_dir_all(&state).unwrap();
        let future = br#"{"formatVersion":99,"opaque":{"doNot":"rewrite"}}"#;
        fs::write(state.join("state.json"), future).unwrap();
        let refused = Store::open(state.clone());
        assert!(!refused.stale("anything"));
        let root = temp.0.join("web");
        write_client(&root, "unknown-official");
        refused.adopt(&root, &NAMES);
        assert!(
            refused
                .record_attempt("jspi", Some(BUILD), false, NONCE)
                .is_ok()
        );
        assert_eq!(fs::read(state.join("state.json")).unwrap(), future);
        assert!(state.join("state.compat-v2.json").is_file());
        drop(refused);
        assert!(matches!(
            Store::open(state.clone())
                .state
                .lock()
                .unwrap()
                .launch_state,
            LaunchState::AttemptingRuntime(_)
        ));
    }

    #[test]
    fn a_later_transform_failure_is_not_hidden_by_a_proven_generation() {
        let temp = TempDir::new("generation-proven-transform");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "shipped");

        // A signed certificate can arrive after these official bytes have
        // already booted. Its first failed launch must still disable it.
        attempt(&store, true);
        assert!(matches!(
            store.recover(&root),
            Recovery::TransformDisabled { .. }
        ));
        assert!(store.transform_disabled("jspi", BUILD));

        // A successful official retry clears the launch attempt even though
        // the generation proof was already true before this launch.
        let launch = store
            .record_attempt("jspi", Some(BUILD), false, NONCE)
            .unwrap();
        store.prove_first_frame(&launch).unwrap();
        drop(store);
        let store = Store::open(state);
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(matches!(
            store.state.lock().unwrap().proof_state,
            Some(ProofState::GameplayProven(_))
        ));
    }

    /// The other half of that sequence: the sync that never gets past the
    /// stash. A download can fail on any of the artifacts, and what it leaves
    /// behind must not look like a build worth going back to.
    #[test]
    fn a_sync_that_downloads_nothing_restores_and_clears_its_stash() {
        let temp = TempDir::new("generation-failed-sync");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        store.stash(&root, &NAMES);
        assert!(state.join("previous").is_dir());

        // The download failed here — nothing was overwritten and nothing was
        // recorded, so the set on disk is still the proven one.
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(
            !state.join("previous").exists(),
            "and nothing on disk either"
        );

        let store = Store::open(state);
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn a_maintenance_download_is_not_a_failed_client_attempt() {
        let temp = TempDir::new("generation-maintenance-download");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        store.stash(&root, &NAMES);
        write_client(&root, "downloaded");
        store.record("downloaded", &root, &NAMES);
        drop(store);

        let store = Store::open(state);
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(!store.rejected("downloaded"));
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "downloaded:Gw.jspi.js"
        );
    }

    #[test]
    fn an_interrupted_download_discards_the_redundant_stash_on_next_launch() {
        let temp = TempDir::new("generation-interrupted-download");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        assert!(store.stash(&root, &NAMES));
        assert!(state.join("previous").is_dir());
        drop(store); // The process disappeared before a live file changed.

        let store = Store::open(state.clone());
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(!state.join("previous").exists());
        assert!(store.state.lock().unwrap().previous.is_none());
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn an_interrupted_promotion_restores_the_entry_generation() {
        let temp = TempDir::new("generation-interrupted-promotion");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        assert!(store.stash(&root, &NAMES));
        write_client(&root, "new");
        drop(store); // The process disappeared before `record`.

        let store = Store::open(state.clone());
        assert_eq!(store.recover(&root), Recovery::InstallationRestored);
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "old:Gw.jspi.js"
        );
        assert_eq!(
            fs::read_to_string(temp.0.join("manifest.cache")).unwrap(),
            "old:manifest"
        );
        assert!(!state.join("previous").exists());
        assert!(!store.rejected("new"));
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn an_interrupted_repair_of_an_unproven_set_restores_the_proven_predecessor() {
        let temp = TempDir::new("generation-interrupted-unproven-repair");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        assert!(store.stash(&root, &NAMES));
        write_client(&root, "new");
        store.record("new", &root, &NAMES);
        // A repair started before this generation reached a frame, promoted
        // only one live artifact, and the process disappeared.
        fs::write(root.join("Gw.jspi.js"), "partial").unwrap();
        drop(store);

        let store = Store::open(state);
        assert_eq!(store.recover(&root), Recovery::InstallationRestored);
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "old:Gw.jspi.js"
        );
        assert_eq!(
            fs::read_to_string(temp.0.join("manifest.cache")).unwrap(),
            "old:manifest"
        );
        assert!(!store.rejected("new"));
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn corrupted_live_bytes_are_never_saved_as_a_rollback_target() {
        let temp = TempDir::new("generation-corrupt-stash");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");

        fs::write(root.join("Gw.jspi.js"), "corrupt").unwrap();

        assert!(!store.stash(&root, &NAMES));
        assert!(!state.join("previous").exists());
        assert!(store.state.lock().unwrap().previous.is_none());
    }

    #[test]
    fn a_changed_manifest_is_never_saved_as_a_rollback_target() {
        let temp = TempDir::new("generation-corrupt-manifest-stash");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");

        fs::write(temp.0.join("manifest.cache"), "changed:manifest").unwrap();

        assert!(!store.stash(&root, &NAMES));
        assert!(!state.join("previous").exists());
        assert!(store.state.lock().unwrap().previous.is_none());
    }

    #[test]
    fn a_stash_is_not_armed_without_a_durable_record() {
        let temp = TempDir::new("generation-undurable-stash");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");

        fs::remove_file(state.join("state.json")).unwrap();
        fs::create_dir(state.join("state.json")).unwrap();

        assert!(!store.stash(&root, &NAMES));
        assert!(state.join("previous").is_dir());
        assert!(store.state.lock().unwrap().previous.is_none());
    }

    #[test]
    fn a_pre_manifest_record_can_still_be_stashed_safely() {
        let temp = TempDir::new("generation-old-record-migration");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");

        {
            let mut saved = store.state.lock().unwrap();
            saved.current.as_mut().unwrap().manifest = None;
            store.save(&saved);
        }
        drop(store);

        let store = Store::open(state.clone());
        assert!(store.stash(&root, &NAMES));
        let saved = store.state.lock().unwrap();
        assert!(
            saved
                .previous
                .as_ref()
                .and_then(|generation| generation.manifest.as_ref())
                .is_some(),
            "stashing migrates the old record by measuring its active manifest"
        );
    }

    #[test]
    fn replacing_a_complete_client_requires_a_complete_rollback_stash() {
        let temp = TempDir::new("generation-stash-precondition");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");

        // The files are readable, but their paired manifest cannot be
        // preserved. The updater must learn that before touching the live set.
        fs::remove_file(temp.0.join("manifest.cache")).unwrap();
        assert!(!store.stash(&root, &NAMES));
        assert!(!state.join("previous").exists());
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "working:Gw.jspi.js"
        );
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
        assert_eq!(store.recover(&root), Recovery::None);
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
        let launch = attempt(&store, false);
        let mut stale = launch.clone();
        stale.nonce = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".into();
        assert!(matches!(
            store.prove_first_frame(&stale),
            Err(RuntimeStateError::Invalid(_))
        ));
        let mut wrong_runtime = launch.clone();
        wrong_runtime.runtime = "asyncify".into();
        assert!(matches!(
            store.prove_first_frame(&wrong_runtime),
            Err(RuntimeStateError::Invalid(_))
        ));
        let mut wrong_artifact = launch.clone();
        wrong_artifact.official_wasm_sha256 = "ee".repeat(32);
        assert!(matches!(
            store.prove_first_frame(&wrong_artifact),
            Err(RuntimeStateError::Invalid(_))
        ));
        store.prove_first_frame(&launch).unwrap();
        store.prove_first_frame(&launch).unwrap();
        assert!(
            state.join("previous").exists(),
            "first-frame proof must preserve the gameplay-proven predecessor"
        );

        let store = Store::open(state);
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(!store.rejected("new"));
        assert!(store.unsound(&root, &NAMES).is_empty());
    }

    #[test]
    fn a_game_connection_retires_only_the_proven_predecessor() {
        let temp = TempDir::new("generation-gameplay-proof");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");
        store.stash(&root, &NAMES);
        write_client(&root, "new");
        store.record("new", &root, &NAMES);
        let launch = attempt(&store, false);
        store.prove_first_frame(&launch).unwrap();
        assert!(store.prove_gameplay(&launch).unwrap());
        let second = store
            .record_attempt(
                "jspi",
                Some(BUILD),
                false,
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            )
            .unwrap();
        assert!(
            !store.prove_gameplay(&launch).unwrap(),
            "sessions must not be mixed"
        );
        store.prove_first_frame(&second).unwrap();
        assert!(
            matches!(
                store.prove_first_frame(&launch),
                Err(RuntimeStateError::Invalid(_))
            ),
            "an older acknowledgement must not become idempotent again"
        );
        assert!(store.prove_gameplay(&second).unwrap());
        drop(store);

        let store = Store::open(state.clone());
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(!state.join("previous").exists());
        assert!(matches!(
            store.state.lock().unwrap().proof_state,
            Some(ProofState::GameplayProven(ref proven)) if proven == &second
        ));
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
        let failed = attempt(&store, false);
        assert_eq!(store.recover(&root), Recovery::RuntimeFailed(failed));
        assert!(store.unsound(&root, &NAMES).is_empty());
        assert!(!store.rejected("first"));
    }

    #[test]
    fn a_record_holding_a_truncated_digest_has_no_authority() {
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
        assert!(store.state.lock().unwrap().current.is_none());
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
    fn persisted_histories_do_not_accumulate_without_bound() {
        let temp = TempDir::new("generation-refusals");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "keeper");
        {
            let mut saved = store.state.lock().unwrap();
            saved.rejected = (0..REJECTED_KEPT + 3)
                .map(|round| format!("bad{round}"))
                .collect();
            store.save(&saved);
        }
        let store = Store::open(state);
        assert_eq!(store.state.lock().unwrap().rejected.len(), REJECTED_KEPT);
        assert!(
            !store.rejected("bad0"),
            "the oldest refusal should have aged out"
        );
        assert!(store.rejected(&format!("bad{}", REJECTED_KEPT + 2)));
    }

    /// A manifest names the build it offers from what it already carries, so a
    /// patch is visible before a byte of it is downloaded. This is the whole
    /// basis of the check: if two manifests offering different bytes could name
    /// the same build, a patch would ship and nothing would install it.
    #[test]
    fn a_manifest_offering_different_bytes_offers_a_different_build() {
        let shipped = identify(&offering([16, 16], ['a', 'b']), &OFFERING_NAMES).unwrap();

        // The same manifest again — the ordinary launch, where nothing has
        // changed and nothing should be fetched.
        assert_eq!(
            identify(&offering([16, 16], ['a', 'b']), &OFFERING_NAMES).unwrap(),
            shipped
        );

        // Different content at the same length, which is what a patch to one
        // artifact usually looks like and what a size check cannot see.
        assert_ne!(
            identify(&offering([16, 16], ['a', 'c']), &OFFERING_NAMES).unwrap(),
            shipped
        );

        // And a different length behind the same chunk hash, which no service
        // would publish but which must not collide either — the size is in the
        // digest for exactly this reason.
        assert_ne!(
            identify(&offering([16, 32], ['a', 'b']), &OFFERING_NAMES).unwrap(),
            shipped
        );

        // Sixteen hex characters, so it can never be mistaken for `ADOPTED`.
        assert_eq!(shipped.len(), 16);
        assert!(shipped.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn an_active_manifest_update_is_reconciled_without_rehashing_the_client() {
        let temp = TempDir::new("generation-refresh-manifest");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "working");
        let before = store
            .state
            .lock()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .manifest
            .clone();

        fs::write(temp.0.join("manifest.cache"), "updated:manifest").unwrap();

        assert!(store.refresh_manifest("working"));
        let saved = store.state.lock().unwrap();
        assert_ne!(
            saved.current.as_ref().unwrap().manifest,
            before,
            "the active manifest digest is updated for the same client generation"
        );
        assert!(matches!(
            saved.proof_state,
            Some(ProofState::GameplayProven(_))
        ));
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
