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

  it('keeps every typed nested game-state domain', () => {
    const party = Object.freeze({
      id: 1,
      players: Object.freeze([{ loginNumber: 42 }]),
    });
    const skillbar = Object.freeze({
      agentId: 2,
      skills: Object.freeze([{ slot: 1, skillId: 100 }]),
    });
    const effects = Object.freeze({
      agentId: 2,
      buffs: Object.freeze([{ skillId: 100, buffId: 7, targetAgentId: 2 }]),
      effects: Object.freeze([]),
    });
    const agents = Object.freeze({
      total: 1,
      agents: Object.freeze([{ agentId: 2, kind: 'Living' }]),
    });
    const quests = Object.freeze({
      activeQuestId: 44,
      quests: Object.freeze([{ questId: 44 }]),
    });
    const inventory = Object.freeze({
      total: 1,
      bags: Object.freeze([{ bagId: 1 }]),
      items: Object.freeze([{ itemId: 500 }]),
    });
    assert.deepEqual(
      publicState({
        status: 'ready', party, skillbar, effects, agents, quests, inventory,
      }),
      {
        status: 'ready', party, skillbar, effects, agents, quests, inventory,
      },
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
