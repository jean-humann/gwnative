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

export const COMPANION_SNAPSHOT_ABI = 13;
export const COMPANION_SNAPSHOT_BYTES = 56_844;

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
  agents: 1 << 7,
  quests: 1 << 8,
  inventory: 1 << 9,
  social: 1 << 10,
  completion: 1 << 11,
  camera: 1 << 12,
  trade: 1 << 13,
  ui: 1 << 14,
  merchant: 1 << 15,
  progression: 1 << 16,
});
const KNOWN_FLAGS =
  FLAGS.ready
  | FLAGS.player
  | FLAGS.target
  | FLAGS.loading
  | FLAGS.party
  | FLAGS.skillbar
  | FLAGS.effects
  | FLAGS.agents
  | FLAGS.quests
  | FLAGS.inventory
  | FLAGS.social
  | FLAGS.completion
  | FLAGS.camera
  | FLAGS.trade
  | FLAGS.ui
  | FLAGS.merchant
  | FLAGS.progression;
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
const MAX_MAP_AGENTS = 128;
const MAX_QUESTS = 64;
const MAX_MISSION_OBJECTIVES = 32;
const NO_QUEST_MARKER = 0xffff_ffff;
const MAX_INVENTORY_BAGS = 22;
const MAX_INVENTORY_ITEMS = 512;
const MAX_INVENTORY_ITEM_ID = 1_000_000;
const MAX_TOTAL_BAG_SLOTS = 1_024;
const MAX_RAW_FRIENDS = 256;
const MAX_FRIENDS = 128;
const MAX_COMPLETION_WORDS = 32;
const MAX_TRADE_ITEMS = 16;
const MAX_TRADE_ITEM_ID = 1_000_000;
const MAX_TRADE_QUANTITY = 250;
const TRADE_STATUS_FLAGS = Object.freeze({
  initiated: 1 << 0,
  offerSent: 1 << 1,
  accepted: 1 << 2,
});
const KNOWN_TRADE_STATUS_FLAGS =
  TRADE_STATUS_FLAGS.initiated
  | TRADE_STATUS_FLAGS.offerSent
  | TRADE_STATUS_FLAGS.accepted;
const TRADE_PAGE_FLAGS = Object.freeze({
  playerItemsTruncated: 1 << 0,
  partnerItemsTruncated: 1 << 1,
});
const KNOWN_TRADE_PAGE_FLAGS =
  TRADE_PAGE_FLAGS.playerItemsTruncated
  | TRADE_PAGE_FLAGS.partnerItemsTruncated;
const MAX_RAW_UI_FRAMES = 2_048;
const MAX_UI_FRAMES = 128;
const UI_PAGE_TRUNCATED = 1 << 0;
const UI_RECORD_POSITION_VALID = 1 << 0;
const UI_FRAME_CREATED = 0x4;
const UI_FRAME_DESTROYING = 0x8;
const UI_FRAME_DISABLED = 0x10;
const UI_FRAME_HIDDEN = 0x200;
const MAX_RAW_MERCHANT_ITEMS = 512;
const MAX_MERCHANT_ITEMS = 128;
const MAX_MERCHANT_ITEM_ID = 1_000_000;
const MAX_EXPERIENCE = 2_000_000_000;
const MAX_FACTION_CURRENT = 100_000_000;
const MAX_FACTION_TOTAL = 2_000_000_000;
const MAX_SKILL_POINTS_CURRENT = 1_000_000;
const MAX_SKILL_POINTS_TOTAL = 2_000_000_000;
const EFFECT_FLAGS = Object.freeze({
  buffsTruncated: 1 << 0,
  effectsTruncated: 1 << 1,
});
const KNOWN_EFFECT_FLAGS =
  EFFECT_FLAGS.buffsTruncated | EFFECT_FLAGS.effectsTruncated;
const MAP_AGENT_FLAGS = Object.freeze({
  truncated: 1 << 0,
});
const QUEST_FLAGS = Object.freeze({
  questsTruncated: 1 << 0,
  objectivesTruncated: 1 << 1,
});
const KNOWN_QUEST_FLAGS =
  QUEST_FLAGS.questsTruncated | QUEST_FLAGS.objectivesTruncated;
const INVENTORY_FLAGS = Object.freeze({
  itemsTruncated: 1 << 0,
});
const SOCIAL_FLAGS = Object.freeze({
  friendsTruncated: 1 << 0,
  guildPresent: 1 << 1,
});
const KNOWN_SOCIAL_FLAGS =
  SOCIAL_FLAGS.friendsTruncated | SOCIAL_FLAGS.guildPresent;
const FRIEND_TYPE_NAMES = Object.freeze([
  'Unknown',
  'Friend',
  'Ignore',
  'Partner',
  'Trade',
]);
const FRIEND_STATUS_NAMES = Object.freeze([
  'Offline',
  'Online',
  'DoNotDisturb',
  'Away',
  'Unknown',
]);
const GUILD_FACTION_NAMES = Object.freeze(['Kurzick', 'Luxon']);
const guildFactionName = (faction) => GUILD_FACTION_NAMES[faction] ?? 'Unknown';

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

