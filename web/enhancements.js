// Installing optional enhancements: the page's half of the enhancement chain.
//
// By the time anything here runs, the host has already decided the hard part.
// It read the player's settings, saw that a tool was wanted, derived a client
// with a hook in its main loop, and told this page so through
// `window.__gwnativeEnhancements`. What is left is to hand that hook something
// to call.
//
// The order matters and is not obvious:
//
//   1. Read the manifest out of the module itself. The host wrote it there as
//      a custom section rather than sending it alongside, so a manifest and
//      the module it describes cannot be separated — a companion reading one
//      build's field offsets out of another build's memory is exactly the bug
//      that would produce.
//   2. Allocate the two published regions from the *client's* allocator, so
//      they live in the memory the companion will be instantiated over.
//   3. Instantiate the companion over that same memory, and let it check the
//      ABI before it is trusted with anything.
//   4. Put its tick in the table slot, and only then set the global that makes
//      the dispatcher call it.
//
// Nothing between steps 1 and 4 can leave the game worse off: the hook global
// stays at zero, which is the untouched client to the byte, and every failure
// path below puts it back there and frees what it took.

import { createCursorConsumer } from './enhancement-cursor.js';
import { createTargetReadout } from './enhancement-readout.js';
import {
  readCompanionSnapshot,
  COMPANION_CURSOR_ABI,
  COMPANION_CURSOR_BYTES,
  COMPANION_SNAPSHOT_ABI,
  COMPANION_SNAPSHOT_BYTES,
} from './companion-snapshot.js';
import * as diagnostics from './diagnostics.js';

/** Must match `FEATURE_*` in `src/companion-kernel/lib.rs`. */
const FEATURE_NATIVE_CURSOR = 1 << 0;
const FEATURE_TARGET_READOUT = 1 << 1;

/** How many render-cost samples to keep for `window.gwCompanionRuntime`. */
const SAMPLE_WINDOW = 240;

/**
 * The manifest the host wrote into the module, or `null` if it is not one this
 * page can act on.
 *
 * Everything is checked rather than read. This page and the host are versioned
 * together, so a disagreement here means something is wrong with the build
 * rather than that a field needs defaulting — and the consequence of guessing
 * is a companion pointed at the wrong offsets in a live game's memory.
 *
 * @param {WebAssembly.Module} module
 */
function decodeManifest(module) {
  const sections = WebAssembly.Module.customSections(module, 'enhancement_manifest');
  if (sections.length !== 1) return null;
  try {
    const value = JSON.parse(new TextDecoder().decode(sections[0]));
    if (
      value?.snapshotAbi !== COMPANION_SNAPSHOT_ABI
      || value?.snapshotBytes !== COMPANION_SNAPSHOT_BYTES
      || value?.cursorSnapshotAbi !== COMPANION_CURSOR_ABI
      || value?.cursorSnapshotBytes !== COMPANION_CURSOR_BYTES
      || !Number.isSafeInteger(value?.buildId)
      || value.buildId <= 0
      || !Number.isSafeInteger(value?.programId)
      || value.programId <= 0
      || !Number.isSafeInteger(value?.tableSlot)
      || value.tableSlot < 0
      || !Array.isArray(value?.layoutWords)
      || value.layoutWords.length === 0
      || value.layoutWords.some(
        (/** @type {unknown} */ word) =>
          !Number.isInteger(word)
          || Number(word) < 0
          || Number(word) > 0xffff_ffff,
      )
      // The companion is handed a byte count and reads that many words out of
      // it, so the two have to be the same statement.
      || value?.configBytes !== value.layoutWords.length * Uint32Array.BYTES_PER_ELEMENT
    ) {
      return null;
    }
    return Object.freeze(value);
  } catch {
    return null;
  }
}

/**
 * Read the snapshot once per frame and hand it to whichever surfaces are on.
 *
 * The cursor polls outside the measured window because what
 * `gw.enhancement.read.us` is for is the cost of the snapshot read itself —
 * the thing that happens on every frame whether or not anything changed.
 *
 * @param {any} runtime
 * @param {{ poll: () => void } | null} cursor
 * @param {{ update: (state: any) => void } | null} readout
 * @param {boolean} observeState
 */
