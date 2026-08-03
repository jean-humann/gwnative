//! Versioned, read-only game-state transport.
//!
//! A separately certified in-page producer may publish a deliberately narrow
//! snapshot through an injection-only capability. External consumers receive a
//! different capability which can only read this state; it cannot reach page
//! administration, credentials, or publication.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_PUBLISH_BYTES: usize = 16 * 1024;
pub const MAX_WAIT_MS: u64 = 15_000;
pub const MAX_READY_AGE_MS: u64 = 2_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
    #[serde(skip)]
    published_at: Instant,
    state: State,
}

impl Envelope {
    fn ready_remaining(&self) -> Option<Duration> {
        (self.state.status == "ready").then(|| {
            Duration::from_millis(MAX_READY_AGE_MS).saturating_sub(self.published_at.elapsed())
        })
    }

    fn is_observable(&self) -> bool {
        self.ready_remaining()
            .is_none_or(|remaining| !remaining.is_zero())
    }

    fn is_available(&self) -> bool {
        self.state.status == "ready" && self.is_observable()
    }
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
        // Serialize the revision assignment with replacement. Two concurrent
        // requests must not allocate revisions 1 and 2, then acquire this lock
        // in the opposite order and leave readers looking at revision 1.
        let mut current = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            published_at: Instant::now(),
            state,
        };
        *current = Some(envelope);
        drop(current);
        self.changed.notify_all();
        Ok(revision)
    }

    #[cfg(test)]
    pub fn state_json(&self) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(|state| serde_json::to_vec(state).ok())
    }

    pub fn state_json_after(&self, after: u64, wait_ms: u64) -> Option<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        loop {
            if let Some(current) = state.as_ref() {
                // A ready revision is telemetry, not durable state. Once its
                // producer misses the freshness deadline, never expose it and
                // never keep a reader asleep waiting for its longer deadline.
                if !current.is_observable() {
                    return None;
                }
                if current.revision > after {
                    return serde_json::to_vec(current).ok();
                }
            }

            let request_remaining = deadline.saturating_duration_since(Instant::now());
            if request_remaining.is_zero() {
                return None;
            }
            let wait_for = state
                .as_ref()
                .and_then(Envelope::ready_remaining)
                .map_or(request_remaining, |fresh_for| {
                    request_remaining.min(fresh_for)
                });
            if wait_for.is_zero() {
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, wait_for)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    fn state_available(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(Envelope::is_available)
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
                "available": self.state_available(),
            },
            "actions": {
                "available": false,
                "reason": "no write operation is certified for this client build",
            },
        }))
        .unwrap_or_default()
    }
}

fn has_game_data(state: &State) -> bool {
    state.tick_count.is_some()
        || state.map_id.is_some()
        || state.instance_type.is_some()
        || state.instance_name.is_some()
        || state.player_id.is_some()
        || state.player_x.is_some()
        || state.player_y.is_some()
        || state.target_valid.is_some()
        || state.target_id.is_some()
        || state.target_kind.is_some()
        || state.target_x.is_some()
        || state.target_y.is_some()
        || state.distance.is_some()
        || state.range_name.is_some()
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
    if state.status != "ready" && has_game_data(state) {
        return Err("unavailable game state cannot retain stale telemetry".into());
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

    const READY: &[u8] = br#"{
        "status":"ready","tickCount":7,"mapId":55,"instanceType":1,
        "instanceName":"Explorable","playerId":4,"playerX":1.5,"playerY":2.5,
        "targetValid":false
    }"#;

    #[test]
    fn a_ready_state_is_versioned_and_revisioned() {
        let hub = Hub::default();
        assert_eq!(hub.publish(READY).unwrap(), 1);
        assert_eq!(hub.publish(READY).unwrap(), 2);
        let value: serde_json::Value = serde_json::from_slice(&hub.state_json().unwrap()).unwrap();
        assert_eq!(value["apiVersion"], 1);
        assert_eq!(value["revision"], 2);
        assert_eq!(value["state"]["mapId"], 55);
    }

    #[test]
    fn malformed_and_partial_ready_states_are_refused() {
        let hub = Hub::default();
        for state in [
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":0,"playerY":0,"targetValid":true}"#.as_slice(),
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":1e40,"playerY":0,"targetValid":false}"#.as_slice(),
            br#"{"status":"mystery"}"#.as_slice(),
            br#"{"status":"waiting","credentials":"never"}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
        assert!(hub.state_json().is_none());
    }

    #[test]
    fn unavailable_state_cannot_retain_stale_game_data() {
        let hub = Hub::default();
        hub.publish(READY).unwrap();
        assert!(
            hub.publish(br#"{"status":"unsupported","reason":"stopped","mapId":55}"#)
                .is_err()
        );
        hub.publish(br#"{"status":"unsupported","reason":"stopped"}"#)
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&hub.state_json().unwrap()).unwrap();
        assert_eq!(value["revision"], 2);
        assert_eq!(value["state"]["status"], "unsupported");
        assert!(value["state"].get("mapId").is_none());
        let description: serde_json::Value =
            serde_json::from_slice(&hub.description_json()).unwrap();
        assert_eq!(description["state"]["available"], false);
    }

    #[test]
    fn a_ready_state_that_stops_refreshing_becomes_unavailable() {
        let hub = Hub::default();
        hub.publish(READY).unwrap();
        {
            let mut state = hub
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.as_mut().unwrap().published_at =
                Instant::now() - Duration::from_millis(MAX_READY_AGE_MS + 1);
        }
        assert!(hub.state_json_after(0, 0).is_none());
        let description: serde_json::Value =
            serde_json::from_slice(&hub.description_json()).unwrap();
        assert_eq!(description["state"]["available"], false);
    }

    #[test]
    fn long_poll_ends_at_the_ready_state_freshness_deadline() {
        let hub = Hub::default();
        hub.publish(READY).unwrap();
        let remaining = Duration::from_millis(80);
        {
            let mut state = hub
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.as_mut().unwrap().published_at = Instant::now()
                - (Duration::from_millis(MAX_READY_AGE_MS).saturating_sub(remaining));
        }

        let started = Instant::now();
        assert!(hub.state_json_after(1, 1_000).is_none());
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(30),
            "returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "waited too long: {elapsed:?}"
        );
    }

    #[test]
    fn an_already_stale_ready_state_never_uses_the_requested_wait() {
        let hub = Hub::default();
        hub.publish(READY).unwrap();
        {
            let mut state = hub
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.as_mut().unwrap().published_at =
                Instant::now() - Duration::from_millis(MAX_READY_AGE_MS + 1);
        }

        let started = Instant::now();
        assert!(hub.state_json_after(1, 1_000).is_none());
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "an already stale state should return immediately"
        );
    }

    #[test]
    fn long_poll_wakes_for_the_terminal_revision() {
        let hub = Arc::new(Hub::default());
        hub.publish(READY).unwrap();
        let reader = Arc::clone(&hub);
        let waiting = thread::spawn(move || reader.state_json_after(1, 1_000));
        thread::sleep(Duration::from_millis(20));
        hub.publish(br#"{"status":"unsupported","reason":"observer stopped"}"#)
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&waiting.join().unwrap().unwrap()).unwrap();
        assert_eq!(value["revision"], 2);
        assert_eq!(value["state"]["status"], "unsupported");
    }
}