function mapAgentKind(typeBits) {
  if ((typeBits & 0xdb) !== 0) return 'Living';
  if ((typeBits & 0x400) !== 0) return 'Item';
  if ((typeBits & 0x200) !== 0) return 'Gadget';
  return 'Unknown';
}

function readMapAgents(view) {
  const flags = view.getUint32(2804, true);
  const count = view.getUint32(2808, true);
  const total = view.getUint32(2812, true);
  const truncated = (flags & MAP_AGENT_FLAGS.truncated) !== 0;
  if (
    (flags & ~MAP_AGENT_FLAGS.truncated) !== 0
    || count < 1
    || count > MAX_MAP_AGENTS
    || total < count
    || total > MAX_AGENT_ID
    || (truncated ? count !== MAX_MAP_AGENTS || total <= count : total !== count)
  ) {
    return null;
  }

  const agents = [];
  let previousId = 0;
  for (let index = 0; index < MAX_MAP_AGENTS; index += 1) {
    const offset = 2816 + index * 56;
    if (index >= count) {
      if (!wordsAreZero(view, offset, 56)) return null;
      continue;
    }
    const agentId = view.getUint32(offset, true);
    const typeBits = view.getUint32(offset + 4, true);
    const playerNumber = view.getUint32(offset + 8, true);
    const primary = view.getUint32(offset + 12, true);
    const secondary = view.getUint32(offset + 16, true);
    const level = view.getUint32(offset + 20, true);
    const health = view.getFloat32(offset + 24, true);
    const rotation = view.getFloat32(offset + 28, true);
    const x = view.getFloat32(offset + 32, true);
    const y = view.getFloat32(offset + 36, true);
    const z = view.getFloat32(offset + 40, true);
    const modelState = view.getUint32(offset + 44, true);
    const effects = view.getUint32(offset + 48, true);
    const allegiance = view.getUint32(offset + 52, true);
    const living = (typeBits & 0xdb) !== 0;
    if (
      agentId <= previousId
      || agentId > MAX_AGENT_ID
      || (typeBits & AGENT_TYPE_BITS) === 0
      || playerNumber > 0xffff
      || primary > 10
      || secondary > 10
      || level > 0xff
      || !Number.isFinite(health)
      || health < -10
      || health > 10
      || !Number.isFinite(rotation)
      || Math.abs(rotation) > 4
      || !validCoordinate(x)
      || !validCoordinate(y)
      || !validCoordinate(z)
      || allegiance > 6
      || (!living && (
        playerNumber !== 0
        || primary !== 0
        || secondary !== 0
        || level !== 0
        || health !== 0
        || modelState !== 0
        || effects !== 0
        || allegiance !== 0
      ))
    ) {
      return null;
    }
    previousId = agentId;
    agents.push(Object.freeze({
      agentId,
      typeBits,
      kind: mapAgentKind(typeBits),
      playerNumber,
      primary,
      secondary,
      level,
      health,
      rotation,
      x,
      y,
      z,
      modelState,
      effects,
      allegiance,
      isLiving: living,
      isItem: (typeBits & 0x400) !== 0,
      isGadget: (typeBits & 0x200) !== 0,
      isDead: living && (effects & 0x10) !== 0,
      isMoving: living && [12, 76, 204].includes(modelState),
      isAttacking: living && [96, 1088, 1120].includes(modelState),
      isKnockedDown: living && modelState === 1104,
      isCasting: living && [65, 581].includes(modelState),
    }));
  }
  return Object.freeze({
    truncated,
    total,
    agents: Object.freeze(agents),
  });
}

function readQuests(view) {
  const activeQuestId = view.getUint32(9984, true);
  const flags = view.getUint32(9988, true);
  const questCount = view.getUint32(9992, true);
  const objectiveCount = view.getUint32(9996, true);
  const questsTruncated = (flags & QUEST_FLAGS.questsTruncated) !== 0;
  const objectivesTruncated = (flags & QUEST_FLAGS.objectivesTruncated) !== 0;
  if (
    activeQuestId > 100_000
    || (flags & ~KNOWN_QUEST_FLAGS) !== 0
    || questCount > MAX_QUESTS
    || objectiveCount > MAX_MISSION_OBJECTIVES
    || (questsTruncated && questCount !== MAX_QUESTS)
    || (objectivesTruncated && objectiveCount !== MAX_MISSION_OBJECTIVES)
  ) {
    return null;
  }

  const quests = [];
  const questIds = new Set();
  for (let index = 0; index < MAX_QUESTS; index += 1) {
    const offset = 10000 + index * 28;
    if (index >= questCount) {
      if (!wordsAreZero(view, offset, 28)) return null;
      continue;
    }
    const markerX = view.getFloat32(offset + 12, true);
    const markerY = view.getFloat32(offset + 16, true);
    const rawMarkerPlane = view.getUint32(offset + 20, true);
    const hasMarker = rawMarkerPlane !== NO_QUEST_MARKER;
    const quest = Object.freeze({
      questId: view.getUint32(offset, true),
      logState: view.getUint32(offset + 4, true),
      mapFrom: view.getUint32(offset + 8, true),
      markerX,
      markerY,
      markerPlane: hasMarker ? rawMarkerPlane : 0,
      hasMarker,
      mapTo: view.getUint32(offset + 24, true),
    });
    if (
      quest.questId === 0
      || quest.questId > 100_000
      || questIds.has(quest.questId)
      || quest.mapFrom > 2_000
      || quest.mapTo > 2_000
      || (
        hasMarker
          ? (
            !validCoordinate(quest.markerX)
            || !validCoordinate(quest.markerY)
            || quest.markerPlane > 100_000
          )
          : quest.markerX !== 0 || quest.markerY !== 0
      )
    ) {
      return null;
    }
    questIds.add(quest.questId);
    quests.push(Object.freeze({
      ...quest,
      completed: (quest.logState & 0x2) !== 0,
      currentMission: (quest.logState & 0x10) !== 0,
      primary: (quest.logState & 0x20) !== 0,
      areaPrimary: (quest.logState & 0x40) !== 0,
    }));
  }
  if (activeQuestId !== 0 && !questsTruncated && !questIds.has(activeQuestId)) {
    return null;
  }

  const missionObjectives = [];
  const objectiveIds = new Set();
  for (let index = 0; index < MAX_MISSION_OBJECTIVES; index += 1) {
    const offset = 11792 + index * 8;
    if (index >= objectiveCount) {
      if (!wordsAreZero(view, offset, 8)) return null;
      continue;
    }
    const objective = Object.freeze({
      objectiveId: view.getUint32(offset, true),
      type: view.getUint32(offset + 4, true),
    });
    if (
      objective.objectiveId === 0
      || objectiveIds.has(objective.objectiveId)
      || objective.type > 100_000
    ) {
      return null;
    }
    objectiveIds.add(objective.objectiveId);
    missionObjectives.push(objective);
  }
  return Object.freeze({
    activeQuestId,
    questsTruncated,
    objectivesTruncated,
    quests: Object.freeze(quests),
    missionObjectives: Object.freeze(missionObjectives),
  });
}

