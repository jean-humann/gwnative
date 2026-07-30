// Page-to-host half of the versioned game-state API.
//
// The companion already validates the build-specific pointer walk. This module
// narrows its result once more to the public schema and publishes at most four
// times a second, irrespective of display refresh rate.

const PUBLIC_FIELDS = Object.freeze([
  'status',
  'reason',
  'tickCount',
  'mapId',
  'instanceType',
  'instanceName',
  'playerId',
  'playerX',
  'playerY',
  'targetValid',
  'targetId',
  'targetKind',
  'targetX',
  'targetY',
  'distance',
  'rangeName',
  'party',
  'skillbar',
  'effects',
  'agents',
  'quests',
  'inventory',
  'social',
  'completion',
  'camera',
  'trade',
  'ui',
  'merchant',
]);

export function publicState(state) {
  const result = {};
  for (const field of PUBLIC_FIELDS) {
    if (state?.[field] !== undefined) result[field] = state[field];
  }
  // The fixed companion block always contains numeric target slots. Zeroes
  // are padding when the target flag is clear, not a partial public target.
  // Rust requires absence in that state so a consumer cannot confuse agent 0
  // at (0, 0) with a real target.
  if (result.targetValid === false) {
    delete result.targetId;
    delete result.targetX;
    delete result.targetY;
    delete result.distance;
  }
  return Object.freeze(result);
}

export function installGameApi({ log, interval = 250 }) {
  let lastPublished = 0;
  let pending = null;
  let timer = null;
  let inFlight = false;
  let lastFailure = '';

  const send = async () => {
    timer = null;
    if (inFlight || !pending) return;
    const state = pending;
    pending = null;
    inFlight = true;
    try {
      const response = await fetch('__game/v1/state', {
        method: 'PUT',
        headers: {
          'X-Gwnative-Token': window.__gwnativeToken ?? '',
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(state),
      });
      if (!response.ok) throw new Error(`state publish failed (${response.status})`);
      lastPublished = performance.now();
      lastFailure = '';
    } catch (error) {
      const message = error?.message ?? String(error);
      if (message !== lastFailure) log(`[warn] game api: ${message}`);
      lastFailure = message;
    } finally {
      inFlight = false;
      if (pending) schedule();
    }
  };

  const schedule = () => {
    if (timer !== null || inFlight) return;
    const delay = Math.max(0, interval - (performance.now() - lastPublished));
    timer = setTimeout(() => void send(), delay);
  };

  return Object.freeze({
    version: 1,
    publish(state) {
      pending = publicState(state);
      window.dispatchEvent(new CustomEvent('gwnative:state', { detail: pending }));
      schedule();
    },
    endpoint: '__game/v1',
  });
}