function observeSnapshots(runtime, cursor, readout, observeState) {
  let frame = 0;
  let cadenceAt = performance.now();
  let cadenceTick = 0;
  const observe = () => {
    if (observeState) {
      const started = performance.now();
      const state = readCompanionSnapshot(
        runtime.memory.buffer,
        runtime.snapshotPointer,
      );
      runtime.snapshotReads += 1;
      if (state.reason === 'writing' || state.reason === 'snapshot') {
        runtime.rejectedSnapshots += 1;
      }
      window.gwCompanionState = state;
      window.gwGameApi?.publish(state);
      const now = performance.now();
      // The companion's own tick rate, which is the game's main loop rate and
      // not this page's frame rate. Worth knowing separately: a client that
      // has stopped ticking still animates.
      if (state.status === 'ready' && now - cadenceAt >= 1_000) {
        runtime.hertz = ((state.tickCount - cadenceTick) * 1_000) / (now - cadenceAt);
        diagnostics.gauge('gw.enhancement.tick.hz', runtime.hertz);
        cadenceAt = now;
        cadenceTick = state.tickCount;
      }
      runtime.lastRenderUs = (performance.now() - started) * 1_000;
      runtime.renderSamples.push(runtime.lastRenderUs);
      if (runtime.renderSamples.length > SAMPLE_WINDOW) runtime.renderSamples.shift();
      diagnostics.peak('gw.enhancement.read.us.max', runtime.lastRenderUs);
      readout?.update(state);
    }
    cursor?.poll();
    frame = requestAnimationFrame(observe);
  };
  frame = requestAnimationFrame(observe);
  return () => cancelAnimationFrame(frame);
}

/**
 * Install the tools the player asked for, over an instantiated client.
 *
 * Returns the runtime handle — also on `window.gwCompanionRuntime`, which is
 * how a live session can be inspected without instrumenting the page — or
 * `null` when there is nothing to install, which is the ordinary case.
 * Throws only when a tool was wanted and could not be given; the caller logs
 * that and the game carries on without it.
 *
 * @param {WebAssembly.Instance} instance
 * @param {WebAssembly.Module} module
 * @param {{ nativeCursor: boolean, targetReadout: boolean, stateApi?: boolean }} selection
 */
