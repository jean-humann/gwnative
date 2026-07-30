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
pub const MAX_PUBLISH_BYTES: usize = 1024 * 1024;
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
pub struct MapAgent {
    pub agent_id: u32,
    pub type_bits: u32,
    pub kind: String,
    pub player_number: u32,
    pub primary: u32,
    pub secondary: u32,
    pub level: u32,
    pub health: f32,
    pub rotation: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub model_state: u32,
    pub effects: u32,
    pub allegiance: u32,
    pub is_living: bool,
    pub is_item: bool,
    pub is_gadget: bool,
    pub is_dead: bool,
    pub is_moving: bool,
    pub is_attacking: bool,
    pub is_knocked_down: bool,
    pub is_casting: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapAgents {
    pub truncated: bool,
    pub total: u32,
    pub agents: Vec<MapAgent>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Quest {
    pub quest_id: u32,
    pub log_state: u32,
    pub map_from: u32,
    pub marker_x: f32,
    pub marker_y: f32,
    pub marker_plane: u32,
    pub map_to: u32,
    pub completed: bool,
    pub current_mission: bool,
    pub primary: bool,
    pub area_primary: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissionObjective {
    pub objective_id: u32,
    #[serde(rename = "type")]
    pub objective_type: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Quests {
    pub active_quest_id: u32,
    pub quests_truncated: bool,
    pub objectives_truncated: bool,
    pub quests: Vec<Quest>,
    pub mission_objectives: Vec<MissionObjective>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryBag {
    pub bag_id: u32,
    pub bag_type: u32,
    pub kind: String,
    pub container_item: u32,
    pub capacity: u32,
    pub item_count: u32,
    pub is_inventory: bool,
    pub is_equipped: bool,
    pub is_not_collected: bool,
    pub is_storage: bool,
    pub is_material_storage: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryItem {
    pub item_id: u32,
    pub agent_id: u32,
    pub bag_id: u32,
    pub slot: u32,
    pub model_file_id: u32,
    #[serde(rename = "type")]
    pub item_type: u32,
    pub type_name: String,
    pub value: u32,
    pub interaction: u32,
    pub model_id: u32,
    pub item_formula: u32,
    pub quantity: u32,
    pub equipped: bool,
    pub profession: u32,
    pub customized: bool,
    pub material_salvageable: bool,
    pub modifier_count: u32,
    pub dye_tint: u32,
    pub dye1: u32,
    pub dye2: u32,
    pub dye3: u32,
    pub dye4: u32,
    pub is_stackable: bool,
    pub is_inscribable: bool,
    pub is_identified: bool,
    pub is_tradable: bool,
    pub is_usable: bool,
    pub is_prefix_upgradable: bool,
    pub is_suffix_upgradable: bool,
    pub is_inscription: bool,
    pub is_purple: bool,
    pub is_green: bool,
    pub is_gold: bool,
    pub is_inventory_item: bool,
    pub is_storage_item: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inventory {
    pub items_truncated: bool,
    pub total: u32,
    pub gold_character: u32,
    pub gold_storage: u32,
    pub storage_panes_unlocked: u32,
    pub bags: Vec<InventoryBag>,
    pub items: Vec<InventoryItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Friend {
    pub slot: u32,
    #[serde(rename = "type")]
    pub friend_type: u32,
    pub type_name: String,
    pub status: u32,
    pub status_name: String,
    pub friend_id: u32,
    pub zone_id: u32,
    pub is_online: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Friends {
    pub truncated: bool,
    pub total: u32,
    pub friends: u32,
    pub ignores: u32,
    pub partners: u32,
    pub traders: u32,
    pub entries: Vec<Friend>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuildCape {
    pub background_color: u32,
    pub detail_color: u32,
    pub emblem_color: u32,
    pub shape: u32,
    pub detail: u32,
    pub emblem: u32,
    pub trim: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Guild {
    pub index: u32,
    pub player_rank: u32,
    pub rank: u32,
    pub features: u32,
    pub rating: u32,
    pub faction: u32,
    pub faction_name: String,
    pub faction_points: u32,
    pub qualifier_points: u32,
    pub roster_total: u32,
    pub cape: GuildCape,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Social {
    pub player_status: u32,
    pub player_status_name: String,
    pub friends: Friends,
    pub guild: Option<Guild>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletionMode {
    pub completed_missions: Vec<u32>,
    pub completed_bonuses: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Completion {
    pub normal_mode: CompletionMode,
    pub hard_mode: CompletionMode,
    pub unlocked_maps: Vec<u32>,
    pub vanquished_areas: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Point3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Camera {
    pub look_at_agent_id: u32,
    pub mode: u32,
    pub mode_name: String,
    pub unlocked: bool,
    pub yaw: f32,
    pub current_yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub max_distance: f32,
    pub position: Point3,
    pub look_at: Point3,
    pub field_of_view: f32,
    pub render_field_of_view: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TradeItem {
    pub slot: u32,
    pub item_id: u32,
    pub quantity: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TradeParticipant {
    pub gold: u32,
    pub items_truncated: bool,
    pub items: Vec<TradeItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Trade {
    pub flags: u32,
    pub status_name: String,
    pub open: bool,
    pub initiated: bool,
    pub offer_sent: bool,
    pub accepted: bool,
    pub player: TradeParticipant,
    pub partner: TradeParticipant,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiPosition {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiFrame {
    pub frame_id: u32,
    pub parent_id: Option<u32>,
    pub child_offset_id: u32,
    pub frame_hash: u32,
    pub visibility_flags: u32,
    #[serde(rename = "type")]
    pub frame_type: u32,
    pub template_type: u32,
    pub state: u32,
    pub created: bool,
    pub destroying: bool,
    pub disabled: bool,
    pub hidden: bool,
    pub locally_visible: bool,
    pub position_valid: bool,
    pub position_flags: u32,
    pub position: UiPosition,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ui {
    pub truncated: bool,
    pub total: u32,
    pub created_total: u32,
    pub visible_total: u32,
    pub frames: Vec<UiFrame>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<MapAgents>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quests: Option<Quests>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Inventory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social: Option<Social>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<Completion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trade: Option<Trade>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<Ui>,
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
                "domains": [
                    "player", "map", "target", "party", "skillbar", "effects",
                    "agents", "quests", "inventory", "social", "completion", "camera",
                    "trade", "ui"
                ],
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
        if let Some(agents) = &state.agents {
            validate_agents(agents)?;
        }
        if let Some(quests) = &state.quests {
            validate_quests(quests)?;
        }
        if let Some(inventory) = &state.inventory {
            validate_inventory(inventory)?;
        }
        if let Some(social) = &state.social {
            validate_social(social)?;
        }
        if let Some(completion) = &state.completion {
            validate_completion(completion)?;
        }
        if let Some(camera) = &state.camera {
            validate_camera(camera)?;
        }
        if let Some(trade) = &state.trade {
            validate_trade(trade)?;
        }
        if let Some(ui) = &state.ui {
            validate_ui(ui)?;
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
        || state.agents.is_some()
        || state.quests.is_some()
        || state.inventory.is_some()
        || state.social.is_some()
        || state.completion.is_some()
        || state.camera.is_some()
        || state.trade.is_some()
        || state.ui.is_some()
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

fn validate_agents(agents: &MapAgents) -> Result<(), String> {
    if agents.agents.is_empty()
        || agents.agents.len() > 128
        || agents.total < agents.agents.len() as u32
        || agents.total > MAX_AGENT_ID
        || if agents.truncated {
            agents.agents.len() != 128 || agents.total <= 128
        } else {
            agents.total != agents.agents.len() as u32
        }
    {
        return Err("map-agent page is outside its certified bounds".into());
    }
    let mut previous_id = 0;
    for agent in &agents.agents {
        let living = agent.type_bits & 0xdb != 0;
        let item = agent.type_bits & 0x400 != 0;
        let gadget = agent.type_bits & 0x200 != 0;
        let kind = if living {
            "Living"
        } else if item {
            "Item"
        } else if gadget {
            "Gadget"
        } else {
            "Unknown"
        };
        if agent.agent_id <= previous_id
            || agent.agent_id > MAX_AGENT_ID
            || agent.type_bits & (0xdb | 0x200 | 0x400) == 0
            || agent.kind != kind
            || agent.player_number > u16::MAX as u32
            || agent.primary > 10
            || agent.secondary > 10
            || agent.level > u8::MAX as u32
            || !agent.health.is_finite()
            || !(-10.0..=10.0).contains(&agent.health)
            || !agent.rotation.is_finite()
            || agent.rotation.abs() > 4.0
            || [agent.x, agent.y, agent.z]
                .into_iter()
                .any(|value| !value.is_finite() || value.abs() > 1_000_000.0)
            || agent.allegiance > 6
            || agent.is_living != living
            || agent.is_item != item
            || agent.is_gadget != gadget
            || agent.is_dead != (living && agent.effects & 0x10 != 0)
            || agent.is_moving != (living && matches!(agent.model_state, 12 | 76 | 204))
            || agent.is_attacking != (living && matches!(agent.model_state, 96 | 1088 | 1120))
            || agent.is_knocked_down != (living && agent.model_state == 1104)
            || agent.is_casting != (living && matches!(agent.model_state, 65 | 581))
            || (!living
                && (agent.player_number != 0
                    || agent.primary != 0
                    || agent.secondary != 0
                    || agent.level != 0
                    || agent.health != 0.0
                    || agent.model_state != 0
                    || agent.effects != 0
                    || agent.allegiance != 0))
        {
            return Err("map-agent record is outside its certified bounds".into());
        }
        previous_id = agent.agent_id;
    }
    Ok(())
}

fn validate_quests(quests: &Quests) -> Result<(), String> {
    if quests.active_quest_id > 100_000
        || quests.quests.len() > 64
        || quests.mission_objectives.len() > 32
        || (quests.quests_truncated && quests.quests.len() != 64)
        || (quests.objectives_truncated && quests.mission_objectives.len() != 32)
    {
        return Err("quest page is outside its certified bounds".into());
    }
    let mut quest_ids = std::collections::HashSet::with_capacity(quests.quests.len());
    if quests.quests.iter().any(|quest| {
        quest.quest_id == 0
            || quest.quest_id > 100_000
            || !quest_ids.insert(quest.quest_id)
            || quest.map_from > 2_000
            || quest.map_to > 2_000
            || !quest.marker_x.is_finite()
            || quest.marker_x.abs() > 1_000_000.0
            || !quest.marker_y.is_finite()
            || quest.marker_y.abs() > 1_000_000.0
            || quest.marker_plane > 100_000
            || quest.completed != (quest.log_state & 0x2 != 0)
            || quest.current_mission != (quest.log_state & 0x10 != 0)
            || quest.primary != (quest.log_state & 0x20 != 0)
            || quest.area_primary != (quest.log_state & 0x40 != 0)
    }) {
        return Err("quest record is outside its certified bounds".into());
    }
    if quests.active_quest_id != 0
        && !quests.quests_truncated
        && !quest_ids.contains(&quests.active_quest_id)
    {
        return Err("active quest is absent from the complete quest page".into());
    }
    let mut objective_ids =
        std::collections::HashSet::with_capacity(quests.mission_objectives.len());
    if quests.mission_objectives.iter().any(|objective| {
        objective.objective_id == 0
            || !objective_ids.insert(objective.objective_id)
            || objective.objective_type > 100_000
    }) {
        return Err("mission objective is outside its certified bounds".into());
    }
    Ok(())
}

fn inventory_bag_type(bag_id: u32) -> Option<(u32, &'static str)> {
    match bag_id {
        1..=5 => Some((1, "Inventory")),
        6 => Some((5, "MaterialStorage")),
        7 => Some((3, "NotCollected")),
        8..=21 => Some((4, "Storage")),
        22 => Some((2, "Equipped")),
        _ => None,
    }
}

fn inventory_item_type(item_type: u32) -> Option<&'static str> {
    Some(match item_type {
        0 => "Salvage",
        2 => "Axe",
        3 => "Bag",
        4 => "Boots",
        5 => "Bow",
        6 => "Bundle",
        7 => "Chestpiece",
        8 => "Rune_Mod",
        9 => "Usable",
        10 => "Dye",
        11 => "Materials_Zcoins",
        12 => "Offhand",
        13 => "Gloves",
        15 => "Hammer",
        16 => "Headpiece",
        17 => "CC_Shards",
        18 => "Key",
        19 => "Leggings",
        20 => "Gold_Coin",
        21 => "Quest_Item",
        22 => "Wand",
        24 => "Shield",
        26 => "Staff",
        27 => "Sword",
        29 => "Kit",
        30 => "Trophy",
        31 => "Scroll",
        32 => "Daggers",
        33 => "Present",
        34 => "Minipet",
        35 => "Scythe",
        36 => "Spear",
        43 => "Storybook",
        44 => "Costume",
        45 => "Costume_Headpiece",
        0xff => "Unknown",
        _ => return None,
    })
}

fn validate_inventory(inventory: &Inventory) -> Result<(), String> {
    if inventory.storage_panes_unlocked > 14
        || inventory.bags.is_empty()
        || inventory.bags.len() > 22
        || inventory.items.len() > 512
        || inventory.total < inventory.items.len() as u32
        || inventory.total > 1_024
        || if inventory.items_truncated {
            inventory.items.len() != 512 || inventory.total <= 512
        } else {
            inventory.total != inventory.items.len() as u32
        }
    {
        return Err("inventory page is outside its certified bounds".into());
    }

    let mut previous_bag_id = 0;
    let mut total_capacity = 0u32;
    let mut expected_items = 0u32;
    let mut bags_by_id = std::collections::HashMap::with_capacity(inventory.bags.len());
    for bag in &inventory.bags {
        let Some((bag_type, kind)) = inventory_bag_type(bag.bag_id) else {
            return Err("inventory bag is outside its certified bounds".into());
        };
        if bag.bag_id <= previous_bag_id
            || bag.bag_type != bag_type
            || bag.kind != kind
            || bag.container_item > 1_000_000
            || bag.capacity > 256
            || bag.item_count > bag.capacity
            || bag.is_inventory != (bag_type == 1)
            || bag.is_equipped != (bag_type == 2)
            || bag.is_not_collected != (bag_type == 3)
            || bag.is_storage != (bag_type == 4)
            || bag.is_material_storage != (bag_type == 5)
        {
            return Err("inventory bag is outside its certified bounds".into());
        }
        total_capacity = total_capacity
            .checked_add(bag.capacity)
            .filter(|value| *value <= 1_024)
            .ok_or("inventory capacity exceeds its certified bound")?;
        expected_items = expected_items
            .checked_add(bag.item_count)
            .filter(|value| *value <= 1_024)
            .ok_or("inventory item total exceeds its certified bound")?;
        previous_bag_id = bag.bag_id;
        bags_by_id.insert(bag.bag_id, bag);
    }
    if !bags_by_id.contains_key(&1) || expected_items != inventory.total {
        return Err("inventory bag totals are inconsistent".into());
    }

    let mut item_ids = std::collections::HashSet::with_capacity(inventory.items.len());
    let mut locations = std::collections::HashSet::with_capacity(inventory.items.len());
    let mut items_by_bag = std::collections::HashMap::<u32, u32>::new();
    let mut previous_bag = 0;
    let mut previous_slot = None;
    for item in &inventory.items {
        let Some(bag) = bags_by_id.get(&item.bag_id) else {
            return Err("inventory item refers to an absent bag".into());
        };
        let Some(type_name) = inventory_item_type(item.item_type) else {
            return Err("inventory item type is outside its certified bounds".into());
        };
        let ordered = item.bag_id > previous_bag
            || (item.bag_id == previous_bag && previous_slot.is_some_and(|slot| item.slot > slot));
        if item.item_id == 0
            || item.item_id > 1_000_000
            || !item_ids.insert(item.item_id)
            || item.agent_id > MAX_AGENT_ID
            || item.slot >= bag.capacity
            || !locations.insert((item.bag_id, item.slot))
            || !ordered
            || item.model_file_id == 0
            || item.type_name != type_name
            || item.value > u16::MAX as u32
            || item.model_id == 0
            || item.item_formula > u16::MAX as u32
            || item.quantity == 0
            || item.quantity > u16::MAX as u32
            || (item.profession > 10 && item.profession != 0xff)
            || item.modifier_count > 64
            || [item.dye1, item.dye2, item.dye3, item.dye4]
                .into_iter()
                .any(|dye| dye > 0xf)
            || item.dye_tint > u8::MAX as u32
            || item.is_stackable != (item.interaction & 0x8_0000 != 0)
            || item.is_inscribable != (item.interaction & 0x800_0000 != 0)
            || item.is_identified != (item.interaction & 1 != 0)
            || item.is_tradable != (item.interaction & 0x100 == 0)
            || item.is_usable != (item.interaction & 0x100_0000 != 0)
            || item.is_prefix_upgradable != (item.interaction & 0x4000 == 0)
            || item.is_suffix_upgradable != (item.interaction & 0x8000 == 0)
            || item.is_inscription != (item.interaction & 0x2500_0000 == 0x2500_0000)
            || item.is_purple != (item.interaction & 0x40_0000 != 0)
            || item.is_green != (item.interaction & 0x10 != 0)
            || item.is_gold != (item.interaction & 0x2_0000 != 0)
            || item.is_inventory_item != matches!(bag.bag_type, 1 | 2)
            || item.is_storage_item != matches!(bag.bag_type, 4 | 5)
        {
            return Err("inventory item is outside its certified bounds".into());
        }
        *items_by_bag.entry(item.bag_id).or_default() += 1;
        previous_bag = item.bag_id;
        previous_slot = Some(item.slot);
    }
    for bag in &inventory.bags {
        let published = items_by_bag.get(&bag.bag_id).copied().unwrap_or_default();
        if published > bag.item_count || (!inventory.items_truncated && published != bag.item_count)
        {
            return Err("inventory item counts are inconsistent".into());
        }
    }
    Ok(())
}

fn friend_type_name(value: u32) -> Option<&'static str> {
    ["Unknown", "Friend", "Ignore", "Partner", "Trade"]
        .get(value as usize)
        .copied()
}

fn friend_status_name(value: u32) -> Option<&'static str> {
    ["Offline", "Online", "DoNotDisturb", "Away", "Unknown"]
        .get(value as usize)
        .copied()
}

fn validate_social(social: &Social) -> Result<(), String> {
    let Some(player_status_name) = friend_status_name(social.player_status) else {
        return Err("player social status is outside its certified bounds".into());
    };
    if social.player_status_name != player_status_name {
        return Err("player social status name is inconsistent".into());
    }

    let friends = &social.friends;
    let declared = friends
        .friends
        .checked_add(friends.ignores)
        .and_then(|value| value.checked_add(friends.partners))
        .and_then(|value| value.checked_add(friends.traders));
    if friends.entries.len() > 128
        || friends.total < friends.entries.len() as u32
        || friends.total > 256
        || declared.is_none_or(|value| value > friends.total)
        || if friends.truncated {
            friends.entries.len() != 128 || friends.total <= 128
        } else {
            friends.total != friends.entries.len() as u32
        }
    {
        return Err("friend page is outside its certified bounds".into());
    }

    let mut previous_slot = None;
    let mut observed = [0u32; 5];
    for friend in &friends.entries {
        let Some(type_name) = friend_type_name(friend.friend_type) else {
            return Err("friend type is outside its certified bounds".into());
        };
        let Some(status_name) = friend_status_name(friend.status) else {
            return Err("friend status is outside its certified bounds".into());
        };
        if previous_slot.is_some_and(|slot| friend.slot <= slot)
            || friend.slot >= 256
            || friend.type_name != type_name
            || friend.status_name != status_name
            || friend.friend_id > 1_000_000
            || friend.zone_id > 2_000
            || friend.is_online != matches!(friend.status, 1..=3)
        {
            return Err("friend record is outside its certified bounds".into());
        }
        previous_slot = Some(friend.slot);
        observed[friend.friend_type as usize] += 1;
    }
    let unknown = friends
        .total
        .checked_sub(declared.unwrap_or_default())
        .ok_or("friend category totals are inconsistent")?;
    let declared_by_type = [
        unknown,
        friends.friends,
        friends.ignores,
        friends.partners,
        friends.traders,
    ];
    if observed
        .iter()
        .zip(declared_by_type)
        .any(|(actual, expected)| *actual > expected || (!friends.truncated && *actual != expected))
    {
        return Err("friend category totals are inconsistent".into());
    }

    if let Some(guild) = &social.guild
        && (!(1..64).contains(&guild.index)
            || guild.faction > 1
            || guild.faction_name != ["Kurzick", "Luxon"][guild.faction as usize]
            || guild.roster_total > 100)
    {
        return Err("guild summary is outside its certified bounds".into());
    }
    Ok(())
}

fn validate_map_bitmap(values: &[u32]) -> bool {
    values.len() <= 1_024
        && values.iter().all(|map_id| *map_id < 1_024)
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn validate_completion(completion: &Completion) -> Result<(), String> {
    let categories = [
        completion.normal_mode.completed_missions.as_slice(),
        completion.normal_mode.completed_bonuses.as_slice(),
        completion.hard_mode.completed_missions.as_slice(),
        completion.hard_mode.completed_bonuses.as_slice(),
        completion.unlocked_maps.as_slice(),
        completion.vanquished_areas.as_slice(),
    ];
    if categories
        .iter()
        .any(|category| !validate_map_bitmap(category))
    {
        return Err("completion bitmap is outside its certified bounds".into());
    }
    Ok(())
}

fn validate_camera(camera: &Camera) -> Result<(), String> {
    let mode_name = match camera.mode {
        0 => "Default",
        2 => "Follow",
        3 => "Unlocked",
        1 | 4..=9 => "Unknown",
        _ => return Err("camera mode is outside its certified bounds".into()),
    };
    let coordinates = [
        camera.position.x,
        camera.position.y,
        camera.position.z,
        camera.look_at.x,
        camera.look_at.y,
        camera.look_at.z,
    ];
    let tangent =
        (camera.position.y - camera.look_at.y).atan2(camera.position.x - camera.look_at.x);
    let current_yaw = if tangent >= 0.0 {
        tangent - std::f32::consts::PI
    } else {
        tangent + std::f32::consts::PI
    };
    let render_field_of_view =
        1.0_f32.atan2((5.0 / 3.0) / (camera.field_of_view * 0.5).tan()) * 2.0;
    let approximately = |left: f32, right: f32| {
        left.is_finite() && right.is_finite() && (left - right).abs() <= 0.0001
    };
    if camera.look_at_agent_id > MAX_AGENT_ID
        || camera.mode_name != mode_name
        || camera.unlocked != (camera.mode == 3)
        || !camera.yaw.is_finite()
        || camera.yaw.abs() > 10.0
        || !camera.current_yaw.is_finite()
        || camera.current_yaw.abs() > std::f32::consts::PI
        || !approximately(camera.current_yaw, current_yaw)
        || !camera.pitch.is_finite()
        || !(-1.01..=1.01).contains(&camera.pitch)
        || !camera.distance.is_finite()
        || !(0.0..=100_000.0).contains(&camera.distance)
        || !camera.max_distance.is_finite()
        || !(0.0..=100_000.0).contains(&camera.max_distance)
        || coordinates
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 1_000_000.0)
        || !camera.field_of_view.is_finite()
        || !(0.0..=std::f32::consts::PI).contains(&camera.field_of_view)
        || camera.field_of_view == 0.0
        || !approximately(camera.render_field_of_view, render_field_of_view)
    {
        return Err("camera state is outside its certified bounds".into());
    }
    Ok(())
}

fn validate_trade_participant(participant: &TradeParticipant) -> bool {
    if participant.gold > 100_000
        || participant.items.len() > 16
        || (participant.items_truncated && participant.items.len() != 16)
    {
        return false;
    }
    let mut item_ids = std::collections::HashSet::with_capacity(participant.items.len());
    participant.items.iter().enumerate().all(|(index, item)| {
        item.slot == index as u32 + 1
            && (1..=1_000_000).contains(&item.item_id)
            && (1..=250).contains(&item.quantity)
            && item_ids.insert(item.item_id)
    })
}

fn validate_trade(trade: &Trade) -> Result<(), String> {
    const INITIATED: u32 = 1;
    const OFFER_SENT: u32 = 2;
    const ACCEPTED: u32 = 4;
    if trade.flags & !(INITIATED | OFFER_SENT | ACCEPTED) != 0 {
        return Err("trade status has unknown flags".into());
    }
    let initiated = trade.flags & INITIATED != 0;
    let offer_sent = trade.flags & OFFER_SENT != 0;
    let accepted = trade.flags & ACCEPTED != 0;
    let open = trade.flags != 0;
    let status_name = if accepted {
        "Accepted"
    } else if offer_sent {
        "OfferSent"
    } else if initiated {
        "Initiated"
    } else {
        "Closed"
    };
    if trade.initiated != initiated
        || trade.offer_sent != offer_sent
        || trade.accepted != accepted
        || trade.open != open
        || trade.status_name != status_name
        || !validate_trade_participant(&trade.player)
        || !validate_trade_participant(&trade.partner)
        || (!open
            && (trade.player.gold != 0
                || trade.partner.gold != 0
                || trade.player.items_truncated
                || trade.partner.items_truncated
                || !trade.player.items.is_empty()
                || !trade.partner.items.is_empty()))
    {
        return Err("trade state is outside its certified bounds".into());
    }
    Ok(())
}

fn validate_ui(ui: &Ui) -> Result<(), String> {
    if ui.frames.len() > 128
        || ui.total > 2_048
        || ui.total < ui.frames.len() as u32
        || ui.created_total > ui.total
        || ui.visible_total > ui.created_total
        || (ui.truncated && (ui.frames.len() != 128 || ui.total <= 128))
        || (!ui.truncated && ui.total != ui.frames.len() as u32)
    {
        return Err("ui frame page is outside its certified bounds".into());
    }

    let mut ids = std::collections::HashSet::with_capacity(ui.frames.len());
    let mut created = 0u32;
    let mut visible = 0u32;
    for frame in &ui.frames {
        let position = [
            frame.position.left,
            frame.position.bottom,
            frame.position.right,
            frame.position.top,
        ];
        let expected_created = frame.state & 0x4 != 0;
        let expected_destroying = frame.state & 0x8 != 0;
        let expected_disabled = frame.state & 0x10 != 0;
        let expected_hidden = frame.state & 0x200 != 0;
        let expected_visible = expected_created && !expected_destroying && !expected_hidden;
        if frame.frame_id >= 2_048
            || !ids.insert(frame.frame_id)
            || frame
                .parent_id
                .is_some_and(|id| id >= 2_048 || id == frame.frame_id)
            || frame.created != expected_created
            || frame.destroying != expected_destroying
            || frame.disabled != expected_disabled
            || frame.hidden != expected_hidden
            || frame.locally_visible != expected_visible
            || (frame.position_valid
                && position
                    .iter()
                    .any(|value| !value.is_finite() || value.abs() > 1_000_000.0))
            || (!frame.position_valid
                && (frame.position_flags != 0 || position.iter().any(|value| *value != 0.0)))
        {
            return Err("ui frame state is outside its certified bounds".into());
        }
        created += u32::from(frame.created);
        visible += u32::from(frame.locally_visible);
    }
    if created > ui.created_total
        || visible > ui.visible_total
        || (!ui.truncated && (created != ui.created_total || visible != ui.visible_total))
        || (!ui.truncated
            && ui.frames.iter().any(|frame| {
                frame
                    .parent_id
                    .is_some_and(|parent_id| !ids.contains(&parent_id))
            }))
    {
        return Err("ui frame totals do not match the published page".into());
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
    fn certified_nested_domains_are_typed_and_bounded() {
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
            },
            "agents":{
                "truncated":false,"total":1,
                "agents":[{
                    "agentId":4,"typeBits":219,"kind":"Living","playerNumber":42,
                    "primary":7,"secondary":0,"level":20,"health":0.75,
                    "rotation":1.25,"x":1.5,"y":2.5,"z":3,
                    "modelState":65,"effects":0,"allegiance":1,
                    "isLiving":true,"isItem":false,"isGadget":false,
                    "isDead":false,"isMoving":false,"isAttacking":false,
                    "isKnockedDown":false,"isCasting":true
                }]
            },
            "quests":{
                "activeQuestId":44,"questsTruncated":false,
                "objectivesTruncated":false,
                "quests":[{
                    "questId":44,"logState":34,"mapFrom":55,
                    "markerX":10,"markerY":20,"markerPlane":3,"mapTo":56,
                    "completed":true,"currentMission":false,
                    "primary":true,"areaPrimary":false
                }],
                "missionObjectives":[{"objectiveId":7,"type":2}]
            },
            "inventory":{
                "itemsTruncated":false,"total":1,
                "goldCharacter":1234,"goldStorage":50000,
                "storagePanesUnlocked":4,
                "bags":[{
                    "bagId":1,"bagType":1,"kind":"Inventory",
                    "containerItem":0,"capacity":20,"itemCount":1,
                    "isInventory":true,"isEquipped":false,
                    "isNotCollected":false,"isStorage":false,
                    "isMaterialStorage":false
                }],
                "items":[{
                    "itemId":500,"agentId":0,"bagId":1,"slot":0,
                    "modelFileId":123,"type":9,"typeName":"Usable",
                    "value":100,"interaction":17432577,"modelId":456,
                    "itemFormula":0,"quantity":5,"equipped":false,
                    "profession":255,"customized":true,
                    "materialSalvageable":false,"modifierCount":2,
                    "dyeTint":7,"dye1":2,"dye2":3,"dye3":4,"dye4":5,
                    "isStackable":true,"isInscribable":false,
                    "isIdentified":true,"isTradable":true,"isUsable":true,
                    "isPrefixUpgradable":true,"isSuffixUpgradable":true,
                    "isInscription":false,"isPurple":false,"isGreen":false,
                    "isGold":true,"isInventoryItem":true,
                    "isStorageItem":false
                }]
            },
            "social":{
                "playerStatus":1,"playerStatusName":"Online",
                "friends":{
                    "truncated":false,"total":1,"friends":1,
                    "ignores":0,"partners":0,"traders":0,
                    "entries":[{
                        "slot":0,"type":1,"typeName":"Friend",
                        "status":1,"statusName":"Online",
                        "friendId":77,"zoneId":55,"isOnline":true
                    }]
                },
                "guild":{
                    "index":2,"playerRank":3,"rank":1,"features":9,
                    "rating":1200,"faction":0,"factionName":"Kurzick",
                    "factionPoints":1000,"qualifierPoints":10,
                    "rosterTotal":50,
                    "cape":{
                        "backgroundColor":1,"detailColor":2,
                        "emblemColor":3,"shape":4,"detail":5,
                        "emblem":6,"trim":7
                    }
                }
            },
            "camera":{
                "lookAtAgentId":4,"mode":3,"modeName":"Unlocked",
                "unlocked":true,"yaw":1.25,"currentYaw":2.3561945,
                "pitch":0.25,"distance":1000,"maxDistance":5000,
                "position":{"x":110,"y":-260,"z":-50},
                "lookAt":{"x":100,"y":-250,"z":3},
                "fieldOfView":1.2,"renderFieldOfView":0.77901974
            },
            "trade":{
                "flags":3,"statusName":"OfferSent","open":true,
                "initiated":true,"offerSent":true,"accepted":false,
                "player":{
                    "gold":2222,"itemsTruncated":false,
                    "items":[
                        {"slot":1,"itemId":700,"quantity":5},
                        {"slot":2,"itemId":701,"quantity":1}
                    ]
                },
                "partner":{
                    "gold":3333,"itemsTruncated":false,
                    "items":[{"slot":1,"itemId":800,"quantity":2}]
                }
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
        assert_eq!(value["state"]["agents"]["agents"][0]["kind"], "Living");
        assert_eq!(value["state"]["agents"]["agents"][0]["isCasting"], true);
        assert_eq!(value["state"]["quests"]["activeQuestId"], 44);
        assert_eq!(value["state"]["quests"]["quests"][0]["completed"], true);
        assert_eq!(
            value["state"]["inventory"]["items"][0]["typeName"],
            "Usable"
        );
        assert_eq!(value["state"]["inventory"]["items"][0]["isGold"], true);
        assert_eq!(
            value["state"]["social"]["friends"]["entries"][0]["typeName"],
            "Friend"
        );
        assert_eq!(value["state"]["social"]["guild"]["factionName"], "Kurzick");
        assert_eq!(value["state"]["camera"]["modeName"], "Unlocked");
        assert_eq!(
            value["state"]["camera"]["position"]["z"].as_f64(),
            Some(-50.0),
        );
        assert_eq!(value["state"]["trade"]["statusName"], "OfferSent");
        assert_eq!(value["state"]["trade"]["player"]["items"][1]["itemId"], 701);
    }

    #[test]
    fn completion_map_ids_are_sorted_and_bounded() {
        let hub = Hub::default();
        let state = br#"{
            "status":"ready","tickCount":1,"mapId":55,"instanceType":0,
            "instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,
            "targetValid":false,"targetKind":"None","rangeName":"None",
            "completion":{
                "normalMode":{"completedMissions":[55,56],"completedBonuses":[55]},
                "hardMode":{"completedMissions":[55],"completedBonuses":[]},
                "unlockedMaps":[55,248],"vanquishedAreas":[56]
            }
        }"#;
        hub.publish(state).unwrap();

        for state in [
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","completion":{"normalMode":{"completedMissions":[56,55],"completedBonuses":[]},"hardMode":{"completedMissions":[],"completedBonuses":[]},"unlockedMaps":[],"vanquishedAreas":[]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","completion":{"normalMode":{"completedMissions":[],"completedBonuses":[]},"hardMode":{"completedMissions":[],"completedBonuses":[]},"unlockedMaps":[1024],"vanquishedAreas":[]}}"#.as_slice(),
            br#"{"status":"waiting","completion":{"normalMode":{"completedMissions":[],"completedBonuses":[]},"hardMode":{"completedMissions":[],"completedBonuses":[]},"unlockedMaps":[],"vanquishedAreas":[]}}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
    }

    #[test]
    fn camera_state_is_derived_and_bounded() {
        let hub = Hub::default();
        let valid = br#"{
            "status":"ready","tickCount":1,"mapId":55,"instanceType":0,
            "instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,
            "targetValid":false,"targetKind":"None","rangeName":"None",
            "camera":{
                "lookAtAgentId":2,"mode":2,"modeName":"Follow",
                "unlocked":false,"yaw":1.25,"currentYaw":2.3561945,
                "pitch":0.25,"distance":1000,"maxDistance":5000,
                "position":{"x":110,"y":-260,"z":-50},
                "lookAt":{"x":100,"y":-250,"z":3},
                "fieldOfView":1.2,"renderFieldOfView":0.77901974
            }
        }"#;
        hub.publish(valid).unwrap();

        for state in [
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","camera":{"lookAtAgentId":2,"mode":10,"modeName":"Unknown","unlocked":false,"yaw":1.25,"currentYaw":2.3561945,"pitch":0.25,"distance":1000,"maxDistance":5000,"position":{"x":110,"y":-260,"z":-50},"lookAt":{"x":100,"y":-250,"z":3},"fieldOfView":1.2,"renderFieldOfView":0.77901974}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","camera":{"lookAtAgentId":2,"mode":2,"modeName":"Follow","unlocked":false,"yaw":1.25,"currentYaw":0,"pitch":0.25,"distance":1000,"maxDistance":5000,"position":{"x":110,"y":-260,"z":-50},"lookAt":{"x":100,"y":-250,"z":3},"fieldOfView":1.2,"renderFieldOfView":0.77901974}}"#.as_slice(),
            br#"{"status":"waiting","camera":{"lookAtAgentId":0,"mode":0,"modeName":"Default","unlocked":false,"yaw":0,"currentYaw":0,"pitch":0,"distance":0,"maxDistance":0,"position":{"x":0,"y":0,"z":0},"lookAt":{"x":0,"y":0,"z":0},"fieldOfView":1,"renderFieldOfView":1}}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
    }

    #[test]
    fn trade_state_is_derived_and_bounded() {
        let hub = Hub::default();
        let valid = br#"{
            "status":"ready","tickCount":1,"mapId":55,"instanceType":0,
            "instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,
            "targetValid":false,"targetKind":"None","rangeName":"None",
            "trade":{
                "flags":3,"statusName":"OfferSent","open":true,
                "initiated":true,"offerSent":true,"accepted":false,
                "player":{"gold":100000,"itemsTruncated":false,"items":[{"slot":1,"itemId":7,"quantity":250}]},
                "partner":{"gold":0,"itemsTruncated":false,"items":[]}
            }
        }"#;
        hub.publish(valid).unwrap();

        let closed = br#"{
            "status":"ready","tickCount":2,"mapId":55,"instanceType":0,
            "instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,
            "targetValid":false,"targetKind":"None","rangeName":"None",
            "trade":{
                "flags":0,"statusName":"Closed","open":false,
                "initiated":false,"offerSent":false,"accepted":false,
                "player":{"gold":0,"itemsTruncated":false,"items":[]},
                "partner":{"gold":0,"itemsTruncated":false,"items":[]}
            }
        }"#;
        hub.publish(closed).unwrap();

        for state in [
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","trade":{"flags":3,"statusName":"Initiated","open":true,"initiated":true,"offerSent":true,"accepted":false,"player":{"gold":0,"itemsTruncated":false,"items":[]},"partner":{"gold":0,"itemsTruncated":false,"items":[]}}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","trade":{"flags":1,"statusName":"Initiated","open":true,"initiated":true,"offerSent":false,"accepted":false,"player":{"gold":0,"itemsTruncated":false,"items":[{"slot":1,"itemId":7,"quantity":1},{"slot":2,"itemId":7,"quantity":2}]},"partner":{"gold":0,"itemsTruncated":false,"items":[]}}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","trade":{"flags":0,"statusName":"Closed","open":false,"initiated":false,"offerSent":false,"accepted":false,"player":{"gold":1,"itemsTruncated":false,"items":[]},"partner":{"gold":0,"itemsTruncated":false,"items":[]}}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","trade":{"flags":1,"statusName":"Initiated","open":true,"initiated":true,"offerSent":false,"accepted":false,"player":{"gold":0,"itemsTruncated":true,"items":[{"slot":1,"itemId":7,"quantity":1}]},"partner":{"gold":0,"itemsTruncated":false,"items":[]}}}"#.as_slice(),
            br#"{"status":"waiting","trade":{"flags":0,"statusName":"Closed","open":false,"initiated":false,"offerSent":false,"accepted":false,"player":{"gold":0,"itemsTruncated":false,"items":[]},"partner":{"gold":0,"itemsTruncated":false,"items":[]}}}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
    }

    #[test]
    fn ui_frame_state_is_derived_and_bounded() {
        let hub = Hub::default();
        let valid = br#"{
            "status":"ready","tickCount":1,"mapId":55,"instanceType":0,
            "instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,
            "targetValid":false,"targetKind":"None","rangeName":"None",
            "ui":{
                "truncated":false,"total":2,"createdTotal":2,"visibleTotal":1,
                "frames":[
                    {
                        "frameId":0,"parentId":null,"childOffsetId":0,
                        "frameHash":4369,"visibilityFlags":3,"type":4,
                        "templateType":5,"state":4,"created":true,
                        "destroying":false,"disabled":false,"hidden":false,
                        "locallyVisible":true,"positionValid":true,
                        "positionFlags":9,
                        "position":{"left":10,"bottom":100,"right":200,"top":20}
                    },
                    {
                        "frameId":1,"parentId":0,"childOffsetId":2,
                        "frameHash":8738,"visibilityFlags":1,"type":7,
                        "templateType":8,"state":516,"created":true,
                        "destroying":false,"disabled":false,"hidden":true,
                        "locallyVisible":false,"positionValid":false,
                        "positionFlags":0,
                        "position":{"left":0,"bottom":0,"right":0,"top":0}
                    }
                ]
            }
        }"#;
        hub.publish(valid).unwrap();

        for state in [
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","ui":{"truncated":false,"total":1,"createdTotal":1,"visibleTotal":1,"frames":[{"frameId":1,"parentId":7,"childOffsetId":0,"frameHash":1,"visibilityFlags":0,"type":0,"templateType":0,"state":4,"created":true,"destroying":false,"disabled":false,"hidden":false,"locallyVisible":true,"positionValid":false,"positionFlags":0,"position":{"left":0,"bottom":0,"right":0,"top":0}}]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","ui":{"truncated":false,"total":1,"createdTotal":1,"visibleTotal":1,"frames":[{"frameId":1,"parentId":null,"childOffsetId":0,"frameHash":1,"visibilityFlags":0,"type":0,"templateType":0,"state":4,"created":false,"destroying":false,"disabled":false,"hidden":false,"locallyVisible":true,"positionValid":false,"positionFlags":0,"position":{"left":0,"bottom":0,"right":0,"top":0}}]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":55,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","ui":{"truncated":false,"total":1,"createdTotal":1,"visibleTotal":1,"frames":[{"frameId":1,"parentId":null,"childOffsetId":0,"frameHash":1,"visibilityFlags":0,"type":0,"templateType":0,"state":4,"created":true,"destroying":false,"disabled":false,"hidden":false,"locallyVisible":true,"positionValid":false,"positionFlags":1,"position":{"left":0,"bottom":0,"right":0,"top":0}}]}}"#.as_slice(),
            br#"{"status":"waiting","ui":{"truncated":false,"total":0,"createdTotal":0,"visibleTotal":0,"frames":[]}}"#.as_slice(),
        ] {
            assert!(hub.publish(state).is_err());
        }
    }

    #[test]
    fn malformed_nested_domain_state_is_refused() {
        let hub = Hub::default();
        for state in [
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":0,"playerY":0,"targetValid":false,"party":{"id":1,"hardMode":false,"defeated":false,"leader":false,"alliesTruncated":false,"players":[],"heroes":[],"henchmen":[],"allies":[]}}"#.as_slice(),
            br#"{"status":"ready","mapId":1,"playerId":2,"playerX":0,"playerY":0,"targetValid":false,"skillbar":{"agentId":3,"disabledMask":0,"castCount":0,"casting":false,"skills":[]}}"#.as_slice(),
            br#"{"status":"waiting","party":{"id":1,"hardMode":false,"defeated":false,"leader":false,"alliesTruncated":false,"players":[],"heroes":[],"henchmen":[],"allies":[]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","skillbar":{"agentId":2,"disabledMask":1,"castCount":0,"casting":false,"skills":[{"slot":1,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":1,"event":0,"disabled":false},{"slot":2,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":2,"event":0,"disabled":false},{"slot":3,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":3,"event":0,"disabled":false},{"slot":4,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":4,"event":0,"disabled":false},{"slot":5,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":5,"event":0,"disabled":false},{"slot":6,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":6,"event":0,"disabled":false},{"slot":7,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":7,"event":0,"disabled":false},{"slot":8,"adrenalineA":0,"adrenalineB":0,"recharge":0,"skillId":8,"event":0,"disabled":false}]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","effects":{"agentId":2,"buffsTruncated":false,"effectsTruncated":false,"buffs":[],"effects":[{"skillId":1,"attributeLevel":0,"effectId":7,"agentId":0,"duration":1,"timestamp":1},{"skillId":2,"attributeLevel":0,"effectId":7,"agentId":0,"duration":1,"timestamp":2}]}}"#.as_slice(),
            br#"{"status":"waiting","effects":{"agentId":2,"buffsTruncated":false,"effectsTruncated":false,"buffs":[],"effects":[]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","agents":{"truncated":false,"total":1,"agents":[{"agentId":2,"typeBits":219,"kind":"Living","playerNumber":1,"primary":0,"secondary":0,"level":20,"health":1,"rotation":0,"x":0,"y":0,"z":0,"modelState":65,"effects":0,"allegiance":1,"isLiving":true,"isItem":false,"isGadget":false,"isDead":false,"isMoving":false,"isAttacking":false,"isKnockedDown":false,"isCasting":false}]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","quests":{"activeQuestId":2,"questsTruncated":false,"objectivesTruncated":false,"quests":[{"questId":1,"logState":0,"mapFrom":0,"markerX":0,"markerY":0,"markerPlane":0,"mapTo":0,"completed":false,"currentMission":false,"primary":false,"areaPrimary":false}],"missionObjectives":[]}}"#.as_slice(),
            br#"{"status":"waiting","agents":{"truncated":false,"total":0,"agents":[]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","inventory":{"itemsTruncated":false,"total":1,"goldCharacter":0,"goldStorage":0,"storagePanesUnlocked":0,"bags":[{"bagId":1,"bagType":4,"kind":"Storage","containerItem":0,"capacity":20,"itemCount":1,"isInventory":false,"isEquipped":false,"isNotCollected":false,"isStorage":true,"isMaterialStorage":false}],"items":[]}}"#.as_slice(),
            br#"{"status":"waiting","inventory":{"itemsTruncated":false,"total":0,"goldCharacter":0,"goldStorage":0,"storagePanesUnlocked":0,"bags":[],"items":[]}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","social":{"playerStatus":1,"playerStatusName":"Away","friends":{"truncated":false,"total":0,"friends":0,"ignores":0,"partners":0,"traders":0,"entries":[]},"guild":null}}"#.as_slice(),
            br#"{"status":"ready","tickCount":1,"mapId":1,"instanceType":0,"instanceName":"Outpost","playerId":2,"playerX":0,"playerY":0,"targetValid":false,"targetKind":"None","rangeName":"None","social":{"playerStatus":1,"playerStatusName":"Online","friends":{"truncated":false,"total":1,"friends":1,"ignores":0,"partners":0,"traders":0,"entries":[]},"guild":null}}"#.as_slice(),
            br#"{"status":"waiting","social":{"playerStatus":0,"playerStatusName":"Offline","friends":{"truncated":false,"total":0,"friends":0,"ignores":0,"partners":0,"traders":0,"entries":[]},"guild":null}}"#.as_slice(),
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
