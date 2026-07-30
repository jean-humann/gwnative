//! The companion module: a second WebAssembly instance that shares the client's
//! memory and runs once per frame inside it.
//!
//! It is built by `build.rs`, not by Cargo — no `Cargo.toml`, no dependencies,
//! `no_std`, and a bare `rustc` invocation with `--import-memory` so the linker
//! leaves `env.memory` to be supplied at instantiation. That memory is the
//! game's own. Everything below reads it and nothing writes to it except the
//! two regions the host allocated from the game's allocator and passed to
//! [`companion_init`].
//!
//! Why a separate module rather than code in the host: what it needs is a read
//! of game state *at a consistent point in the frame*, and the only such point
//! is inside the main loop. The host is on the other side of an event loop and
//! sees memory at arbitrary moments, halfway through the update that moves
//! every agent. So `crate::wasm::enhancement` clones the client's main loop and
//! points its export at a dispatcher, the page installs [`companion_tick`] in
//! the table slot that dispatcher calls, and this module calls the original
//! loop first and then reads what it just finished writing.
//!
//! Nothing here can trap. A trap inside the main loop takes the client with it,
//! so every read is bounds-checked against the current memory size and every
//! pointer chase is an `Option` that gives up rather than guesses. A layout
//! that has gone stale therefore costs a snapshot, never the session.
//!
//! Both snapshots are published under a seqlock: an odd sequence means a write
//! is in flight, and a reader that sees the same even value before and after
//! its copy knows nothing moved underneath it. That is what lets the renderer
//! read from JavaScript without stopping the frame.
//!
//! `static mut` throughout, and `unsafe` throughout, because the wasm32 target
//! is single-threaded and this is a freestanding module with no allocator, no
//! atomics and no runtime — there is nothing for a `Mutex` or a `Cell` to buy
//! and no second thread for either to protect against.

#![no_std]

use core::mem::size_of;
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

const SNAPSHOT_BYTES: u32 = size_of::<Snapshot>() as u32;
const CONFIG_BYTES: u32 = size_of::<Layout>() as u32;
const MAGIC: u32 = 0x4254_5747;
const ABI_AND_SIZE: u32 = (SNAPSHOT_BYTES << 16) | 5;

const FLAG_READY: u32 = 1 << 0;
const FLAG_PLAYER_VALID: u32 = 1 << 1;
const FLAG_TARGET_VALID: u32 = 1 << 2;
const FLAG_LOADING: u32 = 1 << 3;
const FLAG_PARTY_VALID: u32 = 1 << 4;
const FLAG_SKILLBAR_VALID: u32 = 1 << 5;
const FLAG_EFFECTS_VALID: u32 = 1 << 6;
const FLAG_MAP_AGENTS_VALID: u32 = 1 << 7;
const FLAG_QUESTS_VALID: u32 = 1 << 8;
const FLAG_INVENTORY_VALID: u32 = 1 << 9;

const MAX_PARTY_PLAYERS: usize = 12;
const MAX_PARTY_HEROES: usize = 12;
const MAX_PARTY_HENCHMEN: usize = 12;
const MAX_PARTY_ALLIES: usize = 32;
const SKILL_SLOTS: usize = 8;
const MAX_EFFECT_AGENTS: u32 = 64;
const MAX_RAW_BUFFS: u32 = 240;
const MAX_RAW_EFFECTS: u32 = 240;
const MAX_PLAYER_BUFFS: usize = 32;
const MAX_PLAYER_EFFECTS: usize = 64;
const MAX_MAP_AGENTS: usize = 128;
const MAX_RAW_QUESTS: u32 = 256;
const MAX_QUESTS: usize = 64;
const MAX_RAW_MISSION_OBJECTIVES: u32 = 128;
const MAX_MISSION_OBJECTIVES: usize = 32;
const MAX_INVENTORY_BAGS: usize = 22;
const MAX_INVENTORY_ITEMS: usize = 512;
const MAX_BAG_SLOTS: u32 = 256;
const MAX_TOTAL_BAG_SLOTS: u32 = 1_024;

const FEATURE_NATIVE_CURSOR: u32 = 1 << 0;
const FEATURE_TARGET_READOUT: u32 = 1 << 1;
const KNOWN_FEATURES: u32 = FEATURE_NATIVE_CURSOR | FEATURE_TARGET_READOUT;

const CURSOR_BYTES: u32 = size_of::<CursorSnapshot>() as u32;
const CURSOR_MAGIC: u32 = 0x4354_5747;
const CURSOR_ABI_AND_SIZE: u32 = (CURSOR_BYTES << 16) | 1;

const FLAG_CURSOR_VALID: u32 = 1 << 0;
const FLAG_CURSOR_HIDDEN: u32 = 1 << 1;
const FLAG_CURSOR_UNSUPPORTED: u32 = 1 << 2;

const CURSOR_EDGE: u32 = 32;
const CURSOR_WORDS: u32 = CURSOR_EDGE * CURSOR_EDGE;
const CURSOR_PIXEL_BYTES: u32 = CURSOR_WORDS * 4;
// 'grtx', the texture handle's access key.
const CURSOR_TEXTURE_KEY: u32 = 0x6772_7478;
const CURSOR_TEXTURE_TYPE: u32 = 10;

#[repr(C)]
#[derive(Clone, Copy)]
struct Layout {
    context_root: u32,
    agent_array: u32,
    manual_target_agent_id: u32,
    automatic_target_agent_id: u32,
    game_context_slot: u32,
    character_context: u32,
    map_id: u32,
    is_explorable: u32,
    current_map_id: u32,
    current_instance_type: u32,
    player_number: u32,
    agent_id: u32,
    agent_x: u32,
    agent_y: u32,
    agent_z: u32,
    agent_rotation: u32,
    agent_type: u32,
    agent_player_number: u32,
    agent_model_type: u32,
    agent_primary: u32,
    agent_secondary: u32,
    agent_level: u32,
    agent_hp: u32,
    agent_model_state: u32,
    agent_effects: u32,
    agent_allegiance: u32,
    game_world_context: u32,
    game_party_context: u32,
    party_flag: u32,
    party_player_party: u32,
    party_id: u32,
    party_players: u32,
    party_henchmen: u32,
    party_heroes: u32,
    party_others: u32,
    party_player_stride: u32,
    party_player_login_number: u32,
    party_player_called_target_id: u32,
    party_player_state: u32,
    party_hero_stride: u32,
    party_hero_agent_id: u32,
    party_hero_owner_player_id: u32,
    party_hero_id: u32,
    party_hero_level: u32,
    party_henchman_stride: u32,
    party_henchman_agent_id: u32,
    party_henchman_profession: u32,
    party_henchman_level: u32,
    world_skillbar: u32,
    skillbar_stride: u32,
    skillbar_agent_id: u32,
    skillbar_skills: u32,
    skillbar_disabled: u32,
    skillbar_cast_count: u32,
    skill_stride: u32,
    skill_adrenaline_a: u32,
    skill_adrenaline_b: u32,
    skill_recharge: u32,
    skill_id: u32,
    skill_event: u32,
    world_party_effects: u32,
    agent_effects_stride: u32,
    agent_effects_agent_id: u32,
    agent_effects_buffs: u32,
    agent_effects_effects: u32,
    buff_stride: u32,
    buff_skill_id: u32,
    buff_id: u32,
    buff_target_agent_id: u32,
    effect_stride: u32,
    effect_skill_id: u32,
    effect_attribute_level: u32,
    effect_id: u32,
    effect_agent_id: u32,
    effect_duration: u32,
    effect_timestamp: u32,
    world_active_quest: u32,
    world_quest_log: u32,
    quest_stride: u32,
    quest_id: u32,
    quest_log_state: u32,
    quest_map_from: u32,
    quest_marker: u32,
    quest_map_to: u32,
    world_mission_objectives: u32,
    mission_objective_stride: u32,
    mission_objective_id: u32,
    mission_objective_type: u32,
    game_item_context: u32,
    item_context_inventory: u32,
    inventory_bags: u32,
    inventory_storage_panes: u32,
    inventory_gold_character: u32,
    inventory_gold_storage: u32,
    bag_type: u32,
    bag_index: u32,
    bag_container_item: u32,
    bag_items_count: u32,
    bag_items: u32,
    item_id: u32,
    item_agent_id: u32,
    item_bag: u32,
    item_modifiers: u32,
    item_modifier_count: u32,
    item_customized: u32,
    item_model_file_id: u32,
    item_type: u32,
    item_dye: u32,
    item_value: u32,
    item_interaction: u32,
    item_model_id: u32,
    item_formula: u32,
    item_material_salvageable: u32,
    item_quantity: u32,
    item_equipped: u32,
    item_profession: u32,
    item_slot: u32,
    cursor_active_art: u32,
    cursor_software_model: u32,
    cursor_show_count: u32,
    cursor_color_buffer: u32,
    cursor_art_hotspot: u32,
    cursor_art_texture: u32,
    cursor_handle_key: u32,
    cursor_handle_object: u32,
    cursor_view_texture: u32,
    cursor_texture_type: u32,
    cursor_texture_width: u32,
    cursor_texture_height: u32,
}

