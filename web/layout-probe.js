// Bounded live-layout certification for E2E builds.
//
// This never exposes memory contents. It tests nearby aligned addresses
// against the same map, character, agent-array, and player invariants as the
// companion and returns only the deltas that satisfy all of them.

const WORD = 4;
const MAX_RADIUS = 4096;
const MAX_CANDIDATES = 16;
const MAX_UI_FRAMES = 2048;
const MAX_UI_CAPACITY = 4096;
const UI_FRAME_CREATED = 0x4;
const UI_FRAME_DESTROYING = 0x8;
const UI_FRAME_HIDDEN = 0x200;
const UI_LAYOUT = Object.freeze({
  frameArray: 182,
  frameSize: 183,
  frameId: 188,
  parentRelation: 195,
  frameHash: 196,
  frameState: 197,
});
const FRAME_HASH_MIX = Object.freeze([
  0x92b9a528, 0x25d4fc88, 0xedcbefb8, 0x51063a80,
  0x91341c61, 0x0261229d, 0x726f48ed, 0xce1c088c,
  0x76253eb5, 0x31e3a0de, 0xa2aad215, 0xca7d6d27,
  0xa5f98970, 0x0541c365, 0x3c14ff04, 0x5056af4f,
]);
const QUEST_LAYOUT = Object.freeze({
  gameWorldContext: 26,
  activeQuest: 76,
  questLog: 77,
  questStride: 78,
  questId: 79,
  questLogState: 80,
  questMapFrom: 81,
  questMarker: 82,
  questMapTo: 83,
  missionObjectives: 84,
  objectiveStride: 85,
  objectiveId: 86,
  objectiveType: 87,
});
const INVENTORY_LAYOUT = Object.freeze({
  gameItemContext: 94,
  itemContextInventory: 95,
  inventoryBags: 96,
  inventoryStoragePanes: 97,
  inventoryGoldCharacter: 98,
  inventoryGoldStorage: 99,
  bagType: 100,
  bagIndex: 101,
  bagContainerItem: 102,
  bagItemsCount: 103,
  bagItems: 104,
  itemId: 105,
  itemAgentId: 106,
  itemBag: 107,
  itemModifiers: 108,
  itemModifierCount: 109,
  itemModelFileId: 111,
  itemType: 112,
  itemModelId: 116,
  itemMaterialSalvageable: 118,
  itemQuantity: 119,
  itemEquipped: 120,
  itemProfession: 121,
  itemSlot: 122,
});
const COMPLETION_LAYOUT = Object.freeze({
  gameWorldContext: 26,
  fields: Object.freeze([88, 89, 90, 91, 92, 93]),
});
const SOCIAL_LAYOUT = Object.freeze({
  friendListAddress: 123,
  friendListFriends: 124,
  friendListNumberFriend: 125,
  friendListNumberIgnore: 126,
  friendListNumberPartner: 127,
  friendListNumberTrade: 128,
  friendListPlayerStatus: 129,
  friendType: 130,
  friendStatus: 131,
  friendId: 132,
  friendZoneId: 133,
  gameGuildContext: 134,
  guildContextPlayerIndex: 135,
  guildContextPlayerKey: 136,
  guildContextPlayerRank: 137,
  guildContextGuilds: 138,
  guildContextRoster: 139,
  guildKey: 140,
  guildIndex: 141,
  guildFaction: 145,
  guildCape: 148,
  guildPlayerStride: 149,
});

const finitePosition = (value) =>
  Number.isFinite(value) && Math.abs(value) <= 1_000_000;

const validAgentType = (value) => (value & (0x400 | 0x200 | 0xdb)) !== 0;

function reader(buffer) {
  const view = new DataView(buffer);
  const contains = (address, bytes) =>
    Number.isInteger(address)
    && address >= 0
    && Number.isInteger(bytes)
    && bytes >= 0
    && address + bytes <= view.byteLength;
  const u32 = (address) =>
    contains(address, 4) ? view.getUint32(address, true) : null;
  const u16 = (address) =>
    contains(address, 2) ? view.getUint16(address, true) : null;
  const u8 = (address) =>
    contains(address, 1) ? view.getUint8(address) : null;
  const f32 = (address) =>
    contains(address, 4) ? view.getFloat32(address, true) : null;
  const pointer = (address, bytes) => {
    const value = u32(address);
    return value !== null && value % WORD === 0 && contains(value, bytes)
      ? value
      : null;
  };
  return { contains, u8, u16, u32, f32, pointer };
}

