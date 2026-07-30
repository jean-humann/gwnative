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
      quests: Object.freeze([{ questId: 44, hasMarker: true }]),
    });
    const inventory = Object.freeze({
      total: 1,
      bags: Object.freeze([{ bagId: 1 }]),
      items: Object.freeze([{ itemId: 500 }]),
    });
    const social = Object.freeze({
      playerStatus: 1,
      friends: Object.freeze({ total: 1, entries: Object.freeze([]) }),
      guild: null,
    });
    const completion = Object.freeze({
      normalMode: Object.freeze({ completedMissions: Object.freeze([55]), completedBonuses: Object.freeze([]) }),
      hardMode: Object.freeze({ completedMissions: Object.freeze([]), completedBonuses: Object.freeze([]) }),
      unlockedMaps: Object.freeze([55]),
      vanquishedAreas: Object.freeze([]),
    });
    const camera = Object.freeze({
      mode: 3,
      modeName: 'Unlocked',
      position: Object.freeze({ x: 1, y: 2, z: 3 }),
    });
    const trade = Object.freeze({
      flags: 1,
      statusName: 'Initiated',
      open: true,
      player: Object.freeze({ gold: 0, items: Object.freeze([]) }),
      partner: Object.freeze({ gold: 0, items: Object.freeze([]) }),
    });
    const ui = Object.freeze({
      total: 1,
      frames: Object.freeze([Object.freeze({ frameId: 0, frameHash: 7 })]),
    });
    const merchant = Object.freeze({
      truncated: false,
      total: 2,
      itemIds: Object.freeze([900, 901]),
    });
    const progression = Object.freeze({
      hardModeUnlocked: true,
      level: 20,
      experience: 1_337_500,
      factions: Object.freeze({
        kurzick: Object.freeze({ current: 1_000, totalEarned: 5_000, maximum: 10_000 }),
        luxon: Object.freeze({ current: 2_000, totalEarned: 6_000, maximum: 10_000 }),
        imperial: Object.freeze({ current: 100, totalEarned: 1_000, maximum: 15_000 }),
        balthazar: Object.freeze({ current: 500, totalEarned: 2_500, maximum: 10_000 }),
      }),
      skillPoints: Object.freeze({ current: 5, totalEarned: 125 }),
    });
    const skillUnlocks = Object.freeze({
      learnableTruncated: false,
      learnableTotal: 2,
      learnableSkillIds: Object.freeze([111, 222]),
      characterLearnedSkillIds: Object.freeze([3, 100]),
      accountUnlockedSkillIds: Object.freeze([3, 200]),
    });
    assert.deepEqual(
      publicState({
        status: 'ready',
        party,
        skillbar,
        effects,
        agents,
        quests,
        inventory,
        social,
        completion,
        camera,
        trade,
        ui,
        merchant,
        progression,
        skillUnlocks,
      }),
      {
        status: 'ready',
        party,
        skillbar,
        effects,
        agents,
        quests,
        inventory,
        social,
        completion,
        camera,
        trade,
        ui,
        merchant,
        progression,
        skillUnlocks,
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