const ITEM_TYPE_NAMES = Object.freeze({
  0: 'Salvage',
  2: 'Axe',
  3: 'Bag',
  4: 'Boots',
  5: 'Bow',
  6: 'Bundle',
  7: 'Chestpiece',
  8: 'Rune_Mod',
  9: 'Usable',
  10: 'Dye',
  11: 'Materials_Zcoins',
  12: 'Offhand',
  13: 'Gloves',
  15: 'Hammer',
  16: 'Headpiece',
  17: 'CC_Shards',
  18: 'Key',
  19: 'Leggings',
  20: 'Gold_Coin',
  21: 'Quest_Item',
  22: 'Wand',
  24: 'Shield',
  26: 'Staff',
  27: 'Sword',
  29: 'Kit',
  30: 'Trophy',
  31: 'Scroll',
  32: 'Daggers',
  33: 'Present',
  34: 'Minipet',
  35: 'Scythe',
  36: 'Spear',
  43: 'Storybook',
  44: 'Costume',
  45: 'Costume_Headpiece',
  255: 'Unknown',
});

function expectedBagType(bagId) {
  if (bagId >= 1 && bagId <= 5) return 1;
  if (bagId === 6) return 5;
  if (bagId === 7) return 3;
  if (bagId >= 8 && bagId <= 21) return 4;
  if (bagId === 22) return 2;
  return null;
}

function bagKind(bagType) {
  return ['None', 'Inventory', 'Equipped', 'NotCollected', 'Storage', 'MaterialStorage']
    [bagType] ?? 'Unknown';
}