/** Guild Wars' case-insensitive `StrHashI(label, -1)` frame-label hash. */
export function frameLabelHash(label) {
  let hash = 0x325d1eae;
  let first = 0xe2c15c9d;
  let second = 0x2170a28a;
  for (const character of String(label)) {
    let code = character.codePointAt(0) & 0xffff;
    if (code >= 0x61 && code <= 0x7a) code -= 0x20;
    first = (code ^ (first << 3)) >>> 0;
    second = (FRAME_HASH_MIX[first & 0xf] + second) >>> 0;
    hash = (((second + first) >>> 0) ^ hash) >>> 0;
  }
  return hash;
}

const SELECTOR_HASH = frameLabelHash('Selector');
const PLAY_HASH = frameLabelHash('Play');
const DISTRICT_HASH = frameLabelHash('District');

function readUiFrames(buffer, layoutWords) {
  if (
    !(buffer instanceof ArrayBuffer)
    || !Array.isArray(layoutWords)
    || layoutWords.length !== 232
    || layoutWords[UI_LAYOUT.frameSize] !== 0x1c8
  ) return null;

  const read = reader(buffer);
  const array = layoutWords[UI_LAYOUT.frameArray];
  if (!Number.isInteger(array) || array % WORD !== 0 || !read.contains(array, 12)) {
    return null;
  }
  const frameBuffer = read.u32(array);
  const capacity = read.u32(array + 4);
  const size = read.u32(array + 8);
  if (
    frameBuffer === null
    || capacity === null
    || size === null
    || size > capacity
    || size > MAX_UI_FRAMES
    || capacity > MAX_UI_CAPACITY
    || frameBuffer % WORD !== 0
    || !read.contains(frameBuffer, size * WORD)
  ) return null;

  const frames = new Map();
  for (let frameId = 0; frameId < size; frameId += 1) {
    const address = read.u32(frameBuffer + frameId * WORD);
    if (address === null) return null;
    if (address === 0 || address === 0xffff_ffff) continue;
    if (
      address % WORD !== 0
      || !read.contains(address, layoutWords[UI_LAYOUT.frameSize])
      || read.u32(address + layoutWords[UI_LAYOUT.frameId]) !== frameId
    ) return null;
    const relation = read.u32(address + layoutWords[UI_LAYOUT.parentRelation]);
    const hash = read.u32(address + layoutWords[UI_LAYOUT.frameHash]);
    const state = read.u32(address + layoutWords[UI_LAYOUT.frameState]);
    if (relation === null || hash === null || state === null) return null;
    let parentId = null;
    if (relation !== 0) {
      const parent = relation - layoutWords[UI_LAYOUT.parentRelation];
      if (parent < 0 || parent % WORD !== 0) return null;
      parentId = read.u32(parent + layoutWords[UI_LAYOUT.frameId]);
      if (
        parentId === null
        || parentId >= size
        || read.u32(frameBuffer + parentId * WORD) !== parent
      ) return null;
    }
    frames.set(frameId, { parentId, hash, state });
  }
  return frames;
}

/**
 * Prove that the visible character selector and its Play control are created.
 *
 * This E2E-only reader uses only fields already covered by the signed passive
 * layout. It returns one boolean, never frame contents or character names.
 */
export function characterSelectionState(buffer, layoutWords) {
  const frames = readUiFrames(buffer, layoutWords);
  if (!frames) return 'layout-invalid';

  const selector = [...frames.values()].find((frame) => frame.hash === SELECTOR_HASH);
  const play = [...frames.values()].find((frame) => frame.hash === PLAY_HASH);
  if (!selector) return 'selector-missing';
  if ((selector.state & UI_FRAME_HIDDEN) !== 0) return 'selector-hidden';
  if (
    (selector.state & UI_FRAME_CREATED) === 0
    || (selector.state & UI_FRAME_DESTROYING) !== 0
  ) return 'selector-not-created';
  if (!play) return 'play-missing';
  if (
    (play.state & UI_FRAME_CREATED) === 0
    || (play.state & UI_FRAME_DESTROYING) !== 0
  ) return 'play-not-created';
  const playParent = frames.get(play.parentId);
  if (
    !playParent
    || (playParent.state & UI_FRAME_CREATED) === 0
    || (playParent.state & UI_FRAME_DESTROYING) !== 0
  ) return 'play-parent-not-created';
  return 'ready';
}

export const characterSelectionReady = (buffer, layoutWords) =>
  characterSelectionState(buffer, layoutWords) === 'ready';

/**
 * Read only the map placement fields needed to verify the fixed benchmark
 * travel command. District and language are adjacent members of the same
 * certified CharacterContext as current_map_id; no pointer or label leaves
 * the page.
 */
