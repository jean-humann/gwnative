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
const friendList = 0x15a000;
const friendPointers = 0x15b000;
const friend = 0x15c000;
const guildContext = 0x15d000;
const guildPointers = 0x15e000;
const guild = 0x15f000;
const rosterPointers = 0x160000;
const guildPlayer = 0x161000;
const completionWords = 0x162000;
const camera = 0x163000;
const trade = 0x164000;
const tradePlayerItems = 0x165000;
const tradePartnerItems = 0x166000;
const frameArray = 0x167000;
const framePointers = 0x168000;
const rootFrame = 0x169000;
const childFrame = 0x16a000;
const merchantItems = 0x16b000;
const account = 0x16c000;
const learnableSkills = 0x16d000;
const learnedSkills = 0x16e000;
const accountUnlockedSkills = 0x16f000;
const snapshot = 0x180000;
const config = 0x1a0000;

// Context and current-map invariants.
u32(contextRoot, contexts);
u32(contexts + 6 * 4, game);
u32(game + 0x28, account);
u32(game + 0x44, character);
u32(game + 0x2c, world);
u32(game + 0x3c, guildContext);
u32(game + 0x40, itemContext);
u32(game + 0x58, trade);
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

// WorldContext's merchant-facing item-ID array. It does not imply that a
// merchant window is open and deliberately carries no price or quote data.
u32(world + 0x24, merchantItems);
u32(world + 0x28, 2);
u32(world + 0x2c, 2);
u32(merchantItems, 900);
u32(merchantItems + 4, 901);

// Duplicated progression counters intentionally differ by one so the
// companion exercises the same highest-valid-copy rule as GWCA/Py4GW.
u32(world + 0x684, 1);
u32(world + 0x740, 1_337_499);
u32(world + 0x744, 1_337_500);
u32(world + 0x748, 999);
u32(world + 0x74c, 1_000);
u32(world + 0x750, 4_999);
u32(world + 0x754, 5_000);
u32(world + 0x758, 1_999);
u32(world + 0x75c, 2_000);
u32(world + 0x760, 5_999);
u32(world + 0x764, 6_000);
u32(world + 0x768, 99);
u32(world + 0x76c, 100);
u32(world + 0x770, 999);
u32(world + 0x774, 1_000);
u32(world + 0x788, 19);
u32(world + 0x78c, 20);
u32(world + 0x798, 499);
u32(world + 0x79c, 500);
u32(world + 0x7a0, 2_499);
u32(world + 0x7a4, 2_500);
u32(world + 0x7a8, 4);
u32(world + 0x7ac, 5);
u32(world + 0x7b0, 124);
u32(world + 0x7b4, 125);
u32(world + 0x7b8, 10_000);
u32(world + 0x7bc, 10_000);
u32(world + 0x7c0, 10_000);
u32(world + 0x7c4, 15_000);

// Trainer-visible IDs are a plain list; character-learned and account-unlocked
// skills are separate bitmaps in WorldContext and AccountContext respectively.
u32(world + 0x700, learnableSkills);
u32(world + 0x704, 2);
u32(world + 0x708, 2);
u32(learnableSkills, 111);
u32(learnableSkills + 4, 222);
u32(world + 0x710, learnedSkills);
u32(world + 0x714, 4);
u32(world + 0x718, 4);
u32(learnedSkills, 1 << 3);
u32(learnedSkills + 3 * 4, 1 << 4);
u32(account + 0x124, accountUnlockedSkills);
u32(account + 0x128, 7);
u32(account + 0x12c, 7);
u32(accountUnlockedSkills, 1 << 3);
u32(accountUnlockedSkills + 6 * 4, 1 << 8);