function readInventory(view) {
  const flags = view.getUint32(12048, true);
  const goldCharacter = view.getUint32(12052, true);
  const goldStorage = view.getUint32(12056, true);
  const storagePanesUnlocked = view.getUint32(12060, true);
  const bagCount = view.getUint32(12064, true);
  const itemCount = view.getUint32(12068, true);
  const total = view.getUint32(12072, true);
  const itemsTruncated = (flags & INVENTORY_FLAGS.itemsTruncated) !== 0;
  if (
    (flags & ~INVENTORY_FLAGS.itemsTruncated) !== 0
    || storagePanesUnlocked > 14
    || bagCount < 1
    || bagCount > MAX_INVENTORY_BAGS
    || itemCount > MAX_INVENTORY_ITEMS
    || total < itemCount
    || total > MAX_TOTAL_BAG_SLOTS
    || (
      itemsTruncated
        ? itemCount !== MAX_INVENTORY_ITEMS || total <= itemCount
        : total !== itemCount
    )
  ) {
    return null;
  }

  const bags = [];
  const bagsById = new Map();
  let previousBagId = 0;
  let totalCapacity = 0;
  let expectedItems = 0;
  for (let index = 0; index < MAX_INVENTORY_BAGS; index += 1) {
    const offset = 12076 + index * 20;
    if (index >= bagCount) {
      if (!wordsAreZero(view, offset, 20)) return null;
      continue;
    }
    const bagId = view.getUint32(offset, true);
    const bagType = view.getUint32(offset + 4, true);
    const containerItem = view.getUint32(offset + 8, true);
    const capacity = view.getUint32(offset + 12, true);
    const bagItemCount = view.getUint32(offset + 16, true);
    if (
      bagId <= previousBagId
      || bagId > MAX_INVENTORY_BAGS
      || expectedBagType(bagId) !== bagType
      || capacity > 256
      || bagItemCount > capacity
    ) {
      return null;
    }
    previousBagId = bagId;
    totalCapacity += capacity;
    expectedItems += bagItemCount;
    if (totalCapacity > MAX_TOTAL_BAG_SLOTS || expectedItems > MAX_TOTAL_BAG_SLOTS) {
      return null;
    }
    const bag = Object.freeze({
      bagId,
      bagType,
      kind: bagKind(bagType),
      containerItem,
      capacity,
      itemCount: bagItemCount,
      isInventory: bagType === 1,
      isEquipped: bagType === 2,
      isNotCollected: bagType === 3,
      isStorage: bagType === 4,
      isMaterialStorage: bagType === 5,
    });
    bags.push(bag);
    bagsById.set(bagId, bag);
  }
  if (!bagsById.has(1) || expectedItems !== total) return null;

  const items = [];
  const itemIds = new Set();
  const locations = new Set();
  const itemsByBag = new Map();
  let previousBag = 0;
  let previousSlot = -1;
  for (let index = 0; index < MAX_INVENTORY_ITEMS; index += 1) {
    const offset = 12516 + index * 64;
    if (index >= itemCount) {
      if (!wordsAreZero(view, offset, 64)) return null;
      continue;
    }
    const itemId = view.getUint32(offset, true);
    const agentId = view.getUint32(offset + 4, true);
    const bagId = view.getUint32(offset + 8, true);
    const slot = view.getUint32(offset + 12, true);
    const modelFileId = view.getUint32(offset + 16, true);
    const type = view.getUint32(offset + 20, true);
    const value = view.getUint32(offset + 24, true);
    const interaction = view.getUint32(offset + 28, true);
    const modelId = view.getUint32(offset + 32, true);
    const itemFormula = view.getUint32(offset + 36, true);
    const quantity = view.getUint32(offset + 40, true);
    const equipped = view.getUint32(offset + 44, true);
    const profession = view.getUint32(offset + 48, true);
    const metadataFlags = view.getUint32(offset + 52, true);
    const modifierCount = view.getUint32(offset + 56, true);
    const dyeInfo = view.getUint32(offset + 60, true);
    const bag = bagsById.get(bagId);
    const location = `${bagId}:${slot}`;
    if (
      itemId === 0
      || itemId > MAX_INVENTORY_ITEM_ID
      || itemIds.has(itemId)
      || agentId > MAX_AGENT_ID
      || !bag
      || slot >= bag.capacity
      || locations.has(location)
      || bagId < previousBag
      || (bagId === previousBag && slot <= previousSlot)
      || modelFileId === 0
      || ITEM_TYPE_NAMES[type] === undefined
      || value > 0xffff
      || modelId === 0
      || itemFormula > 0xffff
      || quantity === 0
      || quantity > 0xffff
      || equipped > 1
      || profession > 0xff
      || (metadataFlags & ~0x3) !== 0
      || modifierCount > 64
      || dyeInfo > 0xff_ffff
    ) {
      return null;
    }
    itemIds.add(itemId);
    locations.add(location);
    previousBag = bagId;
    previousSlot = slot;
    itemsByBag.set(bagId, (itemsByBag.get(bagId) ?? 0) + 1);
    items.push(Object.freeze({
      itemId,
      agentId,
      bagId,
      slot,
      modelFileId,
      type,
      typeName: ITEM_TYPE_NAMES[type],
      value,
      interaction,
      modelId,
      itemFormula,
      quantity,
      equipped: equipped === 1,
      profession,
      customized: (metadataFlags & 1) !== 0,
      materialSalvageable: (metadataFlags & 2) !== 0,
      modifierCount,
      dyeTint: dyeInfo & 0xff,
      dye1: (dyeInfo >>> 8) & 0xf,
      dye2: (dyeInfo >>> 12) & 0xf,
      dye3: (dyeInfo >>> 16) & 0xf,
      dye4: (dyeInfo >>> 20) & 0xf,
      isStackable: (interaction & 0x8_0000) !== 0,
      isInscribable: (interaction & 0x800_0000) !== 0,
      isIdentified: (interaction & 1) !== 0,
      isTradable: (interaction & 0x100) === 0,
      isUsable: (interaction & 0x100_0000) !== 0,
      isPrefixUpgradable: (interaction & 0x4000) === 0,
      isSuffixUpgradable: (interaction & 0x8000) === 0,
      isInscription: (interaction & 0x2500_0000) === 0x2500_0000,
      isPurple: (interaction & 0x40_0000) !== 0,
      isGreen: (interaction & 0x10) !== 0,
      isGold: (interaction & 0x2_0000) !== 0,
      isInventoryItem: bag.bagType === 1 || bag.bagType === 2,
      isStorageItem: bag.bagType === 4 || bag.bagType === 5,
    }));
  }
  for (const bag of bags) {
    const published = itemsByBag.get(bag.bagId) ?? 0;
    if (
      published > bag.itemCount
      || (!itemsTruncated && published !== bag.itemCount)
    ) {
      return null;
    }
  }
  return Object.freeze({
    itemsTruncated,
    total,
    goldCharacter,
    goldStorage,
    storagePanesUnlocked,
    bags: Object.freeze(bags),
    items: Object.freeze(items),
  });
}