export function benchmarkSceneState(buffer, layoutWords) {
  if (
    !(buffer instanceof ArrayBuffer)
    || !Array.isArray(layoutWords)
    || layoutWords.length !== 232
  ) return null;
  const read = reader(buffer);
  const contexts = read.pointer(layoutWords[0], 28);
  const game = contexts === null
    ? null
    : read.pointer(contexts + layoutWords[4] * WORD, 0x50);
  const character = game === null
    ? null
    : read.pointer(game + layoutWords[5], 0x2b0);
  const currentMapOffset = layoutWords[8];
  if (character === null || currentMapOffset < 12) return null;
  const mapId = read.u32(character + currentMapOffset);
  const district = read.u32(character + currentMapOffset - 12);
  const language = read.u32(character + currentMapOffset - 8);
  const instanceType = read.u32(character + layoutWords[9]);
  if (
    mapId === null
    || mapId > 2_000
    || district === null
    || district > 1_000
    || language === null
    || language > 11
    || instanceType === null
    || instanceType > 2
  ) return null;
  return Object.freeze({ mapId, district, language, instanceType });
}

/** Prove that one visible District selector exists in the certified frame tree. */
export function benchmarkUiState(buffer, layoutWords) {
  const frames = readUiFrames(buffer, layoutWords);
  if (!frames) return { districtPresent: false };
  const district = [...frames.values()].filter((frame) => frame.hash === DISTRICT_HASH);
  const visible = (frame) => (
    (frame.state & UI_FRAME_CREATED) !== 0
    && (frame.state & (UI_FRAME_DESTROYING | UI_FRAME_HIDDEN)) === 0
  );
  return {
    districtPresent: district.length === 1 && visible(district[0]),
  };
}

function contextAt(read, layout, delta) {
  const contexts = read.pointer(layout[0] + delta, 28);
  if (contexts === null) return null;
  const game = read.pointer(contexts + layout[4] * WORD, 0x50);
  if (game === null) return null;
  const character = read.pointer(game + layout[5], 0x2b0);
  if (character === null) return null;
  const baseMap = read.u32(character + layout[6]);
  const isExplorable = read.u32(character + layout[7]);
  const mapId = read.u32(character + layout[8]);
  const instanceType = read.u32(character + layout[9]);
  const playerNumber = read.u32(character + layout[10]);
  if (
    mapId === null
    || mapId === 0
    || mapId > 2000
    || baseMap !== mapId
    || instanceType === null
    || instanceType > 1
    || isExplorable !== Number(instanceType === 1)
    || playerNumber === null
    || playerNumber === 0
    || playerNumber > 0xffff
  ) {
    return null;
  }
  return { game, playerNumber };
}

function agentArrayAt(read, layout, delta, playerNumber) {
  const array = layout[1] + delta;
  if (!read.contains(array, 16)) return false;
  const buffer = read.u32(array);
  const capacity = read.u32(array + 4);
  const size = read.u32(array + 8);
  if (
    buffer === null
    || capacity === null
    || size === null
    || size === 0
    || size > capacity
    || capacity > 4096
    || !read.contains(buffer, size * WORD)
  ) {
    return false;
  }
  for (let id = 1; id < size; id += 1) {
    const agent = read.pointer(buffer + id * WORD, 0x100);
    if (agent === null) continue;
    if (
      read.u32(agent + layout[11]) !== id
      || read.u16(agent + layout[17]) !== playerNumber
      || ((read.u16(agent + layout[18]) ?? 0) & 0xf000) !== 0x3000
    ) {
      continue;
    }
    const type = read.u32(agent + layout[16]);
    const x = read.f32(agent + layout[12]);
    const y = read.f32(agent + layout[13]);
    return (
      type !== null
      && validAgentType(type)
      && x !== null
      && y !== null
      && finitePosition(x)
      && finitePosition(y)
    );
  }
  return false;
}

