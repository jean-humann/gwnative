//! Test-only, event-driven control plane for the native WKWebView.
//!
//! A live game test needs to cross three process boundaries: the runner, the
//! native loopback server and WebKit's content process. Screen capture and OCR
//! crossed them by repeatedly waking Vision and the window server, which made
//! the test itself contend with the single-threaded game. This hub instead
//! sleeps on condition variables until either side has something to say.
//!
//! The surface exists only when `GWNATIVE_E2E` was present at launch. Its
//! command vocabulary is deliberately finite: activate the focused game
//! control, or hold forward for a bounded interval. There is no JavaScript
//! evaluator, coordinate click, text entry or credential operation.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const VERSION: u32 = 1;
pub const MAX_WAIT_MS: u64 = 15_000;
const MAX_BODY_BYTES: usize = 2 * 1024;
const MAX_EVENTS: usize = 256;
const MAX_ACTIONS: usize = 32;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub sequence: u64,
    pub action: String,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    sequence: u64,
    kind: String,
    detail: Value,
}

#[derive(Default)]
struct Inner {
    next_action: u64,
    actions: VecDeque<Action>,
    next_event: u64,
    events: VecDeque<Event>,
}

#[derive(Default)]
pub struct Hub {
    inner: Mutex<Inner>,
    actions_changed: Condvar,
    events_changed: Condvar,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActionRequest {
    action: String,
    #[serde(default)]
    duration_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventRequest {
    kind: String,
    #[serde(default = "empty_object")]
    detail: Value,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

impl Hub {
    pub fn description_json(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "apiVersion": VERSION,
            "mode": "test-only",
            "transport": {
                "longPoll": true,
                "loopbackOnly": true,
                "tokenRequired": true,
                "maximumWaitMs": MAX_WAIT_MS,
            },
            "actions": [
                {
                    "name": "activate",
                    "description": "press and release Enter on the game input target",
                },
                {
                    "name": "move-forward",
                    "description": "hold Arrow Up on the game canvas",
                    "minimumDurationMs": 50,
                    "maximumDurationMs": 1000,
                },
            ],
            "prohibited": ["javascript", "coordinates", "text", "credentials"],
        }))
        .unwrap_or_default()
    }

    pub fn submit_action(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        if bytes.len() > MAX_BODY_BYTES {
            return Err("E2E action exceeds the 2 KiB limit".into());
        }
        let request: ActionRequest = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid E2E action: {error}"))?;
        let duration_ms = match request.action.as_str() {
            "activate" => {
                if request.duration_ms.is_some() {
                    return Err("activate does not accept a duration".into());
                }
                40
            }
            "move-forward" => {
                let duration = request.duration_ms.unwrap_or(500);
                if !(50..=1_000).contains(&duration) {
                    return Err("move-forward duration must be between 50 and 1000 ms".into());
                }
                duration
            }
            _ => return Err("E2E action is not in the allowed vocabulary".into()),
        };

        let mut inner = lock(&self.inner);
        if inner.actions.len() >= MAX_ACTIONS {
            return Err("E2E action queue is full".into());
        }
        inner.next_action += 1;
        let action = Action {
            sequence: inner.next_action,
            action: request.action,
            duration_ms,
        };
        inner.actions.push_back(action.clone());
        self.actions_changed.notify_all();
        serde_json::to_vec(&action).map_err(|error| error.to_string())
    }

    pub fn actions_json_after(&self, after: u64, wait_ms: u64) -> Vec<u8> {
        let timeout = Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
        let mut inner = lock(&self.inner);
        if !inner.actions.iter().any(|action| action.sequence > after) && !timeout.is_zero() {
            inner = wait(&self.actions_changed, inner, timeout, |state| {
                !state.actions.iter().any(|action| action.sequence > after)
            });
        }
        while inner
            .actions
            .front()
            .is_some_and(|action| action.sequence <= after)
        {
            inner.actions.pop_front();
        }
        // At-most-once delivery is the safer failure mode for input. If this
        // response is lost the runner times out; a reloaded page never repeats
        // an old Enter or movement command.
        let actions: Vec<_> = inner.actions.drain(..).collect();
        serde_json::to_vec(&serde_json::json!({
            "apiVersion": VERSION,
            "actions": actions,
        }))
        .unwrap_or_default()
    }