function readSocial(view) {
  const flags = view.getUint32(45284, true);
  const playerStatus = view.getUint32(45288, true);
  const count = view.getUint32(45292, true);
  const total = view.getUint32(45296, true);
  const friends = view.getUint32(45300, true);
  const ignores = view.getUint32(45304, true);
  const partners = view.getUint32(45308, true);
  const traders = view.getUint32(45312, true);
  const friendsTruncated = (flags & SOCIAL_FLAGS.friendsTruncated) !== 0;
  const guildPresent = (flags & SOCIAL_FLAGS.guildPresent) !== 0;
  if (
    (flags & ~KNOWN_SOCIAL_FLAGS) !== 0
    || playerStatus >= FRIEND_STATUS_NAMES.length
    || count > MAX_FRIENDS
    || total < count
    || total > MAX_RAW_FRIENDS
    || friends > MAX_RAW_FRIENDS
    || ignores > MAX_RAW_FRIENDS
    || partners > MAX_RAW_FRIENDS
    || traders > MAX_RAW_FRIENDS
    || friends + ignores + partners + traders > total
    || (
      friendsTruncated
        ? count !== MAX_FRIENDS || total <= count
        : total !== count
    )
  ) {
    return null;
  }

  let guild = null;
  const guildIndex = view.getUint32(45316, true);
  if (!guildPresent) {
    if (guildIndex !== 0 || !wordsAreZero(view, 45320, 60)) return null;
  } else {
    const playerRank = view.getUint32(45320, true);
    const rank = view.getUint32(45324, true);
    const features = view.getUint32(45328, true);
    const rating = view.getUint32(45332, true);
    const faction = view.getUint32(45336, true);
    const factionPoints = view.getUint32(45340, true);
    const qualifierPoints = view.getUint32(45344, true);
    const rosterTotal = view.getUint32(45348, true);
    if (
      guildIndex === 0
      || guildIndex >= 64
      || rosterTotal > 100
    ) {
      return null;
    }
    guild = Object.freeze({
      index: guildIndex,
      playerRank,
      rank,
      features,
      rating,
      faction,
      factionName: guildFactionName(faction),
      factionPoints,
      qualifierPoints,
      rosterTotal,
      cape: Object.freeze({
        backgroundColor: view.getUint32(45352, true),
        detailColor: view.getUint32(45356, true),
        emblemColor: view.getUint32(45360, true),
        shape: view.getUint32(45364, true),
        detail: view.getUint32(45368, true),
        emblem: view.getUint32(45372, true),
        trim: view.getUint32(45376, true),
      }),
    });
  }

  const entries = [];
  const observed = [0, 0, 0, 0, 0];
  let previousSlot = -1;
  for (let index = 0; index < MAX_FRIENDS; index += 1) {
    const offset = 45380 + index * 20;
    if (index >= count) {
      if (!wordsAreZero(view, offset, 20)) return null;
      continue;
    }
    const slot = view.getUint32(offset, true);
    const type = view.getUint32(offset + 4, true);
    const status = view.getUint32(offset + 8, true);
    const friendId = view.getUint32(offset + 12, true);
    const zoneId = view.getUint32(offset + 16, true);
    if (
      slot <= previousSlot
      || slot >= MAX_RAW_FRIENDS
      || type >= FRIEND_TYPE_NAMES.length
      || status >= FRIEND_STATUS_NAMES.length
    ) {
      return null;
    }
    previousSlot = slot;
    observed[type] += 1;
    entries.push(Object.freeze({
      slot,
      type,
      typeName: FRIEND_TYPE_NAMES[type],
      status,
      statusName: FRIEND_STATUS_NAMES[status],
      friendId,
      zoneId,
      isOnline: status >= 1 && status <= 3,
    }));
  }
  const declared = [total - friends - ignores - partners - traders, friends, ignores, partners, traders];
  for (let type = 0; type < declared.length; type += 1) {
    if (
      observed[type] > declared[type]
      || (!friendsTruncated && observed[type] !== declared[type])
    ) {
      return null;
    }
  }
  return Object.freeze({
    playerStatus,
    playerStatusName: FRIEND_STATUS_NAMES[playerStatus],
    friends: Object.freeze({
      truncated: friendsTruncated,
      total,
      friends,
      ignores,
      partners,
      traders,
      entries: Object.freeze(entries),
    }),
    guild,
  });
}

function readCompletion(view) {
  const counts = [];
  for (let category = 0; category < 6; category += 1) {
    const count = view.getUint32(47940 + category * 4, true);
    if (count > MAX_COMPLETION_WORDS) return null;
    counts.push(count);
  }

  const categories = [];
  for (let category = 0; category < 6; category += 1) {
    const mapIds = [];
    const base = 47964 + category * MAX_COMPLETION_WORDS * 4;
    for (let wordIndex = 0; wordIndex < MAX_COMPLETION_WORDS; wordIndex += 1) {
      const word = view.getUint32(base + wordIndex * 4, true);
      if (wordIndex >= counts[category]) {
        if (word !== 0) return null;
        continue;
      }
      for (let bit = 0; bit < 32; bit += 1) {
        if ((word & (1 << bit)) !== 0) mapIds.push(wordIndex * 32 + bit);
      }
    }
    categories.push(Object.freeze(mapIds));
  }
  return Object.freeze({
    normalMode: Object.freeze({
      completedMissions: categories[0],
      completedBonuses: categories[1],
    }),
    hardMode: Object.freeze({
      completedMissions: categories[2],
      completedBonuses: categories[3],
    }),
    unlockedMaps: categories[4],
    vanquishedAreas: categories[5],
  });
}

