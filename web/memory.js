// What the client's memory is doing, from inside the process that holds it.
//
// The host samples its own footprint every second, and that number is honest
// about the host and blind to the game: the WASM heap, the GL resources and the
// JavaScript objects all live in the web content process, which is a different
// process with a different accounting. Anyone reading `footprintMiB` and
// concluding something about the game's memory is reading the wrong number.
//
// WebKit exposes no `performance.memory` and no
// `measureUserAgentSpecificMemory`, so there is exactly one measurable thing in
// here: the linear memory the client grows for itself. That happens to be the
// one that matters — it is most of the process, it only ever goes up, and every
// increase is an explicit call the client makes.

import * as diagnostics from './diagnostics.js';

const MiB = 1048576;

/**
 * Report the heap the client grows for itself.
 *
 * Wraps the growth import rather than sampling on a timer, because the heap
 * only changes when this is called: a sampler would spend the whole session
 * re-reading a number that had not moved, and would still miss the moment it
 * did.
 *
 * @param {{
 *   env: Record<string, unknown>,
 *   heapBytes(): number,
 *   log(...values: unknown[]): void,
 * }} options
 */
export function installMemorySensor({ env, heapBytes, log }) {
  const resize = env?.emscripten_resize_heap;
  if (typeof resize !== 'function') {
    // Not fatal, and not silent: a build with a fixed heap cannot grow, and
    // that is worth knowing before somebody wonders why the gauge never moved.
    log('[warn] no emscripten_resize_heap import — heap growth will not be reported');
    return;
  }

  let last = 0;
  const observe = () => {
    const bytes = heapBytes();
    if (!bytes || bytes === last) return;
    last = bytes;
    diagnostics.gauge('gw.memory.heap.MiB', bytes / MiB);
    diagnostics.peak('gw.memory.heap.MiB.max', bytes / MiB);
  };

  env.emscripten_resize_heap = (requested) => {
    const before = heapBytes();
    const ok = resize(requested);
    if (!ok) {
      // The client asked for memory and the platform said no. From here the
      // client either aborts or fails a load, and neither says why. This is the
      // only place the actual cause is visible.
      diagnostics.count('gw.memory.heap.refused');
      log(`[warn] the heap could not grow to ${Math.round(requested / MiB)} MiB`);
      return ok;
    }
    diagnostics.count('gw.memory.heap.grow');
    observe();
    // Per-growth rather than cumulative, because the interesting shape is
    // whether the steps get bigger: a heap that doubles is normal, one that
    // keeps taking the same small step is something holding a reference.
    diagnostics.peak('gw.memory.heap.step.MiB.max', Math.max(0, heapBytes() - before) / MiB);
    return ok;
  };

  // The heap starts at whatever the module declared, and a session that never
  // grows would otherwise report nothing at all.
  observe();

  return observe;
}
