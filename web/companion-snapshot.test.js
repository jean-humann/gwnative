// Tests for the companion snapshot decoders.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// This is the page's boundary with a block of the game's own heap, and it is
// the one file here that has to assume the worst. The companion writes those
// bytes now, but they are ordinary memory inside a process that is running
// eight megabytes of somebody else's compiled code, and a decoder that trusted
// them would put a coordinate from the middle of a texture on screen — or, with
// the cursor block, hand four kilobytes of it to a canvas.
//
// So what is tested is refusal. Every assertion below builds a block that is
// wrong in exactly one way and checks that the decoder says `waiting` instead
// of returning a reading. The happy path is one test; the rest are the ways a
// happy path stops being one.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  COMPANION_CURSOR_BYTES,
  COMPANION_SNAPSHOT_BYTES,
  readCompanionCursorHeader,
  readCompanionCursorPixels,
  readCompanionSnapshot,
} from './companion-snapshot.js';

const MAGIC = 0x42545747;
const CURSOR_MAGIC = 0x43545747;
const SNAPSHOT_ABI = 14;
const CURSOR_ABI = 1;

const FLAG_READY = 1 << 0;
const FLAG_PLAYER = 1 << 1;
const FLAG_TARGET = 1 << 2;
const FLAG_LOADING = 1 << 3;
const FLAG_PARTY = 1 << 4;
const FLAG_SKILLBAR = 1 << 5;
const FLAG_EFFECTS = 1 << 6;
const FLAG_AGENTS = 1 << 7;
const FLAG_QUESTS = 1 << 8;
const FLAG_INVENTORY = 1 << 9;
const FLAG_SOCIAL = 1 << 10;
const FLAG_COMPLETION = 1 << 11;
const FLAG_CAMERA = 1 << 12;
const FLAG_TRADE = 1 << 13;
const FLAG_UI = 1 << 14;
const FLAG_MERCHANT = 1 << 15;
const FLAG_PROGRESSION = 1 << 16;
const FLAG_SKILL_UNLOCKS = 1 << 17;

const CURSOR_VALID = 1 << 0;
const CURSOR_HIDDEN = 1 << 1;
const CURSOR_UNSUPPORTED = 1 << 2;

/**
 * A snapshot in a buffer, laid out exactly as the companion publishes one.
 * The defaults are a player standing in an outpost with a living target 300
 * units away, which is the only combination of fields the decoder accepts in
 * full — so every test below is that, minus one thing.
 *
 * @param {Record<string, number>} [overrides]
 */
function snapshot(overrides = {}) {
  const fields = {
    magic: MAGIC,
    abi: SNAPSHOT_ABI,
    byteLength: COMPANION_SNAPSHOT_BYTES,
    sequence: 2,
    flags: FLAG_READY | FLAG_PLAYER | FLAG_TARGET,
    tickCount: 1234,
    mapId: 248,
    instanceType: 0,
    playerId: 1,
    playerX: 100,
    playerY: -250.5,
    targetId: 7,
    agentTypeBits: 0xdb,
    targetX: 400,
    targetY: -250.5,
    distance: 300,
    rangeBand: 4,
    ...overrides,
  };
  const buffer = new ArrayBuffer(COMPANION_SNAPSHOT_BYTES);
  const view = new DataView(buffer);
  view.setUint32(0, fields.magic, true);
  view.setUint16(4, fields.abi, true);
  view.setUint16(6, fields.byteLength, true);
  view.setUint32(8, fields.sequence, true);
  view.setUint32(12, fields.flags, true);
  view.setUint32(16, fields.tickCount, true);
  view.setUint32(20, fields.mapId, true);
  view.setUint32(24, fields.instanceType, true);
  view.setUint32(28, fields.playerId, true);
  view.setFloat32(32, fields.playerX, true);
  view.setFloat32(36, fields.playerY, true);
  view.setUint32(40, fields.targetId, true);
  view.setUint32(44, fields.agentTypeBits, true);
  view.setFloat32(48, fields.targetX, true);
  view.setFloat32(52, fields.targetY, true);
  view.setFloat32(56, fields.distance, true);
  view.setUint32(60, fields.rangeBand, true);
  return buffer;
}

/** @param {Record<string, number>} [overrides] */
function read(overrides) {
  return readCompanionSnapshot(snapshot(overrides), 0);
}