function readCamera(view) {
  const lookAtAgentId = view.getUint32(48732, true);
  const mode = view.getUint32(48736, true);
  const yaw = view.getFloat32(48740, true);
  const pitch = view.getFloat32(48744, true);
  const distance = view.getFloat32(48748, true);
  const maxDistance = view.getFloat32(48752, true);
  const position = Object.freeze({
    x: view.getFloat32(48756, true),
    y: view.getFloat32(48760, true),
    z: view.getFloat32(48764, true),
  });
  const lookAt = Object.freeze({
    x: view.getFloat32(48768, true),
    y: view.getFloat32(48772, true),
    z: view.getFloat32(48776, true),
  });
  const fieldOfView = view.getFloat32(48780, true);
  if (
    lookAtAgentId > MAX_AGENT_ID
    || mode > 9
    || !Number.isFinite(yaw)
    || Math.abs(yaw) > 10
    || !Number.isFinite(pitch)
    || pitch < -1.01
    || pitch > 1.01
    || !Number.isFinite(distance)
    || distance < 0
    || distance > 100_000
    || !Number.isFinite(maxDistance)
    || maxDistance < 0
    || maxDistance > 100_000
    || !Object.values(position).every(validCoordinate)
    || !Object.values(lookAt).every(validCoordinate)
    || !Number.isFinite(fieldOfView)
    || fieldOfView <= 0
    || fieldOfView > Math.PI
  ) {
    return null;
  }
  const tangent = Math.atan2(position.y - lookAt.y, position.x - lookAt.x);
  const currentYaw = tangent >= 0 ? tangent - Math.PI : tangent + Math.PI;
  const renderFieldOfView = Math.atan2(
    1,
    (5 / 3) / Math.tan(fieldOfView * 0.5),
  ) * 2;
  if (!Number.isFinite(currentYaw) || !Number.isFinite(renderFieldOfView)) {
    return null;
  }
  const modeName = mode === 0
    ? 'Default'
    : mode === 2
      ? 'Follow'
      : mode === 3
        ? 'Unlocked'
        : 'Unknown';
  return Object.freeze({
    lookAtAgentId,
    mode,
    modeName,
    unlocked: mode === 3,
    yaw,
    currentYaw,
    pitch,
    distance,
    maxDistance,
    position,
    lookAt,
    fieldOfView,
    renderFieldOfView,
  });
}

function readTradeItems(view, base, count) {
  const items = [];
  const seen = new Set();
  for (let index = 0; index < MAX_TRADE_ITEMS; index += 1) {
    const offset = base + index * 8;
    const itemId = view.getUint32(offset, true);
    const quantity = view.getUint32(offset + 4, true);
    if (index >= count) {
      if (itemId !== 0 || quantity !== 0) return null;
      continue;
    }
    if (
      itemId === 0
      || itemId > MAX_TRADE_ITEM_ID
      || quantity === 0
      || quantity > MAX_TRADE_QUANTITY
      || seen.has(itemId)
    ) {
      return null;
    }
    seen.add(itemId);
    items.push(Object.freeze({ slot: index + 1, itemId, quantity }));
  }
  return Object.freeze(items);
}

function readTrade(view) {
  const flags = view.getUint32(48784, true);
  const playerGold = view.getUint32(48788, true);
  const partnerGold = view.getUint32(48792, true);
  const playerCount = view.getUint32(48796, true);
  const partnerCount = view.getUint32(48800, true);
  const pageFlags = view.getUint32(48804, true);
  if (
    (flags & ~KNOWN_TRADE_STATUS_FLAGS) !== 0
    || playerGold > 100_000
    || partnerGold > 100_000
    || playerCount > MAX_TRADE_ITEMS
    || partnerCount > MAX_TRADE_ITEMS
    || (pageFlags & ~KNOWN_TRADE_PAGE_FLAGS) !== 0
    || ((pageFlags & TRADE_PAGE_FLAGS.playerItemsTruncated) !== 0
      && playerCount !== MAX_TRADE_ITEMS)
    || ((pageFlags & TRADE_PAGE_FLAGS.partnerItemsTruncated) !== 0
      && partnerCount !== MAX_TRADE_ITEMS)
  ) {
    return null;
  }
  const playerItems = readTradeItems(view, 48808, playerCount);
  const partnerItems = readTradeItems(view, 48936, partnerCount);
  if (playerItems === null || partnerItems === null) return null;
  const open = flags !== 0;
  if (
    !open
    && (
      playerGold !== 0
      || partnerGold !== 0
      || playerCount !== 0
      || partnerCount !== 0
      || pageFlags !== 0
    )
  ) {
    return null;
  }
  const initiated = (flags & TRADE_STATUS_FLAGS.initiated) !== 0;
  const offerSent = (flags & TRADE_STATUS_FLAGS.offerSent) !== 0;
  const accepted = (flags & TRADE_STATUS_FLAGS.accepted) !== 0;
  return Object.freeze({
    flags,
    statusName: accepted
      ? 'Accepted'
      : offerSent
        ? 'OfferSent'
        : initiated
          ? 'Initiated'
          : 'Closed',
    open,
    initiated,
    offerSent,
    accepted,
    player: Object.freeze({
      gold: playerGold,
      itemsTruncated:
        (pageFlags & TRADE_PAGE_FLAGS.playerItemsTruncated) !== 0,
      items: playerItems,
    }),
    partner: Object.freeze({
      gold: partnerGold,
      itemsTruncated:
        (pageFlags & TRADE_PAGE_FLAGS.partnerItemsTruncated) !== 0,
      items: partnerItems,
    }),
  });
}

