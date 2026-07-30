// Reading what the companion published, without trusting it.
//
// The companion (`src/companion-kernel/lib.rs`) runs inside the client's own
// memory and writes two fixed-size blocks: a bounded state snapshot and a
// 4160-byte cursor block. This file turns those bytes back into values, and it
// is the only place in the page that knows their layout.
//
// Both blocks are published under a seqlock. The writer bumps the sequence to
// an odd number before it starts and to the next even one when it is done, so
// a reader that sees the same even value before and after its copy knows
// nothing moved underneath it. That is what lets these run on the animation
// frame without stopping the game: no lock, no message, no copy of the heap.
//
// Every field is then re-checked here even though the companion checked it
// first. The two halves are compiled separately and share nothing but a
// manifest, so this side treats the region as what it is — a span of the
// client's heap that anything in the client could in principle have written —
// and answers `waiting` rather than rendering a coordinate it does not believe.

export const COMPANION_SNAPSHOT_ABI = 3;
export const COMPANION_SNAPSHOT_BYTES = 2804;

/** 'GWTB' little-endian, the first word of every published snapshot. */
const MAGIC = 0x42545747;
const INSTANCE_NAMES = Object.freeze(['Outpost', 'Explorable', 'Loading']);
/** The game's own range bands, in the order the companion numbers them. */
const RANGE_NAMES = Object.freeze([
  'None',
  'Adjacent',
  'Nearby',
  'Area',
  'Earshot',
  'Spellcast',
  'Spirit',
  'Compass',
  'Beyond compass',
]);
const FLAGS = Object.freeze({
  ready: 1 << 0,
  player: 1 << 1,
  target: 1 << 2,
  loading: 1 << 3,
  party: 1 << 4,
  skillbar: 1 << 5,
  effects: 1 << 6,
});
const KNOWN_FLAGS =
  FLAGS.ready
  | FLAGS.player
  | FLAGS.target
  | FLAGS.loading
  | FLAGS.party
  | FLAGS.skillbar
  | FLAGS.effects;
const PARTY_FLAGS = Object.freeze({
  hardMode: 1 << 0,
  defeated: 1 << 1,
  leader: 1 << 2,
  alliesTruncated: 1 << 3,
});
const KNOWN_PARTY_FLAGS =
  PARTY_FLAGS.hardMode | PARTY_FLAGS.defeated | PARTY_FLAGS.leader | PARTY_FLAGS.alliesTruncated;
const MAX_PARTY_PLAYERS = 12;
const MAX_PARTY_HEROES = 12;
const MAX_PARTY_HENCHMEN = 12;
const MAX_PARTY_ALLIES = 32;
const MAX_AGENT_ID = 4_095;
const SKILL_SLOTS = 8;
const MAX_PLAYER_BUFFS = 32;
const MAX_PLAYER_EFFECTS = 64;
const EFFECT_FLAGS = Object.freeze({
  buffsTruncated: 1 << 0,
  effectsTruncated: 1 << 1,
});
const KNOWN_EFFECT_FLAGS =
  EFFECT_FLAGS.buffsTruncated | EFFECT_FLAGS.effectsTruncated;

/** @param {number} value */
function validCoordinate(value) {
  return Number.isFinite(value) && Math.abs(value) <= 1_000_000;
}

// The companion publishes a target only when its type word carries one of
// these bits (`valid_agent_type`, lib.rs). This side checks the same property
// independently rather than taking the writer's word for it.
const AGENT_TYPE_BITS = 0x400 | 0x200 | 0xdb;

/**
 * Only the Living pattern has been seen against a live target, so every other
 * accepted word is reported as `agentTypeBits` under a kind that claims
 * nothing. Naming a value that has not been checked is how a guess becomes a
 * fact somebody later relies on.
 *
 * @param {number} bits
 */
function agentKind(bits) {
  return (bits & 0xdb) !== 0 ? 'Living' : 'Unknown';
}

