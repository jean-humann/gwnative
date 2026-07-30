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
