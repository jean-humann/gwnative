//! Test-only, event-driven control plane for the native WKWebView.
//!
//! A live game test needs to cross three process boundaries: the runner, the
//! native loopback server and WebKit's content process. Screen capture and OCR
//! crossed them by repeatedly waking Vision and the window server, which made
//! the test itself contend with the single-threaded game. This hub instead
//! sleeps on condition variables until either side has something to say.
//!
//! The surface exists only when `GWNATIVE_E2E` was present at launch. Its
//! command vocabulary is deliberately finite: named navigation, targeting,
//! interaction, cancellation, and one skill key with bounded holds where
//! applicable. There is no JavaScript evaluator, coordinate click, text entry
//! or credential operation.

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
    page_actions: VecDeque<Action>,
    native_actions: VecDeque<Action>,
    prepared_native_actions: VecDeque<u64>,
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
                    "name": "focus-window",
                    "description": "bring the signed game window forward without sending input",
                },
                {
                    "name": "move-forward",
                    "description": "hold Arrow Up on the game canvas",
                    "minimumDurationMs": 50,
                    "maximumDurationMs": 1000,
                },
                {
                    "name": "move-backward",
                    "description": "hold Arrow Down on the game canvas",
                    "minimumDurationMs": 50,
                    "maximumDurationMs": 1000,
                },
                {
                    "name": "turn-left",
                    "description": "hold Arrow Left on the game canvas",
                    "minimumDurationMs": 50,
                    "maximumDurationMs": 1000,
                },
                {
                    "name": "turn-right",
                    "description": "hold Arrow Right on the game canvas",
                    "minimumDurationMs": 50,
                    "maximumDurationMs": 1000,
                },
                {
                    "name": "target-next",
                    "description": "press and release Tab on the game canvas",
                },
                {
                    "name": "interact",
                    "description": "press and release Space on the game canvas",
                },
                {
                    "name": "cancel",
                    "description": "press and release Escape on the game canvas",
                },
                {
                    "name": "skill-1",
                    "description": "press and release the first skill key",
                },
                {
                    "name": "probe-secure-input",
                    "description": "insert one fixed sentinel into an isolated secure field",
                },
                {
                    "name": "test-ui",
                    "description": "exercise the app-owned panels and widgets",
                },
                {
                    "name": "probe-layout",
                    "description": "scan only the bounded certified layout window and return matching deltas",
                },
                {
                    "name": "probe-benchmark-ui",
                    "description": "verify the visible certified District selector frame",
                },
                {
                    "name": "prepare-benchmark-scene",
                    "description": "select Kamadan America-English District 2/1 and move to the certified Xunlai anchor",
                },
                {
                    "name": "sample-performance",
                    "description": "observe logical frame cadence without issuing WebGL commands",
                    "minimumDurationMs": 1000,
                    "maximumDurationMs": 60000,
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
            "activate" | "focus-window" => {
                if request.duration_ms.is_some() {
                    return Err("tapped native actions do not accept a duration".into());
                }
                if request.action == "activate" { 40 } else { 0 }
            }
            "move-forward" | "move-backward" | "turn-left" | "turn-right" => {
                let duration = request.duration_ms.unwrap_or(500);
                if !(50..=1_000).contains(&duration) {
                    return Err(
                        "held gameplay action duration must be between 50 and 1000 ms".into(),
                    );
                }
                duration
            }
            "target-next" | "interact" | "cancel" | "skill-1" | "probe-secure-input" => {
                if request.duration_ms.is_some() {
                    return Err("tapped gameplay actions do not accept a duration".into());
                }
                40
            }
            "test-ui" | "probe-layout" | "probe-benchmark-ui" | "prepare-benchmark-scene" => {
                if request.duration_ms.is_some() {
                    return Err("page-owned E2E actions do not accept a duration".into());
                }
                0
            }
            "sample-performance" => {
                let duration = request.duration_ms.unwrap_or(10_000);
                if !(1_000..=60_000).contains(&duration) {
                    return Err(
                        "performance sample duration must be between 1000 and 60000 ms".into(),
                    );
                }
                duration
            }
            _ => return Err("E2E action is not in the allowed vocabulary".into()),
        };

        let mut inner = lock(&self.inner);
        if inner.page_actions.len() >= MAX_ACTIONS || inner.native_actions.len() >= MAX_ACTIONS {
            return Err("E2E action queue is full".into());
        }
        inner.next_action += 1;
        let action = Action {
            sequence: inner.next_action,
            action: request.action,
            duration_ms,
        };
        // The page observes every action so socket traffic can be associated
        // with the command that caused it. It executes only bounded page-owned
        // checks; native gameplay actions have a second, host-owned queue.
        inner.page_actions.push_back(action.clone());
        if !matches!(
            action.action.as_str(),
            "test-ui"
                | "probe-layout"
                | "probe-benchmark-ui"
                | "prepare-benchmark-scene"
                | "sample-performance"
        ) {
            inner.native_actions.push_back(action.clone());
        }
        self.actions_changed.notify_all();
        serde_json::to_vec(&action).map_err(|error| error.to_string())
    }

    pub fn actions_json_after(&self, after: u64, wait_ms: u64) -> Vec<u8> {
        let timeout = Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
        let mut inner = lock(&self.inner);
        if !inner
            .page_actions
            .iter()
            .any(|action| action.sequence > after)
            && !timeout.is_zero()
        {
            inner = wait(&self.actions_changed, inner, timeout, |state| {
                !state
                    .page_actions
                    .iter()
                    .any(|action| action.sequence > after)
            });
        }
        while inner
            .page_actions
            .front()
            .is_some_and(|action| action.sequence <= after)
        {
            inner.page_actions.pop_front();
        }
        // At-most-once delivery is the safer failure mode for input. If this
        // response is lost the runner times out; a reloaded page never repeats
        // an old Enter or movement command.
        let actions: Vec<_> = inner.page_actions.drain(..).collect();
        serde_json::to_vec(&serde_json::json!({
            "apiVersion": VERSION,
            "actions": actions,
        }))
        .unwrap_or_default()
    }

    /// Take the next gameplay command for AppKit delivery.
    ///
    /// This deliberately does not block the main thread. A command becomes
    /// available only after the page acknowledges that its finite input target
    /// has focus. A normal application never installs the dispatcher and
    /// therefore never schedules E2E work.
    pub fn take_native_action(&self) -> Option<Action> {
        let mut inner = lock(&self.inner);
        let prepared = inner.prepared_native_actions.pop_front()?;
        if inner
            .native_actions
            .front()
            .is_some_and(|action| action.sequence == prepared)
        {
            inner.native_actions.pop_front()
        } else {
            None
        }
    }

    pub fn publish_event(&self, bytes: &[u8]) -> Result<u64, String> {
        if bytes.len() > MAX_BODY_BYTES {
            return Err("E2E event exceeds the 2 KiB limit".into());
        }
        let request: EventRequest =
            serde_json::from_slice(bytes).map_err(|error| format!("invalid E2E event: {error}"))?;
        validate_event(&request.kind, &request.detail)?;
        let wake_native = request.kind == "action-prepared";
        let focus_window = request.kind == "first-frame";
        let failed_action = (request.kind == "action-fail").then(|| {
            (
                request.detail["actionSequence"]
                    .as_u64()
                    .expect("validated failed action sequence"),
                request.detail["action"]
                    .as_str()
                    .expect("validated failed action name"),
            )
        });

        let mut inner = lock(&self.inner);
        if let Some((sequence, action)) = failed_action {
            let was_prepared = inner
                .prepared_native_actions
                .iter()
                .any(|prepared| *prepared == sequence);
            if !was_prepared
                && inner
                    .native_actions
                    .front()
                    .is_some_and(|queued| queued.sequence == sequence && queued.action == action)
            {
                inner.native_actions.pop_front();
            }
        }
        if wake_native {
            let prepared_sequence = request.detail["actionSequence"]
                .as_u64()
                .expect("validated action-prepared sequence");
            let prepared_action = request.detail["action"]
                .as_str()
                .expect("validated action-prepared name");
            let prepared_index = inner.prepared_native_actions.len();
            let Some(queued) = inner.native_actions.get(prepared_index) else {
                return Err("prepared E2E action has no pending native action".into());
            };
            if queued.sequence != prepared_sequence || queued.action != prepared_action {
                return Err("prepared E2E action does not match the native queue order".into());
            }
            inner.prepared_native_actions.push_back(prepared_sequence);
        }
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
        drop(inner);
        // Gameplay actions are woken only after the page has focused the
        // client's current text proxy or canvas. This keeps the native event
        // trusted without racing it against the long-poll delivery that tells
        // the page which finite action is coming.
        if wake_native {
            crate::native_e2e::wake();
        }
        if focus_window {
            crate::native_e2e::focus();
        }
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
        "bridge-ready"
        | "first-frame"
        | "app-pass"
        | "startup-complete"
        | "login-ready"
        | "login-committed"
        | "character-selection-ready"
        | "game-ready" => exact_keys(object, &[]),
        "credential-status" => {
            exact_keys(object, &["status"])?;
            enum_text_field(object, "status", &["available", "unavailable", "error"])
        }
        "window-frame-ready" => {
            exact_keys(object, &["actionSequence"])?;
            positive_u64_field(object, "actionSequence")
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
            exact_keys(object, &["actionSequence", "socketId"])?;
            positive_u64_field(object, "actionSequence")?;
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
        "login-response" => {
            exact_keys(object, &["status", "bytes"])?;
            u32_field(object, "status")?;
            positive_u64_field(object, "bytes")
        }
        "app-fail" => {
            exact_keys(object, &["message"])?;
            safe_text_field(object, "message")
        }
        "layout-probe" => {
            exact_keys(
                object,
                &[
                    "radiusBytes",
                    "contextDeltas",
                    "agentDeltas",
                    "commonDeltas",
                    "quest",
                    "inventory",
                    "social",
                    "completion",
                ],
            )?;
            match object.get("radiusBytes").and_then(Value::as_u64) {
                Some(value) if value <= 4096 && value % 4 == 0 => {}
                _ => return Err("layout probe radius is outside its bound".into()),
            }
            for name in ["contextDeltas", "agentDeltas", "commonDeltas"] {
                signed_delta_array_field(object, name)?;
            }
            let quest = object
                .get("quest")
                .and_then(Value::as_object)
                .ok_or_else(|| "layout probe quest detail must be an object".to_owned())?;
            exact_keys(
                quest,
                &[
                    "worldAvailable",
                    "activeQuestId",
                    "questCapacity",
                    "questCount",
                    "questInvalidIndex",
                    "questInvalidMask",
                    "objectiveCapacity",
                    "objectiveCount",
                    "questRecordsValid",
                    "activeQuestPresent",
                    "objectiveRecordsValid",
                ],
            )?;
            for name in [
                "worldAvailable",
                "questRecordsValid",
                "activeQuestPresent",
                "objectiveRecordsValid",
            ] {
                bool_field(quest, name)?;
            }
            for name in [
                "activeQuestId",
                "questCapacity",
                "questCount",
                "questInvalidIndex",
                "questInvalidMask",
                "objectiveCapacity",
                "objectiveCount",
            ] {
                u32_field(quest, name)?;
            }
            let inventory = object
                .get("inventory")
                .and_then(Value::as_object)
                .ok_or_else(|| "layout probe inventory detail must be an object".to_owned())?;
            exact_keys(
                inventory,
                &[
                    "itemContextAvailable",
                    "inventoryAvailable",
                    "scalarFieldsValid",
                    "storagePanesUnlocked",
                    "bagPointerCount",
                    "backpackPresent",
                    "bagInvalidId",
                    "bagInvalidMask",
                    "itemCount",
                    "itemInvalidBagId",
                    "itemInvalidSlot",
                    "itemInvalidMask",
                    "inventoryRecordsValid",
                ],
            )?;
            for name in [
                "itemContextAvailable",
                "inventoryAvailable",
                "scalarFieldsValid",
                "backpackPresent",
                "inventoryRecordsValid",
            ] {
                bool_field(inventory, name)?;
            }
            for name in [
                "storagePanesUnlocked",
                "bagPointerCount",
                "bagInvalidId",
                "bagInvalidMask",
                "itemCount",
                "itemInvalidBagId",
                "itemInvalidSlot",
                "itemInvalidMask",
            ] {
                u32_field(inventory, name)?;
            }
            let social = object
                .get("social")
                .and_then(Value::as_object)
                .ok_or_else(|| "layout probe social detail must be an object".to_owned())?;
            exact_keys(
                social,
                &[
                    "friendListAvailable",
                    "friendHeaderValid",
                    "playerStatus",
                    "friendCapacity",
                    "friendSlotCount",
                    "friendEntryCount",
                    "friendInvalidSlot",
                    "friendInvalidMask",
                    "friendCountMismatchMask",
                    "friendRecordsValid",
                    "guildContextAvailable",
                    "guildIndex",
                    "guildRecordPresent",
                    "guildRosterCapacity",
                    "guildRosterCount",
                    "guildInvalidMask",
                    "guildRecordsValid",
                    "socialRecordsValid",
                ],
            )?;
            for name in [
                "friendListAvailable",
                "friendHeaderValid",
                "friendRecordsValid",
                "guildContextAvailable",
                "guildRecordPresent",
                "guildRecordsValid",
                "socialRecordsValid",
            ] {
                bool_field(social, name)?;
            }
            for name in [
                "playerStatus",
                "friendCapacity",
                "friendSlotCount",
                "friendEntryCount",
                "friendInvalidSlot",
                "friendInvalidMask",
                "friendCountMismatchMask",
                "guildIndex",
                "guildRosterCapacity",
                "guildRosterCount",
                "guildInvalidMask",
            ] {
                u32_field(social, name)?;
            }
            let completion = object
                .get("completion")
                .and_then(Value::as_object)
                .ok_or_else(|| "layout probe completion detail must be an object".to_owned())?;
            exact_keys(
                completion,
                &[
                    "worldAvailable",
                    "capacities",
                    "sizes",
                    "invalidMasks",
                    "completionRecordsValid",
                ],
            )?;
            bool_field(completion, "worldAvailable")?;
            bool_field(completion, "completionRecordsValid")?;
            for name in ["capacities", "sizes", "invalidMasks"] {
                let values = completion
                    .get(name)
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("layout probe {name} must be an array"))?;
                if values.len() != 6
                    || values
                        .iter()
                        .any(|value| value.as_u64().is_none_or(|n| n > u32::MAX as u64))
                {
                    return Err(format!("layout probe {name} is outside its bound"));
                }
            }
            Ok(())
        }
        "benchmark-ui" => {
            exact_keys(object, &["actionSequence", "districtPresent"])?;
            positive_u64_field(object, "actionSequence")?;
            bool_field(object, "districtPresent")
        }
        "benchmark-scene" => {
            exact_keys(
                object,
                &[
                    "actionSequence",
                    "mapId",
                    "district",
                    "language",
                    "playerX",
                    "playerY",
                    "anchorX",
                    "anchorY",
                    "anchorDistance",
                    "agentCount",
                    "graphicsPreset",
                ],
            )?;
            positive_u64_field(object, "actionSequence")?;
            match object.get("mapId").and_then(Value::as_u64) {
                Some(449) => {}
                _ => return Err("benchmark scene is not Kamadan".into()),
            }
            match object.get("district").and_then(Value::as_u64) {
                Some(1 | 2) => {}
                _ => return Err("benchmark scene is not America district 1 or 2".into()),
            }
            match object.get("language").and_then(Value::as_u64) {
                Some(0) => {}
                _ => return Err("benchmark scene is not English".into()),
            }
            for name in ["playerX", "playerY", "anchorX", "anchorY"] {
                bounded_number_field(object, name, -1_000_000.0, 1_000_000.0, false)?;
            }
            bounded_number_field(object, "anchorDistance", 0.0, 180.0, false)?;
            u32_field(object, "agentCount")?;
            enum_text_field(object, "graphicsPreset", &["high"])
        }
        "performance-sample" => {
            exact_keys(
                object,
                &[
                    "actionSequence",
                    "requestedDurationMs",
                    "runtime",
                    "durationMs",
                    "frames",
                    "framesPerSecond",
                    "intervalMs",
                    "canvas",
                    "webgl",
                    "audit",
                    "gpuTiming",
                ],
            )?;
            positive_u64_field(object, "actionSequence")?;
            match object.get("requestedDurationMs").and_then(Value::as_u64) {
                Some(1_000..=60_000) => {}
                _ => return Err("performance sample request duration is outside its bound".into()),
            }
            enum_text_field(object, "runtime", &["jspi", "asyncify"])?;
            bounded_number_field(object, "durationMs", 0.0, 120_000.0, false)?;
            u32_field(object, "frames")?;
            bounded_number_field(object, "framesPerSecond", 0.0, 1_000.0, false)?;
            enum_text_field(object, "gpuTiming", &["not-sampled"])?;

            let intervals = object
                .get("intervalMs")
                .and_then(Value::as_object)
                .ok_or_else(|| "performance sample intervals must be an object".to_owned())?;
            exact_keys(intervals, &["samples", "mean", "p50", "p95", "p99", "max"])?;
            u32_field(intervals, "samples")?;
            for name in ["mean", "p50", "p95", "p99", "max"] {
                bounded_number_field(intervals, name, 0.0, 120_000.0, true)?;
            }

            let canvas = object
                .get("canvas")
                .and_then(Value::as_object)
                .ok_or_else(|| "performance sample canvas must be an object".to_owned())?;
            exact_keys(canvas, &["width", "height", "cssWidth", "cssHeight"])?;
            for name in ["width", "height"] {
                nullable_u32_field(canvas, name, 32_768)?;
            }
            for name in ["cssWidth", "cssHeight"] {
                bounded_number_field(canvas, name, 0.0, 32_768.0, true)?;
            }

            let webgl = object
                .get("webgl")
                .and_then(Value::as_object)
                .ok_or_else(|| "performance sample WebGL state must be an object".to_owned())?;
            exact_keys(
                webgl,
                &["type", "lost", "drawingBufferWidth", "drawingBufferHeight"],
            )?;
            nullable_enum_text_field(
                webgl,
                "type",
                &["WebGLRenderingContext", "WebGL2RenderingContext"],
            )?;
            nullable_bool_field(webgl, "lost")?;
            for name in ["drawingBufferWidth", "drawingBufferHeight"] {
                nullable_u32_field(webgl, name, 32_768)?;
            }

            let audit = object
                .get("audit")
                .and_then(Value::as_object)
                .ok_or_else(|| "performance sample audit must be an object".to_owned())?;
            exact_keys(
                audit,
                &[
                    "contextLost",
                    "contextRestored",
                    "framesInterruptedAfterDraw",
                    "callbacksDoingWorkDuringSuspension",
                    "outsideWorkDuringSuspension",
                ],
            )?;
            for name in [
                "contextLost",
                "contextRestored",
                "framesInterruptedAfterDraw",
                "callbacksDoingWorkDuringSuspension",
                "outsideWorkDuringSuspension",
            ] {
                u32_field(audit, name)?;
            }
            Ok(())
        }
        "action-prepared" => {
            exact_keys(object, &["actionSequence", "action", "target"])?;
            positive_u64_field(object, "actionSequence")?;
            match object.get("action").and_then(Value::as_str) {
                Some(
                    "activate" | "focus-window" | "move-forward" | "move-backward" | "turn-left"
                    | "turn-right" | "target-next" | "interact" | "cancel" | "skill-1"
                    | "probe-secure-input",
                ) => {}
                _ => return Err("prepared E2E action names an unknown action".into()),
            }
            target_field(object, "target")
        }
        "native-key-observed" | "native-key-released" => {
            exact_keys(object, &["actionSequence", "code", "keyCode"])?;
            positive_u64_field(object, "actionSequence")?;
            enum_text_field(
                object,
                "code",
                &[
                    "enter",
                    "arrow-up",
                    "arrow-down",
                    "arrow-left",
                    "arrow-right",
                    "tab",
                    "space",
                    "escape",
                    "digit-1",
                    "other",
                ],
            )?;
            u32_field(object, "keyCode")
        }
        "secure-input-observed" => {
            exact_keys(object, &["actionSequence", "length"])?;
            positive_u64_field(object, "actionSequence")?;
            match object.get("length").and_then(Value::as_u64) {
                Some(1) => Ok(()),
                _ => Err("secure-input probe did not insert its fixed sentinel".into()),
            }
        }
        "action-complete" => {
            exact_keys(
                object,
                &["actionSequence", "action", "target", "activeTarget"],
            )?;
            action_result(object)
        }
        "action-fail" => {
            exact_keys(
                object,
                &[
                    "actionSequence",
                    "action",
                    "target",
                    "activeTarget",
                    "message",
                ],
            )?;
            action_result(object)?;
            safe_text_field(object, "message")
        }
        _ => Err("E2E event is not in the allowed vocabulary".into()),
    }
}

