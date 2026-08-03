/**
 * Resolve the generated Asyncify state reader for the selected runtime.
 *
 * A missing export means "not applicable" only for JSPI. Treating it that way
 * for Asyncify would turn a changed generated ABI into permission to observe
 * during unwind/rewind.
 *
 * @param {WebAssembly.Exports | Record<string, any>} exports
 * @param {'jspi' | 'asyncify'} runtime
 * @returns {(() => number) | null}
 */
export function asyncifyStateReader(exports, runtime) {
  if (runtime !== 'jspi' && runtime !== 'asyncify') {
    throw new Error(`unknown client runtime: ${runtime}`);
  }
  const state = typeof exports?.asyncify_get_state === 'function'
    ? () => Number(exports.asyncify_get_state())
    : null;
  if (runtime === 'asyncify' && state === null) {
    throw new Error('the Asyncify client does not export asyncify_get_state');
  }
  return state;
}

/**
 * Gate a passive companion read against Asyncify's generated state machine.
 *
 * JSPI has no such export and is always eligible. Asyncify must be Normal (0)
 * both before and after the companion runs; Unwinding (1) and Rewinding (2)
 * never enter it.
 *
 * @param {(() => number) | null} state
 * @param {() => void} observe
 * @returns {() => boolean}
 */
export function createPassiveObserver(state, observe) {
  return () => {
    try {
      if (state !== null && state() !== 0) return false;
      observe();
      return state === null || state() === 0;
    } catch {
      return false;
    }
  };
}

/**
 * Limit companion work without tying it to a particular display refresh rate.
 *
 * An accumulator produces an average 60 Hz cadence on 90/100/120 Hz displays,
 * while a fixed "every other frame" rule would fall to 45/50 Hz. Long pauses
 * add at most one interval, so reactivation cannot burst stale observer work.
 *
 * @param {number} maximumHertz
 */
export function createObserverCadence(maximumHertz = 60) {
  if (!Number.isFinite(maximumHertz) || maximumHertz <= 0 || maximumHertz > 1_000) {
    throw new Error('observer cadence is outside its bound');
  }
  const interval = 1_000 / maximumHertz;
  const tolerance = Math.min(0.5, interval / 20);
  let previous = null;
  let budget = interval;
  return (timestamp) => {
    if (!Number.isFinite(timestamp)) return false;
    if (previous === null) {
      previous = timestamp;
    } else {
      const elapsed = Math.max(0, Math.min(interval, timestamp - previous));
      previous = timestamp;
      budget += elapsed;
    }
    if (budget + tolerance < interval) return false;
    budget = Math.max(0, budget - interval);
    return true;
  };
}
