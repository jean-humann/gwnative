import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { publicState } from './game-api.js';

describe('public game state', () => {
  it('keeps every supported telemetry field', () => {
    const state = publicState({
      status: 'ready',
      mapId: 55,
      playerId: 2,
      targetValid: false,
      rangeName: 'None',
    });
    assert.deepEqual(state, {
      status: 'ready',
      mapId: 55,
      playerId: 2,
      targetValid: false,
      rangeName: 'None',
    });
    assert(Object.isFrozen(state));
  });

  it('cannot publish companion internals or arbitrary page data', () => {
    assert.deepEqual(
      publicState({
        status: 'ready',
        sequence: 99,
        agentTypeBits: 0xffff,
        credentials: { password: 'never' },
      }),
      { status: 'ready' },
    );
  });

  it('omits fixed snapshot target slots when no target is valid', () => {
    assert.deepEqual(
      publicState({
        status: 'ready',
        mapId: 55,
        playerId: 2,
        targetValid: false,
        targetId: 0,
        targetKind: 'None',
        targetX: 0,
        targetY: 0,
        distance: 0,
        rangeName: 'None',
      }),
      {
        status: 'ready',
        mapId: 55,
        playerId: 2,
        targetValid: false,
        targetKind: 'None',
        rangeName: 'None',
      },
    );
  });
});
