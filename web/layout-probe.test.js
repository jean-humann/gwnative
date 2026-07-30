import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { probeLayout } from './layout-probe.js';

const words = () => [
  0x1000, 0x2000, 0, 0, 6, 0x44, 0x198, 0x19c, 0x234, 0x23c, 0x2ac,
  0x2c, 0x74, 0x78, 0x30, 0x4c, 0x9c, 0xf4, 0xf6,
  ...Array(180).fill(0),
];

function fixture(delta) {
  const buffer = new ArrayBuffer(0x10000);
  const view = new DataView(buffer);
  const contexts = 0x3000;
  const game = 0x3100;
  const character = 0x3200;
  const agents = 0x4000;
  const player = 0x5000;
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
  return buffer;
}

describe('bounded layout probe', () => {
  it('returns only the common aligned live-memory delta', () => {
    assert.deepEqual(probeLayout(fixture(0x30), words(), 0x40), {
      radiusBytes: 0x40,
      contextDeltas: [0x30],
      agentDeltas: [0x30],
      commonDeltas: [0x30],
    });
  });

  it('rejects unbounded scans', () => {
    assert.throws(() => probeLayout(new ArrayBuffer(8), words(), 8192), /bounds/);
  });
});