function domainSnapshot() {
  const buffer = snapshot({
    flags:
      FLAG_READY
      | FLAG_PLAYER
      | FLAG_TARGET
      | FLAG_PARTY
      | FLAG_SKILLBAR
      | FLAG_EFFECTS
      | FLAG_AGENTS
      | FLAG_QUESTS
      | FLAG_INVENTORY
      | FLAG_SOCIAL
      | FLAG_COMPLETION
      | FLAG_CAMERA
      | FLAG_TRADE
      | FLAG_UI
      | FLAG_MERCHANT
      | FLAG_PROGRESSION
      | FLAG_SKILL_UNLOCKS,
  });
  const view = new DataView(buffer);
  view.setUint32(64, 3, true);
  view.setUint32(68, 1 << 2, true);
  view.setUint32(72, 1, true);
  view.setUint32(76, 1, true);
  view.setUint32(80, 1, true);
  view.setUint32(84, 1, true);
  view.setUint32(88, 42, true);
  view.setUint32(92, 7, true);
  view.setUint32(96, 3, true);
  view.setUint32(232, 8, true);
  view.setUint32(236, 42, true);
  view.setUint32(240, 5, true);
  view.setUint32(244, 20, true);
  view.setUint32(424, 9, true);
  view.setUint32(428, 6, true);
  view.setUint32(432, 20, true);
  view.setUint32(568, 10, true);
  view.setUint32(696, 1, true);
  view.setUint32(700, 1 << 2, true);
  view.setUint32(704, 1, true);
  for (let slot = 0; slot < 8; slot += 1) {
    const offset = 708 + slot * 20;
    view.setUint32(offset, slot, true);
    view.setUint32(offset + 4, slot + 1, true);
    view.setUint32(offset + 8, slot === 0 ? 500 : 0, true);
    view.setUint32(offset + 12, 100 + slot, true);
    view.setUint32(offset + 16, slot + 10, true);
  }
  view.setUint32(868, 1, true);
  view.setUint32(876, 1, true);
  view.setUint32(880, 1, true);
  view.setUint32(884, 200, true);
  view.setUint32(888, 300, true);
  view.setUint32(892, 1, true);
  view.setUint32(1268, 201, true);
  view.setUint32(1272, 12, true);
  view.setUint32(1276, 301, true);
  view.setUint32(1280, 7, true);
  view.setFloat32(1284, 12.5, true);
  view.setUint32(1288, 400, true);
  view.setUint32(2808, 1, true);
  view.setUint32(2812, 1, true);
  view.setUint32(2816, 1, true);
  view.setUint32(2820, 0xdb, true);
  view.setUint32(2824, 42, true);
  view.setUint32(2828, 7, true);
  view.setUint32(2832, 0, true);
  view.setUint32(2836, 20, true);
  view.setFloat32(2840, 0.75, true);
  view.setFloat32(2844, 1.25, true);
  view.setFloat32(2848, 100, true);
  view.setFloat32(2852, -250.5, true);
  view.setFloat32(2856, 3, true);
  view.setUint32(2860, 65, true);
  view.setUint32(2864, 0, true);
  view.setUint32(2868, 1, true);
  view.setUint32(9984, 44, true);
  view.setUint32(9992, 1, true);
  view.setUint32(9996, 1, true);
  view.setUint32(10000, 44, true);
  view.setUint32(10004, 0x22, true);
  view.setUint32(10008, 55, true);
  view.setFloat32(10012, 10, true);
  view.setFloat32(10016, 20, true);
  view.setUint32(10020, 3, true);
  view.setUint32(10024, 56, true);
  view.setUint32(11792, 7, true);
  view.setUint32(11796, 2, true);
  view.setUint32(12052, 1_234, true);
  view.setUint32(12056, 50_000, true);
  view.setUint32(12060, 4, true);
  view.setUint32(12064, 1, true);
  view.setUint32(12068, 1, true);
  view.setUint32(12072, 1, true);
  view.setUint32(12076, 1, true);
  view.setUint32(12080, 1, true);
  view.setUint32(12084, 0xffff_ffff, true);
  view.setUint32(12088, 20, true);
  view.setUint32(12092, 1, true);
  view.setUint32(12516, 500, true);
  view.setUint32(12520, 0, true);
  view.setUint32(12524, 1, true);
  view.setUint32(12528, 0, true);
  view.setUint32(12532, 123, true);
  view.setUint32(12536, 9, true);
  view.setUint32(12540, 100, true);
  view.setUint32(12544, 0x01_0a_0001, true);
  view.setUint32(12548, 456, true);
  view.setUint32(12552, 0, true);
  view.setUint32(12556, 5, true);
  view.setUint32(12560, 0, true);
  view.setUint32(12564, 0xfe, true);
  view.setUint32(12568, 3, true);
  view.setUint32(12572, 2, true);
  view.setUint32(12576, 7 | (2 << 8) | (3 << 12) | (4 << 16) | (5 << 20), true);
  view.setUint32(45284, 1 << 1, true);
  view.setUint32(45288, 1, true);
  view.setUint32(45292, 1, true);
  view.setUint32(45296, 1, true);
  view.setUint32(45300, 1, true);
  view.setUint32(45316, 2, true);
  view.setUint32(45320, 3, true);
  view.setUint32(45324, 1, true);
  view.setUint32(45328, 9, true);
  view.setUint32(45332, 1_200, true);
  view.setUint32(45336, 0xffff_ffff, true);
  view.setUint32(45340, 1_000, true);
  view.setUint32(45344, 10, true);
  view.setUint32(45348, 50, true);
  for (let index = 0; index < 7; index += 1) {
    view.setUint32(45352 + index * 4, index + 1, true);
  }
  view.setUint32(45380, 0, true);
  view.setUint32(45384, 1, true);
  view.setUint32(45388, 1, true);
  view.setUint32(45392, 0xffff_ffff, true);
  view.setUint32(45396, 0xffff_ffff, true);
  for (let category = 0; category < 6; category += 1) {
    view.setUint32(47940 + category * 4, 25, true);
    const mapId = 55 + category;
    const word = Math.floor(mapId / 32);
    const bit = mapId % 32;
    view.setUint32(47964 + category * 128 + word * 4, 2 ** bit, true);
  }
  view.setUint32(48732, 1, true);
  view.setUint32(48736, 3, true);
  view.setFloat32(48740, 1.25, true);
  view.setFloat32(48744, 0.25, true);
  view.setFloat32(48748, 1_000, true);
  view.setFloat32(48752, 5_000, true);
  view.setFloat32(48756, 110, true);
  view.setFloat32(48760, -260, true);
  view.setFloat32(48764, -50, true);
  view.setFloat32(48768, 100, true);
  view.setFloat32(48772, -250, true);
  view.setFloat32(48776, 3, true);
  view.setFloat32(48780, 1.2, true);
  view.setUint32(48784, 3, true);
  view.setUint32(48788, 2_222, true);
  view.setUint32(48792, 3_333, true);
  view.setUint32(48796, 2, true);
  view.setUint32(48800, 1, true);
  view.setUint32(48808, 700, true);
  view.setUint32(48812, 5, true);
  view.setUint32(48816, 701, true);
  view.setUint32(48820, 1, true);
  view.setUint32(48936, 800, true);
  view.setUint32(48940, 2, true);
  view.setUint32(49068, 2, true);
  view.setUint32(49072, 2, true);
  view.setUint32(49076, 2, true);
  view.setUint32(49080, 1, true);
  view.setUint32(49084, 1, true);
  view.setUint32(49088, 0, true);
  view.setUint32(49092, 0xffff_ffff, true);
  view.setUint32(49096, 0, true);
  view.setUint32(49100, 0x1111, true);
  view.setUint32(49104, 3, true);
  view.setUint32(49108, 4, true);
  view.setUint32(49112, 5, true);
  view.setUint32(49116, 0x4, true);
  view.setUint32(49120, 9, true);
  view.setFloat32(49124, 10, true);
  view.setFloat32(49128, 100, true);
  view.setFloat32(49132, 200, true);
  view.setFloat32(49136, 20, true);
  view.setUint32(49140, 0, true);
  view.setUint32(49144, 1, true);
  view.setUint32(49148, 0, true);
  view.setUint32(49152, 2, true);
  view.setUint32(49156, 0x2222, true);
  view.setUint32(49160, 1, true);
  view.setUint32(49164, 7, true);
  view.setUint32(49168, 8, true);
  view.setUint32(49172, 0x204, true);
  view.setUint32(56256, 2, true);
  view.setUint32(56260, 2, true);
  view.setUint32(56264, 900, true);
  view.setUint32(56268, 901, true);
  view.setUint32(56776, 1, true);
  view.setUint32(56780, 20, true);
  view.setUint32(56784, 1_337_500, true);
  view.setUint32(56788, 1_000, true);
  view.setUint32(56792, 5_000, true);
  view.setUint32(56796, 10_000, true);
  view.setUint32(56800, 2_000, true);
  view.setUint32(56804, 6_000, true);
  view.setUint32(56808, 10_000, true);
  view.setUint32(56812, 100, true);
  view.setUint32(56816, 1_000, true);
  view.setUint32(56820, 15_000, true);
  view.setUint32(56824, 500, true);
  view.setUint32(56828, 2_500, true);
  view.setUint32(56832, 10_000, true);
  view.setUint32(56836, 5, true);
  view.setUint32(56840, 125, true);
  view.setUint32(56848, 2, true);
  view.setUint32(56852, 2, true);
  view.setUint32(56856, 4, true);
  view.setUint32(56860, 7, true);
  view.setUint32(56864, 111, true);
  view.setUint32(56868, 222, true);
  view.setUint32(58912, 1 << 3, true);
  view.setUint32(58924, 1 << 4, true);
  view.setUint32(59344, 1 << 3, true);
  view.setUint32(59368, 1 << 8, true);
  return buffer;
}

