import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { probeLayout } from './layout-probe.js';

const words = () => {
  const layout = Array(228).fill(0);
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
  return buffer;
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
    });
  });

  it('rejects unbounded scans', () => {
    assert.throws(() => probeLayout(new ArrayBuffer(8), words(), 8192), /bounds/);
  });
});
