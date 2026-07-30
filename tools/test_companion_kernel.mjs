// Kernel-to-public-boundary fixture.
//
// This is not a second implementation of the companion. It instantiates the
// exact no_std WASM produced by build.rs over a small deterministic memory
// image, executes one real companion tick, and hands the published bytes to the
// production decoder.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

import {
  COMPANION_SNAPSHOT_BYTES,
  readCompanionSnapshot,
} from '../web/companion-snapshot.js';

const kernelPath = process.env.GWNATIVE_COMPANION_KERNEL;
assert(kernelPath, 'GWNATIVE_COMPANION_KERNEL is not set');

const memory = new WebAssembly.Memory({ initial: 32 });
const view = new DataView(memory.buffer);
const u32 = (address, value) => view.setUint32(address, value, true);
const u16 = (address, value) => view.setUint16(address, value, true);
const u8 = (address, value) => view.setUint8(address, value);
const f32 = (address, value) => view.setFloat32(address, value, true);

const contextRoot = 0x140000;
const contexts = 0x141000;
const game = 0x142000;
const character = 0x143000;
const agentArray = 0x144000;
const agentPointers = 0x145000;
const player = 0x146000;
const item = 0x147000;
const world = 0x150000;
const questLog = 0x152000;
const objectives = 0x153000;
const itemContext = 0x154000;
const inventory = 0x155000;
const backpack = 0x156000;
const backpackItems = 0x157000;
const inventoryItem = 0x158000;
const itemModifiers = 0x159000;
const snapshot = 0x160000;
const config = 0x164000;

// Context and current-map invariants.
u32(contextRoot, contexts);
u32(contexts + 6 * 4, game);
u32(game + 0x44, character);
u32(game + 0x2c, world);
u32(game + 0x40, itemContext);
u32(character + 0x198, 55);
u32(character + 0x19c, 1);
u32(character + 0x234, 55);
u32(character + 0x23c, 1);
u32(character + 0x2ac, 7);

// AgentArray: player plus one non-living item.
u32(agentArray, agentPointers);
u32(agentArray + 4, 4);
u32(agentArray + 8, 3);
u32(agentPointers + 4, player);
u32(agentPointers + 8, item);

const baseAgent = (address, id, typeBits, x, y, z, rotation) => {
  u32(address + 0x2c, id);
  f32(address + 0x30, z);
  f32(address + 0x4c, rotation);
  f32(address + 0x74, x);
  f32(address + 0x78, y);
  u32(address + 0x9c, typeBits);
};
baseAgent(player, 1, 0xdb, 100, -250.5, 3, 1.25);
u16(player + 0xf4, 7);
u16(player + 0xf6, 0x3000);
u8(player + 0x10e, 7);
u8(player + 0x10f, 0);
u8(player + 0x110, 20);
f32(player + 0x134, 0.75);
u32(player + 0x158, 65);
u32(player + 0x13c, 0);
u8(player + 0x1b5, 1);
baseAgent(item, 2, 0x400, 120, -230, 2, 0);

// Empty skillbar/effect arrays are valid certified readings.
u32(world + 0x6f0, 0);
u32(world + 0x6f4, 0);
u32(world + 0x6f8, 0);
u32(world + 0x508, 0);
u32(world + 0x50c, 0);
u32(world + 0x510, 0);

// One active quest and one mission objective.
u32(world + 0x528, 44);
u32(world + 0x52c, questLog);
u32(world + 0x530, 1);
u32(world + 0x534, 1);
u32(questLog, 44);
u32(questLog + 4, 0x22);
u32(questLog + 0x14, 55);
f32(questLog + 0x18, 10);
f32(questLog + 0x1c, 20);
u32(questLog + 0x20, 3);
u32(questLog + 0x28, 56);
u32(world + 0x564, objectives);
u32(world + 0x568, 1);
u32(world + 0x56c, 1);
u32(objectives, 7);
u32(objectives + 8, 2);

// One inventory bag and one item. The companion follows the real ItemContext
// and Inventory graph, including the bag/item back-references and slot.
u32(itemContext + 0xf8, inventory);
u32(inventory + 4, backpack);
u32(inventory + 0x60, 4);
u32(inventory + 0x90, 1_234);
u32(inventory + 0x94, 50_000);
u32(backpack, 1);
u32(backpack + 4, 0);
u32(backpack + 0x0c, 0);
u32(backpack + 0x10, 1);
u32(backpack + 0x18, backpackItems);
u32(backpack + 0x1c, 20);
u32(backpack + 0x20, 20);
u32(backpackItems, inventoryItem);
u32(inventoryItem, 500);
u32(inventoryItem + 4, 0);
u32(inventoryItem + 0x0c, backpack);
u32(inventoryItem + 0x10, itemModifiers);
u32(inventoryItem + 0x14, 2);
u32(inventoryItem + 0x18, 1);
u32(inventoryItem + 0x1c, 123);
u8(inventoryItem + 0x20, 9);
u8(inventoryItem + 0x21, 7);
u16(inventoryItem + 0x22, 2 | (3 << 4) | (4 << 8) | (5 << 12));
u16(inventoryItem + 0x24, 100);
u32(inventoryItem + 0x28, 0x01_0a_0001);
u32(inventoryItem + 0x2c, 456);
u16(inventoryItem + 0x48, 0);
u8(inventoryItem + 0x4a, 0);
u16(inventoryItem + 0x4c, 5);
u8(inventoryItem + 0x4e, 0);
u8(inventoryItem + 0x4f, 0xff);
u8(inventoryItem + 0x50, 0);
u32(itemModifiers, 0x1234_5678);
u32(itemModifiers + 4, 0x8765_4321);

