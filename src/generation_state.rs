use serde::{Deserialize, Serialize};

use crate::generation::Generation;

pub(crate) const FORMAT: u32 = 2;
pub(crate) const REJECTED_KEPT: usize = 8;
pub(crate) const DISABLED_TRANSFORMS_KEPT: usize = 256;
const FAILED_RUNTIMES_KEPT: usize = 8;
const GENERATION_ARTIFACTS: [&str; 5] = [
    "Gw.jspi.js",
    "Gw.jspi.wasm",
    "Gw.js",
    "Gw.wasm",
    "version.json",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchIdentity {
    pub generation_id: String,
    pub runtime: String,
    pub official_glue_sha256: String,
    pub official_wasm_sha256: String,
    pub mode: RuntimeMode,
    pub transform_abi: Option<u32>,
    pub compatibility_id: Option<String>,
    pub nonce: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeMode {
    Original,
    Derived,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "launch", rename_all = "kebab-case")]
pub(crate) enum ProofState {
    InstalledUnproven,
    LegacyFirstFrame,
    FirstFrameProven(LaunchIdentity),
    GameplayProven(LaunchIdentity),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "launch", rename_all = "kebab-case")]
pub(crate) enum LaunchState {
    #[default]
    Idle,
    AttemptingRuntime(LaunchIdentity),
    FailedRuntime(LaunchIdentity),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisabledTransform {
    pub(crate) runtime: String,
    pub(crate) build: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct State {
    pub(crate) format_version: u32,
    pub(crate) current: Option<Generation>,
    pub(crate) proof_state: Option<ProofState>,
    pub(crate) launch_state: LaunchState,
    /// Latest renderer acknowledgement, kept separately so an older gameplay
    /// proof is not falsely attributed to a newer launch nonce.
    #[serde(default)]
    pub(crate) last_first_frame: Option<LaunchIdentity>,
    pub(crate) previous: Option<Generation>,
    #[serde(default)]
    pub(crate) previous_proof: Option<ProofState>,
    #[serde(default)]
    pub(crate) failed_runtimes: Vec<LaunchIdentity>,
    pub(crate) rejected: Vec<String>,
    #[serde(default)]
    pub(crate) disabled_transforms: Vec<DisabledTransform>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            format_version: FORMAT,
            current: None,
            proof_state: None,
            launch_state: LaunchState::Idle,
            last_first_frame: None,
            previous: None,
            previous_proof: None,
            failed_runtimes: Vec::new(),
            rejected: Vec::new(),
            disabled_transforms: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyAttempt {
    runtime: String,
    build: Option<String>,
    transformed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyState {
    #[serde(rename = "formatVersion")]
    _format_version: u32,
    current: Option<Generation>,
    proven: bool,
    previous: Option<Generation>,
    rejected: Vec<String>,
    #[serde(default, rename = "attempt")]
    attempt: Option<LegacyAttempt>,
    #[serde(default)]
    disabled_transforms: Vec<DisabledTransform>,
}

impl From<LegacyState> for State {
    fn from(old: LegacyState) -> Self {
        let proof_state = old.current.as_ref().map(|_| {
            if old.proven {
                ProofState::LegacyFirstFrame
            } else {
                ProofState::InstalledUnproven
            }
        });
        let previous_proof = old.previous.as_ref().map(|_| ProofState::LegacyFirstFrame);
        let mut state = Self {
            current: old.current,
            proof_state,
            previous: old.previous,
            previous_proof,
            rejected: old.rejected,
            disabled_transforms: old.disabled_transforms,
            ..Self::default()
        };
        bound_history(&mut state);
        state
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateHeader {
    format_version: u64,
}

pub(crate) fn read_state(bytes: &[u8]) -> (State, Option<u64>) {
    let decoded = match serde_json::from_slice::<StateHeader>(bytes) {
        Ok(header) if header.format_version == u64::from(FORMAT) => {
            serde_json::from_slice::<State>(bytes)
                .map_err(|error| error.to_string())
                .and_then(|mut state| {
                    bound_history(&mut state);
                    validate_state(&state)?;
                    Ok(state)
                })
        }
        Ok(header) if header.format_version == 1 => serde_json::from_slice::<LegacyState>(bytes)
            .map_err(|error| error.to_string())
            .and_then(|state| {
                validate_legacy(&state)?;
                Ok(state.into())
            }),
        Ok(header) => {
            note!(
                "[generation] refusing unknown state format {}; leaving it untouched",
                header.format_version
            );
            return (State::default(), Some(header.format_version));
        }
        Err(error) => Err(error.to_string()),
    };
    match decoded {
        Ok(state) => (state, None),
        Err(error) => {
            note!("[generation] the record is unreadable ({error}); starting without authority");
            (State::default(), None)
        }
    }
}

pub(crate) fn bound_history(state: &mut State) {
    let rejected = state.rejected.len().saturating_sub(REJECTED_KEPT);
    state.rejected.drain(..rejected);
    let disabled = state
        .disabled_transforms
        .len()
        .saturating_sub(DISABLED_TRANSFORMS_KEPT);
    state.disabled_transforms.drain(..disabled);
    let failed = state
        .failed_runtimes
        .len()
        .saturating_sub(FAILED_RUNTIMES_KEPT);
    state.failed_runtimes.drain(..failed);
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn same_runtime_tuple(left: &LaunchIdentity, right: &LaunchIdentity) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.nonce.clear();
    right.nonce.clear();
    left == right
}

fn valid_generation(generation: &Generation) -> bool {
    !generation.id.is_empty()
        && generation.artifacts.len() == GENERATION_ARTIFACTS.len()
        && GENERATION_ARTIFACTS.iter().all(|name| {
            generation
                .artifacts
                .get(*name)
                .is_some_and(|artifact| valid_digest(&artifact.hash))
        })
        && generation
            .manifest
            .as_ref()
            .is_none_or(|a| valid_digest(&a.hash))
}

fn valid_launch(generation: &Generation, launch: &LaunchIdentity) -> bool {
    if launch.runtime != "jspi" && launch.runtime != "asyncify" {
        return false;
    }
    let (glue, wasm) = runtime_artifacts(&launch.runtime);
    launch.generation_id == generation.id
        && generation
            .artifacts
            .get(glue)
            .is_some_and(|a| a.hash == launch.official_glue_sha256)
        && generation
            .artifacts
            .get(wasm)
            .is_some_and(|a| a.hash == launch.official_wasm_sha256)
        && valid_digest(&launch.nonce)
        && match launch.mode {
            RuntimeMode::Original => {
                launch.transform_abi.is_none() && launch.compatibility_id.is_none()
            }
            RuntimeMode::Derived => {
                launch.transform_abi == Some(crate::wasm::TRANSFORM_ABI)
                    && launch.compatibility_id.as_deref().is_some_and(valid_digest)
            }
        }
}

fn valid_proof(generation: &Generation, proof: &ProofState) -> bool {
    match proof {
        ProofState::InstalledUnproven | ProofState::LegacyFirstFrame => true,
        ProofState::FirstFrameProven(launch) | ProofState::GameplayProven(launch) => {
            valid_launch(generation, launch)
        }
    }
}

fn validate_legacy(state: &LegacyState) -> std::result::Result<(), String> {
    let current = state.current.as_ref().is_none_or(valid_generation);
    let previous = state.previous.as_ref().is_none_or(valid_generation);
    let attempt = state.attempt.as_ref().is_none_or(|attempt| {
        (attempt.runtime == "jspi" || attempt.runtime == "asyncify")
            && (!attempt.transformed || attempt.build.as_deref().is_some_and(valid_digest))
    });
    let relationships = (!state.proven || state.current.is_some())
        && (state.previous.is_none() || state.current.is_some())
        && (state.attempt.is_none() || state.current.is_some());
    (current && previous && attempt && relationships)
        .then_some(())
        .ok_or_else(|| "invalid legacy state combination".to_owned())
}

fn validate_state(state: &State) -> std::result::Result<(), String> {
    let current = matches!((&state.current, &state.proof_state), (None, None))
        || matches!((&state.current, &state.proof_state), (Some(g), Some(p)) if valid_generation(g) && valid_proof(g, p));
    let previous = match (&state.previous, &state.previous_proof) {
        (None, None) => true,
        (Some(generation), Some(ProofState::LegacyFirstFrame)) => valid_generation(generation),
        (
            Some(generation),
            Some(proof @ (ProofState::FirstFrameProven(_) | ProofState::GameplayProven(_))),
        ) => valid_generation(generation) && valid_proof(generation, proof),
        _ => false,
    };
    let launch = matches!(state.launch_state, LaunchState::Idle)
        || matches!((&state.current, &state.launch_state), (Some(g), LaunchState::AttemptingRuntime(l) | LaunchState::FailedRuntime(l)) if valid_launch(g, l));
    let first_frame = match (&state.current, &state.last_first_frame) {
        (_, None) => true,
        (Some(generation), Some(launch)) => valid_launch(generation, launch),
        (None, Some(_)) => false,
    };
    let history = state.rejected.iter().all(|id| !id.is_empty())
        && state
            .disabled_transforms
            .iter()
            .all(|d| (d.runtime == "jspi" || d.runtime == "asyncify") && valid_digest(&d.build))
        && match state.current.as_ref() {
            Some(generation) => state
                .failed_runtimes
                .iter()
                .all(|launch| valid_launch(generation, launch)),
            None => state.failed_runtimes.is_empty(),
        };
    (state.format_version == FORMAT
        && current
        && previous
        && launch
        && first_frame
        && history
        && (state.current.is_some() || state.previous.is_none()))
    .then_some(())
    .ok_or_else(|| "invalid state combination".to_owned())
}

pub(crate) fn runtime_artifacts(runtime: &str) -> (&'static str, &'static str) {
    match runtime {
        "jspi" => ("Gw.jspi.js", "Gw.jspi.wasm"),
        "asyncify" => ("Gw.js", "Gw.wasm"),
        _ => unreachable!("runtime was validated"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::Artifact;

    #[test]
    fn legacy_impossibilities_and_future_formats_have_no_authority() {
        let impossible = br#"{"formatVersion":1,"current":null,"proven":false,"previous":{"id":"forged","artifacts":{},"manifest":null},"rejected":[]}"#;
        assert!(read_state(impossible).0.previous.is_none());
        let future = br#"{"formatVersion":99,"opaque":true}"#;
        assert_eq!(read_state(future).1, Some(99));
    }

    #[test]
    fn only_a_complete_exact_artifact_set_can_hold_authority() {
        let artifact = Artifact {
            size: 1,
            hash: "00".repeat(32),
        };
        let mut generation = Generation {
            id: "generation".to_owned(),
            artifacts: GENERATION_ARTIFACTS
                .map(|name| (name.to_owned(), artifact.clone()))
                .into(),
            manifest: Some(artifact.clone()),
        };
        assert!(valid_generation(&generation));

        generation.artifacts.remove("Gw.wasm");
        assert!(!valid_generation(&generation));
        generation
            .artifacts
            .insert("unexpected.js".to_owned(), artifact);
        assert!(!valid_generation(&generation));
    }

    #[test]
    fn failure_history_cannot_outlive_its_generation_authority() {
        let digest = "00".repeat(32);
        let mut state = State::default();
        let launch = LaunchIdentity {
            generation_id: "poison".to_owned(),
            runtime: "jspi".to_owned(),
            official_glue_sha256: digest.clone(),
            official_wasm_sha256: digest.clone(),
            mode: RuntimeMode::Original,
            transform_abi: None,
            compatibility_id: None,
            nonce: digest,
        };
        state.failed_runtimes.push(launch.clone());
        assert!(validate_state(&state).is_err());
        state.failed_runtimes.clear();
        state.last_first_frame = Some(launch);
        assert!(validate_state(&state).is_err());
    }
}