// One active quest, one quest without a map marker, and one mission objective.
u32(world + 0x528, 44);
u32(world + 0x52c, questLog);
u32(world + 0x530, 2);
u32(world + 0x534, 2);
u32(questLog, 44);
u32(questLog + 4, 0x22);
u32(questLog + 0x14, 55);
f32(questLog + 0x18, 10);
f32(questLog + 0x1c, 20);
u32(questLog + 0x20, 3);
u32(questLog + 0x28, 56);
u32(questLog + 0x34, 45);
u32(questLog + 0x38, 0x20);
u32(questLog + 0x48, 55);
f32(questLog + 0x4c, Number.POSITIVE_INFINITY);
f32(questLog + 0x50, Number.NEGATIVE_INFINITY);
u32(questLog + 0x54, 0xffff_ffff);
u32(questLog + 0x5c, 56);
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
u32(backpack + 0x0c, 0xffff_ffff);
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
u8(inventoryItem + 0x4a, 0xfe);
u16(inventoryItem + 0x4c, 5);
u8(inventoryItem + 0x4e, 0);
u8(inventoryItem + 0x4f, 0xfe);
u8(inventoryItem + 0x50, 0);
u32(itemModifiers, 0x1234_5678);
u32(itemModifiers + 4, 0x8765_4321);

// One online friend and a numeric-only guild summary. Names, UUIDs, and
// announcements are present in neither this fixture nor the public snapshot.
u32(friendList, friendPointers);
u32(friendList + 4, 1);
u32(friendList + 8, 1);
u32(friendList + 0x24, 1);
u32(friendList + 0x28, 0);
u32(friendList + 0x2c, 0);
u32(friendList + 0x30, 0);
u32(friendList + 0xa0, 1);
u32(friendPointers, friend);
u32(friend, 1);
u32(friend + 4, 1);
u32(friend + 0x40, 0xffff_ffff);
u32(friend + 0x44, 0xffff_ffff);

u32(guildContext + 0x60, 2);
for (let index = 0; index < 4; index += 1) {
  u32(guildContext + 0x64 + index * 4, index + 10);
  u32(guild + index * 4, index + 10);
}
u32(guildContext + 0x2a0, 3);
u32(guildContext + 0x2f8, guildPointers);
u32(guildContext + 0x2fc, 4);
u32(guildContext + 0x300, 3);
u32(guildPointers + 8, guild);
u32(guildContext + 0x358, rosterPointers);
u32(guildContext + 0x35c, 1);
u32(guildContext + 0x360, 1);
u32(rosterPointers, guildPlayer);
u32(guild + 0x24, 2);
u32(guild + 0x28, 1);
u32(guild + 0x2c, 9);
u32(guild + 0x70, 1_200);
u32(guild + 0x74, 0xffff_ffff);
u32(guild + 0x78, 1_000);
u32(guild + 0x7c, 10);
for (let index = 0; index < 7; index += 1) {
  u32(guild + 0x90 + index * 4, index + 1);
}

// WorldContext completion arrays are bounded bitmaps: bit index equals map ID.
const completionOffsets = [0x5cc, 0x5dc, 0x5ec, 0x5fc, 0x60c, 0x83c];
for (let category = 0; category < completionOffsets.length; category += 1) {
  const buffer = completionWords + category * 0x100;
  const size = category === 4 ? 27 : 25;
  const capacity = category === 4 ? 52 : 25;
  u32(world + completionOffsets[category], buffer);
  u32(world + completionOffsets[category] + 4, capacity);
  u32(world + completionOffsets[category] + 8, size);
  const mapId = 55 + category;
  u32(buffer + Math.floor(mapId / 32) * 4, 2 ** (mapId % 32));
}

u32(camera, 1);
f32(camera + 0x10, 5_000);
f32(camera + 0x18, 1.25);
f32(camera + 0x1c, -0.3);
f32(camera + 0x20, 1_000);
f32(camera + 0x78, 110);
f32(camera + 0x7c, -260);
f32(camera + 0x80, -50);
f32(camera + 0xa8, 100);
f32(camera + 0xac, -250);
f32(camera + 0xb0, 3);
f32(camera + 0xc0, 1.2);
u32(camera + 0x11c, 2);

