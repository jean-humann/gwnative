/**
 * Gate a passive companion read against Asyncify's generated state machine.
 *
 * JSPI has no such export and is always eligible. Asyncify must be Normal (0)
 * both before and after the companion runs; Unwinding (1) and Rewinding (2)
 * never enter it.
 *
 * @param {WebAssembly.Exports | Record<string, any>} exports
 * @param {() => void} observe
 * @returns {() => boolean}
 */
export function createPassiveObserver(exports, observe) {
  const state = typeof exports?.asyncify_get_state === 'function'
    ? () => Number(exports.asyncify_get_state())
    : null;
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