/**
 * A cursor block. `pixel` fills every pixel word, so a payload read can be
 * checked without spelling out a thousand of them.
 *
 * @param {Record<string, number>} [overrides]
 */
function cursorBlock(overrides = {}) {
  const fields = {
    magic: CURSOR_MAGIC,
    abi: CURSOR_ABI,
    byteLength: COMPANION_CURSOR_BYTES,
    sequence: 4,
    flags: CURSOR_VALID,
    generation: 3,
    width: 32,
    height: 32,
    hotspotX: 5,
    hotspotY: 6,
    pixelHash: 0xdeadbeef,
    reserved: 0,
    pixel: 0x11223344,
    ...overrides,
  };
  const buffer = new ArrayBuffer(COMPANION_CURSOR_BYTES);
  const view = new DataView(buffer);
  view.setUint32(0, fields.magic, true);
  view.setUint16(4, fields.abi, true);
  view.setUint16(6, fields.byteLength, true);
  view.setUint32(8, fields.sequence, true);
  view.setUint32(12, fields.flags, true);
  view.setUint32(16, fields.generation, true);
  view.setUint32(20, fields.width, true);
  view.setUint32(24, fields.height, true);
  view.setUint32(28, fields.hotspotX, true);
  view.setUint32(32, fields.hotspotY, true);
  view.setUint32(36, fields.pixelHash, true);
  for (let offset = 40; offset < 64; offset += 4) {
    view.setUint32(offset, fields.reserved, true);
  }
  for (let offset = 64; offset < COMPANION_CURSOR_BYTES; offset += 4) {
    view.setUint32(offset, fields.pixel, true);
  }
  return buffer;
}

/** @param {Record<string, number>} [overrides] */
function readCursor(overrides) {
  return readCompanionCursorHeader(cursorBlock(overrides), 0);
}