// TradeContext: local offer has two items and the partner offers one. The
// compiled client reaches this pointer through GameContext +0x58.
u32(trade, 3);
u32(trade + 0x10, 2_222);
u32(trade + 0x14, tradePlayerItems);
u32(trade + 0x18, 2);
u32(trade + 0x1c, 2);
u32(tradePlayerItems, 700);
u32(tradePlayerItems + 4, 5);
u32(tradePlayerItems + 8, 701);
u32(tradePlayerItems + 12, 1);
u32(trade + 0x24, 3_333);
u32(trade + 0x28, tradePartnerItems);
u32(trade + 0x2c, 1);
u32(trade + 0x30, 1);
u32(tradePartnerItems, 800);
u32(tradePartnerItems + 4, 2);

// Two validated UI frames. The child relation points at the root's embedded
// relation object, exactly as the compiled GetFrameById/FrameRelation methods
// do; no label or callback memory exists in the fixture.
u32(frameArray, framePointers);
u32(frameArray + 4, 2);
u32(frameArray + 8, 2);
u32(framePointers, rootFrame);
u32(framePointers + 4, childFrame);
u32(rootFrame + 0xbc, 0);
u32(rootFrame + 0x128, 0);
u32(rootFrame + 0x134, 0x1111);
u32(rootFrame + 0x18, 3);
u32(rootFrame + 0x20, 4);
u32(rootFrame + 0x24, 5);
u32(rootFrame + 0x18c, 0x4);
u32(rootFrame + 0xd8, 9);
f32(rootFrame + 0xdc, 10);
f32(rootFrame + 0xe0, 100);
f32(rootFrame + 0xe4, 200);
f32(rootFrame + 0xe8, 20);
u32(childFrame + 0xb8, 2);
u32(childFrame + 0xbc, 1);
u32(childFrame + 0x128, rootFrame + 0x128);
u32(childFrame + 0x134, 0x2222);
u32(childFrame + 0x18, 1);
u32(childFrame + 0x20, 7);
u32(childFrame + 0x24, 8);
u32(childFrame + 0x18c, 0x204);

const layout = Array(232).fill(0);
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
  88: 0x5cc,
  89: 0x5dc,
  90: 0x5ec,
  91: 0x5fc,
  92: 0x60c,
  93: 0x83c,
  94: 0x40,
  95: 0xf8,
  96: 0,
  97: 0x60,
  98: 0x90,
  99: 0x94,
  100: 0,
  101: 4,
  102: 0x0c,
  103: 0x10,
  104: 0x18,
  105: 0,
  106: 4,
  107: 0x0c,
  108: 0x10,
  109: 0x14,
  110: 0x18,
  111: 0x1c,
  112: 0x20,
  113: 0x21,
  114: 0x24,
  115: 0x28,
  116: 0x2c,
  117: 0x48,
  118: 0x4a,
  119: 0x4c,
  120: 0x4e,
  121: 0x4f,
  122: 0x50,
  123: friendList,
  124: 0,
  125: 0x24,
  126: 0x28,
  127: 0x2c,
  128: 0x30,
  129: 0xa0,
  130: 0,
  131: 4,
  132: 0x40,
  133: 0x44,
  134: 0x3c,
  135: 0x60,
  136: 0x64,
  137: 0x2a0,
  138: 0x2f8,
  139: 0x358,
  140: 0,
  141: 0x24,
  142: 0x28,
  143: 0x2c,
  144: 0x70,
  145: 0x74,
  146: 0x78,
  147: 0x7c,
  148: 0x90,
  149: 0x174,
  150: 4,
  163: camera,
  164: 0,
  165: 0x10,
  166: 0x18,
  167: 0x1c,
  168: 0x20,
  169: 0x78,
  170: 0xa8,
  171: 0xc0,
  172: 0x11c,
  173: 0x58,
  174: 0,
  175: 0x10,
  176: 0x14,
  177: 0x24,
  178: 0x28,
  179: 8,
  180: 0,
  181: 4,
  182: frameArray,
  183: 0x1c8,
  184: 0x18,
  185: 0x20,
  186: 0x24,
  187: 0xb8,
  188: 0xbc,
  189: 0xd8,
  190: 0,
  191: 4,
  192: 8,
  193: 0x0c,
  194: 0x10,
  195: 0x128,
  196: 0x134,
  197: 0x18c,
  198: 0x24,
  199: 0x684,
  200: 0x740,
  201: 0x744,
  202: 0x748,
  203: 0x74c,
  204: 0x750,
  205: 0x754,
  206: 0x7b8,
  207: 0x758,
  208: 0x75c,
  209: 0x760,
  210: 0x764,
  211: 0x7bc,
  212: 0x768,
  213: 0x76c,
  214: 0x770,
  215: 0x774,
  216: 0x7c4,
  217: 0x788,
  218: 0x78c,
  219: 0x798,
  220: 0x79c,
  221: 0x7a0,
  222: 0x7a4,
  223: 0x7c0,
  224: 0x7a8,
  225: 0x7ac,
  226: 0x7b0,
  227: 0x7b4,
  228: 0x28,
  229: 0x124,
  230: 0x700,
  231: 0x710,
});
new Uint32Array(memory.buffer, config, layout.length).set(layout);