const wordsAreZero = (view, offset, bytes) => {
  for (let cursor = offset; cursor < offset + bytes; cursor += 4) {
    if (view.getUint32(cursor, true) !== 0) return false;
  }
  return true;
};

function readParty(view) {
  const id = view.getUint32(64, true);
  const flags = view.getUint32(68, true);
  const playerCount = view.getUint32(72, true);
  const heroCount = view.getUint32(76, true);
  const henchmanCount = view.getUint32(80, true);
  const allyCount = view.getUint32(84, true);
  if (
    (flags & ~KNOWN_PARTY_FLAGS) !== 0
    || playerCount < 1
    || playerCount > MAX_PARTY_PLAYERS
    || heroCount > MAX_PARTY_HEROES
    || henchmanCount > MAX_PARTY_HENCHMEN
    || playerCount + heroCount + henchmanCount > MAX_PARTY_PLAYERS
    || allyCount > MAX_PARTY_ALLIES
  ) {
    return null;
  }

  const players = [];
  for (let index = 0; index < MAX_PARTY_PLAYERS; index += 1) {
    const offset = 88 + index * 12;
    const loginNumber = view.getUint32(offset, true);
    const calledTargetId = view.getUint32(offset + 4, true);
    const state = view.getUint32(offset + 8, true);
    if (index >= playerCount) {
      if (loginNumber !== 0 || calledTargetId !== 0 || state !== 0) return null;
      continue;
    }
    if (loginNumber === 0 || calledTargetId > MAX_AGENT_ID) return null;
    players.push(Object.freeze({
      loginNumber,
      calledTargetId,
      state,
      connected: (state & 1) !== 0,
      ticked: (state & 2) !== 0,
    }));
  }

  const heroes = [];
  for (let index = 0; index < MAX_PARTY_HEROES; index += 1) {
    const offset = 232 + index * 16;
    const agentId = view.getUint32(offset, true);
    const ownerPlayerId = view.getUint32(offset + 4, true);
    const heroId = view.getUint32(offset + 8, true);
    const level = view.getUint32(offset + 12, true);
    if (index >= heroCount) {
      if (agentId !== 0 || ownerPlayerId !== 0 || heroId !== 0 || level !== 0) return null;
      continue;
    }
    if (
      agentId === 0
      || agentId > MAX_AGENT_ID
      || ownerPlayerId === 0
      || !players.some((player) => player.loginNumber === ownerPlayerId)
      || heroId === 0
      || heroId > 1_000
      || level < 1
      || level > 20
    ) {
      return null;
    }
    heroes.push(Object.freeze({ agentId, ownerPlayerId, heroId, level }));
  }

  const henchmen = [];
  for (let index = 0; index < MAX_PARTY_HENCHMEN; index += 1) {
    const offset = 424 + index * 12;
    const agentId = view.getUint32(offset, true);
    const profession = view.getUint32(offset + 4, true);
    const level = view.getUint32(offset + 8, true);
    if (index >= henchmanCount) {
      if (agentId !== 0 || profession !== 0 || level !== 0) return null;
      continue;
    }
    if (
      agentId === 0
      || agentId > MAX_AGENT_ID
      || profession > 10
      || level < 1
      || level > 20
    ) return null;
    henchmen.push(Object.freeze({ agentId, profession, level }));
  }

  const allies = [];
  for (let index = 0; index < MAX_PARTY_ALLIES; index += 1) {
    const agentId = view.getUint32(568 + index * 4, true);
    if (index >= allyCount) {
      if (agentId !== 0) return null;
      continue;
    }
    if (agentId === 0 || agentId > MAX_AGENT_ID) return null;
    allies.push(agentId);
  }

  return Object.freeze({
    id,
    hardMode: (flags & PARTY_FLAGS.hardMode) !== 0,
    defeated: (flags & PARTY_FLAGS.defeated) !== 0,
    leader: (flags & PARTY_FLAGS.leader) !== 0,
    alliesTruncated: (flags & PARTY_FLAGS.alliesTruncated) !== 0,
    players: Object.freeze(players),
    heroes: Object.freeze(heroes),
    henchmen: Object.freeze(henchmen),
    allies: Object.freeze(allies),
  });
}