describe('companion snapshot', () => {
  it('decodes a published state into named values', () => {
    const state = read();
    assert.equal(state.status, 'ready');
    assert.equal(state.mapId, 248);
    assert.equal(state.instanceName, 'Outpost');
    assert.equal(state.playerId, 1);
    assert.equal(state.targetValid, true);
    assert.equal(state.targetKind, 'Living');
    assert.equal(state.distance, 300);
    assert.equal(state.rangeName, 'Earshot');
    // Frozen, because it is handed to the readout and put on `window` — and a
    // consumer that could edit it would be editing everyone's copy.
    assert.ok(Object.isFrozen(state));
  });

  it('decodes every bounded nested domain', () => {
    const state = readCompanionSnapshot(domainSnapshot(), 0);
    assert.equal(state.status, 'ready');
    assert.equal(state.party.id, 3);
    assert.equal(state.party.leader, true);
    assert.deepEqual(state.party.players, [{
      loginNumber: 42,
      calledTargetId: 7,
      state: 3,
      connected: true,
      ticked: true,
    }]);
    assert.deepEqual(state.party.heroes, [{
      agentId: 8,
      ownerPlayerId: 42,
      heroId: 5,
      level: 20,
    }]);
    assert.deepEqual(state.party.henchmen, [{
      agentId: 9,
      profession: 6,
      level: 20,
    }]);
    assert.deepEqual(state.party.allies, [10]);
    assert.equal(state.skillbar.agentId, 1);
    assert.equal(state.skillbar.disabledMask, 4);
    assert.equal(state.skillbar.castCount, 1);
    assert.equal(state.skillbar.casting, true);
    assert.equal(state.skillbar.skills.length, 8);
    assert.equal(state.skillbar.skills[0].skillId, 100);
    assert.equal(state.skillbar.skills[0].recharge, 500);
    assert.equal(state.skillbar.skills[0].disabled, false);
    assert.equal(state.skillbar.skills[2].disabled, true);
    assert.deepEqual(state.effects.buffs, [{
      skillId: 200,
      buffId: 300,
      targetAgentId: 1,
    }]);
    assert.deepEqual(state.effects.effects, [{
      skillId: 201,
      attributeLevel: 12,
      effectId: 301,
      agentId: 7,
      duration: 12.5,
      timestamp: 400,
    }]);
    assert.equal(state.effects.buffsTruncated, false);
    assert.equal(state.effects.effectsTruncated, false);
    assert.equal(state.agents.total, 1);
    assert.equal(state.agents.truncated, false);
    assert.deepEqual(state.agents.agents, [{
      agentId: 1,
      typeBits: 0xdb,
      kind: 'Living',
      playerNumber: 42,
      primary: 7,
      secondary: 0,
      level: 20,
      health: 0.75,
      rotation: 1.25,
      x: 100,
      y: -250.5,
      z: 3,
      modelState: 65,
      effects: 0,
      allegiance: 1,
      isLiving: true,
      isItem: false,
      isGadget: false,
      isDead: false,
      isMoving: false,
      isAttacking: false,
      isKnockedDown: false,
      isCasting: true,
    }]);
    assert.equal(state.quests.activeQuestId, 44);
    assert.deepEqual(state.quests.quests, [{
      questId: 44,
      logState: 0x22,
      mapFrom: 55,
      markerX: 10,
      markerY: 20,
      markerPlane: 3,
      hasMarker: true,
      mapTo: 56,
      completed: true,
      currentMission: false,
      primary: true,
      areaPrimary: false,
    }]);
    assert.deepEqual(state.quests.missionObjectives, [{
      objectiveId: 7,
      type: 2,
    }]);
    assert.equal(state.inventory.goldCharacter, 1_234);
    assert.equal(state.inventory.goldStorage, 50_000);
    assert.equal(state.inventory.storagePanesUnlocked, 4);
    assert.equal(state.inventory.total, 1);
    assert.deepEqual(state.inventory.bags, [{
      bagId: 1,
      bagType: 1,
      kind: 'Inventory',
      containerItem: 0xffff_ffff,
      capacity: 20,
      itemCount: 1,
      isInventory: true,
      isEquipped: false,
      isNotCollected: false,
      isStorage: false,
      isMaterialStorage: false,
    }]);
    assert.equal(state.inventory.items[0].itemId, 500);
    assert.equal(state.inventory.items[0].typeName, 'Usable');
    assert.equal(state.inventory.items[0].quantity, 5);
    assert.equal(state.inventory.items[0].customized, true);
    assert.equal(state.inventory.items[0].modifierCount, 2);
    assert.equal(state.inventory.items[0].dyeTint, 7);
    assert.equal(state.inventory.items[0].dye4, 5);
    assert.equal(state.inventory.items[0].isIdentified, true);
    assert.equal(state.inventory.items[0].isStackable, true);
    assert.equal(state.inventory.items[0].isUsable, true);
    assert.equal(state.inventory.items[0].profession, 0xfe);
    assert.equal(state.inventory.items[0].materialSalvageable, true);
    assert.equal(state.social.playerStatusName, 'Online');
    assert.equal(state.social.friends.total, 1);
    assert.deepEqual(state.social.friends.entries, [{
      slot: 0,
      type: 1,
      typeName: 'Friend',
      status: 1,
      statusName: 'Online',
      friendId: 0xffff_ffff,
      zoneId: 0xffff_ffff,
      isOnline: true,
    }]);
    assert.equal(state.social.guild.index, 2);
    assert.equal(state.social.guild.faction, 0xffff_ffff);
    assert.equal(state.social.guild.factionName, 'Unknown');
    assert.equal(state.social.guild.rosterTotal, 50);
    assert.equal(state.social.guild.cape.trim, 7);
    assert.deepEqual(state.completion.normalMode.completedMissions, [55]);
    assert.deepEqual(state.completion.normalMode.completedBonuses, [56]);
    assert.deepEqual(state.completion.hardMode.completedMissions, [57]);
    assert.deepEqual(state.completion.hardMode.completedBonuses, [58]);
    assert.deepEqual(state.completion.unlockedMaps, [59]);
    assert.deepEqual(state.completion.vanquishedAreas, [60]);
    assert.equal(state.camera.lookAtAgentId, 1);
    assert.equal(state.camera.modeName, 'Unlocked');
    assert.equal(state.camera.unlocked, true);
    assert.equal(state.camera.yaw, 1.25);
    assert.equal(state.camera.pitch, 0.25);
    assert.deepEqual(state.camera.position, { x: 110, y: -260, z: -50 });
    assert.deepEqual(state.camera.lookAt, { x: 100, y: -250, z: 3 });
    assert.ok(Math.abs(state.camera.fieldOfView - 1.2) < 0.000001);
    assert.ok(Number.isFinite(state.camera.currentYaw));
    assert.ok(Number.isFinite(state.camera.renderFieldOfView));
    assert.equal(state.trade.statusName, 'OfferSent');
    assert.equal(state.trade.open, true);
    assert.equal(state.trade.initiated, true);
    assert.equal(state.trade.offerSent, true);
    assert.equal(state.trade.accepted, false);
    assert.deepEqual(state.trade.player, {
      gold: 2_222,
      itemsTruncated: false,
      items: [
        { slot: 1, itemId: 700, quantity: 5 },
        { slot: 2, itemId: 701, quantity: 1 },
      ],
    });
    assert.deepEqual(state.trade.partner, {
      gold: 3_333,
      itemsTruncated: false,
      items: [{ slot: 1, itemId: 800, quantity: 2 }],
    });
    assert.equal(state.ui.total, 2);
    assert.equal(state.ui.createdTotal, 2);
    assert.equal(state.ui.visibleTotal, 1);
    assert.equal(state.ui.truncated, false);
    assert.deepEqual(state.ui.frames, [
      {
        frameId: 0,
        parentId: null,
        childOffsetId: 0,
        frameHash: 0x1111,
        visibilityFlags: 3,
        type: 4,
        templateType: 5,
        state: 0x4,
        created: true,
        destroying: false,
        disabled: false,
        hidden: false,
        locallyVisible: true,
        positionValid: true,
        positionFlags: 9,
        position: { left: 10, bottom: 100, right: 200, top: 20 },
      },
      {
        frameId: 1,
        parentId: 0,
        childOffsetId: 2,
        frameHash: 0x2222,
        visibilityFlags: 1,
        type: 7,
        templateType: 8,
        state: 0x204,
        created: true,
        destroying: false,
        disabled: false,
        hidden: true,
        locallyVisible: false,
        positionValid: false,
        positionFlags: 0,
        position: { left: 0, bottom: 0, right: 0, top: 0 },
      },
    ]);
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
    assert.ok(Object.isFrozen(state.party.players));
    assert.ok(Object.isFrozen(state.skillbar.skills));
    assert.ok(Object.isFrozen(state.effects.effects));
    assert.ok(Object.isFrozen(state.agents.agents));
    assert.ok(Object.isFrozen(state.quests.quests));
    assert.ok(Object.isFrozen(state.inventory.items));
    assert.ok(Object.isFrozen(state.social.friends.entries));
    assert.ok(Object.isFrozen(state.social.guild.cape));
    assert.ok(Object.isFrozen(state.completion.normalMode.completedMissions));
    assert.ok(Object.isFrozen(state.completion));
    assert.ok(Object.isFrozen(state.camera.position));
    assert.ok(Object.isFrozen(state.camera));
    assert.ok(Object.isFrozen(state.trade.player.items));
    assert.ok(Object.isFrozen(state.trade.player));
    assert.ok(Object.isFrozen(state.trade));
    assert.ok(Object.isFrozen(state.ui.frames));
    assert.ok(Object.isFrozen(state.ui.frames[0].position));
    assert.ok(Object.isFrozen(state.ui));
    assert.ok(Object.isFrozen(state.merchant.itemIds));
    assert.ok(Object.isFrozen(state.merchant));
    assert.ok(Object.isFrozen(state.progression.factions.kurzick));
    assert.ok(Object.isFrozen(state.progression.factions));
    assert.ok(Object.isFrozen(state.progression.skillPoints));
    assert.ok(Object.isFrozen(state.progression));
    assert.ok(Object.isFrozen(state.skillUnlocks.learnableSkillIds));
    assert.ok(Object.isFrozen(state.skillUnlocks.characterLearnedSkillIds));
    assert.ok(Object.isFrozen(state.skillUnlocks.accountUnlockedSkillIds));
    assert.ok(Object.isFrozen(state.skillUnlocks));
  });

  it('normalizes the GWCA no-map-marker sentinel', () => {
    const buffer = domainSnapshot();
    const view = new DataView(buffer);
    view.setFloat32(10012, 0, true);
    view.setFloat32(10016, 0, true);
    view.setUint32(10020, 0xffff_ffff, true);

    const [quest] = readCompanionSnapshot(buffer, 0).quests.quests;
    assert.deepEqual(
      {
        markerX: quest.markerX,
        markerY: quest.markerY,
        markerPlane: quest.markerPlane,
        hasMarker: quest.hasMarker,
      },
      {
        markerX: 0,
        markerY: 0,
        markerPlane: 0,
        hasMarker: false,
      },
    );
  });

  it('accepts a complete bounded inventory page with an explicit remainder', () => {
    const buffer = domainSnapshot();
    const view = new DataView(buffer);
    view.setUint32(12048, 1, true);
    view.setUint32(12064, 3, true);
    view.setUint32(12068, 512, true);
    view.setUint32(12072, 513, true);
    for (let index = 0; index < 3; index += 1) {
      const offset = 12076 + index * 20;
      view.setUint32(offset, index + 1, true);
      view.setUint32(offset + 4, 1, true);
      view.setUint32(offset + 8, 0, true);
      view.setUint32(offset + 12, index < 2 ? 256 : 1, true);
      view.setUint32(offset + 16, index < 2 ? 256 : 1, true);
    }
    for (let index = 0; index < 512; index += 1) {
      const offset = 12516 + index * 64;
      view.setUint32(offset, index + 1, true);
      view.setUint32(offset + 4, 0, true);
      view.setUint32(offset + 8, index < 256 ? 1 : 2, true);
      view.setUint32(offset + 12, index % 256, true);
      view.setUint32(offset + 16, 1, true);
      view.setUint32(offset + 20, 9, true);
      view.setUint32(offset + 24, 0, true);
      view.setUint32(offset + 28, 0, true);
      view.setUint32(offset + 32, 1, true);
      view.setUint32(offset + 36, 0, true);
      view.setUint32(offset + 40, 1, true);
      view.setUint32(offset + 44, 0, true);
      view.setUint32(offset + 48, 0xff, true);
      view.setUint32(offset + 52, 0, true);
      view.setUint32(offset + 56, 0, true);
      view.setUint32(offset + 60, 0, true);
    }

    const state = readCompanionSnapshot(buffer, 0);
    assert.equal(state.status, 'ready');
    assert.equal(state.inventory.itemsTruncated, true);
    assert.equal(state.inventory.items.length, 512);
    assert.equal(state.inventory.total, 513);
    assert.equal(state.inventory.bags[2].itemCount, 1);
  });

  it('normalizes a closed trade to an empty offer', () => {
    const buffer = domainSnapshot();
    const view = new DataView(buffer);
    view.setUint32(48784, 0, true);
    view.setUint32(48788, 0, true);
    view.setUint32(48792, 0, true);
    view.setUint32(48796, 0, true);
    view.setUint32(48800, 0, true);
    for (let offset = 48808; offset < 49064; offset += 4) {
      view.setUint32(offset, 0, true);
    }
    const state = readCompanionSnapshot(buffer, 0);
    assert.deepEqual(state.trade, {
      flags: 0,
      statusName: 'Closed',
      open: false,
      initiated: false,
      offerSent: false,
      accepted: false,
      player: { gold: 0, itemsTruncated: false, items: [] },
      partner: { gold: 0, itemsTruncated: false, items: [] },
    });
  });

  it('refuses partial or inconsistent nested records', () => {
    const party = domainSnapshot();
    new DataView(party).setUint32(244, 21, true);
    assert.equal(readCompanionSnapshot(party, 0).reason, 'corrupt');

    const heroOwner = domainSnapshot();
    new DataView(heroOwner).setUint32(236, 43, true);
    assert.equal(readCompanionSnapshot(heroOwner, 0).reason, 'corrupt');

    const skillbar = domainSnapshot();
    new DataView(skillbar).setUint32(720, 100_001, true);
    assert.equal(readCompanionSnapshot(skillbar, 0).reason, 'corrupt');

    const disabledMask = domainSnapshot();
    new DataView(disabledMask).setUint32(700, 1 << 8, true);
    assert.equal(readCompanionSnapshot(disabledMask, 0).reason, 'corrupt');

    const castCount = domainSnapshot();
    new DataView(castCount).setUint32(704, 65, true);
    assert.equal(readCompanionSnapshot(castCount, 0).reason, 'corrupt');

    const duplicateEffect = domainSnapshot();
    const duplicateView = new DataView(duplicateEffect);
    duplicateView.setUint32(880, 2, true);
    for (let offset = 0; offset < 24; offset += 4) {
      duplicateView.setUint32(1292 + offset, duplicateView.getUint32(1268 + offset, true), true);
    }
    assert.equal(readCompanionSnapshot(duplicateEffect, 0).reason, 'corrupt');

    const unusedBuff = domainSnapshot();
    new DataView(unusedBuff).setUint32(896, 1, true);
    assert.equal(readCompanionSnapshot(unusedBuff, 0).reason, 'corrupt');

    const badTruncation = domainSnapshot();
    new DataView(badTruncation).setUint32(872, 1, true);
    assert.equal(readCompanionSnapshot(badTruncation, 0).reason, 'corrupt');

    const invalidAgent = domainSnapshot();
    new DataView(invalidAgent).setUint32(2836, 256, true);
    assert.equal(readCompanionSnapshot(invalidAgent, 0).reason, 'corrupt');

    const absentActiveQuest = domainSnapshot();
    new DataView(absentActiveQuest).setUint32(9984, 45, true);
    assert.equal(readCompanionSnapshot(absentActiveQuest, 0).reason, 'corrupt');

    const unusedQuest = domainSnapshot();
    new DataView(unusedQuest).setUint32(10028, 1, true);
    assert.equal(readCompanionSnapshot(unusedQuest, 0).reason, 'corrupt');

    const badQuestTruncation = domainSnapshot();
    new DataView(badQuestTruncation).setUint32(9988, 1, true);
    assert.equal(readCompanionSnapshot(badQuestTruncation, 0).reason, 'corrupt');

    const badBagType = domainSnapshot();
    new DataView(badBagType).setUint32(12080, 4, true);
    assert.equal(readCompanionSnapshot(badBagType, 0).reason, 'corrupt');

    const badItemSlot = domainSnapshot();
    new DataView(badItemSlot).setUint32(12528, 20, true);
    assert.equal(readCompanionSnapshot(badItemSlot, 0).reason, 'corrupt');

    const badInventoryTruncation = domainSnapshot();
    new DataView(badInventoryTruncation).setUint32(12048, 1, true);
    assert.equal(readCompanionSnapshot(badInventoryTruncation, 0).reason, 'corrupt');

    const unusedInventoryItem = domainSnapshot();
    new DataView(unusedInventoryItem).setUint32(12580, 1, true);
    assert.equal(readCompanionSnapshot(unusedInventoryItem, 0).reason, 'corrupt');

    const badFriendCount = domainSnapshot();
    new DataView(badFriendCount).setUint32(45300, 2, true);
    assert.equal(readCompanionSnapshot(badFriendCount, 0).reason, 'corrupt');

    const badFriendStatus = domainSnapshot();
    new DataView(badFriendStatus).setUint32(45388, 5, true);
    assert.equal(readCompanionSnapshot(badFriendStatus, 0).reason, 'corrupt');

    const absentGuildData = domainSnapshot();
    const absentGuildView = new DataView(absentGuildData);
    absentGuildView.setUint32(45284, 0, true);
    assert.equal(readCompanionSnapshot(absentGuildData, 0).reason, 'corrupt');

    const unusedFriend = domainSnapshot();
    new DataView(unusedFriend).setUint32(45400, 1, true);
    assert.equal(readCompanionSnapshot(unusedFriend, 0).reason, 'corrupt');

    const oversizedCompletion = domainSnapshot();
    new DataView(oversizedCompletion).setUint32(47940, 33, true);
    assert.equal(readCompanionSnapshot(oversizedCompletion, 0).reason, 'corrupt');

    const staleCompletion = domainSnapshot();
    const staleCompletionView = new DataView(staleCompletion);
    staleCompletionView.setUint32(47940, 1, true);
    staleCompletionView.setUint32(47968, 1, true);
    assert.equal(readCompanionSnapshot(staleCompletion, 0).reason, 'corrupt');

    const absentCompletion = domainSnapshot();
    const absentCompletionView = new DataView(absentCompletion);
    absentCompletionView.setUint32(
      12,
      absentCompletionView.getUint32(12, true) & ~FLAG_COMPLETION,
      true,
    );
    assert.equal(readCompanionSnapshot(absentCompletion, 0).reason, 'corrupt');

    const badCameraMode = domainSnapshot();
    new DataView(badCameraMode).setUint32(48736, 10, true);
    assert.equal(readCompanionSnapshot(badCameraMode, 0).reason, 'corrupt');

    const badCameraFov = domainSnapshot();
    new DataView(badCameraFov).setFloat32(48780, Number.NaN, true);
    assert.equal(readCompanionSnapshot(badCameraFov, 0).reason, 'corrupt');

    const absentCamera = domainSnapshot();
    const absentCameraView = new DataView(absentCamera);
    absentCameraView.setUint32(
      12,
      absentCameraView.getUint32(12, true) & ~FLAG_CAMERA,
      true,
    );
    assert.equal(readCompanionSnapshot(absentCamera, 0).reason, 'corrupt');

    const unknownTradeFlag = domainSnapshot();
    new DataView(unknownTradeFlag).setUint32(48784, 8, true);
    assert.equal(readCompanionSnapshot(unknownTradeFlag, 0).reason, 'corrupt');

    const duplicateTradeItem = domainSnapshot();
    new DataView(duplicateTradeItem).setUint32(48816, 700, true);
    assert.equal(readCompanionSnapshot(duplicateTradeItem, 0).reason, 'corrupt');

    const badTradeTruncation = domainSnapshot();
    new DataView(badTradeTruncation).setUint32(48804, 1, true);
    assert.equal(readCompanionSnapshot(badTradeTruncation, 0).reason, 'corrupt');

    const staleClosedTrade = domainSnapshot();
    new DataView(staleClosedTrade).setUint32(48784, 0, true);
    assert.equal(readCompanionSnapshot(staleClosedTrade, 0).reason, 'corrupt');

    const unusedTradeItem = domainSnapshot();
    new DataView(unusedTradeItem).setUint32(48824, 1, true);
    assert.equal(readCompanionSnapshot(unusedTradeItem, 0).reason, 'corrupt');

    const absentTrade = domainSnapshot();
    const absentTradeView = new DataView(absentTrade);
    absentTradeView.setUint32(
      12,
      absentTradeView.getUint32(12, true) & ~FLAG_TRADE,
      true,
    );
    assert.equal(readCompanionSnapshot(absentTrade, 0).reason, 'corrupt');

    const unknownUiRecordFlag = domainSnapshot();
    new DataView(unknownUiRecordFlag).setUint32(49084, 2, true);
    assert.equal(readCompanionSnapshot(unknownUiRecordFlag, 0).reason, 'corrupt');

    const duplicateUiFrame = domainSnapshot();
    new DataView(duplicateUiFrame).setUint32(49144, 0, true);
    assert.equal(readCompanionSnapshot(duplicateUiFrame, 0).reason, 'corrupt');

    const absentUiParent = domainSnapshot();
    new DataView(absentUiParent).setUint32(49148, 7, true);
    assert.equal(readCompanionSnapshot(absentUiParent, 0).reason, 'corrupt');

    const badUiGeometry = domainSnapshot();
    new DataView(badUiGeometry).setFloat32(49124, Number.NaN, true);
    assert.equal(readCompanionSnapshot(badUiGeometry, 0).reason, 'corrupt');

    const badUiTotals = domainSnapshot();
    new DataView(badUiTotals).setUint32(49080, 3, true);
    assert.equal(readCompanionSnapshot(badUiTotals, 0).reason, 'corrupt');

    const inconsistentUiTotals = domainSnapshot();
    new DataView(inconsistentUiTotals).setUint32(49076, 1, true);
    assert.equal(
      readCompanionSnapshot(inconsistentUiTotals, 0).reason,
      'corrupt',
    );

    const absentUi = domainSnapshot();
    const absentUiView = new DataView(absentUi);
    absentUiView.setUint32(
      12,
      absentUiView.getUint32(12, true) & ~FLAG_UI,
      true,
    );
    assert.equal(readCompanionSnapshot(absentUi, 0).reason, 'corrupt');

    const invalidMerchantItem = domainSnapshot();
    new DataView(invalidMerchantItem).setUint32(56264, 0, true);
    assert.equal(readCompanionSnapshot(invalidMerchantItem, 0).reason, 'corrupt');

    const badMerchantTruncation = domainSnapshot();
    new DataView(badMerchantTruncation).setUint32(56252, 1, true);
    assert.equal(readCompanionSnapshot(badMerchantTruncation, 0).reason, 'corrupt');

    const unusedMerchantItem = domainSnapshot();
    new DataView(unusedMerchantItem).setUint32(56272, 902, true);
    assert.equal(readCompanionSnapshot(unusedMerchantItem, 0).reason, 'corrupt');

    const absentMerchant = domainSnapshot();
    const absentMerchantView = new DataView(absentMerchant);
    absentMerchantView.setUint32(
      12,
      absentMerchantView.getUint32(12, true) & ~FLAG_MERCHANT,
      true,
    );
    assert.equal(readCompanionSnapshot(absentMerchant, 0).reason, 'corrupt');

    const overCapFaction = domainSnapshot();
    new DataView(overCapFaction).setUint32(56788, 10_001, true);
    assert.equal(readCompanionSnapshot(overCapFaction, 0).reason, 'corrupt');

    const reversedSkillPoints = domainSnapshot();
    new DataView(reversedSkillPoints).setUint32(56836, 126, true);
    assert.equal(readCompanionSnapshot(reversedSkillPoints, 0).reason, 'corrupt');

    const absentProgression = domainSnapshot();
    const absentProgressionView = new DataView(absentProgression);
    absentProgressionView.setUint32(
      12,
      absentProgressionView.getUint32(12, true) & ~FLAG_PROGRESSION,
      true,
    );
    assert.equal(readCompanionSnapshot(absentProgression, 0).reason, 'corrupt');

    const badLearnableTotal = domainSnapshot();
    new DataView(badLearnableTotal).setUint32(56852, 1, true);
    assert.equal(readCompanionSnapshot(badLearnableTotal, 0).reason, 'corrupt');

    const trailingLearnedWord = domainSnapshot();
    new DataView(trailingLearnedWord).setUint32(58928, 1, true);
    assert.equal(readCompanionSnapshot(trailingLearnedWord, 0).reason, 'corrupt');

    const absentSkillUnlocks = domainSnapshot();
    const absentSkillUnlocksView = new DataView(absentSkillUnlocks);
    absentSkillUnlocksView.setUint32(
      12,
      absentSkillUnlocksView.getUint32(12, true) & ~FLAG_SKILL_UNLOCKS,
      true,
    );
    assert.equal(readCompanionSnapshot(absentSkillUnlocks, 0).reason, 'corrupt');
  });

  // The seqlock, which is the whole reason this can be read on the animation
  // frame while the game is mid-update. An odd sequence means a publish is in
  // flight; the reader's job is to come back next frame, not to read anyway.
  it('refuses a block that is being written', () => {
    assert.deepEqual(read({ sequence: 3 }), { status: 'waiting', reason: 'writing' });
  });

  // Everything that says "this is not the block you think it is". Grouped
  // because they are one decision: the header does not describe this ABI, so
  // nothing behind it can be trusted to mean what this file thinks it means.
  it('refuses a header from anything but this ABI', () => {
    for (const overrides of [
      { magic: MAGIC + 1 },
      { abi: SNAPSHOT_ABI + 1 },
      { byteLength: COMPANION_SNAPSHOT_BYTES - 4 },
      // A flag this build has no name for is either a newer companion or not a
      // companion at all.
      { flags: FLAG_READY | FLAG_PLAYER | (1 << 18) },
    ]) {
      assert.equal(read(overrides).reason, 'snapshot', JSON.stringify(overrides));
    }
  });

  it('refuses a pointer that is not wholly inside the memory it was given', () => {
    const buffer = snapshot();
    assert.equal(readCompanionSnapshot(buffer, 4).reason, 'memory');
    assert.equal(readCompanionSnapshot(buffer, -1).reason, 'memory');
    assert.equal(readCompanionSnapshot(buffer, 1.5).reason, 'memory');
    assert.equal(readCompanionSnapshot(null, 0).reason, 'memory');
  });

  // Loading and not-in-a-game are states the decoder reports rather than
  // failures: the game is running, there is simply nothing to read out of it
  // yet. They carry the tick count so a caller can still tell a live client
  // from a stalled one.
  it('reports a map change and a session with no game as their own states', () => {
    const loading = read({ flags: FLAG_LOADING });
    assert.equal(loading.reason, 'loading');
    assert.equal(loading.tickCount, 1234);

    const waiting = read({ flags: FLAG_READY });
    assert.equal(waiting.reason, 'game');
    assert.equal(waiting.tickCount, 1234);

    // Loading is a whole state, not a modifier on a reading.
    assert.equal(read({ flags: FLAG_LOADING | FLAG_PLAYER }).reason, 'corrupt');
  });

  // The fields the companion already checked, checked again. A block that
  // passes the header and then says the player is at 1e30 is not a snapshot
  // that needs rounding, it is a block that was written by something else.
  it('refuses a reading whose values cannot have come from a game', () => {
    for (const overrides of [
      { mapId: 0 },
      { mapId: 2_001 },
      { instanceType: 2 },
      { playerId: 0 },
      { playerX: Number.NaN },
      { playerY: 1e30 },
    ]) {
      assert.equal(read(overrides).reason, 'corrupt', JSON.stringify(overrides));
    }
  });

  // Both directions. A flagged target has to be complete, and an unflagged one
  // has to be absent — a leftover target under a cleared flag is the shape a
  // half-finished publish would have, and it is exactly what would be rendered
  // as a distance to something that is no longer there.
  it('refuses a target that is neither wholly there nor wholly absent', () => {
    for (const overrides of [
      { targetId: 0 },
      { agentTypeBits: 0 },
      { targetX: Number.POSITIVE_INFINITY },
      { distance: -1 },
      { rangeBand: 0 },
      { rangeBand: 9 },
    ]) {
      assert.equal(read(overrides).reason, 'corrupt', JSON.stringify(overrides));
    }
    const clean = { flags: FLAG_READY | FLAG_PLAYER };
    for (const leftover of [
      { targetId: 7 },
      { agentTypeBits: 0xdb },
      { targetX: 1 },
      { distance: 1 },
      { rangeBand: 1 },
    ]) {
      const state = read({
        ...clean,
        targetId: 0,
        agentTypeBits: 0,
        targetX: 0,
        targetY: 0,
        distance: 0,
        rangeBand: 0,
        ...leftover,
      });
      assert.equal(state.reason, 'corrupt', JSON.stringify(leftover));
    }
  });

  // A type word the companion accepted but this page has not seen against a
  // live target of a known kind. Reported under a name that claims nothing
  // rather than guessed at — the alternative is a label that is wrong on
  // screen and looks authoritative.
  it('names only the agent kind it has actually seen', () => {
    assert.equal(read({ agentTypeBits: 0xdb }).targetKind, 'Living');
    assert.equal(read({ agentTypeBits: 0x400 }).targetKind, 'Unknown');
  });
});

