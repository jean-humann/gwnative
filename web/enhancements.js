// Installing optional enhancements as a passive, read-only observer.
//
// The host selected a signed certificate for the exact glue/module pair and
// injected its layout under the selected runtime. The companion shares memory
// but imports no game function and never enters the client's call graph.
// Asyncify frames are observed only while its generated state machine reports
// Normal (0); Unwinding and Rewinding are skipped.

import { createCursorConsumer } from './enhancement-cursor.js';
import { createTargetReadout } from './enhancement-readout.js';
import {
  readCompanionSnapshot,
  COMPANION_CURSOR_ABI,
  COMPANION_CURSOR_BYTES,
  COMPANION_SNAPSHOT_ABI,
  COMPANION_SNAPSHOT_BYTES,
} from './companion-snapshot.js';
import { probeLayout } from './layout-probe.js';
import * as diagnostics from './diagnostics.js';
import {
  asyncifyStateReader,
  createPassiveObserver,
} from './passive-observer.js';

/** Must match `FEATURE_*` in `src/companion-kernel/lib.rs`. */
const FEATURE_NATIVE_CURSOR = 1 << 0;
const FEATURE_TARGET_READOUT = 1 << 1;
const ENHANCEMENT_TRANSFORM_ABI = 11;
const ENHANCEMENT_LAYOUT_WORDS = 173;

/** How many render-cost samples to keep for `window.gwCompanionRuntime`. */
const SAMPLE_WINDOW = 240;
/**
 * Private downward-growing stack for the companion. Its linker-provided stack
 * address is inside the imported game memory and must never be used.
 */
const COMPANION_STACK_BYTES = 64 * 1024;

/**
 * The signed manifest the native host selected, or `null` if it is not one this
 * page can act on.
 *
 * Everything is checked rather than read. This page and the host are versioned
 * together, so a disagreement here means something is wrong with the build
 * rather than that a field needs defaulting — and the consequence of guessing
 * is a companion pointed at the wrong offsets in a live game's memory.
 *
 * @param {unknown} candidate
 */
