// Bounded live-layout certification for E2E builds.
//
// This never exposes memory contents. It tests nearby aligned addresses
// against the same map, character, agent-array, and player invariants as the
// companion and returns only the deltas that satisfy all of them.

const WORD = 4;
const MAX_RADIUS = 4096;
const MAX_CANDIDATES = 16;
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
  const f32 = (address) =>
    contains(address, 4) ? view.getFloat32(address, true) : null;
  const pointer = (address, bytes) => {
    const value = u32(address);
    return value !== null && value % WORD === 0 && contains(value, bytes)
      ? value
      : null;
  };
  return { contains, u16, u32, f32, pointer };
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

/**
 * @param {ArrayBuffer} buffer
 * @param {number[]} layoutWords
 * @param {number} [radiusBytes]
 */
export function probeLayout(buffer, layoutWords, radiusBytes = 2048) {
  if (
    !(buffer instanceof ArrayBuffer)
    || !Array.isArray(layoutWords)
    || layoutWords.length !== 228
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
  });
}