describe('companion cursor', () => {
  it('decodes a published cursor header', () => {
    const header = readCursor();
    assert.equal(header.status, 'ready');
    assert.equal(header.generation, 3);
    assert.equal(header.hotspotX, 5);
    assert.equal(header.hotspotY, 6);
    assert.equal(header.pixelHash, 0xdeadbeef);
    assert.equal(header.hidden, false);
  });

  it('refuses a cursor header from anything but this ABI', () => {
    for (const overrides of [
      { magic: CURSOR_MAGIC + 1 },
      { abi: CURSOR_ABI + 1 },
      { byteLength: COMPANION_CURSOR_BYTES - 4 },
      { flags: CURSOR_VALID | (1 << 8) },
      // The companion zeroes the reserved words once and never writes them
      // again, so anything in them is somebody else writing into this region.
      { reserved: 1 },
    ]) {
      assert.equal(readCursor(overrides).reason, 'cursor', JSON.stringify(overrides));
    }
  });

  // The bitmap is 32x32 because that is what the companion refuses to publish
  // anything else as, and because the readback it fills from uses a fixed
  // pitch. A block claiming any other size did not come from that path.
  it('refuses geometry the companion cannot have published', () => {
    for (const overrides of [
      { generation: 0 },
      { width: 16 },
      { height: 64 },
      { hotspotX: 32 },
      { hotspotY: 99 },
    ]) {
      assert.equal(readCursor(overrides).reason, 'corrupt', JSON.stringify(overrides));
    }
  });

  // Losing the cursor is not an error, and it is not a torn read either: the
  // companion clears VALID with a header-only publish, deliberately leaving
  // the last good bitmap in place. So `invalid` is a state with a generation
  // the consumer can compare against, not a `waiting` it should retry.
  it('reports a cleared cursor as a state, with the stale geometry zeroed', () => {
    const gone = readCursor({ flags: 0, hotspotX: 5, hotspotY: 6 });
    assert.equal(gone.status, 'invalid');
    assert.equal(gone.reason, 'cursor');
    assert.equal(gone.generation, 3);
    assert.equal(gone.hotspotX, 0, 'geometry from the last cursor was reported as current');

    const unsupported = readCursor({ flags: CURSOR_UNSUPPORTED });
    assert.equal(unsupported.status, 'invalid');
    assert.equal(unsupported.reason, 'unsupported');

    // Unsupported is terminal; hidden is meaningless with nothing to hide.
    assert.equal(readCursor({ flags: CURSOR_UNSUPPORTED | CURSOR_VALID }).reason, 'corrupt');
    assert.equal(readCursor({ flags: CURSOR_HIDDEN }).reason, 'corrupt');
  });

  it('carries the hidden flag through rather than dropping the cursor', () => {
    const hidden = readCursor({ flags: CURSOR_VALID | CURSOR_HIDDEN });
    assert.equal(hidden.status, 'ready');
    assert.equal(hidden.hidden, true);
  });

  // The payload read is a second, larger read behind the same seqlock, and it
  // is the one that would put four kilobytes of somebody's texture memory on
  // screen. It copies rather than viewing, so what the caller gets cannot
  // change under it afterwards.
  it('hands back a private copy of the pixels, or nothing', () => {
    const buffer = cursorBlock();
    const full = readCompanionCursorPixels(buffer, 0);
    assert.equal(full.pixels.length, 32 * 32 * 4);
    assert.equal(full.pixels[0], 0x44, 'the payload was not read little-endian');

    const before = full.pixels[0];
    new DataView(buffer).setUint32(64, 0, true);
    assert.equal(full.pixels[0], before, 'the caller got a view, not a copy');

    assert.equal(readCompanionCursorPixels(cursorBlock({ sequence: 5 }), 0), null);
    assert.equal(readCompanionCursorPixels(cursorBlock({ flags: 0 }), 0), null);
    assert.equal(readCompanionCursorPixels(new ArrayBuffer(8), 0), null);
  });
});
