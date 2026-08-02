import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  characterSelectionReady,
  frameLabelHash,
  probeLayout,
} from './layout-probe.js';

const words = () => {
  const layout = Array(232).fill(0);
  Object.assign(layout, {
    0: 0x1000,
    1: 0x2000,
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
    16: 0x9c,
    17: 0xf4,
    18: 0xf6,
    26: 0x2c,
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
    111: 0x1c,
    112: 0x20,
    116: 0x2c,
    118: 0x4a,
    119: 0x4c,
    120: 0x4e,
    121: 0x4f,
    122: 0x50,
    123: 0xa000,
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
    145: 0x74,
    148: 0x90,
    149: 0x174,
  });
  return layout;
};

function fixture(delta) {
  const buffer = new ArrayBuffer(0x10000);
  const view = new DataView(buffer);
  const contexts = 0x3000;
  const game = 0x3100;
  const character = 0x3200;
  const agents = 0x4000;
  const player = 0x5000;
  const world = 0x6000;
  const quests = 0x7000;
  const objectives = 0x8000;
  const itemContext = 0x9000;
  const inventory = 0x9100;
  const backpack = 0x9200;
  const guildContext = 0xb000;
  view.setUint32(0x1000 + delta, contexts, true);
  view.setUint32(contexts + 6 * 4, game, true);
  view.setUint32(game + 0x44, character, true);
  view.setUint32(character + 0x198, 42, true);
  view.setUint32(character + 0x19c, 0, true);
  view.setUint32(character + 0x234, 42, true);
  view.setUint32(character + 0x23c, 0, true);
  view.setUint32(character + 0x2ac, 7, true);
  view.setUint32(0x2000 + delta, agents, true);
  view.setUint32(0x2000 + delta + 4, 3, true);
  view.setUint32(0x2000 + delta + 8, 2, true);
  view.setUint32(agents + 4, player, true);
  view.setUint32(player + 0x2c, 1, true);
  view.setFloat32(player + 0x74, 100, true);
  view.setFloat32(player + 0x78, 200, true);
  view.setUint32(player + 0x9c, 0xdb, true);
  view.setUint16(player + 0xf4, 7, true);
  view.setUint16(player + 0xf6, 0x3000, true);
  view.setUint32(game + 0x2c, world, true);
  view.setUint32(world + 0x528, 44, true);
  view.setUint32(world + 0x52c, quests, true);
  view.setUint32(world + 0x530, 1, true);
  view.setUint32(world + 0x534, 1, true);
  view.setUint32(quests, 44, true);
  view.setUint32(quests + 4, 0x22, true);
  view.setUint32(quests + 0x14, 55, true);
  view.setFloat32(quests + 0x18, 10, true);
  view.setFloat32(quests + 0x1c, 20, true);
  view.setUint32(quests + 0x20, 3, true);
  view.setUint32(quests + 0x28, 56, true);
  view.setUint32(world + 0x564, objectives, true);
  view.setUint32(world + 0x568, 1, true);
  view.setUint32(world + 0x56c, 1, true);
  view.setUint32(objectives, 7, true);
  view.setUint32(objectives + 8, 2, true);
  for (const [index, offset] of [0x5cc, 0x5dc, 0x5ec, 0x5fc, 0x60c, 0x83c].entries()) {
    const completion = 0x8100 + index * 0x40;
    view.setUint32(world + offset, completion, true);
    view.setUint32(world + offset + 4, 1, true);
    view.setUint32(world + offset + 8, 1, true);
  }
  view.setUint32(game + 0x40, itemContext, true);
  view.setUint32(itemContext + 0xf8, inventory, true);
  view.setUint32(inventory + 4, backpack, true);
  view.setUint32(inventory + 0x60, 4, true);
  view.setUint32(inventory + 0x90, 1_234, true);
  view.setUint32(inventory + 0x94, 50_000, true);
  view.setUint32(backpack, 1, true);
  view.setUint32(backpack + 4, 0, true);
  view.setUint32(backpack + 0x0c, 0, true);
  view.setUint32(backpack + 0x10, 0, true);
  view.setUint32(0xa000 + 0xa0, 1, true);
  view.setUint32(game + 0x3c, guildContext, true);
  view.setUint32(guildContext + 0x60, 0, true);
  view.setUint32(guildContext + 0x2a0, 0, true);
  return buffer;
}

function characterSelectorFixture() {
  const layout = Array(232).fill(0);
  Object.assign(layout, {
    182: 0x100,
    183: 0x1c8,
    188: 0xbc,
    195: 0x128,
    196: 0x134,
    197: 0x18c,
  });
  const buffer = new ArrayBuffer(0x2000);
  const view = new DataView(buffer);
  const frameBuffer = 0x200;
  const root = 0x1000;
  const selector = 0x1200;
  const play = 0x1400;
  view.setUint32(0x100, frameBuffer, true);
  view.setUint32(0x104, 3, true);
  view.setUint32(0x108, 3, true);
  for (const [id, address, parent, label] of [
    [0, root, 0, 'Game'],
    [1, selector, root, 'Selector'],
    [2, play, selector, 'Play'],
  ]) {
    view.setUint32(frameBuffer + id * 4, address, true);
    view.setUint32(address + 0xbc, id, true);
    view.setUint32(address + 0x128, parent ? parent + 0x128 : 0, true);
    view.setUint32(address + 0x134, frameLabelHash(label), true);
    view.setUint32(address + 0x18c, 0x4, true);
  }
  return { buffer, layout, view, root, selector, play };
}