function readUi(view) {
  const base = 49064;
  const pageFlags = view.getUint32(base, true);
  const frameCount = view.getUint32(base + 4, true);
  const total = view.getUint32(base + 8, true);
  const createdTotal = view.getUint32(base + 12, true);
  const visibleTotal = view.getUint32(base + 16, true);
  const truncated = (pageFlags & UI_PAGE_TRUNCATED) !== 0;
  if (
    (pageFlags & ~UI_PAGE_TRUNCATED) !== 0
    || frameCount > MAX_UI_FRAMES
    || total < frameCount
    || total > MAX_RAW_UI_FRAMES
    || createdTotal > total
    || visibleTotal > createdTotal
    || (truncated
      ? frameCount !== MAX_UI_FRAMES || total <= MAX_UI_FRAMES
      : frameCount !== total)
  ) {
    return null;
  }

  const frames = [];
  const frameIds = new Set();
  let publishedCreated = 0;
  let publishedVisible = 0;
  for (let index = 0; index < MAX_UI_FRAMES; index += 1) {
    const offset = base + 20 + index * 56;
    if (index >= frameCount) {
      if (!wordsAreZero(view, offset, 56)) return null;
      continue;
    }
    const recordFlags = view.getUint32(offset, true);
    const frameId = view.getUint32(offset + 4, true);
    const parentValue = view.getUint32(offset + 8, true);
    const childOffsetId = view.getUint32(offset + 12, true);
    const frameHash = view.getUint32(offset + 16, true);
    const visibilityFlags = view.getUint32(offset + 20, true);
    const type = view.getUint32(offset + 24, true);
    const templateType = view.getUint32(offset + 28, true);
    const state = view.getUint32(offset + 32, true);
    const positionFlags = view.getUint32(offset + 36, true);
    const position = {
      left: view.getFloat32(offset + 40, true),
      bottom: view.getFloat32(offset + 44, true),
      right: view.getFloat32(offset + 48, true),
      top: view.getFloat32(offset + 52, true),
    };
    const positionValid =
      (recordFlags & UI_RECORD_POSITION_VALID) !== 0;
    if (
      (recordFlags & ~UI_RECORD_POSITION_VALID) !== 0
      || frameId >= MAX_RAW_UI_FRAMES
      || frameIds.has(frameId)
      || (parentValue !== 0xffff_ffff
        && (parentValue >= MAX_RAW_UI_FRAMES || parentValue === frameId))
      || (positionValid
        ? !Object.values(position).every(validCoordinate)
        : positionFlags !== 0
          || Object.values(position).some((value) => value !== 0))
    ) {
      return null;
    }
    frameIds.add(frameId);
    const created = (state & UI_FRAME_CREATED) !== 0;
    const destroying = (state & UI_FRAME_DESTROYING) !== 0;
    const disabled = (state & UI_FRAME_DISABLED) !== 0;
    const hidden = (state & UI_FRAME_HIDDEN) !== 0;
    const locallyVisible = created && !destroying && !hidden;
    publishedCreated += Number(created);
    publishedVisible += Number(locallyVisible);
    frames.push(Object.freeze({
      frameId,
      parentId: parentValue === 0xffff_ffff ? null : parentValue,
      childOffsetId,
      frameHash,
      visibilityFlags,
      type,
      templateType,
      state,
      created,
      destroying,
      disabled,
      hidden,
      locallyVisible,
      positionValid,
      positionFlags,
      position: Object.freeze(position),
    }));
  }
  if (
    publishedCreated > createdTotal
    || publishedVisible > visibleTotal
    || (!truncated
      && (publishedCreated !== createdTotal
        || publishedVisible !== visibleTotal))
    || (!truncated
      && frames.some(
        (frame) => frame.parentId !== null && !frameIds.has(frame.parentId),
      ))
  ) {
    return null;
  }
  return Object.freeze({
    truncated,
    total,
    createdTotal,
    visibleTotal,
    frames: Object.freeze(frames),
  });
}

function readMerchant(view) {
  const base = 56252;
  const pageFlags = view.getUint32(base, true);
  const count = view.getUint32(base + 4, true);
  const total = view.getUint32(base + 8, true);
  const truncated = (pageFlags & 1) !== 0;
  if (
    (pageFlags & ~1) !== 0
    || count > MAX_MERCHANT_ITEMS
    || total > MAX_RAW_MERCHANT_ITEMS
    || total < count
    || (truncated
      ? count !== MAX_MERCHANT_ITEMS || total <= MAX_MERCHANT_ITEMS
      : total !== count)
  ) {
    return null;
  }
  const itemIds = [];
  for (let index = 0; index < MAX_MERCHANT_ITEMS; index += 1) {
    const itemId = view.getUint32(base + 12 + index * 4, true);
    if (index >= count) {
      if (itemId !== 0) return null;
      continue;
    }
    if (itemId === 0 || itemId > MAX_MERCHANT_ITEM_ID) return null;
    itemIds.push(itemId);
  }
  return Object.freeze({
    truncated,
    total,
    itemIds: Object.freeze(itemIds),
  });
}