const emptyQuestProbe = () => ({
  worldAvailable: false,
  activeQuestId: 0,
  questCapacity: 0,
  questCount: 0,
  questInvalidIndex: 0xffff_ffff,
  questInvalidMask: 0,
  objectiveCapacity: 0,
  objectiveCount: 0,
  questRecordsValid: false,
  activeQuestPresent: false,
  objectiveRecordsValid: false,
});
const emptyInventoryProbe = () => ({
  itemContextAvailable: false,
  inventoryAvailable: false,
  scalarFieldsValid: false,
  storagePanesUnlocked: 0,
  bagPointerCount: 0,
  backpackPresent: false,
  bagInvalidId: 0,
  bagInvalidMask: 0,
  itemCount: 0,
  itemInvalidBagId: 0,
  itemInvalidSlot: 0xffff_ffff,
  itemInvalidMask: 0,
  inventoryRecordsValid: false,
});
const emptySocialProbe = () => ({
  friendListAvailable: false,
  friendHeaderValid: false,
  playerStatus: 0xffff_ffff,
  friendCapacity: 0,
  friendSlotCount: 0,
  friendEntryCount: 0,
  friendInvalidSlot: 0xffff_ffff,
  friendInvalidMask: 0,
  friendCountMismatchMask: 0,
  friendRecordsValid: false,
  guildContextAvailable: false,
  guildIndex: 0xffff_ffff,
  guildRecordPresent: false,
  guildRosterCapacity: 0,
  guildRosterCount: 0,
  guildInvalidMask: 0,
  guildRecordsValid: false,
  socialRecordsValid: false,
});
const emptyCompletionProbe = () => ({
  worldAvailable: false,
  capacities: Object.freeze([0, 0, 0, 0, 0, 0]),
  sizes: Object.freeze([0, 0, 0, 0, 0, 0]),
  invalidMasks: Object.freeze([0, 0, 0, 0, 0, 0]),
  completionRecordsValid: false,
});

function arrayDescriptor(read, address, stride, maximumSize, maximumCapacity) {
  const buffer = read.u32(address);
  const capacity = read.u32(address + 4);
  const size = read.u32(address + 8);
  const headerValid = (
    buffer !== null
    && capacity !== null
    && size !== null
    && stride > 0
    && size <= capacity
    && size <= maximumSize
    && capacity <= maximumCapacity
  );
  const storageValid = headerValid && (
    size === 0
    || (buffer % WORD === 0 && read.contains(buffer, size * stride))
  );
  return {
    buffer: buffer ?? 0,
    capacity: capacity ?? 0,
    size: size ?? 0,
    valid: storageValid,
  };
}

function questProbeAt(read, layout, game) {
  const q = QUEST_LAYOUT;
  const world = read.pointer(
    game + layout[q.gameWorldContext],
    layout[q.missionObjectives] + 16,
  );
  if (world === null) return emptyQuestProbe();

  const activeQuestId = read.u32(world + layout[q.activeQuest]) ?? 0;
  const quests = arrayDescriptor(
    read,
    world + layout[q.questLog],
    layout[q.questStride],
    256,
    1024,
  );
  const objectives = arrayDescriptor(
    read,
    world + layout[q.missionObjectives],
    layout[q.objectiveStride],
    128,
    512,
  );

  let questRecordsValid = quests.valid && activeQuestId <= 100_000;
  let activeQuestPresent = activeQuestId === 0;
  let questInvalidIndex = 0xffff_ffff;
  let questInvalidMask = 0;
  const questIds = new Set();
  if (questRecordsValid) {
    for (let index = 0; index < quests.size; index += 1) {
      const entry = quests.buffer + index * layout[q.questStride];
      const marker = entry + layout[q.questMarker];
      const questId = read.u32(entry + layout[q.questId]);
      const mapFrom = read.u32(entry + layout[q.questMapFrom]);
      const markerX = read.f32(marker);
      const markerY = read.f32(marker + 4);
      const markerPlane = read.u32(marker + 8);
      const mapTo = read.u32(entry + layout[q.questMapTo]);
      const invalidMask = (
        Number(questId === null || questId === 0 || questId > 100_000)
        | (Number(questId !== null && questIds.has(questId)) << 1)
        | (Number(read.u32(entry + layout[q.questLogState]) === null) << 2)
        | (Number(mapFrom === null || mapFrom > 2_000) << 3)
        | (Number(markerX === null || !finitePosition(markerX)) << 4)
        | (Number(markerY === null || !finitePosition(markerY)) << 5)
        | (Number(markerPlane === null || markerPlane > 100_000) << 6)
        | (Number(mapTo === null || mapTo > 2_000) << 7)
      );
      if (invalidMask !== 0) {
        questRecordsValid = false;
        questInvalidIndex = index;
        questInvalidMask = invalidMask;
        break;
      }
      questIds.add(questId);
      activeQuestPresent ||= questId === activeQuestId;
    }
  }

  let objectiveRecordsValid = objectives.valid;
  const objectiveIds = new Set();
  if (objectiveRecordsValid) {
    for (let index = 0; index < objectives.size; index += 1) {
      const entry = objectives.buffer + index * layout[q.objectiveStride];
      const objectiveId = read.u32(entry + layout[q.objectiveId]);
      const objectiveType = read.u32(entry + layout[q.objectiveType]);
      if (
        objectiveId === null
        || objectiveId === 0
        || objectiveIds.has(objectiveId)
        || objectiveType === null
        || objectiveType > 100_000
      ) {
        objectiveRecordsValid = false;
        break;
      }
      objectiveIds.add(objectiveId);
    }
  }

  return {
    worldAvailable: true,
    activeQuestId,
    questCapacity: quests.capacity,
    questCount: quests.size,
    questInvalidIndex,
    questInvalidMask,
    objectiveCapacity: objectives.capacity,
    objectiveCount: objectives.size,
    questRecordsValid,
    activeQuestPresent,
    objectiveRecordsValid,
  };
}