impl Layout {
    const EMPTY: Self = Self {
        context_root: 0,
        agent_array: 0,
        manual_target_agent_id: 0,
        automatic_target_agent_id: 0,
        game_context_slot: 0,
        character_context: 0,
        map_id: 0,
        is_explorable: 0,
        current_map_id: 0,
        current_instance_type: 0,
        player_number: 0,
        agent_id: 0,
        agent_x: 0,
        agent_y: 0,
        agent_z: 0,
        agent_rotation: 0,
        agent_type: 0,
        agent_player_number: 0,
        agent_model_type: 0,
        agent_primary: 0,
        agent_secondary: 0,
        agent_level: 0,
        agent_hp: 0,
        agent_model_state: 0,
        agent_effects: 0,
        agent_allegiance: 0,
        game_world_context: 0,
        game_party_context: 0,
        party_flag: 0,
        party_player_party: 0,
        party_id: 0,
        party_players: 0,
        party_henchmen: 0,
        party_heroes: 0,
        party_others: 0,
        party_player_stride: 0,
        party_player_login_number: 0,
        party_player_called_target_id: 0,
        party_player_state: 0,
        party_hero_stride: 0,
        party_hero_agent_id: 0,
        party_hero_owner_player_id: 0,
        party_hero_id: 0,
        party_hero_level: 0,
        party_henchman_stride: 0,
        party_henchman_agent_id: 0,
        party_henchman_profession: 0,
        party_henchman_level: 0,
        world_skillbar: 0,
        skillbar_stride: 0,
        skillbar_agent_id: 0,
        skillbar_skills: 0,
        skillbar_disabled: 0,
        skillbar_cast_count: 0,
        skill_stride: 0,
        skill_adrenaline_a: 0,
        skill_adrenaline_b: 0,
        skill_recharge: 0,
        skill_id: 0,
        skill_event: 0,
        world_party_effects: 0,
        agent_effects_stride: 0,
        agent_effects_agent_id: 0,
        agent_effects_buffs: 0,
        agent_effects_effects: 0,
        buff_stride: 0,
        buff_skill_id: 0,
        buff_id: 0,
        buff_target_agent_id: 0,
        effect_stride: 0,
        effect_skill_id: 0,
        effect_attribute_level: 0,
        effect_id: 0,
        effect_agent_id: 0,
        effect_duration: 0,
        effect_timestamp: 0,
        world_active_quest: 0,
        world_quest_log: 0,
        quest_stride: 0,
        quest_id: 0,
        quest_log_state: 0,
        quest_map_from: 0,
        quest_marker: 0,
        quest_map_to: 0,
        world_mission_objectives: 0,
        mission_objective_stride: 0,
        mission_objective_id: 0,
        mission_objective_type: 0,
        game_item_context: 0,
        item_context_inventory: 0,
        inventory_bags: 0,
        inventory_storage_panes: 0,
        inventory_gold_character: 0,
        inventory_gold_storage: 0,
        bag_type: 0,
        bag_index: 0,
        bag_container_item: 0,
        bag_items_count: 0,
        bag_items: 0,
        item_id: 0,
        item_agent_id: 0,
        item_bag: 0,
        item_modifiers: 0,
        item_modifier_count: 0,
        item_customized: 0,
        item_model_file_id: 0,
        item_type: 0,
        item_dye: 0,
        item_value: 0,
        item_interaction: 0,
        item_model_id: 0,
        item_formula: 0,
        item_material_salvageable: 0,
        item_quantity: 0,
        item_equipped: 0,
        item_profession: 0,
        item_slot: 0,
        cursor_active_art: 0,
        cursor_software_model: 0,
        cursor_show_count: 0,
        cursor_color_buffer: 0,
        cursor_art_hotspot: 0,
        cursor_art_texture: 0,
        cursor_handle_key: 0,
        cursor_handle_object: 0,
        cursor_view_texture: 0,
        cursor_texture_type: 0,
        cursor_texture_width: 0,
        cursor_texture_height: 0,
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PartyPlayer {
    login_number: u32,
    called_target_id: u32,
    state: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PartyHero {
    agent_id: u32,
    owner_player_id: u32,
    hero_id: u32,
    level: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PartyHenchman {
    agent_id: u32,
    profession: u32,
    level: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SkillSlot {
    adrenaline_a: u32,
    adrenaline_b: u32,
    recharge: u32,
    skill_id: u32,
    event: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PlayerBuff {
    skill_id: u32,
    buff_id: u32,
    target_agent_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PlayerEffect {
    skill_id: u32,
    attribute_level: u32,
    effect_id: u32,
    agent_id: u32,
    duration: f32,
    timestamp: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MapAgent {
    agent_id: u32,
    type_bits: u32,
    player_number: u32,
    primary: u32,
    secondary: u32,
    level: u32,
    health: f32,
    rotation: f32,
    x: f32,
    y: f32,
    z: f32,
    model_state: u32,
    effects: u32,
    allegiance: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Quest {
    quest_id: u32,
    log_state: u32,
    map_from: u32,
    marker_x: f32,
    marker_y: f32,
    marker_plane: u32,
    map_to: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MissionObjective {
    objective_id: u32,
    objective_type: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InventoryBag {
    bag_id: u32,
    bag_type: u32,
    container_item: u32,
    capacity: u32,
    item_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InventoryItem {
    item_id: u32,
    agent_id: u32,
    bag_id: u32,
    slot: u32,
    model_file_id: u32,
    item_type: u32,
    value: u32,
    interaction: u32,
    model_id: u32,
    item_formula: u32,
    quantity: u32,
    equipped: u32,
    profession: u32,
    metadata_flags: u32,
    modifier_count: u32,
    dye_info: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Snapshot {
    magic: u32,
    abi_and_size: u32,
    sequence: u32,
    flags: u32,
    tick_count: u32,
    map_id: u32,
    instance_type: u32,
    player_id: u32,
    player_x: f32,
    player_y: f32,
    target_id: u32,
    target_type: u32,
    target_x: f32,
    target_y: f32,
    distance: f32,
    range_band: u32,
    party_id: u32,
    party_flags: u32,
    party_player_count: u32,
    party_hero_count: u32,
    party_henchman_count: u32,
    party_ally_count: u32,
    party_players: [PartyPlayer; MAX_PARTY_PLAYERS],
    party_heroes: [PartyHero; MAX_PARTY_HEROES],
    party_henchmen: [PartyHenchman; MAX_PARTY_HENCHMEN],
    party_allies: [u32; MAX_PARTY_ALLIES],
    skillbar_agent_id: u32,
    skillbar_disabled_mask: u32,
    skillbar_cast_count: u32,
    skills: [SkillSlot; SKILL_SLOTS],
    effects_agent_id: u32,
    effects_flags: u32,
    buff_count: u32,
    effect_count: u32,
    buffs: [PlayerBuff; MAX_PLAYER_BUFFS],
    effects: [PlayerEffect; MAX_PLAYER_EFFECTS],
    map_agent_flags: u32,
    map_agent_count: u32,
    map_agent_total: u32,
    map_agents: [MapAgent; MAX_MAP_AGENTS],
    active_quest_id: u32,
    quest_flags: u32,
    quest_count: u32,
    mission_objective_count: u32,
    quests: [Quest; MAX_QUESTS],
    mission_objectives: [MissionObjective; MAX_MISSION_OBJECTIVES],
    inventory_flags: u32,
    gold_character: u32,
    gold_storage: u32,
    storage_panes_unlocked: u32,
    inventory_bag_count: u32,
    inventory_item_count: u32,
    inventory_item_total: u32,
    inventory_bags: [InventoryBag; MAX_INVENTORY_BAGS],
    inventory_items: [InventoryItem; MAX_INVENTORY_ITEMS],
}

// Separate bounded region: the cursor bitmap is far too large to live in the
// typed state snapshot.
#[repr(C)]
struct CursorSnapshot {
    magic: u32,
    abi_and_size: u32,
    sequence: u32,
    flags: u32,
    generation: u32,
    width: u32,
    height: u32,
    hotspot_x: u32,
    hotspot_y: u32,
    pixel_hash: u32,
    reserved: [u32; 6],
    pixels: [u32; 1024],
}

const _: [(); 516] = [(); size_of::<Layout>()];
const _: [(); 45284] = [(); size_of::<Snapshot>()];
const _: [(); 4160] = [(); size_of::<CursorSnapshot>()];

static mut SNAPSHOT_PTR: u32 = 0;
static mut LAYOUT: Layout = Layout::EMPTY;
static mut INITIALIZED: bool = false;
static mut FEATURES: u32 = 0;
static mut TICK_COUNT: u32 = 0;
static mut SEQUENCE: u32 = 0;
static mut CURSOR_PTR: u32 = 0;
static mut CURSOR_SEQUENCE: u32 = 0;
static mut CURSOR_GENERATION: u32 = 0;
static mut CURSOR_PUBLISHED: CursorPublished = CursorPublished::EMPTY;

#[link(wasm_import_module = "game")]
extern "C" {
    #[link_name = "enhancement_tick_original"]
    fn tick_original(context: u32);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

fn memory_bytes() -> u32 {
    core::arch::wasm32::memory_size(0)
        .saturating_mul(65_536)
        .min(u32::MAX as usize) as u32
}

fn checked_add(left: u32, right: u32) -> Option<u32> {
    left.checked_add(right)
}

fn checked_mul(left: u32, right: u32) -> Option<u32> {
    left.checked_mul(right)
}

fn offset(base: u32, field: u32) -> Option<u32> {
    checked_add(base, field)
}

fn indexed(base: u32, index: u32, stride: u32) -> Option<u32> {
    checked_add(base, checked_mul(index, stride)?)
}

fn contains(address: u32, bytes: u32) -> bool {
    checked_add(address, bytes).is_some_and(|end| end <= memory_bytes())
}

fn valid_region(enabled: bool, address: u32, bytes: u32, expected: u32) -> bool {
    if enabled {
        address != 0 && bytes == expected && address & 3 == 0 && contains(address, bytes)
    } else {
        address == 0 && bytes == 0
    }
}

unsafe fn read_u32(address: u32) -> Option<u32> {
    contains(address, 4).then(|| unsafe { read_volatile(address as *const u32) })
}

unsafe fn read_i32(address: u32) -> Option<i32> {
    contains(address, 4).then(|| unsafe { read_volatile(address as *const i32) })
}

unsafe fn read_u16(address: u32) -> Option<u16> {
    contains(address, 2).then(|| unsafe { read_volatile(address as *const u16) })
}

unsafe fn read_u8(address: u32) -> Option<u8> {
    contains(address, 1).then(|| unsafe { read_volatile(address as *const u8) })
}

unsafe fn read_f32(address: u32) -> Option<f32> {
    contains(address, 4).then(|| unsafe { read_volatile(address as *const f32) })
}

unsafe fn pointer(address: u32, required_bytes: u32) -> Option<u32> {
    let value = unsafe { read_u32(address)? };
    (value & 3 == 0 && contains(value, required_bytes)).then_some(value)
}

fn finite_position(value: f32) -> bool {
    value.is_finite() && value.abs() <= 1_000_000.0
}

fn valid_agent_type(value: u32) -> bool {
    value & (0x400 | 0x200 | 0xdb) != 0
}

fn square_root(value: f32) -> f32 {
    if value <= 0.0 {
        return 0.0;
    }
    let mut estimate = f32::from_bits((value.to_bits() >> 1) + 0x1fc0_0000);
    for _ in 0..5 {
        estimate = 0.5 * (estimate + value / estimate);
    }
    estimate
}

fn range_band(distance_squared: f32) -> u32 {
    const RANGES: [f32; 7] = [
        166.0 * 166.0,
        252.0 * 252.0,
        322.0 * 322.0,
        1_012.0 * 1_012.0,
        1_248.0 * 1_248.0,
        2_500.0 * 2_500.0,
        5_000.0 * 5_000.0,
    ];
    RANGES
        .iter()
        .position(|limit| distance_squared <= *limit)
        .map_or(8, |index| index as u32 + 1)
}

#[derive(Clone, Copy)]
struct AgentState {
    id: u32,
    kind: u32,
    x: f32,
    y: f32,
}

const EMPTY_PARTY_PLAYER: PartyPlayer = PartyPlayer {
    login_number: 0,
    called_target_id: 0,
    state: 0,
};
const EMPTY_PARTY_HERO: PartyHero = PartyHero {
    agent_id: 0,
    owner_player_id: 0,
    hero_id: 0,
    level: 0,
};
const EMPTY_PARTY_HENCHMAN: PartyHenchman = PartyHenchman {
    agent_id: 0,
    profession: 0,
    level: 0,
};
const EMPTY_SKILL_SLOT: SkillSlot = SkillSlot {
    adrenaline_a: 0,
    adrenaline_b: 0,
    recharge: 0,
    skill_id: 0,
    event: 0,
};
const EMPTY_PLAYER_BUFF: PlayerBuff = PlayerBuff {
    skill_id: 0,
    buff_id: 0,
    target_agent_id: 0,
};
const EMPTY_PLAYER_EFFECT: PlayerEffect = PlayerEffect {
    skill_id: 0,
    attribute_level: 0,
    effect_id: 0,
    agent_id: 0,
    duration: 0.0,
    timestamp: 0,
};
const EMPTY_MAP_AGENT: MapAgent = MapAgent {
    agent_id: 0,
    type_bits: 0,
    player_number: 0,
    primary: 0,
    secondary: 0,
    level: 0,
    health: 0.0,
    rotation: 0.0,
    x: 0.0,
    y: 0.0,
    z: 0.0,
    model_state: 0,
    effects: 0,
    allegiance: 0,
};
const EMPTY_QUEST: Quest = Quest {
    quest_id: 0,
    log_state: 0,
    map_from: 0,
    marker_x: 0.0,
    marker_y: 0.0,
    marker_plane: 0,
    map_to: 0,
};
const EMPTY_MISSION_OBJECTIVE: MissionObjective = MissionObjective {
    objective_id: 0,
    objective_type: 0,
};
const EMPTY_INVENTORY_BAG: InventoryBag = InventoryBag {
    bag_id: 0,
    bag_type: 0,
    container_item: 0,
    capacity: 0,
    item_count: 0,
};
const EMPTY_INVENTORY_ITEM: InventoryItem = InventoryItem {
    item_id: 0,
    agent_id: 0,
    bag_id: 0,
    slot: 0,
    model_file_id: 0,
    item_type: 0,
    value: 0,
    interaction: 0,
    model_id: 0,
    item_formula: 0,
    quantity: 0,
    equipped: 0,
    profession: 0,
    metadata_flags: 0,
    modifier_count: 0,
    dye_info: 0,
};
#[derive(Clone, Copy)]
struct PartyState {
    id: u32,
    flags: u32,
    player_count: u32,
    hero_count: u32,
    henchman_count: u32,
    ally_count: u32,
    players: [PartyPlayer; MAX_PARTY_PLAYERS],
    heroes: [PartyHero; MAX_PARTY_HEROES],
    henchmen: [PartyHenchman; MAX_PARTY_HENCHMEN],
    allies: [u32; MAX_PARTY_ALLIES],
}

impl PartyState {
    const EMPTY: Self = Self {
        id: 0,
        flags: 0,
        player_count: 0,
        hero_count: 0,
        henchman_count: 0,
        ally_count: 0,
        players: [EMPTY_PARTY_PLAYER; MAX_PARTY_PLAYERS],
        heroes: [EMPTY_PARTY_HERO; MAX_PARTY_HEROES],
        henchmen: [EMPTY_PARTY_HENCHMAN; MAX_PARTY_HENCHMEN],
        allies: [0; MAX_PARTY_ALLIES],
    };
}

#[derive(Clone, Copy)]
struct SkillbarState {
    agent_id: u32,
    disabled_mask: u32,
    cast_count: u32,
    skills: [SkillSlot; SKILL_SLOTS],
}

impl SkillbarState {
    const EMPTY: Self = Self {
        agent_id: 0,
        disabled_mask: 0,
        cast_count: 0,
        skills: [EMPTY_SKILL_SLOT; SKILL_SLOTS],
    };
}

// Keep only validated source spans here, never the full published page. This
// module imports the client's memory, including its fixed linker stack; putting
// 64 effects in `State` would widen the tick frame into more client-owned
// bytes. The game cannot mutate these arrays between the completed original
// tick and `publish`, so the bounded second read below stays coherent.
#[derive(Clone, Copy)]
struct EffectsSource {
    agent_id: u32,
    flags: u32,
    buff_count: u32,
    effect_count: u32,
    buff_buffer: u32,
    effect_buffer: u32,
}

impl EffectsSource {
    const EMPTY: Self = Self {
        agent_id: 0,
        flags: 0,
        buff_count: 0,
        effect_count: 0,
        buff_buffer: 0,
        effect_buffer: 0,
    };
}

// The map-agent page follows the same stack rule as effects: retain only the
// source descriptor, then validate and write each record directly during the
// seqlock publish after the original game tick has completed. A 128-record
// page must never become a local array on the client's fixed imported-memory
// stack.
#[derive(Clone, Copy)]
struct MapAgentsSource {
    buffer: u32,
    size: u32,
}

impl MapAgentsSource {
    const EMPTY: Self = Self {
        buffer: 0,
        size: 0,
    };
}

#[derive(Clone, Copy)]
struct QuestsSource {
    active_quest_id: u32,
    flags: u32,
    quest_count: u32,
    objective_count: u32,
    quest_buffer: u32,
    objective_buffer: u32,
}

impl QuestsSource {
    const EMPTY: Self = Self {
        active_quest_id: 0,
        flags: 0,
        quest_count: 0,
        objective_count: 0,
        quest_buffer: 0,
        objective_buffer: 0,
    };
}

// Inventory is a graph of bags and item pointers. Retain only the owning
// Inventory pointer across the original tick, then revalidate every bag,
// slot, back-reference, and item directly while publishing. Materialising
// 512 records as a local array would overflow the client's imported-memory
// stack and make the state API interfere with the game it observes.
#[derive(Clone, Copy)]
struct InventorySource {
    inventory: u32,
}

impl InventorySource {
    const EMPTY: Self = Self { inventory: 0 };
}

#[derive(Clone, Copy)]
struct State {
    flags: u32,
    map_id: u32,
    instance_type: u32,
    player: AgentState,
    target: AgentState,
    distance: f32,
    band: u32,
    party: PartyState,
    skillbar: SkillbarState,
    effects: EffectsSource,
    map_agents: MapAgentsSource,
    quests: QuestsSource,
    inventory: InventorySource,
}

impl State {
    const fn empty() -> Self {
        const EMPTY_AGENT: AgentState = AgentState {
            id: 0,
            kind: 0,
            x: 0.0,
            y: 0.0,
        };
        Self {
            flags: 0,
            map_id: 0,
            instance_type: 0,
            player: EMPTY_AGENT,
            target: EMPTY_AGENT,
            distance: 0.0,
            band: 0,
            party: PartyState::EMPTY,
            skillbar: SkillbarState::EMPTY,
            effects: EffectsSource::EMPTY,
            map_agents: MapAgentsSource::EMPTY,
            quests: QuestsSource::EMPTY,
            inventory: InventorySource::EMPTY,
        }
    }
}

unsafe fn read_agent(
    layout: Layout,
    agent_buffer: u32,
    size: u32,
    id: u32,
) -> Option<AgentState> {
    if id == 0 || id >= size {
        return None;
    }
    let entry = indexed(agent_buffer, id, 4)?;
    let address = unsafe { pointer(entry, 0x100)? };
    if unsafe { read_u32(offset(address, layout.agent_id)?)? } != id {
        return None;
    }
    let kind = unsafe { read_u32(offset(address, layout.agent_type)?)? };
    let x = unsafe { read_f32(offset(address, layout.agent_x)?)? };
    let y = unsafe { read_f32(offset(address, layout.agent_y)?)? };
    if !valid_agent_type(kind) || !finite_position(x) || !finite_position(y) {
        return None;
    }
    Some(AgentState { id, kind, x, y })
}

unsafe fn read_map_agent(layout: Layout, address: u32, id: u32) -> Option<MapAgent> {
    if unsafe { read_u32(offset(address, layout.agent_id)?)? } != id {
        return None;
    }
    let type_bits = unsafe { read_u32(offset(address, layout.agent_type)?)? };
    let x = unsafe { read_f32(offset(address, layout.agent_x)?)? };
    let y = unsafe { read_f32(offset(address, layout.agent_y)?)? };
    let z = unsafe { read_f32(offset(address, layout.agent_z)?)? };
    let rotation = unsafe { read_f32(offset(address, layout.agent_rotation)?)? };
    if !valid_agent_type(type_bits)
        || !finite_position(x)
        || !finite_position(y)
        || !finite_position(z)
        || !rotation.is_finite()
        || rotation.abs() > 4.0
    {
        return None;
    }

    let mut agent = MapAgent {
        agent_id: id,
        type_bits,
        rotation,
        x,
        y,
        z,
        ..EMPTY_MAP_AGENT
    };
    if type_bits & 0xdb == 0 {
        return Some(agent);
    }

    agent.player_number =
        unsafe { read_u16(offset(address, layout.agent_player_number)?)? } as u32;
    agent.primary = unsafe { read_u8(offset(address, layout.agent_primary)?)? } as u32;
    agent.secondary =
        unsafe { read_u8(offset(address, layout.agent_secondary)?)? } as u32;
    agent.level = unsafe { read_u8(offset(address, layout.agent_level)?)? } as u32;
    agent.health = unsafe { read_f32(offset(address, layout.agent_hp)?)? };
    agent.model_state = unsafe { read_u32(offset(address, layout.agent_model_state)?)? };
    agent.effects = unsafe { read_u32(offset(address, layout.agent_effects)?)? };
    agent.allegiance =
        unsafe { read_u8(offset(address, layout.agent_allegiance)?)? } as u32;
    if agent.primary > 10
        || agent.secondary > 10
        || !agent.health.is_finite()
        || !(-10.0..=10.0).contains(&agent.health)
        || agent.allegiance > 6
    {
        return None;
    }
    Some(agent)
}

#[derive(Clone, Copy)]
struct ArrayView {
    buffer: u32,
    size: u32,
}

unsafe fn read_array(
    address: u32,
    stride: u32,
    maximum_size: u32,
    maximum_capacity: u32,
) -> Option<ArrayView> {
    if stride == 0 || !contains(address, 16) {
        return None;
    }
    let buffer = unsafe { read_u32(address)? };
    let capacity = unsafe { read_u32(offset(address, 4)?)? };
    let size = unsafe { read_u32(offset(address, 8)?)? };
    if size > capacity || size > maximum_size || capacity > maximum_capacity {
        return None;
    }
    if size == 0 {
        return Some(ArrayView { buffer: 0, size: 0 });
    }
    let bytes = checked_mul(size, stride)?;
    (buffer & 3 == 0 && contains(buffer, bytes)).then_some(ArrayView { buffer, size })
}

unsafe fn read_quest(layout: Layout, buffer: u32, index: u32) -> Option<Quest> {
    let entry = indexed(buffer, index, layout.quest_stride)?;
    let marker = offset(entry, layout.quest_marker)?;
    let quest = Quest {
        quest_id: unsafe { read_u32(offset(entry, layout.quest_id)?)? },
        log_state: unsafe { read_u32(offset(entry, layout.quest_log_state)?)? },
        map_from: unsafe { read_u32(offset(entry, layout.quest_map_from)?)? },
        marker_x: unsafe { read_f32(marker)? },
        marker_y: unsafe { read_f32(offset(marker, 4)?)? },
        marker_plane: unsafe { read_u32(offset(marker, 8)?)? },
        map_to: unsafe { read_u32(offset(entry, layout.quest_map_to)?)? },
    };
    if quest.quest_id == 0
        || quest.quest_id > 100_000
        || quest.map_from > 2_000
        || quest.map_to > 2_000
        || !finite_position(quest.marker_x)
        || !finite_position(quest.marker_y)
        || quest.marker_plane > 100_000
    {
        return None;
    }
    Some(quest)
}

unsafe fn read_mission_objective(
    layout: Layout,
    buffer: u32,
    index: u32,
) -> Option<MissionObjective> {
    let entry = indexed(buffer, index, layout.mission_objective_stride)?;
    let objective = MissionObjective {
        objective_id: unsafe { read_u32(offset(entry, layout.mission_objective_id)?)? },
        objective_type: unsafe {
            read_u32(offset(entry, layout.mission_objective_type)?)?
        },
    };
    if objective.objective_id == 0 || objective.objective_type > 100_000 {
        return None;
    }
    Some(objective)
}

unsafe fn collect_quests(layout: Layout, game: u32) -> Option<QuestsSource> {
    let world = unsafe {
        pointer(
            offset(game, layout.game_world_context)?,
            layout.world_mission_objectives.checked_add(16)?,
        )?
    };
    let active_quest_id =
        unsafe { read_u32(offset(world, layout.world_active_quest)?)? };
    if active_quest_id > 100_000 {
        return None;
    }
    let quests = unsafe {
        read_array(
            offset(world, layout.world_quest_log)?,
            layout.quest_stride,
            MAX_RAW_QUESTS,
            1_024,
        )?
    };
    let objectives = unsafe {
        read_array(
            offset(world, layout.world_mission_objectives)?,
            layout.mission_objective_stride,
            MAX_RAW_MISSION_OBJECTIVES,
            512,
        )?
    };

    let mut active_found = active_quest_id == 0;
    for index in 0..quests.size {
        let quest = unsafe { read_quest(layout, quests.buffer, index)? };
        active_found |= quest.quest_id == active_quest_id;
    }
    if !active_found {
        return None;
    }
    let objective_count = objectives.size.min(MAX_MISSION_OBJECTIVES as u32);
    for index in 0..objective_count {
        unsafe { read_mission_objective(layout, objectives.buffer, index)? };
    }
    Some(QuestsSource {
        active_quest_id,
        flags: u32::from(quests.size > MAX_QUESTS as u32)
            | (u32::from(objectives.size > MAX_MISSION_OBJECTIVES as u32) << 1),
        quest_count: quests.size.min(MAX_QUESTS as u32),
        objective_count,
        quest_buffer: quests.buffer,
        objective_buffer: objectives.buffer,
    })
}

fn expected_bag_type(bag_id: u32) -> Option<u32> {
    match bag_id {
        1..=5 => Some(1),  // Inventory
        6 => Some(5),     // Material storage
        7 => Some(3),     // Not collected
        8..=21 => Some(4), // Storage
        22 => Some(2),    // Equipped
        _ => None,
    }
}

fn valid_item_type(item_type: u32) -> bool {
    matches!(
        item_type,
        0 | 2..=13
            | 15..=22
            | 24
            | 26
            | 27
            | 29..=36
            | 43..=45
            | 0xff
    )
}

unsafe fn read_inventory_bag(
    layout: Layout,
    address: u32,
    bag_id: u32,
) -> Option<(InventoryBag, ArrayView)> {
    let bag_type = unsafe { read_u32(offset(address, layout.bag_type)?)? };
    let index = unsafe { read_u32(offset(address, layout.bag_index)?)? };
    let container_item =
        unsafe { read_u32(offset(address, layout.bag_container_item)?)? };
    let item_count = unsafe { read_u32(offset(address, layout.bag_items_count)?)? };
    let items = unsafe {
        read_array(
            offset(address, layout.bag_items)?,
            4,
            MAX_BAG_SLOTS,
            MAX_BAG_SLOTS,
        )?
    };
    if expected_bag_type(bag_id) != Some(bag_type)
        || index != bag_id - 1
        || container_item > 1_000_000
        || item_count > items.size
    {
        return None;
    }
    Some((
        InventoryBag {
            bag_id,
            bag_type,
            container_item,
            capacity: items.size,
            item_count,
        },
        items,
    ))
}

unsafe fn read_inventory_item(
    layout: Layout,
    address: u32,
    bag_address: u32,
    bag_id: u32,
    slot: u32,
) -> Option<InventoryItem> {
    let item_id = unsafe { read_u32(offset(address, layout.item_id)?)? };
    let agent_id = unsafe { read_u32(offset(address, layout.item_agent_id)?)? };
    let item_bag = unsafe { read_u32(offset(address, layout.item_bag)?)? };
    let modifiers = unsafe { read_u32(offset(address, layout.item_modifiers)?)? };
    let modifier_count =
        unsafe { read_u32(offset(address, layout.item_modifier_count)?)? };
    let customized =
        unsafe { read_u32(offset(address, layout.item_customized)?)? } != 0;
    let model_file_id =
        unsafe { read_u32(offset(address, layout.item_model_file_id)?)? };
    let item_type = unsafe { read_u8(offset(address, layout.item_type)?)? } as u32;
    let dye_tint = unsafe { read_u8(offset(address, layout.item_dye)?)? } as u32;
    let dyes = unsafe { read_u16(offset(address, layout.item_dye.checked_add(1)?)?)? }
        as u32;
    let value = unsafe { read_u16(offset(address, layout.item_value)?)? } as u32;
    let interaction =
        unsafe { read_u32(offset(address, layout.item_interaction)?)? };
    let model_id = unsafe { read_u32(offset(address, layout.item_model_id)?)? };
    let item_formula =
        unsafe { read_u16(offset(address, layout.item_formula)?)? } as u32;
    let material_salvageable =
        unsafe { read_u8(offset(address, layout.item_material_salvageable)?)? }
            as u32;
    let quantity =
        unsafe { read_u16(offset(address, layout.item_quantity)?)? } as u32;
    let equipped =
        unsafe { read_u8(offset(address, layout.item_equipped)?)? } as u32;
    let profession =
        unsafe { read_u8(offset(address, layout.item_profession)?)? } as u32;
    let item_slot = unsafe { read_u8(offset(address, layout.item_slot)?)? } as u32;

    let modifier_bytes = modifier_count.checked_mul(4)?;
    if item_id == 0
        || item_id > 1_000_000
        || agent_id > 4_095
        || item_bag != bag_address
        || item_slot != slot
        || model_file_id == 0
        || !valid_item_type(item_type)
        || model_id == 0
        || quantity == 0
        || equipped > 1
        || (profession > 10 && profession != 0xff)
        || material_salvageable > 1
        || modifier_count > 64
        || (modifier_count > 0
            && (modifiers & 3 != 0 || !contains(modifiers, modifier_bytes)))
    {
        return None;
    }

    Some(InventoryItem {
        item_id,
        agent_id,
        bag_id,
        slot,
        model_file_id,
        item_type,
        value,
        interaction,
        model_id,
        item_formula,
        quantity,
        equipped,
        profession,
        metadata_flags: u32::from(customized) | (material_salvageable << 1),
        modifier_count,
        dye_info: dye_tint | (dyes << 8),
    })
}

unsafe fn collect_inventory(layout: Layout, game: u32) -> Option<InventorySource> {
    let item_context = unsafe {
        pointer(
            offset(game, layout.game_item_context)?,
            layout.item_context_inventory.checked_add(4)?,
        )?
    };
    let inventory = unsafe {
        pointer(
            offset(item_context, layout.item_context_inventory)?,
            layout.inventory_gold_storage.checked_add(4)?,
        )?
    };
    Some(InventorySource { inventory })
}

unsafe fn collect_party(layout: Layout, game: u32, agent_count: u32) -> Option<PartyState> {
    let party = unsafe {
        pointer(
            offset(game, layout.game_party_context)?,
            layout.party_player_party.checked_add(4)?,
        )?
    };
    let info = unsafe {
        pointer(
            offset(party, layout.party_player_party)?,
            layout.party_others.checked_add(16)?,
        )?
    };
    let players = unsafe {
        read_array(
            offset(info, layout.party_players)?,
            layout.party_player_stride,
            MAX_PARTY_PLAYERS as u32,
            64,
        )?
    };
    let heroes = unsafe {
        read_array(
            offset(info, layout.party_heroes)?,
            layout.party_hero_stride,
            MAX_PARTY_HEROES as u32,
            64,
        )?
    };
    let henchmen = unsafe {
        read_array(
            offset(info, layout.party_henchmen)?,
            layout.party_henchman_stride,
            MAX_PARTY_HENCHMEN as u32,
            64,
        )?
    };
    let others = unsafe {
        read_array(offset(info, layout.party_others)?, 4, 256, 1_024)?
    };
    if players.size == 0
        || players
            .size
            .checked_add(heroes.size)?
            .checked_add(henchmen.size)?
            > MAX_PARTY_PLAYERS as u32
    {
        return None;
    }

    let raw_flags = unsafe { read_u32(offset(party, layout.party_flag)?)? };
    let mut state = PartyState {
        id: unsafe { read_u32(offset(info, layout.party_id)?)? },
        flags: ((raw_flags & 0x10 != 0) as u32)
            | (((raw_flags & 0x20 != 0) as u32) << 1)
            | (((raw_flags & 0x80 != 0) as u32) << 2)
            | ((others.size > MAX_PARTY_ALLIES as u32) as u32) << 3,
        player_count: players.size,
        hero_count: heroes.size,
        henchman_count: henchmen.size,
        ally_count: others.size.min(MAX_PARTY_ALLIES as u32),
        ..PartyState::EMPTY
    };

    for index in 0..players.size {
        let entry = indexed(players.buffer, index, layout.party_player_stride)?;
        let login_number =
            unsafe { read_u32(offset(entry, layout.party_player_login_number)?)? };
        let called_target_id =
            unsafe { read_u32(offset(entry, layout.party_player_called_target_id)?)? };
        let member_state = unsafe { read_u32(offset(entry, layout.party_player_state)?)? };
        if login_number == 0 || called_target_id >= agent_count {
            return None;
        }
        state.players[index as usize] = PartyPlayer {
            login_number,
            called_target_id,
            state: member_state,
        };
    }

    for index in 0..heroes.size {
        let entry = indexed(heroes.buffer, index, layout.party_hero_stride)?;
        let hero = PartyHero {
            agent_id: unsafe { read_u32(offset(entry, layout.party_hero_agent_id)?)? },
            owner_player_id: unsafe {
                read_u32(offset(entry, layout.party_hero_owner_player_id)?)?
            },
            hero_id: unsafe { read_u32(offset(entry, layout.party_hero_id)?)? },
            level: unsafe { read_u32(offset(entry, layout.party_hero_level)?)? },
        };
        if hero.agent_id == 0
            || hero.agent_id >= agent_count
            || hero.owner_player_id == 0
            || !(0..players.size).any(|player_index| {
                state.players[player_index as usize].login_number == hero.owner_player_id
            })
            || hero.hero_id == 0
            || hero.hero_id > 1_000
            || !(1..=20).contains(&hero.level)
        {
            return None;
        }
        state.heroes[index as usize] = hero;
    }

    for index in 0..henchmen.size {
        let entry = indexed(henchmen.buffer, index, layout.party_henchman_stride)?;
        let henchman = PartyHenchman {
            agent_id: unsafe { read_u32(offset(entry, layout.party_henchman_agent_id)?)? },
            profession: unsafe {
                read_u32(offset(entry, layout.party_henchman_profession)?)?
            },
            level: unsafe { read_u32(offset(entry, layout.party_henchman_level)?)? },
        };
        if henchman.agent_id == 0
            || henchman.agent_id >= agent_count
            || henchman.profession > 10
            || !(1..=20).contains(&henchman.level)
        {
            return None;
        }
        state.henchmen[index as usize] = henchman;
    }

    for index in 0..state.ally_count {
        let agent_id = unsafe { read_u32(indexed(others.buffer, index, 4)?)? };
        if agent_id == 0 || agent_id >= agent_count {
            return None;
        }
        state.allies[index as usize] = agent_id;
    }
    Some(state)
}

unsafe fn collect_skillbar(layout: Layout, game: u32, player_id: u32) -> Option<SkillbarState> {
    let world = unsafe {
        pointer(
            offset(game, layout.game_world_context)?,
            layout.world_skillbar.checked_add(16)?,
        )?
    };
    let skillbars = unsafe {
        read_array(
            offset(world, layout.world_skillbar)?,
            layout.skillbar_stride,
            16,
            64,
        )?
    };
    for index in 0..skillbars.size {
        let bar = indexed(skillbars.buffer, index, layout.skillbar_stride)?;
        let agent_id = unsafe { read_u32(offset(bar, layout.skillbar_agent_id)?)? };
        if agent_id != player_id {
            continue;
        }
        let disabled_mask = unsafe { read_u32(offset(bar, layout.skillbar_disabled)?)? };
        let cast_count = unsafe { read_u32(offset(bar, layout.skillbar_cast_count)?)? };
        if disabled_mask & !0xff != 0 || cast_count > 64 {
            return None;
        }
        let mut state = SkillbarState {
            agent_id,
            disabled_mask,
            cast_count,
            ..SkillbarState::EMPTY
        };
        let skills = offset(bar, layout.skillbar_skills)?;
        for slot in 0..SKILL_SLOTS as u32 {
            let skill = indexed(skills, slot, layout.skill_stride)?;
            let value = SkillSlot {
                adrenaline_a: unsafe {
                    read_u32(offset(skill, layout.skill_adrenaline_a)?)?
                },
                adrenaline_b: unsafe {
                    read_u32(offset(skill, layout.skill_adrenaline_b)?)?
                },
                recharge: unsafe { read_u32(offset(skill, layout.skill_recharge)?)? },
                skill_id: unsafe { read_u32(offset(skill, layout.skill_id)?)? },
                event: unsafe { read_u32(offset(skill, layout.skill_event)?)? },
            };
            if value.skill_id > 100_000 {
                return None;
            }
            state.skills[slot as usize] = value;
        }
        return Some(state);
    }
    None
}

unsafe fn read_player_buff(layout: Layout, buffer: u32, index: u32) -> Option<PlayerBuff> {
    let entry = indexed(buffer, index, layout.buff_stride)?;
    Some(PlayerBuff {
        skill_id: unsafe { read_u32(offset(entry, layout.buff_skill_id)?)? },
        buff_id: unsafe { read_u32(offset(entry, layout.buff_id)?)? },
        target_agent_id: unsafe { read_u32(offset(entry, layout.buff_target_agent_id)?)? },
    })
}

unsafe fn read_player_effect(
    layout: Layout,
    buffer: u32,
    index: u32,
) -> Option<PlayerEffect> {
    let entry = indexed(buffer, index, layout.effect_stride)?;
    Some(PlayerEffect {
        skill_id: unsafe { read_u32(offset(entry, layout.effect_skill_id)?)? },
        attribute_level: unsafe {
            read_u32(offset(entry, layout.effect_attribute_level)?)?
        },
        effect_id: unsafe { read_u32(offset(entry, layout.effect_id)?)? },
        agent_id: unsafe { read_u32(offset(entry, layout.effect_agent_id)?)? },
        duration: unsafe { read_f32(offset(entry, layout.effect_duration)?)? },
        timestamp: unsafe { read_u32(offset(entry, layout.effect_timestamp)?)? },
    })
}

unsafe fn collect_effects(
    layout: Layout,
    game: u32,
    player_id: u32,
    agent_count: u32,
) -> Option<EffectsSource> {
    let world = unsafe {
        pointer(
            offset(game, layout.game_world_context)?,
            layout.world_party_effects.checked_add(16)?,
        )?
    };
    let agent_effects = unsafe {
        read_array(
            offset(world, layout.world_party_effects)?,
            layout.agent_effects_stride,
            MAX_EFFECT_AGENTS,
            256,
        )?
    };
    let mut player_entry = None;
    for index in 0..agent_effects.size {
        let entry = indexed(agent_effects.buffer, index, layout.agent_effects_stride)?;
        let agent_id = unsafe { read_u32(offset(entry, layout.agent_effects_agent_id)?)? };
        if agent_id == 0 || agent_id >= agent_count {
            return None;
        }
        if agent_id == player_id {
            if player_entry.is_some() {
                return None;
            }
            player_entry = Some(entry);
        }
    }

    let mut state = EffectsSource::EMPTY;
    state.agent_id = player_id;
    let Some(entry) = player_entry else {
        return Some(state);
    };
    let buffs = unsafe {
        read_array(
            offset(entry, layout.agent_effects_buffs)?,
            layout.buff_stride,
            MAX_RAW_BUFFS,
            1_024,
        )?
    };
    let effects = unsafe {
        read_array(
            offset(entry, layout.agent_effects_effects)?,
            layout.effect_stride,
            MAX_RAW_EFFECTS,
            1_024,
        )?
    };
    state.flags = u32::from(buffs.size > MAX_PLAYER_BUFFS as u32)
        | (u32::from(effects.size > MAX_PLAYER_EFFECTS as u32) << 1);
    state.buff_count = buffs.size.min(MAX_PLAYER_BUFFS as u32);
    state.effect_count = effects.size.min(MAX_PLAYER_EFFECTS as u32);
    state.buff_buffer = buffs.buffer;
    state.effect_buffer = effects.buffer;

    for index in 0..state.buff_count {
        let buff = unsafe { read_player_buff(layout, buffs.buffer, index)? };
        if buff.skill_id == 0
            || buff.skill_id > 100_000
            || buff.buff_id == 0
            || (buff.target_agent_id != 0 && buff.target_agent_id >= agent_count)
        {
            return None;
        }
    }

    for index in 0..state.effect_count {
        let effect = unsafe { read_player_effect(layout, effects.buffer, index)? };
        if effect.skill_id == 0
            || effect.skill_id > 100_000
            || effect.attribute_level > 100
            || effect.effect_id == 0
            || (effect.agent_id != 0 && effect.agent_id >= agent_count)
            || !effect.duration.is_finite()
            || effect.duration < 0.0
            || effect.duration > 1_000_000.0
        {
            return None;
        }
    }
    Some(state)
}

unsafe fn collect(layout: Layout) -> State {
    let mut state = State::empty();
    let contexts = match unsafe { pointer(layout.context_root, 28) } {
        Some(value) => value,
        None => return state,
    };
    let game_slot = match indexed(contexts, layout.game_context_slot, 4) {
        Some(value) => value,
        None => return state,
    };
    let game = match unsafe { pointer(game_slot, 0x50) } {
        Some(value) => value,
        None => return state,
    };
    let character = match offset(game, layout.character_context)
        .and_then(|address| unsafe { pointer(address, 0x2b0) })
    {
        Some(value) => value,
        None => return state,
    };

    let read_character = |field| {
        offset(character, field).and_then(|address| unsafe { read_u32(address) })
    };
    let Some(base_map) = read_character(layout.map_id) else {
        return state;
    };
    let Some(is_explorable) = read_character(layout.is_explorable) else {
        return state;
    };
    let Some(map_id) = read_character(layout.current_map_id) else {
        return state;
    };
    let Some(instance_type) = read_character(layout.current_instance_type) else {
        return state;
    };
    let Some(player_number) = read_character(layout.player_number) else {
        return state;
    };
    if instance_type == 2 {
        state.flags = FLAG_LOADING;
        return state;
    }
    if map_id == 0
        || map_id > 2_000
        || base_map != map_id
        || instance_type > 1
        || is_explorable != u32::from(instance_type == 1)
        || player_number == 0
        || player_number > u16::MAX as u32
    {
        return state;
    }

    if !contains(layout.agent_array, 16) {
        return state;
    }
    let Some(agent_buffer) = (unsafe { read_u32(layout.agent_array) }) else {
        return state;
    };
    let Some(capacity_address) = offset(layout.agent_array, 4) else {
        return state;
    };
    let Some(size_address) = offset(layout.agent_array, 8) else {
        return state;
    };
    let Some(capacity) = (unsafe { read_u32(capacity_address) }) else {
        return state;
    };
    let Some(size) = (unsafe { read_u32(size_address) }) else {
        return state;
    };
    let Some(agent_bytes) = checked_mul(size, 4) else {
        return state;
    };
    if size == 0
        || size > capacity
        || capacity > 4_096
        || !contains(agent_buffer, agent_bytes)
    {
        return state;
    }

    for id in 1..size {
        let Some(entry) = indexed(agent_buffer, id, 4) else {
            return state;
        };
        let Some(agent_address) = (unsafe { pointer(entry, 0x100) }) else {
            continue;
        };
        let Some(id_address) = offset(agent_address, layout.agent_id) else {
            return state;
        };
        let Some(player_number_address) =
            offset(agent_address, layout.agent_player_number)
        else {
            return state;
        };
        let Some(model_type_address) = offset(agent_address, layout.agent_model_type)
        else {
            return state;
        };
        if unsafe { read_u32(id_address) } != Some(id)
            || unsafe { read_u16(player_number_address) } != Some(player_number as u16)
            || unsafe { read_u16(model_type_address) }.map(|value| value & 0xf000)
                != Some(0x3000)
        {
            continue;
        }
        let Some(player) = (unsafe { read_agent(layout, agent_buffer, size, id) })
        else {
            return state;
        };
        state.flags = FLAG_READY | FLAG_PLAYER_VALID;
        state.map_id = map_id;
        state.instance_type = instance_type;
        state.player = player;
        break;
    }
    if state.flags & FLAG_PLAYER_VALID == 0 {
        return State::empty();
    }

    if layout.manual_target_agent_id != 0
        && layout.automatic_target_agent_id != 0
    {
        let target_id = unsafe { read_u32(layout.manual_target_agent_id) }
            .filter(|id| *id != 0)
            .or_else(|| unsafe { read_u32(layout.automatic_target_agent_id) });
        if let Some(target_id) = target_id {
            if let Some(target) =
                unsafe { read_agent(layout, agent_buffer, size, target_id) }
            {
                let dx = target.x - state.player.x;
                let dy = target.y - state.player.y;
                let distance_squared = dx * dx + dy * dy;
                if distance_squared.is_finite() {
                    state.flags |= FLAG_TARGET_VALID;
                    state.target = target;
                    state.distance = square_root(distance_squared);
                    state.band = range_band(distance_squared);
                }
            }
        }
    }
    if let Some(party) = unsafe { collect_party(layout, game, size) } {
        state.flags |= FLAG_PARTY_VALID;
        state.party = party;
    }
    if let Some(skillbar) = unsafe { collect_skillbar(layout, game, state.player.id) } {
        state.flags |= FLAG_SKILLBAR_VALID;
        state.skillbar = skillbar;
    }
    if let Some(effects) =
        unsafe { collect_effects(layout, game, state.player.id, size) }
    {
        state.flags |= FLAG_EFFECTS_VALID;
        state.effects = effects;
    }
    state.flags |= FLAG_MAP_AGENTS_VALID;
    state.map_agents = MapAgentsSource {
        buffer: agent_buffer,
        size,
    };
    if let Some(quests) = unsafe { collect_quests(layout, game) } {
        state.flags |= FLAG_QUESTS_VALID;
        state.quests = quests;
    }
    if let Some(inventory) = unsafe { collect_inventory(layout, game) } {
        state.flags |= FLAG_INVENTORY_VALID;
        state.inventory = inventory;
    }
    state
}

unsafe fn publish_map_agents(
    snapshot: *mut Snapshot,
    layout: Layout,
    source: MapAgentsSource,
) -> bool {
    let mut count = 0usize;
    let mut total = 0u32;
    let mut valid = source.buffer != 0 && source.size > 1 && source.size <= 4_096;
    if valid {
        for id in 1..source.size {
            let value = indexed(source.buffer, id, 4)
                .and_then(|entry| unsafe { read_u32(entry) });
            let Some(address) = value else {
                valid = false;
                break;
            };
            if address == 0 {
                continue;
            }
            let record = if address & 3 == 0 && contains(address, 0xa0) {
                unsafe { read_map_agent(layout, address, id) }
            } else {
                None
            };
            let Some(record) = record else {
                valid = false;
                break;
            };
            total = match total.checked_add(1) {
                Some(value) => value,
                None => {
                    valid = false;
                    break;
                }
            };
            if count < MAX_MAP_AGENTS {
                unsafe {
                    write_volatile(&mut (*snapshot).map_agents[count], record);
                }
                count += 1;
            }
        }
    }
    valid &= total > 0;
    if !valid {
        count = 0;
        total = 0;
    }
    for index in count..MAX_MAP_AGENTS {
        unsafe {
            write_volatile(&mut (*snapshot).map_agents[index], EMPTY_MAP_AGENT);
        }
    }
    unsafe {
        write_volatile(
            &mut (*snapshot).map_agent_flags,
            u32::from(valid && total > count as u32),
        );
        write_volatile(
            &mut (*snapshot).map_agent_count,
            if valid { count as u32 } else { 0 },
        );
        write_volatile(&mut (*snapshot).map_agent_total, total);
    }
    valid
}

unsafe fn publish_inventory(
    snapshot: *mut Snapshot,
    layout: Layout,
    source: InventorySource,
) -> bool {
    let inventory_bytes = layout.inventory_gold_storage.checked_add(4);
    let mut valid = source.inventory != 0
        && source.inventory & 3 == 0
        && inventory_bytes.is_some_and(|bytes| contains(source.inventory, bytes));
    let mut gold_character = 0;
    let mut gold_storage = 0;
    let mut storage_panes_unlocked = 0;
    let mut bag_count = 0usize;
    let mut item_count = 0usize;
    let mut item_total = 0u32;
    let mut total_slots = 0u32;
    let mut backpack_found = false;

    if valid {
        let character = offset(source.inventory, layout.inventory_gold_character)
            .and_then(|address| unsafe { read_u32(address) });
        let storage = offset(source.inventory, layout.inventory_gold_storage)
            .and_then(|address| unsafe { read_u32(address) });
        let panes = offset(source.inventory, layout.inventory_storage_panes)
            .and_then(|address| unsafe { read_u32(address) });
        if let (Some(character), Some(storage), Some(panes)) =
            (character, storage, panes)
        {
            gold_character = character;
            gold_storage = storage;
            storage_panes_unlocked = panes;
            valid &= storage_panes_unlocked <= 14;
        } else {
            valid = false;
        }
    }

    if valid {
        for bag_id in 1..=MAX_INVENTORY_BAGS as u32 {
            let Some(pointer_offset) = bag_id
                .checked_mul(4)
                .and_then(|value| layout.inventory_bags.checked_add(value))
                .and_then(|value| offset(source.inventory, value))
            else {
                valid = false;
                break;
            };
            let Some(bag_address) = (unsafe { read_u32(pointer_offset) }) else {
                valid = false;
                break;
            };
            if bag_address == 0 {
                continue;
            }
            if bag_address & 3 != 0 {
                valid = false;
                break;
            }
            let Some((bag, items)) =
                (unsafe { read_inventory_bag(layout, bag_address, bag_id) })
            else {
                valid = false;
                break;
            };
            let Some(next_slots) = total_slots.checked_add(bag.capacity) else {
                valid = false;
                break;
            };
            if next_slots > MAX_TOTAL_BAG_SLOTS {
                valid = false;
                break;
            }
            total_slots = next_slots;
            backpack_found |= bag_id == 1;
            unsafe {
                write_volatile(&mut (*snapshot).inventory_bags[bag_count], bag);
            }
            bag_count += 1;

            let mut actual_count = 0u32;
            for slot in 0..items.size {
                let Some(entry) = indexed(items.buffer, slot, 4) else {
                    valid = false;
                    break;
                };
                let Some(item_address) = (unsafe { read_u32(entry) }) else {
                    valid = false;
                    break;
                };
                if item_address == 0 {
                    continue;
                }
                if item_address & 3 != 0 {
                    valid = false;
                    break;
                }
                let Some(item) = (unsafe {
                    read_inventory_item(
                        layout,
                        item_address,
                        bag_address,
                        bag_id,
                        slot,
                    )
                }) else {
                    valid = false;
                    break;
                };
                actual_count += 1;
                let Some(next_total) = item_total.checked_add(1) else {
                    valid = false;
                    break;
                };
                item_total = next_total;
                if item_count < MAX_INVENTORY_ITEMS {
                    unsafe {
                        write_volatile(
                            &mut (*snapshot).inventory_items[item_count],
                            item,
                        );
                    }
                    item_count += 1;
                }
            }
            if !valid || actual_count != bag.item_count {
                valid = false;
                break;
            }
        }
    }
    valid &= backpack_found && bag_count > 0;
    if !valid {
        gold_character = 0;
        gold_storage = 0;
        storage_panes_unlocked = 0;
        bag_count = 0;
        item_count = 0;
        item_total = 0;
    }
    for index in bag_count..MAX_INVENTORY_BAGS {
        unsafe {
            write_volatile(
                &mut (*snapshot).inventory_bags[index],
                EMPTY_INVENTORY_BAG,
            );
        }
    }
    for index in item_count..MAX_INVENTORY_ITEMS {
        unsafe {
            write_volatile(
                &mut (*snapshot).inventory_items[index],
                EMPTY_INVENTORY_ITEM,
            );
        }
    }
    unsafe {
        write_volatile(
            &mut (*snapshot).inventory_flags,
            u32::from(valid && item_total > item_count as u32),
        );
        write_volatile(&mut (*snapshot).gold_character, gold_character);
        write_volatile(&mut (*snapshot).gold_storage, gold_storage);
        write_volatile(
            &mut (*snapshot).storage_panes_unlocked,
            storage_panes_unlocked,
        );
        write_volatile(
            &mut (*snapshot).inventory_bag_count,
            if valid { bag_count as u32 } else { 0 },
        );
        write_volatile(
            &mut (*snapshot).inventory_item_count,
            if valid { item_count as u32 } else { 0 },
        );
        write_volatile(&mut (*snapshot).inventory_item_total, item_total);
    }
    valid
}

unsafe fn publish(mut state: State, layout: Layout) {
    let next = unsafe { SEQUENCE }.wrapping_add(2) & !1;
    let snapshot = unsafe { SNAPSHOT_PTR as *mut Snapshot };
    unsafe {
        write_volatile(&mut (*snapshot).sequence, next.wrapping_sub(1));
        write_volatile(&mut (*snapshot).magic, MAGIC);
        write_volatile(&mut (*snapshot).abi_and_size, ABI_AND_SIZE);
        write_volatile(&mut (*snapshot).tick_count, TICK_COUNT);
        write_volatile(&mut (*snapshot).map_id, state.map_id);
        write_volatile(&mut (*snapshot).instance_type, state.instance_type);
        write_volatile(&mut (*snapshot).player_id, state.player.id);
        write_volatile(&mut (*snapshot).player_x, state.player.x);
        write_volatile(&mut (*snapshot).player_y, state.player.y);
        write_volatile(&mut (*snapshot).target_id, state.target.id);
        write_volatile(&mut (*snapshot).target_type, state.target.kind);
        write_volatile(&mut (*snapshot).target_x, state.target.x);
        write_volatile(&mut (*snapshot).target_y, state.target.y);
        write_volatile(&mut (*snapshot).distance, state.distance);
        write_volatile(&mut (*snapshot).range_band, state.band);
        write_volatile(&mut (*snapshot).party_id, state.party.id);
        write_volatile(&mut (*snapshot).party_flags, state.party.flags);
        write_volatile(
            &mut (*snapshot).party_player_count,
            state.party.player_count,
        );
        write_volatile(&mut (*snapshot).party_hero_count, state.party.hero_count);
        write_volatile(
            &mut (*snapshot).party_henchman_count,
            state.party.henchman_count,
        );
        write_volatile(&mut (*snapshot).party_ally_count, state.party.ally_count);
        for index in 0..MAX_PARTY_PLAYERS {
            write_volatile(
                &mut (*snapshot).party_players[index],
                state.party.players[index],
            );
        }
        for index in 0..MAX_PARTY_HEROES {
            write_volatile(
                &mut (*snapshot).party_heroes[index],
                state.party.heroes[index],
            );
        }
        for index in 0..MAX_PARTY_HENCHMEN {
            write_volatile(
                &mut (*snapshot).party_henchmen[index],
                state.party.henchmen[index],
            );
        }
        for index in 0..MAX_PARTY_ALLIES {
            write_volatile(
                &mut (*snapshot).party_allies[index],
                state.party.allies[index],
            );
        }
        write_volatile(
            &mut (*snapshot).skillbar_agent_id,
            state.skillbar.agent_id,
        );
        write_volatile(
            &mut (*snapshot).skillbar_disabled_mask,
            state.skillbar.disabled_mask,
        );
        write_volatile(
            &mut (*snapshot).skillbar_cast_count,
            state.skillbar.cast_count,
        );
        for index in 0..SKILL_SLOTS {
            write_volatile(&mut (*snapshot).skills[index], state.skillbar.skills[index]);
        }
        write_volatile(
            &mut (*snapshot).effects_agent_id,
            state.effects.agent_id,
        );
        write_volatile(&mut (*snapshot).effects_flags, state.effects.flags);
        write_volatile(&mut (*snapshot).buff_count, state.effects.buff_count);
        write_volatile(
            &mut (*snapshot).effect_count,
            state.effects.effect_count,
        );
        for index in 0..MAX_PLAYER_BUFFS {
            let value = if index < state.effects.buff_count as usize {
                read_player_buff(layout, state.effects.buff_buffer, index as u32)
                    .unwrap_or(EMPTY_PLAYER_BUFF)
            } else {
                EMPTY_PLAYER_BUFF
            };
            write_volatile(&mut (*snapshot).buffs[index], value);
        }
        for index in 0..MAX_PLAYER_EFFECTS {
            let value = if index < state.effects.effect_count as usize {
                read_player_effect(layout, state.effects.effect_buffer, index as u32)
                    .unwrap_or(EMPTY_PLAYER_EFFECT)
            } else {
                EMPTY_PLAYER_EFFECT
            };
            write_volatile(&mut (*snapshot).effects[index], value);
        }
        if !publish_map_agents(snapshot, layout, state.map_agents) {
            state.flags &= !FLAG_MAP_AGENTS_VALID;
        }
        write_volatile(
            &mut (*snapshot).active_quest_id,
            state.quests.active_quest_id,
        );
        write_volatile(&mut (*snapshot).quest_flags, state.quests.flags);
        write_volatile(&mut (*snapshot).quest_count, state.quests.quest_count);
        write_volatile(
            &mut (*snapshot).mission_objective_count,
            state.quests.objective_count,
        );
        for index in 0..MAX_QUESTS {
            let value = if index < state.quests.quest_count as usize {
                read_quest(layout, state.quests.quest_buffer, index as u32)
                    .unwrap_or(EMPTY_QUEST)
            } else {
                EMPTY_QUEST
            };
            write_volatile(&mut (*snapshot).quests[index], value);
        }
        for index in 0..MAX_MISSION_OBJECTIVES {
            let value = if index < state.quests.objective_count as usize {
                read_mission_objective(
                    layout,
                    state.quests.objective_buffer,
                    index as u32,
                )
                .unwrap_or(EMPTY_MISSION_OBJECTIVE)
            } else {
                EMPTY_MISSION_OBJECTIVE
            };
            write_volatile(&mut (*snapshot).mission_objectives[index], value);
        }
        if !publish_inventory(snapshot, layout, state.inventory) {
            state.flags &= !FLAG_INVENTORY_VALID;
        }
        write_volatile(&mut (*snapshot).flags, state.flags);
        write_volatile(&mut (*snapshot).sequence, next);
        SEQUENCE = next;
    }
}

#[derive(Clone, Copy)]
struct CursorState {
    hash: u32,
    hotspot_x: u32,
    hotspot_y: u32,
    hidden: bool,
    source: u32,
}

// The published identity. The active art pointer is not stable across cursor
// changes, so the pixel hash is the only usable change key.
#[derive(Clone, Copy, PartialEq)]
struct CursorPublished {
    flags: u32,
    hash: u32,
    hotspot_x: u32,
    hotspot_y: u32,
}

impl CursorPublished {
    const EMPTY: Self = Self {
        flags: 0,
        hash: 0,
        hotspot_x: 0,
        hotspot_y: 0,
    };
}

// FNV-1a over the source BGRA words, so an unchanged cursor costs one pass and
// no conversion. None means unreadable or never committed by the game.
unsafe fn hash_cursor_pixels(source: u32) -> Option<u32> {
    let mut hash: u32 = 0x811c_9dc5;
    let mut committed: u32 = 0;
    for index in 0..CURSOR_WORDS {
        let word = unsafe { read_u32(indexed(source, index, 4)?)? };
        hash = (hash ^ word).wrapping_mul(0x0100_0193);
        committed |= word;
    }
    (committed != 0).then_some(hash)
}

// The readback that fills the colour buffer uses a hard-coded pitch, so a
// source texture that is not 32x32 would have misfilled it.
unsafe fn read_cursor(layout: Layout) -> Option<CursorState> {
    let art = unsafe { pointer(layout.cursor_active_art, 24)? };
    let handle = unsafe { pointer(offset(art, layout.cursor_art_texture)?, 12)? };
    if unsafe { read_u32(offset(handle, layout.cursor_handle_key)?)? }
        != CURSOR_TEXTURE_KEY
    {
        return None;
    }
    let view = unsafe { pointer(offset(handle, layout.cursor_handle_object)?, 12)? };
    let texture = unsafe { pointer(offset(view, layout.cursor_view_texture)?, 0x68)? };
    if unsafe { read_u32(offset(texture, layout.cursor_texture_type)?)? }
        != CURSOR_TEXTURE_TYPE
        || unsafe { read_u32(offset(texture, layout.cursor_texture_width)?)? }
            != CURSOR_EDGE
        || unsafe { read_u32(offset(texture, layout.cursor_texture_height)?)? }
            != CURSOR_EDGE
    {
        return None;
    }

    let hotspot = offset(art, layout.cursor_art_hotspot)?;
    let hotspot_x = unsafe { read_u32(hotspot)? };
    let hotspot_y = unsafe { read_u32(offset(hotspot, 4)?)? };
    if hotspot_x >= CURSOR_EDGE || hotspot_y >= CURSOR_EDGE {
        return None;
    }

    let source = layout.cursor_color_buffer;
    if !contains(source, CURSOR_PIXEL_BYTES) {
        return None;
    }
    let hash = unsafe { hash_cursor_pixels(source)? };
    let hidden = unsafe { read_i32(layout.cursor_show_count) }
        .is_some_and(|count| count < 0);
    Some(CursorState {
        hash,
        hotspot_x,
        hotspot_y,
        hidden,
        source,
    })
}

unsafe fn collect_cursor(layout: Layout) -> Result<CursorState, u32> {
    if unsafe { read_u32(layout.cursor_software_model) } != Some(0) {
        return Err(FLAG_CURSOR_UNSUPPORTED);
    }
    unsafe { read_cursor(layout) }.ok_or(0)
}

// `source` is None for a header-only update: it clears CURSOR_VALID without
// disturbing the last good pixels.
unsafe fn publish_cursor(published: CursorPublished, source: Option<u32>) {
    let next = unsafe { CURSOR_SEQUENCE }.wrapping_add(2) & !1;
    let cursor = unsafe { CURSOR_PTR as *mut CursorSnapshot };
    unsafe {
        write_volatile(&mut (*cursor).sequence, next.wrapping_sub(1));
        write_volatile(&mut (*cursor).magic, CURSOR_MAGIC);
        write_volatile(&mut (*cursor).abi_and_size, CURSOR_ABI_AND_SIZE);
        write_volatile(&mut (*cursor).flags, published.flags);
    }
    if let Some(source) = source {
        unsafe {
            CURSOR_GENERATION = CURSOR_GENERATION.wrapping_add(1);
            write_volatile(&mut (*cursor).generation, CURSOR_GENERATION);
            write_volatile(&mut (*cursor).width, CURSOR_EDGE);
            write_volatile(&mut (*cursor).height, CURSOR_EDGE);
            write_volatile(&mut (*cursor).hotspot_x, published.hotspot_x);
            write_volatile(&mut (*cursor).hotspot_y, published.hotspot_y);
            write_volatile(&mut (*cursor).pixel_hash, published.hash);
        }
        for index in 0..CURSOR_WORDS {
            let word = indexed(source, index, 4)
                .and_then(|address| unsafe { read_u32(address) })
                .unwrap_or(0);
            // BGRA -> RGBA: keep alpha and green, swap red and blue.
            let rgba =
                (word & 0xff00_ff00) | ((word >> 16) & 0xff) | ((word & 0xff) << 16);
            unsafe { write_volatile(&mut (*cursor).pixels[index as usize], rgba) };
        }
    }
    unsafe {
        write_volatile(&mut (*cursor).sequence, next);
        CURSOR_SEQUENCE = next;
        CURSOR_PUBLISHED = published;
    }
}

unsafe fn tick_cursor(layout: Layout) {
    let last = unsafe { CURSOR_PUBLISHED };
    match unsafe { collect_cursor(layout) } {
        Ok(state) => {
            let published = CursorPublished {
                flags: FLAG_CURSOR_VALID
                    | if state.hidden { FLAG_CURSOR_HIDDEN } else { 0 },
                hash: state.hash,
                hotspot_x: state.hotspot_x,
                hotspot_y: state.hotspot_y,
            };
            if published != last {
                // Show/hide moves the flags alone, and the region already holds
                // the bitmap `published.hash` names, so skip the 4 KB rewrite.
                let bitmap = last.flags & FLAG_CURSOR_VALID == 0
                    || published.hash != last.hash
                    || published.hotspot_x != last.hotspot_x
                    || published.hotspot_y != last.hotspot_y;
                unsafe { publish_cursor(published, bitmap.then_some(state.source)) };
            }
        }
        Err(flags) => {
            if flags != last.flags {
                unsafe { publish_cursor(CursorPublished { flags, ..last }, None) };
            }
        }
    }
}

// The region comes from the game's allocator, so clear it before the renderer
// can observe it.
unsafe fn clear_cursor() {
    let cursor = unsafe { CURSOR_PTR as *mut CursorSnapshot };
    unsafe {
        write_volatile(&mut (*cursor).generation, 0);
        write_volatile(&mut (*cursor).width, 0);
        write_volatile(&mut (*cursor).height, 0);
        write_volatile(&mut (*cursor).hotspot_x, 0);
        write_volatile(&mut (*cursor).hotspot_y, 0);
        write_volatile(&mut (*cursor).pixel_hash, 0);
    }
    for index in 0..6 {
        unsafe { write_volatile(&mut (*cursor).reserved[index], 0) };
    }
    for index in 0..CURSOR_WORDS {
        unsafe { write_volatile(&mut (*cursor).pixels[index as usize], 0) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn companion_init(
    snapshot_ptr: u32,
    snapshot_size: u32,
    config_ptr: u32,
    config_size: u32,
    cursor_ptr: u32,
    cursor_size: u32,
    features: u32,
) -> u32 {
    if features == 0
        || features & !KNOWN_FEATURES != 0
        || config_size != CONFIG_BYTES
        || config_ptr & 3 != 0
        || !contains(config_ptr, config_size)
        || !valid_region(
            features & FEATURE_TARGET_READOUT != 0,
            snapshot_ptr,
            snapshot_size,
            SNAPSHOT_BYTES,
        )
        || !valid_region(
            features & FEATURE_NATIVE_CURSOR != 0,
            cursor_ptr,
            cursor_size,
            CURSOR_BYTES,
        )
    {
        return 0;
    }
    unsafe {
        SNAPSHOT_PTR = snapshot_ptr;
        CURSOR_PTR = cursor_ptr;
        LAYOUT = read_volatile(config_ptr as *const Layout);
        FEATURES = features;
        INITIALIZED = true;
        TICK_COUNT = 0;
        SEQUENCE = 0;
        CURSOR_SEQUENCE = 0;
        CURSOR_GENERATION = 0;
        CURSOR_PUBLISHED = CursorPublished::EMPTY;
        if features & FEATURE_TARGET_READOUT != 0 {
            publish(State::empty(), LAYOUT);
        }
        if features & FEATURE_NATIVE_CURSOR != 0 {
            clear_cursor();
            publish_cursor(CursorPublished::EMPTY, None);
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn companion_tick(context: u32) {
    unsafe {
        tick_original(context);
        if !INITIALIZED {
            return;
        }
        if FEATURES & FEATURE_TARGET_READOUT != 0 {
            TICK_COUNT = TICK_COUNT.wrapping_add(1);
            publish(collect(LAYOUT), LAYOUT);
        }
        if FEATURES & FEATURE_NATIVE_CURSOR != 0 {
            tick_cursor(LAYOUT);
        }
    }
}