function readProgression(view) {
  const base = 56776;
  const hardModeUnlocked = view.getUint32(base, true);
  const level = view.getUint32(base + 4, true);
  const experience = view.getUint32(base + 8, true);
  const readFaction = (offset) => {
    const current = view.getUint32(offset, true);
    const totalEarned = view.getUint32(offset + 4, true);
    const maximum = view.getUint32(offset + 8, true);
    if (
      current > MAX_FACTION_CURRENT
      || totalEarned > MAX_FACTION_TOTAL
      || maximum > MAX_FACTION_CURRENT
      || current > maximum
      || totalEarned < current
    ) {
      return null;
    }
    return Object.freeze({ current, totalEarned, maximum });
  };
  const kurzick = readFaction(base + 12);
  const luxon = readFaction(base + 24);
  const imperial = readFaction(base + 36);
  const balthazar = readFaction(base + 48);
  const currentSkillPoints = view.getUint32(base + 60, true);
  const totalSkillPoints = view.getUint32(base + 64, true);
  if (
    hardModeUnlocked > 1
    || level < 1
    || level > 20
    || experience > MAX_EXPERIENCE
    || kurzick === null
    || luxon === null
    || imperial === null
    || balthazar === null
    || currentSkillPoints > MAX_SKILL_POINTS_CURRENT
    || totalSkillPoints > MAX_SKILL_POINTS_TOTAL
    || currentSkillPoints > totalSkillPoints
  ) {
    return null;
  }
  return Object.freeze({
    hardModeUnlocked: hardModeUnlocked === 1,
    level,
    experience,
    factions: Object.freeze({ kurzick, luxon, imperial, balthazar }),
    skillPoints: Object.freeze({
      current: currentSkillPoints,
      totalEarned: totalSkillPoints,
    }),
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
  const agentsValid = (flags & FLAGS.agents) !== 0;
  const agents = agentsValid ? readMapAgents(view) : null;
  if (
    (agentsValid && agents === null)
    || (!agentsValid && !wordsAreZero(view, 2804, 7180))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const questsValid = (flags & FLAGS.quests) !== 0;
  const quests = questsValid ? readQuests(view) : null;
  if (
    (questsValid && quests === null)
    || (!questsValid && !wordsAreZero(view, 9984, 2064))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const inventoryValid = (flags & FLAGS.inventory) !== 0;
  const inventory = inventoryValid ? readInventory(view) : null;
  if (
    (inventoryValid && inventory === null)
    || (!inventoryValid && !wordsAreZero(view, 12048, 33236))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const socialValid = (flags & FLAGS.social) !== 0;
  const social = socialValid ? readSocial(view) : null;
  if (
    (socialValid && social === null)
    || (!socialValid && !wordsAreZero(view, 45284, 2656))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const completionValid = (flags & FLAGS.completion) !== 0;
  const completion = completionValid ? readCompletion(view) : null;
  if (
    (completionValid && completion === null)
    || (!completionValid && !wordsAreZero(view, 47940, 792))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const cameraValid = (flags & FLAGS.camera) !== 0;
  const camera = cameraValid ? readCamera(view) : null;
  if (
    (cameraValid && camera === null)
    || (!cameraValid && !wordsAreZero(view, 48732, 52))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const tradeValid = (flags & FLAGS.trade) !== 0;
  const trade = tradeValid ? readTrade(view) : null;
  if (
    (tradeValid && trade === null)
    || (!tradeValid && !wordsAreZero(view, 48784, 280))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const uiValid = (flags & FLAGS.ui) !== 0;
  const ui = uiValid ? readUi(view) : null;
  if (
    (uiValid && ui === null)
    || (!uiValid && !wordsAreZero(view, 49064, 7188))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const merchantValid = (flags & FLAGS.merchant) !== 0;
  const merchant = merchantValid ? readMerchant(view) : null;
  if (
    (merchantValid && merchant === null)
    || (!merchantValid && !wordsAreZero(view, 56252, 524))
  ) {
    return Object.freeze({ status: 'waiting', reason: 'corrupt' });
  }
  const progressionValid = (flags & FLAGS.progression) !== 0;
  const progression = progressionValid ? readProgression(view) : null;
  if (
    (progressionValid && progression === null)
    || (!progressionValid && !wordsAreZero(view, 56776, 68))
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
    ...(agents ? { agents } : {}),
    ...(quests ? { quests } : {}),
    ...(inventory ? { inventory } : {}),
    ...(social ? { social } : {}),
    ...(completion ? { completion } : {}),
    ...(camera ? { camera } : {}),
    ...(trade ? { trade } : {}),
    ...(ui ? { ui } : {}),
    ...(merchant ? { merchant } : {}),
    ...(progression ? { progression } : {}),
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
