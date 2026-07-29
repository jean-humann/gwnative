//! Versioned, read-only game-state exchange.
//!
//! The certified companion runs in WebKit's process, so the native loopback
//! cannot read it directly. The page publishes a small validated snapshot here;
//! token-authenticated tools can then read the same state without touching the
//! game memory or depending on build-specific offsets.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_PUBLISH_BYTES: usize = 16 * 1024;
pub const MAX_WAIT_MS: u64 = 15_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_type: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_valid: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    api_version: u32,
    revision: u64,
    published_at_ms: u64,
    state: State,
}

pub struct Hub {
    revision: AtomicU64,
    state: Mutex<Option<Envelope>>,
    changed: Condvar,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            revision: AtomicU64::new(0),
            state: Mutex::new(None),
            changed: Condvar::new(),
        }
    }
}

impl Hub {
    pub fn publish(&self, bytes: &[u8]) -> Result<u64, String> {
        if bytes.len() > MAX_PUBLISH_BYTES {
            return Err(format!(
                "game state exceeds the {} KiB limit",
                MAX_PUBLISH_BYTES / 1024
            ));
        }
        let state: State = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid game state: {error}"))?;
        validate(&state)?;
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        let envelope = Envelope {
            api_version: VERSION,
            revision,
            published_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            state,
        };
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(envelope);
        self.changed.notify_all();
        Ok(revision)
    }

    pub fn state_json(&self) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(|state| serde_json::to_vec(state).ok())
    }

    /// Wait for a revision newer than `after`, then return it.
    ///
    /// A caller with no state yet gets `None` after the bounded wait. Existing
    /// callers pass zero milliseconds and retain the immediate GET contract.
    pub fn state_json_after(&self, after: u64, wait_ms: u64) -> Option<Vec<u8>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let is_newer =
            |state: &Option<Envelope>| state.as_ref().is_some_and(|value| value.revision > after);
        if !is_newer(&state) && wait_ms > 0 {
            state = self
                .changed
                .wait_timeout_while(
                    state,
                    Duration::from_millis(wait_ms.min(MAX_WAIT_MS)),
                    |value| !is_newer(value),
                )
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        state
            .as_ref()
            .filter(|value| value.revision > after)
            .and_then(|value| serde_json::to_vec(value).ok())
    }

    pub fn description_json(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "apiVersion": VERSION,
            "transport": {
                "rest": true,
                "webSocket": false,
                "longPoll": true,
                "maximumWaitMs": MAX_WAIT_MS,
                "loopbackOnly": true,
                "tokenRequired": true,
            },
            "state": {
                "domains": ["player", "map", "target"],
                "available": self.state_json().is_some(),
            },
            "actions": {
                "available": false,
                "reason": "no write operation is certified for this client build",
            },
        }))
        .unwrap_or_default()
    }
}

fn validate(state: &State) -> Result<(), String> {
    if !matches!(state.status.as_str(), "ready" | "waiting" | "unsupported") {
        return Err("game state status is not recognised".into());
    }
    if state
        .reason
        .as_ref()
        .is_some_and(|reason| reason.len() > 128 || reason.chars().any(char::is_control))
    {
        return Err("game state reason is not safe text".into());
    }
    for (name, value) in [
        ("playerX", state.player_x),
        ("playerY", state.player_y),
        ("targetX", state.target_x),
        ("targetY", state.target_y),
        ("distance", state.distance),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value.abs() > 1_000_000.0) {
            return Err(format!("{name} is outside the accepted range"));
        }
    }
    if state.distance.is_some_and(|distance| distance < 0.0) {
        return Err("distance cannot be negative".into());
    }
    if state.status == "ready" {
        if state.map_id.is_none()
            || state.player_id.is_none()
            || state.player_x.is_none()
            || state.player_y.is_none()
        {
            return Err("ready game state has no complete player and map reading".into());
        }
        match state.target_valid {
            Some(true)
                if state.target_id.is_some()
                    && state.target_x.is_some()
                    && state.target_y.is_some()
                    && state.distance.is_some() => {}
            Some(false)
                if state.target_id.is_none()
                    && state.target_x.is_none()
                    && state.target_y.is_none()
                    && state.distance.is_none() => {}
            _ => return Err("ready game state has an inconsistent target".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn a_ready_state_is_versioned_and_revisioned() {
        let hub = Hub::default();
        let state = br#"{
            "status":"ready","tickCount":7,"mapId":55,"instanceType":1,
            "instanceName":"Explorable","playerId":4,"playerX":1.5,"playerY":2.5,
            "targetValid":false
        }"#;
        assert_eq!(hub.publish(state).unwrap(), 1);
        assert_eq!(hub.publish(state).unwrap(), 2);
        let value: serde_json::Value = serde_json::from_slice(&hub.state_json().unwrap()).unwrap();
        assert_eq!(value["apiVersion"], 1);
        assert_eq!(value["revision"], 2);
        assert_eq!(value["state"]["mapId"], 55);
    }

    #[test]
    fn partial_ready_targets_and_non_finite_coordinates_are_refused() {
        let hub = Hub::default();
        for state in [
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":0,"playerY":0,"targetValid":true}"#.as_slice(),
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":1e40,"playerY":0,"targetValid":false}"#.as_slice(),
            br#"{"status":"mystery"}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
        assert!(hub.state_json().is_none());
    }

    #[test]
    fn waiting_states_need_no_partial_game_data() {
        let hub = Hub::default();
        hub.publish(br#"{"status":"waiting","reason":"loading","tickCount":8}"#)
            .unwrap();
        assert!(hub.state_json().is_some());
    }

    #[test]
    fn long_poll_wakes_only_for_a_newer_revision() {
        let hub = Arc::new(Hub::default());
        hub.publish(br#"{"status":"waiting","reason":"loading"}"#)
            .unwrap();
        let reader = Arc::clone(&hub);
        let waiting = thread::spawn(move || reader.state_json_after(1, 1_000));
        thread::sleep(Duration::from_millis(30));
        hub.publish(br#"{"status":"waiting","reason":"login"}"#)
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&waiting.join().unwrap().unwrap()).unwrap();
        assert_eq!(value["revision"], 2);
        assert_eq!(value["state"]["reason"], "login");
    }
}