describe('bounded layout probe', () => {
  it('returns only the common aligned live-memory delta', () => {
    assert.deepEqual(probeLayout(fixture(0x30), words(), 0x40), {
      radiusBytes: 0x40,
      contextDeltas: [0x30],
      agentDeltas: [0x30],
      commonDeltas: [0x30],
      quest: {
        worldAvailable: true,
        activeQuestId: 44,
        questCapacity: 1,
        questCount: 1,
        questInvalidIndex: 0xffff_ffff,
        questInvalidMask: 0,
        objectiveCapacity: 1,
        objectiveCount: 1,
        questRecordsValid: true,
        activeQuestPresent: true,
        objectiveRecordsValid: true,
      },
      inventory: {
        itemContextAvailable: true,
        inventoryAvailable: true,
        scalarFieldsValid: true,
        storagePanesUnlocked: 4,
        bagPointerCount: 1,
        backpackPresent: true,
        bagInvalidId: 0,
        bagInvalidMask: 0,
        itemCount: 0,
        itemInvalidBagId: 0,
        itemInvalidSlot: 0xffff_ffff,
        itemInvalidMask: 0,
        inventoryRecordsValid: true,
      },
      social: {
        friendListAvailable: true,
        friendHeaderValid: true,
        playerStatus: 1,
        friendCapacity: 0,
        friendSlotCount: 0,
        friendEntryCount: 0,
        friendInvalidSlot: 0xffff_ffff,
        friendInvalidMask: 0,
        friendCountMismatchMask: 0,
        friendRecordsValid: true,
        guildContextAvailable: true,
        guildIndex: 0,
        guildRecordPresent: false,
        guildRosterCapacity: 0,
        guildRosterCount: 0,
        guildInvalidMask: 0,
        guildRecordsValid: true,
        socialRecordsValid: true,
      },
      completion: {
        worldAvailable: true,
        capacities: [1, 1, 1, 1, 1, 1],
        sizes: [1, 1, 1, 1, 1, 1],
        invalidMasks: [0, 0, 0, 0, 0, 0],
        completionRecordsValid: true,
      },
    });
  });

  it('reports bounded inventory validation masks without exposing contents', () => {
    const buffer = fixture(0);
    const view = new DataView(buffer);
    view.setUint32(0x9200 + 4, 9, true);
    const result = probeLayout(buffer, words(), 0);
    assert.equal(result.inventory.bagInvalidId, 1);
    assert.equal(result.inventory.bagInvalidMask, 2);
    assert.equal(result.inventory.inventoryRecordsValid, false);
    assert.equal('address' in result.inventory, false);
  });

  it('rejects unbounded scans', () => {
    assert.throws(() => probeLayout(new ArrayBuffer(8), words(), 8192), /bounds/);
  });

  it('reports bounded social validation masks without exposing identities', () => {
    const buffer = fixture(0);
    const view = new DataView(buffer);
    const friends = 0xa200;
    const entry = 0xa300;
    view.setUint32(0xa000, friends, true);
    view.setUint32(0xa004, 1, true);
    view.setUint32(0xa008, 1, true);
    view.setUint32(0xa024, 1, true);
    view.setUint32(friends, entry, true);
    view.setUint32(entry, 1, true);
    view.setUint32(entry + 4, 5, true);
    view.setUint32(entry + 0x40, 0xffff_ffff, true);
    view.setUint32(entry + 0x44, 0xffff_ffff, true);
    const social = probeLayout(buffer, words(), 0).social;
    assert.equal(social.friendInvalidSlot, 0);
    assert.equal(social.friendInvalidMask, 1 << 2);
    assert.equal(social.socialRecordsValid, false);
    assert.equal('friendId' in social, false);
    assert.equal('address' in social, false);
  });

  it('reports only completion descriptors, never bitmap contents', () => {
    const buffer = fixture(0);
    const view = new DataView(buffer);
    view.setUint32(0x6000 + 0x5cc + 4, 1_025, true);
    const completion = probeLayout(buffer, words(), 0).completion;
    assert.deepEqual(completion.invalidMasks, [8, 0, 0, 0, 0, 0]);
    assert.equal(completion.completionRecordsValid, false);
    assert.equal('words' in completion, false);
    assert.equal('buffer' in completion, false);
  });
});

describe('certified character selector probe', () => {
  it('matches Guild Wars frame labels case-insensitively', () => {
    assert.equal(frameLabelHash('Game'), 140452905);
    assert.equal(frameLabelHash('selector'), frameLabelHash('Selector'));
  });

  it('requires a visible selector and created Play control', () => {
    const { buffer, layout, view, selector, play } = characterSelectorFixture();
    assert.equal(characterSelectionReady(buffer, layout), true);
    view.setUint32(play + 0x18c, 0x8, true);
    assert.equal(characterSelectionReady(buffer, layout), false);
    view.setUint32(play + 0x18c, 0x14, true);
    assert.equal(characterSelectionReady(buffer, layout), true);
    view.setUint32(play + 0x18c, 0x4, true);
    view.setUint32(selector + 0x18c, 0x204, true);
    assert.equal(characterSelectionReady(buffer, layout), false);
  });

  it('fails closed on a corrupt certified frame tree', () => {
    const { buffer, layout, view, play } = characterSelectorFixture();
    view.setUint32(play + 0xbc, 1, true);
    assert.equal(characterSelectionReady(buffer, layout), false);
  });
});