const layout = Array(129).fill(0);
Object.assign(layout, {
  0: contextRoot,
  1: agentArray,
  4: 6,
  5: 0x44,
  6: 0x198,
  7: 0x19c,
  8: 0x234,
  9: 0x23c,
  10: 0x2ac,
  11: 0x2c,
  12: 0x74,
  13: 0x78,
  14: 0x30,
  15: 0x4c,
  16: 0x9c,
  17: 0xf4,
  18: 0xf6,
  19: 0x10e,
  20: 0x10f,
  21: 0x110,
  22: 0x134,
  23: 0x158,
  24: 0x13c,
  25: 0x1b5,
  26: 0x2c,
  27: 0x4c,
  48: 0x6f0,
  49: 0xbc,
  50: 0,
  51: 4,
  52: 0xa4,
  53: 0xb0,
  54: 0x14,
  55: 0,
  56: 4,
  57: 8,
  58: 0x0c,
  59: 0x10,
  60: 0x508,
  61: 0x24,
  62: 0,
  63: 4,
  64: 0x14,
  65: 0x10,
  66: 0,
  67: 8,
  68: 0x0c,
  69: 0x18,
  70: 0,
  71: 4,
  72: 8,
  73: 0x0c,
  74: 0x10,
  75: 0x14,
  76: 0x528,
  77: 0x52c,
  78: 0x34,
  79: 0,
  80: 4,
  81: 0x14,
  82: 0x18,
  83: 0x28,
  84: 0x564,
  85: 0x0c,
  86: 0,
  87: 8,
  88: 0x40,
  89: 0xf8,
  90: 0,
  91: 0x60,
  92: 0x90,
  93: 0x94,
  94: 0,
  95: 4,
  96: 0x0c,
  97: 0x10,
  98: 0x18,
  99: 0,
  100: 4,
  101: 0x0c,
  102: 0x10,
  103: 0x14,
  104: 0x18,
  105: 0x1c,
  106: 0x20,
  107: 0x21,
  108: 0x24,
  109: 0x28,
  110: 0x2c,
  111: 0x48,
  112: 0x4a,
  113: 0x4c,
  114: 0x4e,
  115: 0x4f,
  116: 0x50,
});
new Uint32Array(memory.buffer, config, layout.length).set(layout);

const kernel = await WebAssembly.instantiate(await readFile(kernelPath), {
  env: { memory },
  game: { enhancement_tick_original: () => {} },
});
const { companion_init: init, companion_tick: tick } = kernel.instance.exports;
assert.equal(
  init(snapshot, COMPANION_SNAPSHOT_BYTES, config, 516, 0, 0, 1 << 1),
  1,
);
tick(0);

const state = readCompanionSnapshot(memory.buffer, snapshot);
assert.equal(state.status, 'ready');
assert.equal(state.playerId, 1);
assert.equal(state.mapId, 55);
assert.equal(state.agents.total, 2);
assert.deepEqual(
  state.agents.agents.map(({ agentId, kind }) => ({ agentId, kind })),
  [
    { agentId: 1, kind: 'Living' },
    { agentId: 2, kind: 'Item' },
  ],
);
assert.equal(state.agents.agents[0].isCasting, true);
assert.equal(state.quests.activeQuestId, 44);
assert.equal(state.quests.quests[0].completed, true);
assert.equal(state.quests.quests[0].primary, true);
assert.deepEqual(state.quests.missionObjectives, [{ objectiveId: 7, type: 2 }]);
assert.equal(state.inventory.goldCharacter, 1_234);
assert.equal(state.inventory.goldStorage, 50_000);
assert.equal(state.inventory.total, 1);
assert.equal(state.inventory.bags[0].kind, 'Inventory');
assert.equal(state.inventory.items[0].typeName, 'Usable');
assert.equal(state.inventory.items[0].quantity, 5);
assert.equal(state.inventory.items[0].modifierCount, 2);
assert.equal(state.inventory.items[0].isStackable, true);
assert.equal(state.inventory.items[0].isGold, true);
