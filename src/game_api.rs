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
pub const MAX_PUBLISH_BYTES: usize = 32 * 1024;
pub const MAX_WAIT_MS: u64 = 15_000;
const MAX_AGENT_ID: u32 = 4_095;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartyPlayer {
    pub login_number: u32,
    pub called_target_id: u32,
    pub state: u32,
    pub connected: bool,
    pub ticked: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartyHero {
    pub agent_id: u32,
    pub owner_player_id: u32,
    pub hero_id: u32,
    pub level: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartyHenchman {
    pub agent_id: u32,
    pub profession: u32,
    pub level: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Party {
    pub id: u32,
    pub hard_mode: bool,
    pub defeated: bool,
    pub leader: bool,
    pub allies_truncated: bool,
    pub players: Vec<PartyPlayer>,
    pub heroes: Vec<PartyHero>,
    pub henchmen: Vec<PartyHenchman>,
    pub allies: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Skill {
    pub slot: u32,
    pub adrenaline_a: u32,
    pub adrenaline_b: u32,
    pub recharge: u32,
    pub skill_id: u32,
    pub event: u32,
    pub disabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Skillbar {
    pub agent_id: u32,
    pub disabled_mask: u32,
    pub cast_count: u32,
    pub casting: bool,
    pub skills: Vec<Skill>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Buff {
    pub skill_id: u32,
    pub buff_id: u32,
    pub target_agent_id: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Effect {
    pub skill_id: u32,
    pub attribute_level: u32,
    pub effect_id: u32,
    pub agent_id: u32,
    pub duration: f32,
    pub timestamp: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlayerEffects {
    pub agent_id: u32,
    pub buffs_truncated: bool,
    pub effects_truncated: bool,
    pub buffs: Vec<Buff>,
    pub effects: Vec<Effect>,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub party: Option<Party>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skillbar: Option<Skillbar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<PlayerEffects>,
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
                "domains": ["player", "map", "target", "party", "skillbar", "effects"],
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
        if state.reason.is_some()
            || state.tick_count.is_none()
            || !state.map_id.is_some_and(|id| (1..=2_000).contains(&id))
            || !matches!(
                (state.instance_type, state.instance_name.as_deref()),
                (Some(0), Some("Outpost")) | (Some(1), Some("Explorable"))
            )
            || !state
                .player_id
                .is_some_and(|id| (1..=MAX_AGENT_ID).contains(&id))
            || state.player_x.is_none()
            || state.player_y.is_none()
        {
            return Err("ready game state has no complete player and map reading".into());
        }
        match state.target_valid {
            Some(true)
                if state
                    .target_id
                    .is_some_and(|id| (1..=MAX_AGENT_ID).contains(&id))
                    && matches!(state.target_kind.as_deref(), Some("Living" | "Unknown"))
                    && state.target_x.is_some()
                    && state.target_y.is_some()
                    && state.distance.is_some()
                    && state.range_name.as_deref().is_some_and(|name| {
                        matches!(
                            name,
                            "Adjacent"
                                | "Nearby"
                                | "Area"
                                | "Earshot"
                                | "Spellcast"
                                | "Spirit"
                                | "Compass"
                                | "Beyond compass"
                        )
                    }) => {}
            Some(false)
                if state.target_id.is_none()
                    && state.target_kind.as_deref() == Some("None")
                    && state.target_x.is_none()
                    && state.target_y.is_none()
                    && state.distance.is_none()
                    && state.range_name.as_deref() == Some("None") => {}
            _ => return Err("ready game state has an inconsistent target".into()),
        }
        if let Some(party) = &state.party {
            validate_party(party)?;
        }
        if let Some(skillbar) = &state.skillbar {
            validate_skillbar(skillbar, state.player_id.unwrap_or_default())?;
        }
        if let Some(effects) = &state.effects {
            validate_effects(effects, state.player_id.unwrap_or_default())?;
        }
    } else if state.map_id.is_some()
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
        || state.party.is_some()
        || state.skillbar.is_some()
        || state.effects.is_some()
    {
        return Err("non-ready game state carries live game data".into());
    }
    Ok(())
}

fn validate_party(party: &Party) -> Result<(), String> {
    if party.players.is_empty()
        || party.players.len() > 12
        || party.heroes.len() > 12
        || party.henchmen.len() > 12
        || party.players.len() + party.heroes.len() + party.henchmen.len() > 12
        || party.allies.len() > 32
    {
        return Err("party roster is outside its certified bounds".into());
    }
    if party.players.iter().any(|player| {
        player.login_number == 0
            || player.called_target_id > MAX_AGENT_ID
            || player.connected != (player.state & 1 != 0)
            || player.ticked != (player.state & 2 != 0)
    }) {
        return Err("party player state is inconsistent".into());
    }
    if party.heroes.iter().any(|hero| {
        hero.agent_id == 0
            || hero.agent_id > MAX_AGENT_ID
            || hero.owner_player_id == 0
            || !party
                .players
                .iter()
                .any(|player| player.login_number == hero.owner_player_id)
            || hero.hero_id == 0
            || hero.hero_id > 1_000
            || !(1..=20).contains(&hero.level)
    }) {
        return Err("party hero state is outside its certified bounds".into());
    }
    if party.henchmen.iter().any(|henchman| {
        henchman.agent_id == 0
            || henchman.agent_id > MAX_AGENT_ID
            || henchman.profession > 10
            || !(1..=20).contains(&henchman.level)
    }) {
        return Err("party henchman state is outside its certified bounds".into());
    }
    if party
        .allies
        .iter()
        .any(|agent_id| !(1..=MAX_AGENT_ID).contains(agent_id))
    {
        return Err("party ally state is outside its certified agent bounds".into());
    }
    Ok(())
}

fn validate_skillbar(skillbar: &Skillbar, player_id: u32) -> Result<(), String> {
    if skillbar.agent_id != player_id
        || skillbar.disabled_mask & !0xff != 0
        || skillbar.cast_count > 64
        || skillbar.casting != (skillbar.cast_count != 0)
        || skillbar.skills.len() != 8
    {
        return Err("skillbar does not belong to the certified player".into());
    }
    if skillbar.skills.iter().enumerate().any(|(index, skill)| {
        skill.slot != index as u32 + 1
            || skill.skill_id > 100_000
            || skill.disabled != (skillbar.disabled_mask & (1 << index) != 0)
    }) {
        return Err("skillbar slots are outside their certified bounds".into());
    }
    Ok(())
}

fn validate_effects(effects: &PlayerEffects, player_id: u32) -> Result<(), String> {
    if effects.agent_id != player_id
        || effects.buffs.len() > 32
        || effects.effects.len() > 64
        || (effects.buffs_truncated && effects.buffs.len() != 32)
        || (effects.effects_truncated && effects.effects.len() != 64)
    {
        return Err("effects do not belong to the certified player".into());
    }
    let mut buff_ids = std::collections::HashSet::with_capacity(effects.buffs.len());
    if effects.buffs.iter().any(|buff| {
        buff.skill_id == 0
            || buff.skill_id > 100_000
            || buff.buff_id == 0
            || !buff_ids.insert(buff.buff_id)
            || buff.target_agent_id > MAX_AGENT_ID
    }) {
        return Err("buffs are outside their certified bounds".into());
    }
    let mut effect_ids = std::collections::HashSet::with_capacity(effects.effects.len());
    if effects.effects.iter().any(|effect| {
        effect.skill_id == 0
            || effect.skill_id > 100_000
            || effect.attribute_level > 100
            || effect.effect_id == 0
            || !effect_ids.insert(effect.effect_id)
            || effect.agent_id > MAX_AGENT_ID
            || !effect.duration.is_finite()
            || !(0.0..=1_000_000.0).contains(&effect.duration)
    }) {
        return Err("effects are outside their certified bounds".into());
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
            "targetValid":false,"targetKind":"None","rangeName":"None"
        }"#;
        assert_eq!(hub.publish(state).unwrap(), 1);
        assert_eq!(hub.publish(state).unwrap(), 2);
        let value: serde_json::Value = serde_json::from_slice(&hub.state_json().unwrap()).unwrap();
        assert_eq!(value["apiVersion"], 1);
        assert_eq!(value["revision"], 2);
        assert_eq!(value["state"]["mapId"], 55);
    }

    #[test]
    fn party_skillbar_and_effects_are_typed_and_bounded() {
        let hub = Hub::default();
        let state = br#"{
            "status":"ready","tickCount":7,"mapId":55,"instanceType":1,
            "instanceName":"Explorable","playerId":4,"playerX":1.5,"playerY":2.5,
            "targetValid":false,"targetKind":"None","rangeName":"None",
            "party":{
                "id":3,"hardMode":false,"defeated":false,"leader":true,
                "alliesTruncated":false,
                "players":[{"loginNumber":42,"calledTargetId":0,"state":3,"connected":true,"ticked":true}],
                "heroes":[{"agentId":8,"ownerPlayerId":42,"heroId":5,"level":20}],
                "henchmen":[],"allies":[9]
            },
            "skillbar":{
                "agentId":4,"disabledMask":0,"castCount":0,"casting":false,
                "skills":[
                    {"slot":1,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":100,"event":0,"disabled":false},
                    {"slot":2,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":101,"event":0,"disabled":false},
                    {"slot":3,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":102,"event":0,"disabled":false},
                    {"slot":4,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":103,"event":0,"disabled":false},
                    {"slot":5,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":104,"event":0,"disabled":false},
                    {"slot":6,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":105,"event":0,"disabled":false},
                    {"slot":7,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":106,"event":0,"disabled":false},
                    {"slot":8,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":107,"event":0,"disabled":false}
                ]
            },
            "effects":{
                "agentId":4,"buffsTruncated":false,"effectsTruncated":false,
                "buffs":[{"skillId":200,"buffId":300,"targetAgentId":4}],
                "effects":[{"skillId":201,"attributeLevel":12,"effectId":301,"agentId":8,"duration":12.5,"timestamp":400}]
            }
        }"#;
        hub.publish(state).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&hub.state_json().unwrap()).unwrap();
        assert_eq!(value["state"]["party"]["players"][0]["loginNumber"], 42);
        assert_eq!(value["state"]["skillbar"]["skills"][7]["slot"], 8);
        assert_eq!(value["state"]["skillbar"]["disabledMask"], 0);
        assert_eq!(value["state"]["skillbar"]["castCount"], 0);
        assert_eq!(value["state"]["effects"]["buffs"][0]["buffId"], 300);
        assert_eq!(value["state"]["effects"]["effects"][0]["duration"], 12.5);
    }

    #[test]
    fn malformed_party_skillbar_and_effects_state_is_refused() {
        let hub = Hub::default();
        for state in [
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":0,"playerY":0,"targetValid":false,"party":{"id":1,"hardMode":false,"defeated":false,"leader":false,"alliesTruncated":false,"players":[],"heroes":[],"henchmen":[],"allies":[]}}"#.as_slice(),
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":0,"playerY":0,"targetValid":false,"skillbar":{"agentId":3,"disabledMask":0,"castCount":0,"casting":false,"skills":[]}}"#.as_slice(),
            br#"{"status":"waiting","party":{"id":1,"hardMode":false,"defeated":false,"leader":false,"alliesTruncated":false,"players":[],"heroes":[],"henchmen":[],"allies":[]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","skillbar":{"agentId":2,"disabledMask":1,"castCount":0,"casting":false,"skills":[{"slot":1,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":1,"event":0,"disabled":false},{"slot":2,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":2,"event":0,"disabled":false},{"slot":3,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":3,"event":0,"disabled":false},{"slot":4,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":4,"event":0,"disabled":false},{"slot":5,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":5,"event":0,"disabled":false},{"slot":6,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":6,"event":0,"disabled":false},{"slot":7,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":7,"event":0,"disabled":false},{"slot":8,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":8,"event":0,"disabled":false}]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","effects":{"agentId":2,"buffsTruncated":false,"effectsTruncated":false,"buffs":[],"effects":[{"skillId":1,"attributeLevel":0,"effectId":7,"agentId":0,"duration":1,"timestamp":1},{"skillId":2,"attributeLevel":0,"effectId":7,"agentId":0,"duration":1,"timestamp":2}]}}"#.as_slice(),
            br#"{"status":"waiting","effects":{"agentId":2,"buffsTruncated":false,"effectsTruncated":false,"buffs":[],"effects":[]}}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
    }

    #[test]
    fn unknown_and_inconsistent_live_fields_are_refused() {
        let hub = Hub::default();
        for state in [
            br#"{"status":"waiting","reason":"loading","mapId":1}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Explorable","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None"}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","unexpected":true}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
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