export async function installEnhancements(instance, module, selection) {
  const observeState = selection.targetReadout || selection.stateApi === true;
  const featureFlags =
    (selection.nativeCursor ? FEATURE_NATIVE_CURSOR : 0)
    | (observeState ? FEATURE_TARGET_READOUT : 0);
  if (featureFlags === 0) return null;

  const manifest = decodeManifest(module);
  const exports = instance?.exports;
  // Every one of these is something the transform or Emscripten is supposed to
  // have left behind. Checked together, and before anything is allocated, so
  // that a client this page cannot drive is a state rather than a half-install.
  const missing = !manifest
    ? 'the module carries no manifest this page can act on'
    : [
      ['memory', exports?.memory instanceof WebAssembly.Memory],
      [
        '__indirect_function_table',
        exports?.__indirect_function_table instanceof WebAssembly.Table,
      ],
      ['malloc', typeof exports?.malloc === 'function'],
      ['free', typeof exports?.free === 'function'],
      ['enhancement_tick_original', typeof exports?.enhancement_tick_original === 'function'],
      ['enhancement_hook_slot', exports?.enhancement_hook_slot instanceof WebAssembly.Global],
    ].filter(([, present]) => !present).map(([name]) => name).join(', ');
  if (missing !== '') {
    // Said out loud rather than returned quietly. The host has already told
    // this page the client is `ready`, so reaching here means the two disagree
    // about the module in front of them — and the player's symptom is a tool
    // they turned on doing nothing, which is not something to leave them to
    // work out. `console.warn` is wrapped by the harness, so it reaches the
    // diagnostics overlay and the log file rather than a terminal nobody has.
    console.warn(`[enhancement] this client cannot be driven: ${missing}`);
    window.gwCompanionState = Object.freeze({ status: 'unsupported', reason: missing });
    return null;
  }

  const table = exports.__indirect_function_table;
  const hookSlot = /** @type {WebAssembly.Global} */ (exports.enhancement_hook_slot);
  const free = /** @type {(pointer: number) => void} */ (exports.free);
  // The host checked the same thing before it certified the module. Checked
  // again because between then and now the client has run its own start
  // function, and overwriting an occupied slot would break whatever put it
  // there.
  if (table.get(manifest.tableSlot) !== null) {
    throw new Error(`table slot ${manifest.tableSlot} is occupied`);
  }

  let snapshotPointer = 0;
  let configPointer = 0;
  let cursorPointer = 0;
  let stopObserver = () => {};
  let disposeCursor = () => {};
  let disposeReadout = () => {};
  try {
    // The client's own allocator, so these are inside the memory the companion
    // is about to be instantiated over. Nothing the page allocates for itself
    // would be visible from there at all.
    if (observeState) {
      snapshotPointer = Number(exports.malloc(COMPANION_SNAPSHOT_BYTES));
    }
    configPointer = Number(exports.malloc(manifest.configBytes));
    if (selection.nativeCursor) {
      cursorPointer = Number(exports.malloc(COMPANION_CURSOR_BYTES));
    }
    if (
      !configPointer
      || (observeState && !snapshotPointer)
      || (selection.nativeCursor && !cursorPointer)
    ) {
      throw new Error('the client would not allocate the companion regions');
    }
    new Uint32Array(
      exports.memory.buffer,
      configPointer,
      manifest.layoutWords.length,
    ).set(manifest.layoutWords);

    const response = await fetch('companion-kernel.wasm');
    if (!response.ok) throw new Error('the companion module is unavailable');
    const kernel = await WebAssembly.instantiate(await response.arrayBuffer(), {
      // The whole trick: `env.memory` is an import, so the companion is
      // instantiated over the game's heap rather than one of its own.
      env: { memory: exports.memory },
      game: { enhancement_tick_original: exports.enhancement_tick_original },
    });
    const kernelInit = kernel.instance.exports.companion_init;
    // `companion_init` re-checks every region against the memory size it can
    // see and answers 1 only if it accepted all of them, so this is the
    // companion's own veto rather than a formality.
    if (
      typeof kernelInit !== 'function'
      || kernelInit.length !== 7
      || typeof kernel.instance.exports.companion_tick !== 'function'
      || kernelInit(
        snapshotPointer,
        observeState ? COMPANION_SNAPSHOT_BYTES : 0,
        configPointer,
        manifest.configBytes,
        cursorPointer,
        selection.nativeCursor ? COMPANION_CURSOR_BYTES : 0,
        featureFlags,
      ) !== 1
    ) {
      throw new Error('the companion module refused its ABI');
    }

    let cursor = null;
    if (selection.nativeCursor) {
      const element = document.getElementById('canvas');
      if (!element) throw new Error('there is no canvas to take the cursor of');
      cursor = createCursorConsumer({
        element,
        memory: exports.memory,
        cursorPointer,
        // The empty string hands the canvas back to the stylesheet.
        fallback: '',
      });
      disposeCursor = cursor.dispose;
    }
    const readout = selection.targetReadout ? createTargetReadout(document.body) : null;
    if (readout) disposeReadout = readout.dispose;

    table.set(manifest.tableSlot, kernel.instance.exports.companion_tick);
    const runtime = {
      status: 'installed',
      buildId: manifest.buildId,
      programId: manifest.programId,
      memory: exports.memory,
      snapshotPointer,
      configPointer,
      tableSlot: manifest.tableSlot,
      hertz: 0,
      lastRenderUs: 0,
      renderSamples: [],
      snapshotReads: 0,
      rejectedSnapshots: 0,
      // Presentation state only: no pixels and no pointer leave this module.
      get cursor() {
        return cursor?.state ?? null;
      },
      // The rendered line, so a live session can be read without a screenshot.
      get readout() {
        return readout?.state ?? null;
      },
      /**
       * Turn the hook off and on without tearing anything down, which is how
       * the cost of the whole chain is measured against the same session
       * rather than against a second launch.
       *
       * @param {boolean} enabled
       */
      setHookEnabled(enabled) {
        hookSlot.value = enabled ? manifest.tableSlot + 1 : 0;
      },
    };
    window.gwCompanionRuntime = runtime;
    stopObserver = observeSnapshots(runtime, cursor, readout, observeState);
    // Last: from here the client's main loop is calling into the companion.
    hookSlot.value = manifest.tableSlot + 1;

    const teardown = () => {
      // Same order reversed. The global goes first, so nothing is calling the
      // tick by the time its slot is cleared and its regions are freed.
      hookSlot.value = 0;
      stopObserver();
      disposeCursor();
      disposeReadout();
      if (table.get(manifest.tableSlot) === kernel.instance.exports.companion_tick) {
        table.set(manifest.tableSlot, null);
      }
      if (cursorPointer) free(cursorPointer);
      free(configPointer);
      if (snapshotPointer) free(snapshotPointer);
      window.gwCompanionRuntime = null;
    };
    window.addEventListener('pagehide', teardown, { once: true });
    // `log`, not `info`: the harness forwards log, warn and error to the host
    // and nothing else, so an `info` here is a line that reaches the WebKit
    // inspector and no log file, report or overlay anyone will actually open.
    console.log(`[enhancement] installed for client build ${manifest.buildId}`);
    return runtime;
  } catch (error) {
    hookSlot.value = 0;
    stopObserver();
    disposeCursor();
    disposeReadout();
    if (cursorPointer) free(cursorPointer);
    if (configPointer) free(configPointer);
    if (snapshotPointer) free(snapshotPointer);
    window.gwCompanionState = Object.freeze({
      status: 'error',
      reason: error instanceof Error ? error.message : String(error),
    });
    throw error;
  }
}