    pub fn publish_event(&self, bytes: &[u8]) -> Result<u64, String> {
        if bytes.len() > MAX_BODY_BYTES {
            return Err("E2E event exceeds the 2 KiB limit".into());
        }
        let request: EventRequest =
            serde_json::from_slice(bytes).map_err(|error| format!("invalid E2E event: {error}"))?;
        validate_event(&request.kind, &request.detail)?;

        let mut inner = lock(&self.inner);
        inner.next_event += 1;
        let sequence = inner.next_event;
        inner.events.push_back(Event {
            sequence,
            kind: request.kind,
            detail: request.detail,
        });
        while inner.events.len() > MAX_EVENTS {
            inner.events.pop_front();
        }
        self.events_changed.notify_all();
        Ok(sequence)
    }

    pub fn events_json_after(&self, after: u64, wait_ms: u64) -> Vec<u8> {
        let timeout = Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
        let mut inner = lock(&self.inner);
        if !inner.events.iter().any(|event| event.sequence > after) && !timeout.is_zero() {
            inner = wait(&self.events_changed, inner, timeout, |state| {
                !state.events.iter().any(|event| event.sequence > after)
            });
        }
        let events: Vec<_> = inner
            .events
            .iter()
            .filter(|event| event.sequence > after)
            .cloned()
            .collect();
        serde_json::to_vec(&serde_json::json!({
            "apiVersion": VERSION,
            "events": events,
        }))
        .unwrap_or_default()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait<'a, T>(
    condvar: &Condvar,
    guard: std::sync::MutexGuard<'a, T>,
    timeout: Duration,
    condition: impl FnMut(&mut T) -> bool,
) -> std::sync::MutexGuard<'a, T> {
    condvar
        .wait_timeout_while(guard, timeout, condition)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .0
}

fn validate_event(kind: &str, detail: &Value) -> Result<(), String> {
    let object = detail
        .as_object()
        .ok_or_else(|| "E2E event detail must be an object".to_owned())?;
    match kind {
        "bridge-ready" | "first-frame" | "app-pass" | "startup-complete" | "game-ready" => {
            exact_keys(object, &[])
        }
        "credentials-offered" => {
            exact_keys(object, &["accountSet", "passwordSet"])?;
            bool_field(object, "accountSet")?;
            bool_field(object, "passwordSet")
        }
        "client-build" => {
            exact_keys(object, &["programId", "buildId"])?;
            u32_field(object, "programId")?;
            u32_field(object, "buildId")
        }
        "client-capabilities" => {
            exact_keys(object, &["enhancements", "templateSave"])?;
            enum_text_field(
                object,
                "enhancements",
                &["off", "ready", "uncertified", "failed"],
            )?;
            enum_text_field(
                object,
                "templateSave",
                &["off", "ready", "uncertified", "failed"],
            )
        }
        "socket-open" => {
            exact_keys(object, &["socketId"])?;
            positive_u64_field(object, "socketId")
        }
        "client-traffic" => {
            exact_keys(
                object,
                &["actionSequence", "direction", "socketId", "bytes"],
            )?;
            positive_u64_field(object, "actionSequence")?;
            positive_u64_field(object, "socketId")?;
            positive_u64_field(object, "bytes")?;
            enum_text_field(object, "direction", &["send", "receive"])
        }
        "app-fail" => {
            exact_keys(object, &["message"])?;
            safe_text_field(object, "message")
        }
        "action-complete" => {
            exact_keys(object, &["actionSequence", "action", "target"])?;
            action_result(object)
        }
        "action-fail" => {
            exact_keys(object, &["actionSequence", "action", "target", "message"])?;
            action_result(object)?;
            safe_text_field(object, "message")
        }
        _ => Err("E2E event is not in the allowed vocabulary".into()),
    }
}

