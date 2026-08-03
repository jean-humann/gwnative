// Page-to-host half of the versioned, read-only game-state API.
//
// A later certified producer can call publish(). This transport does no polling
// or observation itself, and limits host publication to four times per second.

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
]);

export function publicState(state) {
  const result = {};
  for (const field of PUBLIC_FIELDS) {
    if (state?.[field] !== undefined) result[field] = state[field];
  }
  if (result.status !== 'ready') {
    return Object.freeze({
      ...(result.status === undefined ? {} : { status: result.status }),
      ...(result.reason === undefined ? {} : { reason: result.reason }),
    });
  }
  // Fixed snapshots contain numeric target slots even when the target flag is
  // clear. Those zeroes are padding, not a partial public target.
  if (result.targetValid === false) {
    delete result.targetId;
    delete result.targetX;
    delete result.targetY;
    delete result.distance;
  }
  return Object.freeze(result);
}

/** Replace any previously published ready state with a terminal revision. */
export function publishGameStateUnavailable(reason, target = window) {
  const state = Object.freeze({ status: 'unsupported', reason });
  target.gwGameState = state;
  target.gwGameApi?.publish(state);
  return state;
}

export function installGameApi({
  log,
  interval = 250,
  now = () => performance.now(),
  setTimer = (callback, delay) => setTimeout(callback, delay),
}) {
  const cadence = Math.max(250, interval);
  let lastAttempt = null;
  let pending = null;
  let timer = null;
  let inFlight = false;
  let lastFailure = '';

  const schedule = () => {
    if (timer !== null || inFlight) return;
    const delay = lastAttempt === null
      ? 0
      : Math.max(0, cadence - (now() - lastAttempt));
    timer = setTimer(() => void send(), delay);
  };

  const send = async () => {
    timer = null;
    if (inFlight || !pending) return;
    const state = pending;
    pending = null;
    inFlight = true;
    // Failed attempts consume the same cadence budget as successful ones. A
    // disconnected host must not turn a frame-rate producer into a request
    // storm.
    lastAttempt = now();
    try {
      const response = await fetch('__game/v1/state', {
        method: 'PUT',
        headers: {
          'X-Gwnative-Token': window.__gwnativeGamePublisherToken ?? '',
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(state),
      });
      if (!response.ok) throw new Error('state publish failed (' + response.status + ')');
      lastFailure = '';
    } catch (error) {
      const message = error?.message ?? String(error);
      if (message !== lastFailure) log('[warn] game api: ' + message);
      lastFailure = message;
    } finally {
      inFlight = false;
      if (pending) schedule();
    }
  };

  return Object.freeze({
    version: 1,
    endpoint: '__game/v1',
    publish(state) {
      pending = publicState(state);
      schedule();
    },
  });
}