const expectedBagType = (bagId) => {
  if (bagId >= 1 && bagId <= 5) return 1;
  if (bagId === 6) return 5;
  if (bagId === 7) return 3;
  if (bagId >= 8 && bagId <= 21) return 4;
  if (bagId === 22) return 2;
  return 0;
};

const validItemType = (itemType) => (
  itemType === 0
  || (itemType >= 2 && itemType <= 13)
  || (itemType >= 15 && itemType <= 22)
  || itemType === 24
  || itemType === 26
  || itemType === 27
  || (itemType >= 29 && itemType <= 36)
  || (itemType >= 43 && itemType <= 45)
  || itemType === 0xff
);

function inventoryProbeAt(read, layout, game) {
  const i = INVENTORY_LAYOUT;
  const itemContext = read.pointer(
    game + layout[i.gameItemContext],
    layout[i.itemContextInventory] + 4,
  );
  if (itemContext === null) return emptyInventoryProbe();
  const inventory = read.pointer(
    itemContext + layout[i.itemContextInventory],
    layout[i.inventoryGoldStorage] + 4,
  );
  if (inventory === null) {
    return {
      ...emptyInventoryProbe(),
      itemContextAvailable: true,
    };
  }

  const storagePanesUnlocked = read.u32(
    inventory + layout[i.inventoryStoragePanes],
  );
  const scalarFieldsValid = (
    storagePanesUnlocked !== null
    && storagePanesUnlocked <= 14
    && read.u32(inventory + layout[i.inventoryGoldCharacter]) !== null
    && read.u32(inventory + layout[i.inventoryGoldStorage]) !== null
  );
  let bagPointerCount = 0;
  let backpackPresent = false;
  let bagInvalidId = 0;
  let bagInvalidMask = 0;
  let itemCount = 0;
  let itemInvalidBagId = 0;
  let itemInvalidSlot = 0xffff_ffff;
  let itemInvalidMask = 0;
  let totalSlots = 0;

  for (let bagId = 1; scalarFieldsValid && bagId <= 22; bagId += 1) {
    const bagAddress = read.u32(
      inventory + layout[i.inventoryBags] + bagId * WORD,
    );
    if (bagAddress === 0) continue;
    if (
      bagAddress === null
      || bagAddress % WORD !== 0
      || !read.contains(bagAddress, layout[i.bagItems] + 12)
    ) {
      bagInvalidId = bagId;
      bagInvalidMask = 1;
      break;
    }

    const items = arrayDescriptor(
      read,
      bagAddress + layout[i.bagItems],
      WORD,
      256,
      256,
    );
    const itemCountField = read.u32(bagAddress + layout[i.bagItemsCount]);
    const bagMask = (
      Number(read.u32(bagAddress + layout[i.bagType]) !== expectedBagType(bagId))
      | (Number(read.u32(bagAddress + layout[i.bagIndex]) !== bagId - 1) << 1)
      | (Number(read.u32(bagAddress + layout[i.bagContainerItem]) === null) << 2)
      | (Number(!items.valid) << 3)
      | (Number(itemCountField === null || itemCountField > items.size) << 4)
      | (Number(totalSlots + items.size > 1024) << 5)
    );
    if (bagMask !== 0) {
      bagInvalidId = bagId;
      bagInvalidMask = bagMask;
      break;
    }

    totalSlots += items.size;
    bagPointerCount += 1;
    backpackPresent ||= bagId === 1;
    let actualItemCount = 0;
    for (let slot = 0; slot < items.size; slot += 1) {
      const itemAddress = read.u32(items.buffer + slot * WORD);
      if (itemAddress === 0) continue;
      const itemReadable = (
        itemAddress !== null
        && itemAddress % WORD === 0
        && read.contains(itemAddress, layout[i.itemSlot] + 1)
      );
      if (!itemReadable) {
        itemInvalidBagId = bagId;
        itemInvalidSlot = slot;
        itemInvalidMask = 1;
        break;
      }

      const modifierCount = read.u32(itemAddress + layout[i.itemModifierCount]);
      const modifiers = read.u32(itemAddress + layout[i.itemModifiers]);
      const itemType = read.u8(itemAddress + layout[i.itemType]);
      const profession = read.u8(itemAddress + layout[i.itemProfession]);
      const itemMask = (
        Number(
          (read.u32(itemAddress + layout[i.itemId]) ?? 0) === 0
          || (read.u32(itemAddress + layout[i.itemId]) ?? 1_000_001) > 1_000_000,
        )
        | (Number((read.u32(itemAddress + layout[i.itemAgentId]) ?? 4096) > 4095) << 1)
        | (Number(read.u32(itemAddress + layout[i.itemBag]) !== bagAddress) << 2)
        | (Number(read.u8(itemAddress + layout[i.itemSlot]) !== slot) << 3)
        | (Number((read.u32(itemAddress + layout[i.itemModelFileId]) ?? 0) === 0) << 4)
        | (Number(itemType === null || !validItemType(itemType)) << 5)
        | (Number((read.u32(itemAddress + layout[i.itemModelId]) ?? 0) === 0) << 6)
        | (Number((read.u16(itemAddress + layout[i.itemQuantity]) ?? 0) === 0) << 7)
        | (Number((read.u8(itemAddress + layout[i.itemEquipped]) ?? 2) > 1) << 8)
        | (Number(profession === null) << 9)
        | (Number(
          read.u8(itemAddress + layout[i.itemMaterialSalvageable]) === null,
        ) << 10)
        | (Number(modifierCount === null || modifierCount > 64) << 11)
        | (Number(
          modifierCount !== null
          && modifierCount > 0
          && (
            modifiers === null
            || modifiers % WORD !== 0
            || !read.contains(modifiers, modifierCount * WORD)
          ),
        ) << 12)
      );
      if (itemMask !== 0) {
        itemInvalidBagId = bagId;
        itemInvalidSlot = slot;
        itemInvalidMask = itemMask;
        break;
      }
      actualItemCount += 1;
      itemCount += 1;
    }
    if (itemInvalidMask !== 0) break;
    if (actualItemCount !== itemCountField) {
      bagInvalidId = bagId;
      bagInvalidMask = 1 << 6;
      break;
    }
  }

  return {
    itemContextAvailable: true,
    inventoryAvailable: true,
    scalarFieldsValid,
    storagePanesUnlocked: storagePanesUnlocked ?? 0,
    bagPointerCount,
    backpackPresent,
    bagInvalidId,
    bagInvalidMask,
    itemCount,
    itemInvalidBagId,
    itemInvalidSlot,
    itemInvalidMask,
    inventoryRecordsValid: (
      scalarFieldsValid
      && backpackPresent
      && bagPointerCount > 0
      && bagInvalidMask === 0
      && itemInvalidMask === 0
    ),
  };
}

