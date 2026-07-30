// Bounded live-layout certification for E2E builds.
//
// This never exposes memory contents. It tests nearby aligned addresses
// against the same map, character, agent-array, and player invariants as the
// companion and returns only the deltas that satisfy all of them.

const WORD = 4;
const MAX_RADIUS = 4096;
const MAX_CANDIDATES = 16;

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
  return playerNumber;
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

/**
 * @param {ArrayBuffer} buffer
 * @param {number[]} layoutWords
 * @param {number} [radiusBytes]
 */
export function probeLayout(buffer, layoutWords, radiusBytes = 2048) {
  if (
    !(buffer instanceof ArrayBuffer)
    || !Array.isArray(layoutWords)
    || layoutWords.length !== 157
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
    const playerNumber = contextAt(read, layoutWords, delta);
    if (playerNumber !== null) {
      contexts.push({ delta, playerNumber });
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
  return Object.freeze({
    radiusBytes,
    contextDeltas: Object.freeze(contextDeltas),
    agentDeltas: Object.freeze(agentDeltas),
    commonDeltas: Object.freeze(commonDeltas),
  });
}