fn action_result(object: &Map<String, Value>) -> Result<(), String> {
    positive_u64_field(object, "actionSequence")?;
    match object.get("action").and_then(Value::as_str) {
        Some(
            "activate"
            | "focus-window"
            | "move-forward"
            | "move-backward"
            | "turn-left"
            | "turn-right"
            | "target-next"
            | "interact"
            | "cancel"
            | "skill-1"
            | "probe-secure-input"
            | "test-ui"
            | "probe-layout"
            | "probe-benchmark-ui"
            | "prepare-benchmark-scene"
            | "sample-performance",
        ) => {}
        _ => return Err("E2E action result names an unknown action".into()),
    }
    for name in ["target", "activeTarget"] {
        target_field(object, name)?;
    }
    Ok(())
}

fn signed_delta_array_field(object: &Map<String, Value>, name: &str) -> Result<(), String> {
    let values = object
        .get(name)
        .and_then(Value::as_array)
        .filter(|values| values.len() <= 16)
        .ok_or_else(|| format!("layout probe {name} is not a bounded array"))?;
    if values.iter().all(|value| {
        value
            .as_i64()
            .is_some_and(|value| (-4096..=4096).contains(&value) && value % 4 == 0)
    }) {
        Ok(())
    } else {
        Err(format!("layout probe {name} contains an invalid delta"))
    }
}