function socialProbeAt(read, layout, game) {
  const s = SOCIAL_LAYOUT;
  const friendList = layout[s.friendListAddress];
  const friendListAvailable = (
    friendList !== 0
    && friendList % WORD === 0
    && read.contains(friendList, layout[s.friendListPlayerStatus] + WORD)
  );
  const playerStatus = friendListAvailable
    ? read.u32(friendList + layout[s.friendListPlayerStatus])
    : null;
  const expectedCounts = friendListAvailable
    ? [
      read.u32(friendList + layout[s.friendListNumberFriend]),
      read.u32(friendList + layout[s.friendListNumberIgnore]),
      read.u32(friendList + layout[s.friendListNumberPartner]),
      read.u32(friendList + layout[s.friendListNumberTrade]),
    ]
    : [null, null, null, null];
  const friendHeaderValid = (
    friendListAvailable
    && playerStatus !== null
    && playerStatus <= 4
    && expectedCounts.every((count) => count !== null && count <= 256)
  );
  const friends = friendListAvailable
    ? arrayDescriptor(
      read,
      friendList + layout[s.friendListFriends],
      WORD,
      256,
      256,
    )
    : { buffer: 0, capacity: 0, size: 0, valid: false };
  let friendEntryCount = 0;
  let friendInvalidSlot = 0xffff_ffff;
  let friendInvalidMask = 0;
  const observedCounts = [0, 0, 0, 0, 0];
  if (friendHeaderValid && friends.valid) {
    for (let slot = 0; slot < friends.size; slot += 1) {
      const address = read.u32(friends.buffer + slot * WORD);
      if (address === 0) continue;
      const readable = (
        address !== null
        && address % WORD === 0
        && read.contains(address, layout[s.friendZoneId] + WORD)
      );
      const type = readable ? read.u32(address + layout[s.friendType]) : null;
      const status = readable ? read.u32(address + layout[s.friendStatus]) : null;
      const invalidMask = (
        Number(!readable)
        | (Number(type === null || type > 4) << 1)
        | (Number(status === null || status > 4) << 2)
      );
      if (invalidMask !== 0) {
        friendInvalidSlot = slot;
        friendInvalidMask = invalidMask;
        break;
      }
      observedCounts[type] += 1;
      friendEntryCount += 1;
    }
  }
  let friendCountMismatchMask = 0;
  if (friendHeaderValid && friends.valid && friendInvalidMask === 0) {
    for (let index = 0; index < expectedCounts.length; index += 1) {
      if (observedCounts[index + 1] !== expectedCounts[index]) {
        friendCountMismatchMask |= 1 << index;
      }
    }
  }
  const friendRecordsValid = (
    friendHeaderValid
    && friends.valid
    && friendInvalidMask === 0
    && friendCountMismatchMask === 0
  );

  const guildContext = read.pointer(
    game + layout[s.gameGuildContext],
    layout[s.guildContextRoster] + 16,
  );
  if (guildContext === null) {
    return {
      ...emptySocialProbe(),
      friendListAvailable,
      friendHeaderValid,
      playerStatus: playerStatus ?? 0xffff_ffff,
      friendCapacity: friends.capacity,
      friendSlotCount: friends.size,
      friendEntryCount,
      friendInvalidSlot,
      friendInvalidMask,
      friendCountMismatchMask,
      friendRecordsValid,
    };
  }

  const guildIndex = read.u32(
    guildContext + layout[s.guildContextPlayerIndex],
  );
  const playerRank = read.u32(
    guildContext + layout[s.guildContextPlayerRank],
  );
  let guildRecordPresent = false;
  let guildRosterCapacity = 0;
  let guildRosterCount = 0;
  let guildInvalidMask = Number(
    guildIndex === null
    || playerRank === null
    || (guildIndex ?? 64) >= 64,
  );
  if (guildInvalidMask === 0 && guildIndex !== 0) {
    const guilds = arrayDescriptor(
      read,
      guildContext + layout[s.guildContextGuilds],
      WORD,
      64,
      256,
    );
    if (!guilds.valid || guildIndex >= guilds.size) {
      guildInvalidMask |= 1 << 1;
    } else {
      const guild = read.u32(guilds.buffer + guildIndex * WORD);
      const readable = (
        guild !== null
        && guild !== 0
        && guild % WORD === 0
        && read.contains(guild, layout[s.guildCape] + 28)
      );
      if (!readable) {
        guildInvalidMask |= 1 << 2;
      } else {
        guildRecordPresent = true;
        let keyMatches = false;
        let keyNonzero = false;
        for (let index = 0; index < 4; index += 1) {
          const contextWord = read.u32(
            guildContext + layout[s.guildContextPlayerKey] + index * WORD,
          );
          const recordWord = read.u32(
            guild + layout[s.guildKey] + index * WORD,
          );
          keyMatches ||= index === 0;
          keyMatches &&= contextWord !== null && contextWord === recordWord;
          keyNonzero ||= contextWord !== 0;
        }
        if (!keyMatches || !keyNonzero) guildInvalidMask |= 1 << 3;
        if (read.u32(guild + layout[s.guildIndex]) !== guildIndex) {
          guildInvalidMask |= 1 << 4;
        }
      }
    }

    const roster = arrayDescriptor(
      read,
      guildContext + layout[s.guildContextRoster],
      WORD,
      100,
      256,
    );
    guildRosterCapacity = roster.capacity;
    if (
      !roster.valid
      || layout[s.guildPlayerStride] < 0x40
      || layout[s.guildPlayerStride] > 0x400
      || layout[s.guildPlayerStride] % WORD !== 0
    ) {
      guildInvalidMask |= 1 << 6;
    } else {
      for (let index = 0; index < roster.size; index += 1) {
        const member = read.u32(roster.buffer + index * WORD);
        if (
          member !== 0
          && (
            member === null
            || member % WORD !== 0
            || !read.contains(member, layout[s.guildPlayerStride])
          )
        ) {
          guildInvalidMask |= 1 << 7;
          break;
        }
        guildRosterCount += Number(member !== 0);
      }
    }
  }
  const guildRecordsValid = guildInvalidMask === 0;
  return {
    friendListAvailable,
    friendHeaderValid,
    playerStatus: playerStatus ?? 0xffff_ffff,
    friendCapacity: friends.capacity,
    friendSlotCount: friends.size,
    friendEntryCount,
    friendInvalidSlot,
    friendInvalidMask,
    friendCountMismatchMask,
    friendRecordsValid,
    guildContextAvailable: true,
    guildIndex: guildIndex ?? 0xffff_ffff,
    guildRecordPresent,
    guildRosterCapacity,
    guildRosterCount,
    guildInvalidMask,
    guildRecordsValid,
    socialRecordsValid: friendRecordsValid && guildRecordsValid,
  };
}

