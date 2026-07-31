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
const DISABLED_TRANSFORMS_KEPT: usize = 256;

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
    /// Manifest-derived patch-generation identity. See the module docs.
    id: String,
    artifacts: BTreeMap<String, Artifact>,
    /// The active manifest that describes this generation's snapshot.
    #[serde(default)]
    manifest: Option<Artifact>,
}

/// What the page actually tried on the most recent launch.
///
/// A transformed failure says nothing about ArenaNet's original client, so it
/// disables only that transform. An untransformed failure is the evidence that
/// can justify rolling the official generation back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeAttempt {
    runtime: String,
    /// Domain-separated SHA-256 of runtime, artifacts, transform ABI and
    /// selected transform output.
    build: Option<String>,
    transformed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisabledTransform {
    runtime: String,
    /// Domain-separated SHA-256 of runtime, artifacts, transform ABI and
    /// selected transform output.
    build: String,
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
    #[serde(default)]
    attempt: Option<RuntimeAttempt>,
    #[serde(default)]
    disabled_transforms: Vec<DisabledTransform>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            format_version: FORMAT,
            current: None,
            proven: false,
            previous: None,
            rejected: Vec::new(),
            attempt: None,
            disabled_transforms: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    None,
    InstallationRestored,
    TransformDisabled { runtime: String, build: String },
    GenerationRolledBack(String),
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
        let active_manifest = dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("manifest.cache");
        Self {
            dir,
            active_manifest,
            state: Mutex::new(state),
        }
    }

    fn save(&self, state: &State) -> bool {
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
            note!("[generation] could not write the record: {e}");
            return false;
        }
        true
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
    ) -> std::result::Result<(), String> {
        validate_runtime_attempt(runtime, build, transformed)?;
        let mut state = self.state.lock().unwrap();
        state.attempt = Some(RuntimeAttempt {
            runtime: runtime.to_owned(),
            build: build.map(str::to_owned),
            transformed,
        });
        self.save(&state);
        Ok(())
    }

    /// The derived module failed before gameplay, so remember to serve the
    /// exact official module for this runtime/artifact from now on.
    pub fn disable_transform(&self, runtime: &str, build: &str) -> std::result::Result<(), String> {
        validate_runtime_attempt(runtime, Some(build), true)?;
        let mut state = self.state.lock().unwrap();
        remember_disabled(&mut state, runtime, build);
        // The page is about to retry the official module in the same launch.
        state.attempt = Some(RuntimeAttempt {
            runtime: runtime.to_owned(),
            build: Some(build.to_owned()),
            transformed: false,
        });
        self.save(&state);
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
            let live_is_expected = if state.proven {
                generation_matches(root, &self.active_manifest, &previous).is_ok()
            } else {
                state.current.as_ref().is_some_and(|current| {
                    generation_matches(root, &self.active_manifest, current).is_ok()
                })
            };
            if state.proven && live_is_expected {
                state.previous = None;
                self.save(&state);
                let _ = fs::remove_dir_all(self.dir.join("previous"));
            } else if !live_is_expected {
                match self.restore_recorded(root, &previous) {
                    Ok(()) => {
                        state.current = Some(previous);
                        state.proven = true;
                        state.previous = None;
                        state.attempt = None;
                        self.save(&state);
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

        let Some(attempt) = state.attempt.take() else {
            return Recovery::None;
        };
        // A transform can arrive later than the official client generation
        // through the signed feed. The generation may therefore already be
        // proven when this exact transform fails. Judge the transform before
        // consulting the generation proof so it cannot become permanently
        // crash-looped behind an older successful first frame.
        if attempt.transformed {
            let Some(build) = attempt.build else {
                self.save(&state);
                return Recovery::None;
            };
            remember_disabled(&mut state, &attempt.runtime, &build);
            self.save(&state);
            return Recovery::TransformDisabled {
                runtime: attempt.runtime,
                build,
            };
        }
        if state.proven {
            // The exact installed generation has reached a first frame before.
            // A later unmodified attempt that did not is not evidence that an
            // ArenaNet patch is bad, and there is no unproven installation to
            // undo. Clear the completed attempt and keep the known client.
            self.save(&state);
            return Recovery::None;
        }
        let (Some(current), Some(previous)) = (state.current.clone(), state.previous.clone())
        else {
            self.save(&state);
            return Recovery::None;
        };

        if let Err(reason) = self.restore_recorded(root, &previous) {
            note!("[generation] cannot roll back — {reason}");
            self.save(&state);
            return Recovery::None;
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
        state.attempt = None;
        self.save(&state);
        let _ = fs::remove_dir_all(self.dir.join("previous"));
        Recovery::GenerationRolledBack(current.id)
    }

    #[cfg(test)]
    fn roll_back(&self, root: &Path) -> Option<String> {
        self.record_attempt("asyncify", None, false).unwrap();
        match self.recover(root) {
            Recovery::GenerationRolledBack(id) => Some(id),
            Recovery::None
            | Recovery::InstallationRestored
            | Recovery::TransformDisabled { .. } => None,
        }
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
        let mut state = self.state.lock().unwrap();
        if !state.proven {
            return true;
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
        let mut copy = || -> Result<()> {
            fs::create_dir_all(&stash)?;
            for name in names {
                fs::copy(root.join(name), stash.join(name))?;
            }
            let manifest = hash_file(&self.active_manifest)?;
            fs::copy(&self.active_manifest, stash.join("manifest.cache"))?;
            current.manifest = Some(manifest);
            Ok(())
        };
        if let Err(e) = copy() {
            note!(
                "[generation] could not stash the current client ({e}); a bad sync will not be undoable"
            );
            let _ = fs::remove_dir_all(&stash);
            return false;
        }
        let prior_previous = state.previous.replace(current);
        if !self.save(&state) {
            state.previous = prior_previous;
            let _ = fs::remove_dir_all(&stash);
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
    pub fn record(&self, id: &str, root: &Path, names: &[&'static str]) {
        let Some((artifacts, manifest)) = weigh(root, names, &self.active_manifest) else {
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
            manifest: Some(manifest),
        });
        state.proven = same && state.proven;
        state.attempt = None;
        if state.proven {
            // And with nothing to undo, nothing to undo it with.
            state.previous = None;
        }
        self.save(&state);
        if state.proven {
            note!("[generation] client generation {id} reinstalled; it had already booted here");
        } else {
            note!("[generation] client generation {id} installed, not yet proven");
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
    /// to is nothing at all. Its id is deliberately not a generation id — no manifest
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
        state.current = Some(Generation {
            id: ADOPTED.to_owned(),
            artifacts,
            manifest: Some(manifest),
        });
        state.proven = true;
        state.attempt = None;
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
            if state.attempt.take().is_some() {
                self.save(&state);
            }
            return;
        }
        state.proven = true;
        state.previous = None;
        state.attempt = None;
        self.save(&state);
        let id = state.current.as_ref().map_or("", |g| g.id.as_str());
        note!("[generation] client generation {id} reached a first frame; keeping it");
        let _ = fs::remove_dir_all(self.dir.join("previous"));
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
) -> std::result::Result<(), String> {
    if runtime != "jspi" && runtime != "asyncify" {
        return Err("runtime must be jspi or asyncify".to_owned());
    }
    if transformed && build.is_none() {
        return Err("a transformed runtime must name its artifact".to_owned());
    }
    if let Some(build) = build
        && (build.len() != 64
            || !build
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err("runtime artifact must be a lowercase SHA-256".to_owned());
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
    let temporary = destination.with_extension(format!("{}.tmp", std::process::id()));
    fs::copy(source, &temporary)?;
    fs::rename(&temporary, destination).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
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

    const NAMES: [&str; 2] = ["Gw.jspi.js", "Gw.jspi.wasm"];

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

    #[test]
    fn a_failed_transform_is_disabled_without_rolling_back_official_files() {
        const BUILD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let temp = TempDir::new("generation-transform-fallback");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "old");

        store.stash(&root, &NAMES);
        write_client(&root, "new");
        store.record("new", &root, &NAMES);
        store.record_attempt("jspi", Some(BUILD), true).unwrap();

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
        store.record_attempt("jspi", Some(BUILD), false).unwrap();
        assert_eq!(
            store.recover(&root),
            Recovery::GenerationRolledBack("new".to_owned())
        );
        assert_eq!(
            fs::read_to_string(root.join("Gw.jspi.js")).unwrap(),
            "old:Gw.jspi.js"
        );
        assert_eq!(
            fs::read_to_string(temp.0.join("manifest.cache")).unwrap(),
            "old:manifest"
        );
    }

    #[test]
    fn a_later_transform_failure_is_not_hidden_by_a_proven_generation() {
        const BUILD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let temp = TempDir::new("generation-proven-transform");
        let root = temp.0.join("web");
        let state = temp.0.join("state");
        let store = proven(state.clone(), &root, "shipped");

        // A signed certificate can arrive after these official bytes have
        // already booted. Its first failed launch must still disable it.
        store.record_attempt("asyncify", Some(BUILD), true).unwrap();
        assert!(matches!(
            store.recover(&root),
            Recovery::TransformDisabled { .. }
        ));
        assert!(store.transform_disabled("asyncify", BUILD));

        // A successful official retry clears the launch attempt even though
        // the generation proof was already true before this launch.
        store
            .record_attempt("asyncify", Some(BUILD), false)
            .unwrap();
        store.prove();
        drop(store);
        let store = Store::open(state);
        assert_eq!(store.recover(&root), Recovery::None);
        assert!(store.state.lock().unwrap().attempt.is_none());
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
        assert_eq!(
            store.roll_back(&root),
            None,
            "there was never anything to undo"
        );
        assert!(store.unsound(&root, &NAMES).is_empty());
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
        assert!(!state.join("previous").exists());
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
        assert!(
            saved.proven,
            "snapshot metadata does not unprove the client"
        );
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