function readSkillbar(view, playerId) {
  const agentId = view.getUint32(696, true);
  const disabledMask = view.getUint32(700, true);
  const castCount = view.getUint32(704, true);
  if (agentId !== playerId || (disabledMask & ~0xff) !== 0 || castCount > 64) return null;
  const skills = [];
  for (let index = 0; index < SKILL_SLOTS; index += 1) {
    const offset = 708 + index * 20;
    const skill = Object.freeze({
      slot: index + 1,
      adrenalineA: view.getUint32(offset, true),
      adrenalineB: view.getUint32(offset + 4, true),
      recharge: view.getUint32(offset + 8, true),
      skillId: view.getUint32(offset + 12, true),
      event: view.getUint32(offset + 16, true),
      disabled: (disabledMask & (1 << index)) !== 0,
    });
    if (skill.skillId > 100_000) return null;
    skills.push(skill);
  }
  return Object.freeze({
    agentId,
    disabledMask,
    castCount,
    casting: castCount !== 0,
    skills: Object.freeze(skills),
  });
}

function readEffects(view, playerId) {
  const agentId = view.getUint32(868, true);
  const flags = view.getUint32(872, true);
  const buffCount = view.getUint32(876, true);
  const effectCount = view.getUint32(880, true);
  if (
    agentId !== playerId
    || (flags & ~KNOWN_EFFECT_FLAGS) !== 0
    || buffCount > MAX_PLAYER_BUFFS
    || effectCount > MAX_PLAYER_EFFECTS
    || ((flags & EFFECT_FLAGS.buffsTruncated) !== 0 && buffCount !== MAX_PLAYER_BUFFS)
    || ((flags & EFFECT_FLAGS.effectsTruncated) !== 0 && effectCount !== MAX_PLAYER_EFFECTS)
  ) {
    return null;
  }

  const buffs = [];
  const buffIds = new Set();
  for (let index = 0; index < MAX_PLAYER_BUFFS; index += 1) {
    const offset = 884 + index * 12;
    if (index >= buffCount) {
      if (!wordsAreZero(view, offset, 12)) return null;
      continue;
    }
    const buff = Object.freeze({
      skillId: view.getUint32(offset, true),
      buffId: view.getUint32(offset + 4, true),
      targetAgentId: view.getUint32(offset + 8, true),
    });
    if (
      buff.skillId === 0
      || buff.skillId > 100_000
      || buff.buffId === 0
      || buffIds.has(buff.buffId)
      || buff.targetAgentId > MAX_AGENT_ID
    ) {
      return null;
    }
    buffIds.add(buff.buffId);
    buffs.push(buff);
  }

  const effects = [];
  const effectIds = new Set();
  for (let index = 0; index < MAX_PLAYER_EFFECTS; index += 1) {
    const offset = 1268 + index * 24;
    if (index >= effectCount) {
      if (!wordsAreZero(view, offset, 24)) return null;
      continue;
    }
    const effect = Object.freeze({
      skillId: view.getUint32(offset, true),
      attributeLevel: view.getUint32(offset + 4, true),
      effectId: view.getUint32(offset + 8, true),
      agentId: view.getUint32(offset + 12, true),
      duration: view.getFloat32(offset + 16, true),
      timestamp: view.getUint32(offset + 20, true),
    });
    if (
      effect.skillId === 0
      || effect.skillId > 100_000
      || effect.attributeLevel > 100
      || effect.effectId === 0
      || effectIds.has(effect.effectId)
      || effect.agentId > MAX_AGENT_ID
      || !Number.isFinite(effect.duration)
      || effect.duration < 0
      || effect.duration > 1_000_000
    ) {
      return null;
    }
    effectIds.add(effect.effectId);
    effects.push(effect);
  }

  return Object.freeze({
    agentId,
    buffsTruncated: (flags & EFFECT_FLAGS.buffsTruncated) !== 0,
    effectsTruncated: (flags & EFFECT_FLAGS.effectsTruncated) !== 0,
    buffs: Object.freeze(buffs),
    effects: Object.freeze(effects),
  });
}