function completionProbeAt(read, layout, game) {
  const c = COMPLETION_LAYOUT;
  const world = read.pointer(
    game + layout[c.gameWorldContext],
    layout[c.fields[c.fields.length - 1]] + 16,
  );
  if (world === null) return emptyCompletionProbe();
  const capacities = [];
  const sizes = [];
  const invalidMasks = [];
  for (const field of c.fields) {
    const address = world + layout[field];
    const buffer = read.u32(address);
    const capacity = read.u32(address + WORD);
    const size = read.u32(address + 2 * WORD);
    const invalidMask = (
      Number(buffer === null || capacity === null || size === null)
      | (Number(size !== null && capacity !== null && size > capacity) << 1)
      | (Number(size !== null && size > 32) << 2)
      | (Number(capacity !== null && capacity > 1_024) << 3)
      | (Number(
        size !== null
        && size > 0
        && (
          buffer === null
          || buffer % WORD !== 0
          || !read.contains(buffer, size * WORD)
        ),
      ) << 4)
    );
    capacities.push(capacity ?? 0);
    sizes.push(size ?? 0);
    invalidMasks.push(invalidMask);
  }
  return {
    worldAvailable: true,
    capacities: Object.freeze(capacities),
    sizes: Object.freeze(sizes),
    invalidMasks: Object.freeze(invalidMasks),
    completionRecordsValid: invalidMasks.every((mask) => mask === 0),
  };
}