fn action_result(object: &Map<String, Value>) -> Result<(), String> {
    positive_u64_field(object, "actionSequence")?;
    match object.get("action").and_then(Value::as_str) {
        Some("activate" | "move-forward") => {}
        _ => return Err("E2E action result names an unknown action".into()),
    }
    match object.get("target").and_then(Value::as_str) {
        Some("canvas" | "text-proxy") => Ok(()),
        _ => Err("E2E action result names an unknown target".into()),
    }
}

fn exact_keys(object: &Map<String, Value>, expected: &[&str]) -> Result<(), String> {
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err("E2E event detail has unexpected fields".into());
    }
    Ok(())
}

fn bool_field(object: &Map<String, Value>, name: &str) -> Result<(), String> {
    object
        .get(name)
        .and_then(Value::as_bool)
        .map(|_| ())
        .ok_or_else(|| format!("E2E event {name} must be boolean"))
}

fn u32_field(object: &Map<String, Value>, name: &str) -> Result<(), String> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .map(|_| ())
        .ok_or_else(|| format!("E2E event {name} must be an unsigned 32-bit integer"))
}

fn positive_u64_field(object: &Map<String, Value>, name: &str) -> Result<(), String> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .map(|_| ())
        .ok_or_else(|| format!("E2E event {name} must be a positive integer"))
}

fn safe_text_field(object: &Map<String, Value>, name: &str) -> Result<(), String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 160 && !value.chars().any(char::is_control))
        .map(|_| ())
        .ok_or_else(|| format!("E2E event {name} is not safe text"))
}

fn enum_text_field(
    object: &Map<String, Value>,
    name: &str,
    allowed: &[&str],
) -> Result<(), String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| allowed.contains(value))
        .map(|_| ())
        .ok_or_else(|| format!("E2E event {name} is not a recognised state"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn action_vocabulary_is_finite_and_durations_are_bounded() {
        let hub = Hub::default();
        let first: Value =
            serde_json::from_slice(&hub.submit_action(br#"{"action":"activate"}"#).unwrap())
                .unwrap();
        assert_eq!(first["sequence"], 1);
        assert_eq!(first["durationMs"], 40);
        assert!(
            hub.submit_action(br#"{"action":"click","x":1,"y":2}"#)
                .is_err()
        );
        assert!(
            hub.submit_action(br#"{"action":"move-forward","durationMs":1001}"#)
                .is_err()
        );
    }

    #[test]
    fn long_poll_sleeps_until_an_action_arrives() {
        let hub = Arc::new(Hub::default());
        let reader = Arc::clone(&hub);
        let started = Instant::now();
        let waiting = thread::spawn(move || reader.actions_json_after(0, 1_000));
        thread::sleep(Duration::from_millis(30));
        hub.submit_action(br#"{"action":"activate"}"#).unwrap();
        let value: Value = serde_json::from_slice(&waiting.join().unwrap()).unwrap();
        assert_eq!(value["actions"][0]["action"], "activate");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn an_action_is_never_replayed_after_delivery() {
        let hub = Hub::default();
        hub.submit_action(br#"{"action":"activate"}"#).unwrap();
        let first: Value = serde_json::from_slice(&hub.actions_json_after(0, 0)).unwrap();
        let second: Value = serde_json::from_slice(&hub.actions_json_after(0, 0)).unwrap();
        assert_eq!(first["actions"].as_array().unwrap().len(), 1);
        assert_eq!(second["actions"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn events_cannot_carry_credentials_or_arbitrary_fields() {
        let hub = Hub::default();
        assert_eq!(
            hub.publish_event(
                br#"{"kind":"credentials-offered","detail":{"accountSet":true,"passwordSet":true}}"#,
            )
            .unwrap(),
            1,
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"credentials-offered","detail":{"accountSet":true,"passwordSet":true,"password":"never"}}"#,
            )
            .is_err()
        );
        assert!(
            hub.publish_event(br#"{"kind":"javascript","detail":{}}"#)
                .is_err()
        );
    }
}