fn target_field(object: &Map<String, Value>, name: &str) -> Result<(), String> {
    match object.get(name).and_then(Value::as_str) {
        Some(
            "canvas" | "app-ui" | "text-proxy" | "email-proxy" | "password-proxy" | "number-proxy"
            | "multiline-proxy" | "native-window",
        ) => Ok(()),
        _ => Err(format!("E2E event names an unknown {name}")),
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

fn nullable_u32_field(object: &Map<String, Value>, name: &str, maximum: u64) -> Result<(), String> {
    match object.get(name) {
        Some(Value::Null) => Ok(()),
        Some(value) if value.as_u64().is_some_and(|number| number <= maximum) => Ok(()),
        _ => Err(format!(
            "E2E event {name} must be a bounded integer or null"
        )),
    }
}

fn bounded_number_field(
    object: &Map<String, Value>,
    name: &str,
    minimum: f64,
    maximum: f64,
    nullable: bool,
) -> Result<(), String> {
    match object.get(name) {
        Some(Value::Null) if nullable => Ok(()),
        Some(value)
            if value.as_f64().is_some_and(|number| {
                number.is_finite() && (minimum..=maximum).contains(&number)
            }) =>
        {
            Ok(())
        }
        _ => Err(format!("E2E event {name} must be a bounded number")),
    }
}

fn nullable_bool_field(object: &Map<String, Value>, name: &str) -> Result<(), String> {
    match object.get(name) {
        Some(Value::Null | Value::Bool(_)) => Ok(()),
        _ => Err(format!("E2E event {name} must be boolean or null")),
    }
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

fn nullable_enum_text_field(
    object: &Map<String, Value>,
    name: &str,
    allowed: &[&str],
) -> Result<(), String> {
    match object.get(name) {
        Some(Value::Null) => Ok(()),
        Some(Value::String(value)) if allowed.contains(&value.as_str()) => Ok(()),
        _ => Err(format!(
            "E2E event {name} is not a recognised state or null"
        )),
    }
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
        let focus: Value =
            serde_json::from_slice(&hub.submit_action(br#"{"action":"focus-window"}"#).unwrap())
                .unwrap();
        assert_eq!(focus["durationMs"], 0);
        assert!(
            hub.submit_action(br#"{"action":"click","x":1,"y":2}"#)
                .is_err()
        );
        assert!(
            hub.submit_action(br#"{"action":"move-forward","durationMs":1001}"#)
                .is_err()
        );
        assert!(hub.submit_action(br#"{"action":"target-next"}"#).is_ok());
        assert!(
            hub.submit_action(br#"{"action":"probe-secure-input"}"#)
                .is_ok()
        );
        assert!(
            hub.submit_action(br#"{"action":"skill-1","durationMs":40}"#)
                .is_err()
        );
        let sample: Value = serde_json::from_slice(
            &hub.submit_action(br#"{"action":"sample-performance"}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(sample["durationMs"], 10_000);
        assert!(
            hub.submit_action(br#"{"action":"sample-performance","durationMs":999}"#)
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
    fn only_native_actions_enter_the_native_queue() {
        let hub = Hub::default();
        hub.submit_action(br#"{"action":"test-ui"}"#).unwrap();
        hub.submit_action(br#"{"action":"probe-layout"}"#).unwrap();
        hub.submit_action(br#"{"action":"sample-performance","durationMs":1000}"#)
            .unwrap();
        hub.submit_action(br#"{"action":"focus-window"}"#).unwrap();
        hub.submit_action(br#"{"action":"activate"}"#).unwrap();
        hub.submit_action(br#"{"action":"move-forward","durationMs":250}"#)
            .unwrap();

        assert!(hub.take_native_action().is_none());
        hub.publish_event(
            br#"{"kind":"action-prepared","detail":{"actionSequence":4,"action":"focus-window","target":"canvas"}}"#,
        )
        .unwrap();
        hub.publish_event(
            br#"{"kind":"action-prepared","detail":{"actionSequence":5,"action":"activate","target":"password-proxy"}}"#,
        )
        .unwrap();
        hub.publish_event(
            br#"{"kind":"action-prepared","detail":{"actionSequence":6,"action":"move-forward","target":"canvas"}}"#,
        )
        .unwrap();
        let focus = hub.take_native_action().unwrap();
        let first = hub.take_native_action().unwrap();
        let second = hub.take_native_action().unwrap();
        assert_eq!(focus.action, "focus-window");
        assert_eq!(focus.sequence, 4);
        assert_eq!(focus.duration_ms, 0);
        assert_eq!(first.action, "activate");
        assert_eq!(first.sequence, 5);
        assert_eq!(second.action, "move-forward");
        assert_eq!(second.sequence, 6);
        assert_eq!(second.duration_ms, 250);
        assert!(hub.take_native_action().is_none());
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
            hub.publish_event(br#"{"kind":"login-ready","detail":{}}"#)
                .is_ok()
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"credential-status","detail":{"status":"unavailable"}}"#,
            )
            .is_ok()
        );
        assert!(
            hub.publish_event(br#"{"kind":"login-committed","detail":{}}"#)
                .is_ok()
        );
        assert!(
            hub.publish_event(br#"{"kind":"character-selection-ready","detail":{}}"#)
                .is_ok()
        );
        let frame_ready = br#"{"kind":"window-frame-ready","detail":{"actionSequence":1}}"#;
        assert!(hub.publish_event(frame_ready).is_ok());
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
        assert!(
            hub.publish_event(
                br#"{"kind":"secure-input-observed","detail":{"actionSequence":1,"length":1}}"#,
            )
            .is_ok()
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"secure-input-observed","detail":{"actionSequence":1,"length":2}}"#,
            )
            .is_err()
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"secure-input-observed","detail":{"actionSequence":1,"length":1,"value":"never"}}"#,
            )
            .is_err()
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"layout-probe","detail":{"radiusBytes":2048,"contextDeltas":[-48],"agentDeltas":[-48],"commonDeltas":[-48],"quest":{"worldAvailable":true,"activeQuestId":0,"questCapacity":0,"questCount":0,"questInvalidIndex":4294967295,"questInvalidMask":0,"objectiveCapacity":0,"objectiveCount":0,"questRecordsValid":true,"activeQuestPresent":true,"objectiveRecordsValid":true},"inventory":{"itemContextAvailable":true,"inventoryAvailable":true,"scalarFieldsValid":true,"storagePanesUnlocked":4,"bagPointerCount":1,"backpackPresent":true,"bagInvalidId":0,"bagInvalidMask":0,"itemCount":0,"itemInvalidBagId":0,"itemInvalidSlot":4294967295,"itemInvalidMask":0,"inventoryRecordsValid":true},"social":{"friendListAvailable":true,"friendHeaderValid":true,"playerStatus":1,"friendCapacity":0,"friendSlotCount":0,"friendEntryCount":0,"friendInvalidSlot":4294967295,"friendInvalidMask":0,"friendCountMismatchMask":0,"friendRecordsValid":true,"guildContextAvailable":true,"guildIndex":0,"guildRecordPresent":false,"guildRosterCapacity":0,"guildRosterCount":0,"guildInvalidMask":0,"guildRecordsValid":true,"socialRecordsValid":true},"completion":{"worldAvailable":true,"capacities":[1,1,1,1,1,1],"sizes":[1,1,1,1,1,1],"invalidMasks":[0,0,0,0,0,0],"completionRecordsValid":true}}}"#,
            )
            .is_ok()
        );
        hub.submit_action(br#"{"action":"activate"}"#).unwrap();
        assert!(
            hub.publish_event(
                br#"{"kind":"action-prepared","detail":{"actionSequence":1,"action":"activate","target":"password-proxy"}}"#,
            )
            .is_ok()
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"action-prepared","detail":{"actionSequence":2,"action":"activate","target":"body"}}"#,
            )
            .is_err()
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"login-response","detail":{"status":200,"bytes":69,"body":"never"}}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn performance_events_are_bounded_and_carry_no_frame_pixels() {
        let hub = Hub::default();
        let detail = serde_json::json!({
            "actionSequence": 1,
            "requestedDurationMs": 10_000,
            "runtime": "jspi",
            "durationMs": 10_001.25,
            "frames": 601,
            "framesPerSecond": 60.0,
            "intervalMs": {
                "samples": 600,
                "mean": 16.667,
                "p50": 16.6,
                "p95": 17.2,
                "p99": 20.0,
                "max": 24.0,
            },
            "canvas": {
                "width": 2560,
                "height": 1364,
                "cssWidth": 1280.0,
                "cssHeight": 682.0,
            },
            "webgl": {
                "type": "WebGL2RenderingContext",
                "lost": false,
                "drawingBufferWidth": 2560,
                "drawingBufferHeight": 1364,
            },
            "audit": {
                "contextLost": 0,
                "contextRestored": 0,
                "framesInterruptedAfterDraw": 0,
                "callbacksDoingWorkDuringSuspension": 0,
                "outsideWorkDuringSuspension": 0,
            },
            "gpuTiming": "not-sampled",
        });
        let valid = serde_json::to_vec(&serde_json::json!({
            "kind": "performance-sample",
            "detail": detail,
        }))
        .unwrap();
        assert!(hub.publish_event(&valid).is_ok());

        let mut invalid = serde_json::from_slice::<Value>(&valid).unwrap();
        invalid["detail"]["framesPerSecond"] = Value::from(1_001);
        assert!(
            hub.publish_event(&serde_json::to_vec(&invalid).unwrap())
                .is_err()
        );
        invalid["detail"]["framesPerSecond"] = Value::from(60);
        invalid["detail"]["pixels"] = Value::String("never".into());
        assert!(
            hub.publish_event(&serde_json::to_vec(&invalid).unwrap())
                .is_err()
        );
    }

    #[test]
    fn a_prepared_action_must_match_the_native_queue_head() {
        let hub = Hub::default();
        hub.submit_action(br#"{"action":"activate"}"#).unwrap();
        assert!(
            hub.publish_event(
                br#"{"kind":"action-prepared","detail":{"actionSequence":2,"action":"activate","target":"password-proxy"}}"#,
            )
            .is_err()
        );
        assert!(
            hub.publish_event(
                br#"{"kind":"action-prepared","detail":{"actionSequence":1,"action":"move-forward","target":"canvas"}}"#,
            )
            .is_err()
        );
        assert!(hub.take_native_action().is_none());
    }
}