/**
 * @param {ArrayBuffer} buffer
 * @param {number[]} layoutWords
 * @param {number} [radiusBytes]
 */
export function probeLayout(buffer, layoutWords, radiusBytes = 2048) {
  if (
    !(buffer instanceof ArrayBuffer)
    || !Array.isArray(layoutWords)
    || layoutWords.length !== 232
    || layoutWords.some((word) => !Number.isInteger(word) || word < 0)
    || !Number.isInteger(radiusBytes)
    || radiusBytes < 0
    || radiusBytes > MAX_RADIUS
    || radiusBytes % WORD !== 0
  ) {
    throw new Error('layout probe input is outside its certified bounds');
  }
  const read = reader(buffer);
  const contexts = [];
  for (let delta = -radiusBytes; delta <= radiusBytes; delta += WORD) {
    const context = contextAt(read, layoutWords, delta);
    if (context !== null) {
      contexts.push({ delta, ...context });
      if (contexts.length >= MAX_CANDIDATES) break;
    }
  }
  const agents = new Set();
  for (const context of contexts) {
    for (let delta = -radiusBytes; delta <= radiusBytes; delta += WORD) {
      if (agentArrayAt(read, layoutWords, delta, context.playerNumber)) {
        agents.add(delta);
        if (agents.size >= MAX_CANDIDATES) break;
      }
    }
  }
  const contextDeltas = contexts.map(({ delta }) => delta);
  const agentDeltas = [...agents].sort((left, right) => left - right);
  const commonDeltas = contextDeltas.filter((delta) => agents.has(delta));
  const commonContext = contexts.find(({ delta }) => agents.has(delta));
  return Object.freeze({
    radiusBytes,
    contextDeltas: Object.freeze(contextDeltas),
    agentDeltas: Object.freeze(agentDeltas),
    commonDeltas: Object.freeze(commonDeltas),
    quest: Object.freeze(
      commonContext
        ? questProbeAt(read, layoutWords, commonContext.game)
        : emptyQuestProbe(),
    ),
    inventory: Object.freeze(
      commonContext
        ? inventoryProbeAt(read, layoutWords, commonContext.game)
        : emptyInventoryProbe(),
    ),
    social: Object.freeze(
      commonContext
        ? socialProbeAt(read, layoutWords, commonContext.game)
        : emptySocialProbe(),
    ),
    completion: Object.freeze(
      commonContext
        ? completionProbeAt(read, layoutWords, commonContext.game)
        : emptyCompletionProbe(),
    ),
  });
}