/**
 * Decode one state snapshot.
 *
 * Never throws and never returns a partial reading: the result is either
 * `status: 'ready'` with every field checked, or a `waiting` with a reason —
 * `memory` for a pointer outside the heap, `writing` for a publish in flight,
 * `snapshot` for a header that does not describe this ABI, `loading` for a map
 * change, `game` for a session that has not reached one, and `corrupt` for a
 * combination the companion cannot have written.
 *
 * @param {ArrayBuffer} buffer
 * @param {number} pointer
 */
export function readCompanionSnapshot(buffer, pointer) {
  if (
    !(buffer instanceof ArrayBuffer)
    || !Number.isInteger(pointer)
    || pointer < 0
    || pointer + COMPANION_SNAPSHOT_BYTES > buffer.byteLength
  ) {
    return Object.freeze({ status: 'waiting', reason: 'memory' });
  }
  const view = new DataView(buffer, pointer, COMPANION_SNAPSHOT_BYTES);
  const firstSequence = view.getUint32(8, true);
  if ((firstSequence & 1) !== 0) {
    return Object.freeze({ status: 'waiting', reason: 'writing' });
  }
  const magic = view.getUint32(0, true);
  // One word carries both, so a module built against another ABI cannot look
  // like this one merely by being the same length.
  const abi = view.getUint16(4, true);
  const byteLength = view.getUint16(6, true);
  const flags = view.getUint32(12, true);
  const state = {
    sequence: firstSequence,
    tickCount: view.getUint32(16, true),
    mapId: view.getUint32(20, true),
    instanceType: view.getUint32(24, true),
    playerId: view.getUint32(28, true),
    playerX: view.getFloat32(32, true),
    playerY: view.getFloat32(36, true),
    targetId: view.getUint32(40, true),
    agentTypeBits: view.getUint32(44, true),
    targetX: view.getFloat32(48, true),
    targetY: view.getFloat32(52, true),
    distance: view.getFloat32(56, true),
    rangeBand: view.getUint32(60, true),
  };
  // The other half of the seqlock: everything above was read from one publish
  // only if this still matches.
  const secondSequence = view.getUint32(8, true);
  if (
    magic !== MAGIC
    || abi !== COMPANION_SNAPSHOT_ABI
    || byteLength !== COMPANION_SNAPSHOT_BYTES
    || firstSequence !== secondSequence
    || (secondSequence & 1) !== 0
    || (flags & ~KNOWN_FLAGS) !== 0
  ) {
    return Object.freeze({ status: 'waiting', reason: 'snapshot' });
  }
  if ((flags & FLAGS.loading) !== 0) {
    // Loading is a whole state, not a modifier.
    if (flags !== FLAGS.loading) {
      return Object.freeze({ status: 'waiting', reason: 'corrupt' });
    }
    return Object.freeze({
      status: 'waiting',
      reason: 'loading',
      sequence: secondSequence,
      tickCount: state.tickCount,
    });
  }
  if ((flags & (FLAGS.ready | FLAGS.player)) !== (FLAGS.ready | FLAGS.player)) {
    return Object.freeze({
      status: 'waiting',
      reason: 'game',
      sequence: secondSequence,
      tickCount: state.tickCount,
    });
  }
  if (
    state.mapId === 0
    || state.mapId > 2_000
    || state.instanceType > 1
    || state.playerId === 0
    || state.playerId > MAX_AGENT_ID
    || !validCoordinate(state.playerX)
    || !validCoordinate(state.playerY)
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const targetValid = (flags & FLAGS.target) !== 0;
  // Both directions: a target that is flagged has to be complete, and one that
  // is not has to be absent. A half-written target is not a smaller reading,
  // it is evidence that this is not a snapshot.
  if (
    targetValid
      ? state.targetId === 0
        || state.targetId > MAX_AGENT_ID
        || (state.agentTypeBits & AGENT_TYPE_BITS) === 0
        || !validCoordinate(state.targetX)
        || !validCoordinate(state.targetY)
        || !Number.isFinite(state.distance)
        || state.distance < 0
        || state.rangeBand < 1
        || state.rangeBand >= RANGE_NAMES.length
      : state.targetId !== 0
        || state.agentTypeBits !== 0
        || state.targetX !== 0
        || state.targetY !== 0
        || state.distance !== 0
        || state.rangeBand !== 0
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const partyValid = (flags & FLAGS.party) !== 0;
  const party = partyValid ? readParty(view) : null;
  if (
    (partyValid && party === null)
    || (!partyValid && !wordsAreZero(view, 64, 632))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const skillbarValid = (flags & FLAGS.skillbar) !== 0;
  const skillbar = skillbarValid ? readSkillbar(view, state.playerId) : null;
  if (
    (skillbarValid && skillbar === null)
    || (!skillbarValid && !wordsAreZero(view, 696, 172))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const effectsValid = (flags & FLAGS.effects) !== 0;
  const effects = effectsValid ? readEffects(view, state.playerId) : null;
  if (
    (effectsValid && effects === null)
    || (!effectsValid && !wordsAreZero(view, 868, 1936))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  // The nested records are read after the inexpensive header check above.
  // Close the seqlock around them as well: the writer may have started a new
  // frame while those arrays were being copied.
  if (view.getUint32(8, true) !== firstSequence) {
    return Object.freeze({ status: 'waiting', reason: 'writing' });
  }
  return Object.freeze({
    status: 'ready',
    ...state,
    instanceName: INSTANCE_NAMES[state.instanceType] ?? 'Unknown',
    targetValid,
    targetKind: targetValid ? agentKind(state.agentTypeBits) : 'None',
    rangeName: RANGE_NAMES[state.rangeBand] ?? 'None',
    ...(party ? { party } : {}),
    ...(skillbar ? { skillbar } : {}),
    ...(effects ? { effects } : {}),
  });
}

// The cursor bitmap has its own region: four kilobytes of pixels do not belong
// in the typed state read that happens every frame.

export const COMPANION_CURSOR_ABI = 1;
export const COMPANION_CURSOR_BYTES = 4160;

/** 'GWTC' little-endian. */
const CURSOR_MAGIC = 0x43545747;
const CURSOR_EDGE = 32;
const CURSOR_PIXEL_OFFSET = 64;
const CURSOR_PIXEL_BYTES = CURSOR_EDGE * CURSOR_EDGE * 4;
const CURSOR_FLAGS = Object.freeze({
  valid: 1 << 0,
  hidden: 1 << 1,
  unsupported: 1 << 2,
});
const KNOWN_CURSOR_FLAGS =
  CURSOR_FLAGS.valid | CURSOR_FLAGS.hidden | CURSOR_FLAGS.unsupported;

/**
 * @param {ArrayBuffer} buffer
 * @param {number} pointer
 */
function cursorView(buffer, pointer) {
  if (
    !(buffer instanceof ArrayBuffer)
    || !Number.isInteger(pointer)
    || pointer < 0
    || pointer + COMPANION_CURSOR_BYTES > buffer.byteLength
  ) {
    return null;
  }
  return new DataView(buffer, pointer, COMPANION_CURSOR_BYTES);
}

/**
 * The header alone, which is what the per-frame change check needs. Never
 * touches the four kilobytes behind it — that is the whole point of splitting
 * the read in two.
 *
 * @param {ArrayBuffer} buffer
 * @param {number} pointer
 */
export function readCompanionCursorHeader(buffer, pointer) {
  const view = cursorView(buffer, pointer);
  if (view === null) {
    return Object.freeze({ status: 'waiting', reason: 'memory' });
  }
  const firstSequence = view.getUint32(8, true);
  if ((firstSequence & 1) !== 0) {
    return Object.freeze({ status: 'waiting', reason: 'writing' });
  }
  const magic = view.getUint32(0, true);
  const abi = view.getUint16(4, true);
  const byteLength = view.getUint16(6, true);
  const flags = view.getUint32(12, true);
  const generation = view.getUint32(16, true);
  const width = view.getUint32(20, true);
  const height = view.getUint32(24, true);
  const hotspotX = view.getUint32(28, true);
  const hotspotY = view.getUint32(32, true);
  const pixelHash = view.getUint32(36, true);
  // The companion zeroes these and never writes them again, so anything here
  // is something else writing into the region.
  let reserved = 0;
  for (let offset = 40; offset < CURSOR_PIXEL_OFFSET; offset += 4) {
    reserved |= view.getUint32(offset, true);
  }
  const secondSequence = view.getUint32(8, true);
  if (
    magic !== CURSOR_MAGIC
    || abi !== COMPANION_CURSOR_ABI
    || byteLength !== COMPANION_CURSOR_BYTES
    || firstSequence !== secondSequence
    || (secondSequence & 1) !== 0
    || (flags & ~KNOWN_CURSOR_FLAGS) !== 0
    || reserved !== 0
  ) {
    return Object.freeze({ status: 'waiting', reason: 'cursor' });
  }
  // Unsupported is terminal, not a modifier: it must stand alone.
  if (
    (flags & CURSOR_FLAGS.unsupported) !== 0
    && flags !== CURSOR_FLAGS.unsupported
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  if ((flags & CURSOR_FLAGS.valid) === 0) {
    // The companion clears VALID with a header-only publish, which leaves the
    // geometry of the last good cursor behind — so none of it is checked here.
    // Hidden means nothing without a cursor to hide.
    if ((flags & CURSOR_FLAGS.hidden) !== 0) {
      return Object.freeze({ status: 'waiting', reason: 'corrupt' });
    }
    return Object.freeze({
      status: 'invalid',
      reason: flags === CURSOR_FLAGS.unsupported ? 'unsupported' : 'cursor',
      generation,
      flags,
      hotspotX: 0,
      hotspotY: 0,
      pixelHash: 0,
      hidden: false,
    });
  }
  if (
    generation === 0
    || width !== CURSOR_EDGE
    || height !== CURSOR_EDGE
    || hotspotX >= CURSOR_EDGE
    || hotspotY >= CURSOR_EDGE
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  return Object.freeze({
    status: 'ready',
    generation,
    flags,
    hotspotX,
    hotspotY,
    pixelHash,
    hidden: (flags & CURSOR_FLAGS.hidden) !== 0,
  });
}

/**
 * The header and a private copy of the RGBA payload, both from one publish.
 * `null` when the region is torn, malformed, or carries no cursor.
 *
 * The sequence is read before the header and again after the pixels, so a
 * publish that landed anywhere in between is caught — including one that
 * changed the bitmap under a header that was already valid.
 *
 * @param {ArrayBuffer} buffer
 * @param {number} pointer
 */
export function readCompanionCursorPixels(buffer, pointer) {
  const view = cursorView(buffer, pointer);
  if (view === null) return null;
  const firstSequence = view.getUint32(8, true);
  const header = readCompanionCursorHeader(buffer, pointer);
  if (header.status !== 'ready') return null;
  const pixels = new Uint8ClampedArray(
    new Uint8Array(buffer, pointer + CURSOR_PIXEL_OFFSET, CURSOR_PIXEL_BYTES),
  );
  if ((firstSequence & 1) !== 0 || view.getUint32(8, true) !== firstSequence) {
    return null;
  }
  return Object.freeze({ ...header, pixels });
}