const kernel = await WebAssembly.instantiate(await readFile(kernelPath), {
  env: { memory },
  game: { enhancement_tick_original: () => {} },
});
const { companion_init: init, companion_tick: tick } = kernel.instance.exports;
assert.equal(
  init(snapshot, COMPANION_SNAPSHOT_BYTES, config, 928, 0, 0, 1 << 1),
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
assert.deepEqual(state.merchant, {
  truncated: false,
  total: 2,
  itemIds: [900, 901],
});
assert.deepEqual(state.progression, {
  hardModeUnlocked: true,
  level: 20,
  experience: 1_337_500,
  factions: {
    kurzick: { current: 1_000, totalEarned: 5_000, maximum: 10_000 },
    luxon: { current: 2_000, totalEarned: 6_000, maximum: 10_000 },
    imperial: { current: 100, totalEarned: 1_000, maximum: 15_000 },
    balthazar: { current: 500, totalEarned: 2_500, maximum: 10_000 },
  },
  skillPoints: { current: 5, totalEarned: 125 },
});
assert.deepEqual(state.skillUnlocks, {
  learnableTruncated: false,
  learnableTotal: 2,
  learnableSkillIds: [111, 222],
  characterLearnedSkillIds: [3, 100],
  accountUnlockedSkillIds: [3, 200],
});
assert.equal(state.agents.agents[0].isCasting, true);
assert.equal(state.quests.activeQuestId, 44);
assert.equal(state.quests.quests[0].completed, true);
assert.equal(state.quests.quests[0].primary, true);
assert.equal(state.quests.quests[0].hasMarker, true);
assert.deepEqual(state.quests.quests[1], {
  questId: 45,
  logState: 0x20,
  mapFrom: 55,
  markerX: 0,
  markerY: 0,
  markerPlane: 0,
  hasMarker: false,
  mapTo: 56,
  completed: false,
  currentMission: false,
  primary: true,
  areaPrimary: false,
});
assert.deepEqual(state.quests.missionObjectives, [{ objectiveId: 7, type: 2 }]);
assert.equal(state.inventory.goldCharacter, 1_234);
assert.equal(state.inventory.goldStorage, 50_000);
assert.equal(state.inventory.total, 1);
assert.equal(state.inventory.bags[0].kind, 'Inventory');
assert.equal(state.inventory.bags[0].containerItem, 0xffff_ffff);
assert.equal(state.inventory.items[0].typeName, 'Usable');
assert.equal(state.inventory.items[0].quantity, 5);
assert.equal(state.inventory.items[0].profession, 0xfe);
assert.equal(state.inventory.items[0].modifierCount, 2);
assert.equal(state.inventory.items[0].materialSalvageable, true);
assert.equal(state.inventory.items[0].isStackable, true);
assert.equal(state.inventory.items[0].isGold, true);
assert.equal(state.social.playerStatusName, 'Online');
assert.equal(state.social.friends.entries[0].typeName, 'Friend');
assert.equal(state.social.friends.entries[0].friendId, 0xffff_ffff);
assert.equal(state.social.friends.entries[0].zoneId, 0xffff_ffff);
assert.equal(state.social.guild.index, 2);
assert.equal(state.social.guild.faction, 0xffff_ffff);
assert.equal(state.social.guild.factionName, 'Unknown');
assert.equal(state.social.guild.rosterTotal, 1);
assert.equal(state.social.guild.cape.trim, 7);
assert.deepEqual(state.completion.normalMode.completedMissions, [55]);
assert.deepEqual(state.completion.normalMode.completedBonuses, [56]);
assert.deepEqual(state.completion.hardMode.completedMissions, [57]);
assert.deepEqual(state.completion.hardMode.completedBonuses, [58]);
assert.deepEqual(state.completion.unlockedMaps, [59]);
assert.deepEqual(state.completion.vanquishedAreas, [60]);
assert.equal(state.camera.lookAtAgentId, 1);
assert.equal(state.camera.modeName, 'Follow');
assert.equal(state.camera.unlocked, false);
assert.equal(state.camera.yaw, 1.25);
assert.equal(state.camera.pitch, -0.30000001192092896);
assert.deepEqual(state.camera.position, { x: 110, y: -260, z: -50 });
assert.deepEqual(state.camera.lookAt, { x: 100, y: -250, z: 3 });
assert.ok(Math.abs(state.camera.fieldOfView - 1.2) < 0.000001);
assert.ok(state.camera.renderFieldOfView > 0);
assert.equal(state.trade.statusName, 'OfferSent');
assert.equal(state.trade.open, true);
assert.equal(state.trade.initiated, true);
assert.equal(state.trade.offerSent, true);
assert.equal(state.trade.accepted, false);
assert.equal(state.trade.player.gold, 2_222);
assert.deepEqual(state.trade.player.items, [
  { slot: 1, itemId: 700, quantity: 5 },
  { slot: 2, itemId: 701, quantity: 1 },
]);
assert.equal(state.trade.partner.gold, 3_333);
assert.deepEqual(state.trade.partner.items, [
  { slot: 1, itemId: 800, quantity: 2 },
]);
assert.equal(state.ui.total, 2);
assert.equal(state.ui.createdTotal, 2);
assert.equal(state.ui.visibleTotal, 1);
assert.deepEqual(
  state.ui.frames.map(
    ({ frameId, parentId, childOffsetId, frameHash, locallyVisible }) => ({
      frameId,
      parentId,
      childOffsetId,
      frameHash,
      locallyVisible,
    }),
  ),
  [
    {
      frameId: 0,
      parentId: null,
      childOffsetId: 0,
      frameHash: 0x1111,
      locallyVisible: true,
    },
    {
      frameId: 1,
      parentId: 0,
      childOffsetId: 2,
      frameHash: 0x2222,
      locallyVisible: false,
    },
  ],
);

u32(trade + 0x18, 17);
u32(trade + 0x1c, 17);
for (let index = 0; index < 17; index += 1) {
  u32(tradePlayerItems + index * 8, 700 + index);
  u32(tradePlayerItems + index * 8 + 4, 1);
}
tick(0);
const truncatedTrade = readCompanionSnapshot(memory.buffer, snapshot);
assert.equal(truncatedTrade.status, 'ready');
assert.equal(truncatedTrade.trade.player.items.length, 16);
assert.equal(truncatedTrade.trade.player.itemsTruncated, true);
assert.equal(truncatedTrade.trade.player.items[15].itemId, 715);

// The client can leave the last offer in memory after closing the window.
// Closed is authoritative and must never leak that stale gold or item list.
u32(trade, 0);
tick(0);
const closedTrade = readCompanionSnapshot(memory.buffer, snapshot);
assert.equal(closedTrade.status, 'ready');
assert.deepEqual(closedTrade.trade, {
  flags: 0,
  statusName: 'Closed',
  open: false,
  initiated: false,
  offerSent: false,
  accepted: false,
  player: { gold: 0, itemsTruncated: false, items: [] },
  partner: { gold: 0, itemsTruncated: false, items: [] },
});

// Index zero is the authoritative no-guild state even when the context keeps
// stale rank and roster fields alive across a transition.
u32(guildContext + 0x60, 0);
tick(0);
const guildless = readCompanionSnapshot(memory.buffer, snapshot);
assert.equal(guildless.status, 'ready');
assert.equal(guildless.social.guild, null);
assert.equal(guildless.social.friends.total, 1);
