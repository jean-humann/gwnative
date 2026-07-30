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
const SNAPSHOT_ABI = 3;
const CURSOR_ABI = 1;

const FLAG_READY = 1 << 0;
const FLAG_PLAYER = 1 << 1;
const FLAG_TARGET = 1 << 2;
const FLAG_LOADING = 1 << 3;
const FLAG_PARTY = 1 << 4;
const FLAG_SKILLBAR = 1 << 5;
const FLAG_EFFECTS = 1 << 6;

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
      | FLAG_EFFECTS,
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

  it('decodes bounded party, skillbar, and player effect state', () => {
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
    assert.ok(Object.isFrozen(state.party.players));
    assert.ok(Object.isFrozen(state.skillbar.skills));
    assert.ok(Object.isFrozen(state.effects.effects));
  });

  it('refuses partial party, skillbar, and effect records', () => {
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
      // A flag this build has no name for. The companion sets only four, so a
      // fifth is either a newer companion or not a companion at all.
      { flags: FLAG_READY | FLAG_PLAYER | (1 << 8) },
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