function decodeManifest(candidate) {
  try {
    const value = candidate;
    if (
      value?.transformAbi !== ENHANCEMENT_TRANSFORM_ABI
      || value?.snapshotAbi !== COMPANION_SNAPSHOT_ABI
      || value?.snapshotBytes !== COMPANION_SNAPSHOT_BYTES
      || value?.cursorSnapshotAbi !== COMPANION_CURSOR_ABI
      || value?.cursorSnapshotBytes !== COMPANION_CURSOR_BYTES
      || typeof value?.familyId !== 'string'
      || !/^[0-9a-f]{64}$/.test(value.familyId)
      || !Array.isArray(value?.layoutWords)
      || value.layoutWords.length !== ENHANCEMENT_LAYOUT_WORDS
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
    return Object.freeze({
      ...value,
      layoutWords: Object.freeze([...value.layoutWords]),
    });
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
 * @param {() => boolean} observeGame
 */
function observeSnapshots(runtime, cursor, readout, observeState, observeGame) {
  let frame = 0;
  let cadenceAt = performance.now();
  let cadenceTick = 0;
  const observe = () => {
    if (!observeGame()) {
      runtime.observerSkips += 1;
      frame = requestAnimationFrame(observe);
      return;
    }
    runtime.observerRuns += 1;
    if (runtime.observerRuns === 1) {
      console.log('[enhancement] passive observer active');
    }
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
 * @param {unknown} manifestValue
 * @param {{ nativeCursor: boolean, targetReadout: boolean,
 *           runtime: 'jspi' | 'asyncify', stateApi?: boolean }} selection
 */
export async function installEnhancements(instance, manifestValue, selection) {
  const observeState = selection.targetReadout || selection.stateApi === true;
  const featureFlags =
    (selection.nativeCursor ? FEATURE_NATIVE_CURSOR : 0)
    | (observeState ? FEATURE_TARGET_READOUT : 0);
  if (featureFlags === 0) return null;

  const manifest = decodeManifest(manifestValue);
  const exports = instance?.exports;
  // Every one of these is something the transform or Emscripten is supposed to
  // have left behind. Checked together, and before anything is allocated, so
  // that a client this page cannot drive is a state rather than a half-install.
  const missing = !manifest
    ? 'the module carries no manifest this page can act on'
    : [
      ['memory', exports?.memory instanceof WebAssembly.Memory],
      ['malloc', typeof exports?.malloc === 'function'],
      ['free', typeof exports?.free === 'function'],
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

  const free = /** @type {(pointer: number) => void} */ (exports.free);
  const asyncifyState = asyncifyStateReader(exports, selection.runtime);
  const runtimeIdle = () => asyncifyState === null || asyncifyState() === 0;
  if (!runtimeIdle()) throw new Error('the client is currently unwinding or rewinding');

  let snapshotPointer = 0;
  let configPointer = 0;
  let cursorPointer = 0;
  let statePointer = 0;
  let stackAllocationPointer = 0;
  let stopObserver = () => {};
  let disposeCursor = () => {};
  let disposeReadout = () => {};
  const release = () => {
    stopObserver();
    disposeCursor();
    disposeReadout();
    // Page teardown is not a reason to enter an Asyncify module during
    // unwind/rewind. Leaking these page-lifetime allocations is harmless
    // because the instance is being discarded with the page.
    if (!runtimeIdle()) return;
    for (const pointer of [
      stackAllocationPointer,
      statePointer,
      cursorPointer,
      configPointer,
      snapshotPointer,
    ]) {
      if (pointer) free(pointer);
    }
  };
  try {
    const response = await fetch('companion-kernel.wasm');
    if (!response.ok) throw new Error('the companion module is unavailable');
    if (!runtimeIdle()) {
      throw new Error('the client began unwinding or rewinding during installation');
    }
    const kernel = await WebAssembly.instantiate(await response.arrayBuffer(), {
      // The whole trick: `env.memory` is an import, so the companion is
      // instantiated over the game's heap rather than one of its own.
      env: { memory: exports.memory },
    });
    const kernelExports = kernel.instance.exports;
    const kernelInit = kernelExports.companion_init;
    const kernelObserve = kernelExports.companion_observe;
    const kernelRuntimeSize = kernelExports.companion_runtime_size;
    const kernelStack = kernelExports.__stack_pointer;
    if (
      typeof kernelInit !== 'function'
      || kernelInit.length !== 9
      || typeof kernelObserve !== 'function'
      || kernelObserve.length !== 1
      || typeof kernelRuntimeSize !== 'function'
      || kernelRuntimeSize.length !== 0
    ) {
      throw new Error('the companion exports do not match their ABI');
    }
    const runtimeBytes = Number(kernelRuntimeSize());
    if (
      !Number.isSafeInteger(runtimeBytes)
      || runtimeBytes <= 0
      || runtimeBytes > 0xffff
      || runtimeBytes % Uint32Array.BYTES_PER_ELEMENT !== 0
    ) {
      throw new Error('the companion reported an invalid runtime size');
    }

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
    statePointer = Number(exports.malloc(runtimeBytes));
    // Fifteen spare bytes let us align the stack base without losing the
    // original allocation pointer needed by `free`.
    stackAllocationPointer = Number(exports.malloc(COMPANION_STACK_BYTES + 15));
    if (
      !configPointer
      || !statePointer
      || !stackAllocationPointer
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

    if (!runtimeIdle()) {
      throw new Error('the client began unwinding or rewinding during allocation');
    }
    const stackBase = Math.ceil(stackAllocationPointer / 16) * 16;
    const stackTop = stackBase + COMPANION_STACK_BYTES;
    if (
      !(kernelStack instanceof WebAssembly.Global)
      || !Number.isSafeInteger(stackTop)
      || stackTop > exports.memory.buffer.byteLength
    ) {
      throw new Error('the companion has no relocatable private stack');
    }
    try {
      kernelStack.value = stackTop;
    } catch {
      throw new Error('the companion stack pointer is not mutable');
    }
    // `companion_init` re-checks every region against the memory size it can
    // see and answers 1 only if it accepted all of them, so this is the
    // companion's own veto rather than a formality.
    const initStatus = kernelInit(
      statePointer,
      runtimeBytes,
      snapshotPointer,
      observeState ? COMPANION_SNAPSHOT_BYTES : 0,
      configPointer,
      manifest.configBytes,
      cursorPointer,
      selection.nativeCursor ? COMPANION_CURSOR_BYTES : 0,
      featureFlags,
    );
    if (initStatus !== 1) {
      throw new Error(`the companion module refused its ABI (status ${initStatus})`);
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

    const runtime = {
      status: 'installed',
      familyId: manifest.familyId,
      memory: exports.memory,
      snapshotPointer,
      configPointer,
      statePointer,
      stackPointer: stackBase,
      hertz: 0,
      lastRenderUs: 0,
      renderSamples: [],
      snapshotReads: 0,
      rejectedSnapshots: 0,
      observerRuns: 0,
      observerSkips: 0,
      probeLayout() {
        if (window.__gwnativeE2E !== true) {
          throw new Error('layout probing is available only during E2E certification');
        }
        return probeLayout(exports.memory.buffer, manifest.layoutWords);
      },
      // Presentation state only: no pixels and no pointer leave this module.
      get cursor() {
        return cursor?.state ?? null;
      },
      // The rendered line, so a live session can be read without a screenshot.
      get readout() {
        return readout?.state ?? null;
      },
      /**
       * Turn passive observation off and on without changing the game module.
       *
       * @param {boolean} enabled
       */
      setObserverEnabled(enabled) {
        runtime.observerEnabled = enabled === true;
      },
      observerEnabled: true,
    };
    window.gwCompanionRuntime = runtime;
    let kernelFailed = false;
    const passiveObserve = createPassiveObserver(asyncifyState, () => {
      try {
        kernelObserve(statePointer);
      } catch (error) {
        kernelFailed = true;
        console.warn('[enhancement] passive observer stopped after a trap');
        throw error;
      }
    });
    const observeGame = () =>
      runtime.observerEnabled && !kernelFailed && passiveObserve();
    stopObserver = observeSnapshots(
      runtime,
      cursor,
      readout,
      observeState,
      observeGame,
    );

    const teardown = () => {
      runtime.observerEnabled = false;
      release();
      window.gwCompanionRuntime = null;
    };
    window.addEventListener('pagehide', teardown, { once: true });
    // `log`, not `info`: the harness forwards log, warn and error to the host
    // and nothing else, so an `info` here is a line that reaches the WebKit
    // inspector and no log file, report or overlay anyone will actually open.
    console.log(`[enhancement] installed for artifact family ${manifest.familyId.slice(0, 12)}`);
    return runtime;
  } catch (error) {
    release();
    window.gwCompanionState = Object.freeze({
      status: 'error',
      reason: error instanceof Error ? error.message : String(error),
    });
    throw error;
  }
}
