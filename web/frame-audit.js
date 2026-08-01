// Mark-time evidence for rendering stalls and apparent tearing.
//
// `eglSwapBuffers` is a logical boundary in Emscripten's browser EGL shim; it
// does not prove which pixels WebKit composited.  This tracker therefore keeps
// the other events needed to interpret a mark: animation-frame entry/exit,
// draw calls, snapshot reads, and WebGL context loss.  Draw-call and callback
// correlation is opt-in because wrapping every draw is too expensive to ship as
// an always-on diagnostic.  Snapshot-read and context evidence is cheap enough
// to keep in every build.

const round = (value) => Number.isFinite(value) ? Number(value.toFixed(3)) : null;
const MAX_EVENT_LOGS = 20;
const MAX_RECENT_EVENTS = 8;

const contextAttributes = (context) => {
  try {
    const attributes = context?.getContextAttributes?.();
    if (!attributes) return null;
    return Object.fromEntries(
      Object.entries(attributes).filter(([, value]) =>
        typeof value === 'boolean' || typeof value === 'number'),
    );
  } catch {
    return null;
  }
};

/**
 * @param {{
 *   enabled?: boolean,
 *   runtime?: string,
 *   canvas?: HTMLCanvasElement | null,
 *   clock?: () => number,
 *   diagnostics?: { count(name: string, value?: number): void, peak(name: string, value: number): void },
 *   log?: (...values: unknown[]) => void,
 *   page?: () => Record<string, unknown>,
 *   suspensionSite?: () => string[],
 *   scheduleMicrotask?: (callback: () => void) => void,
 * }} options
 */
export function createFrameAudit(options = {}) {
  const enabled = options.enabled === true;
  const runtime = options.runtime ?? 'unknown';
  const canvas = options.canvas ?? null;
  const clock = options.clock ?? (() => performance.now());
  const diagnostics = options.diagnostics ?? { count() {}, peak() {} };
  const log = options.log ?? (() => {});
  const page = options.page ?? (() => ({}));
  const scheduleMicrotask = options.scheduleMicrotask ?? queueMicrotask;
  const suspensionSite = options.suspensionSite ?? (() =>
    (new Error().stack ?? '')
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => /wasm|ImageWait|ImageReadSync/.test(line))
      .slice(0, 12));

  const totals = {
    animationCallbacks: 0,
    submittedLogicalFrames: 0,
    drawCalls: 0,
    clearCalls: 0,
    drawsOutsideAnimationFrame: 0,
    swaps: 0,
    swapsOutsideAnimationFrame: 0,
    callbacksWithMultipleSwaps: 0,
    reads: 0,
    readBytes: 0,
    readFailures: 0,
    readMs: 0,
    readMsMax: 0,
    readsStartedAfterDraw: 0,
    suspendingReads: 0,
    suspendingWaits: 0,
    suspensionsStartedAfterDraw: 0,
    framesInterruptedAfterDraw: 0,
    callbacksStartedDuringSuspension: 0,
    callbacksDoingWorkDuringSuspension: 0,
    externalCallbacksDuringSuspension: 0,
    outsideWorkDuringSuspension: 0,
    suspensionsResumedAfterLaterCallback: 0,
    contextLost: 0,
    contextRestored: 0,
    activations: 0,
    deactivations: 0,
  };
  const frameIntervals = { count: 0, total: 0, min: Infinity, max: 0 };
  const stack = [];
  const externalStack = [];
  const pendingSuspensions = new Set();
  const imageWaits = new Map();
  const imageReadDetails = new WeakMap();
  const imageReadsById = new Map();
  const recentImageReads = [];
  const recentEvents = [];
  let nextFrame = 0;
  let activeReads = 0;
  let activeSuspensions = 0;
  let lastSubmittedTimestamp = null;
  let context = null;
  let contextWasCreated = false;
  let lastSwapAt = null;
  let lastActivatedAt = null;
  let awaitingActivationSwap = false;
  let loggedInterruptedFrames = 0;
  let loggedSuspensionResumes = 0;
  let loggedOverlappingWork = 0;
  let resumedFrame = null;
  const activation = {
    lastFrameAgeMs: null,
    lastToFirstSwapMs: null,
    maxToFirstSwapMs: 0,
  };

  const currentBrowserFrame = () => stack.at(-1) ?? null;
  const currentLogicalFrame = () => currentBrowserFrame() ?? resumedFrame;
  const rememberEvent = (event) => {
    recentEvents.push(event);
    if (recentEvents.length > MAX_RECENT_EVENTS) recentEvents.shift();
    return event;
  };

  const reportInterruptedFrame = (frame) => {
    if (
      frame.interrupted
      || frame.draws === 0
      || frame.swaps > 0
      || frame.pendingSuspensionsAfterDraw === 0
    ) return;
    frame.interrupted = true;
    totals.framesInterruptedAfterDraw++;
    diagnostics.count('gw.frame.interrupted-after-draw');
    const now = clock();
    const event = rememberEvent({
      kind: 'frame-interrupted-after-draw',
      runtime,
      frame: frame.id,
      callbackTimestamp: round(frame.timestamp),
      callbackMs: round(now - frame.started),
      draws: frame.draws,
      clears: frame.clears,
      browserCallbackEnded: frame.browserEnded,
      pendingSuspensions: frame.suspensionsAfterDraw
        .filter((suspension) => !suspension.ended)
        .map((suspension) => ({
          kind: suspension.kind,
          ageMs: round(now - suspension.started),
          ...suspension.detail,
        })),
    });
    if (loggedInterruptedFrames >= MAX_EVENT_LOGS) return;
    loggedInterruptedFrames++;
    log('[frame-audit-event]', JSON.stringify(event));
  };

  const submitFrame = (frame) => {
    if (!frame) return;
    frame.swaps++;
    if (frame.swaps > 1) {
      if (frame.swaps === 2) totals.callbacksWithMultipleSwaps++;
      return;
    }
    totals.submittedLogicalFrames++;
    if (lastSubmittedTimestamp !== null && Number.isFinite(frame.timestamp)) {
      const interval = frame.timestamp - lastSubmittedTimestamp;
      if (interval > 0 && interval < 1_000) {
        frameIntervals.count++;
        frameIntervals.total += interval;
        frameIntervals.min = Math.min(frameIntervals.min, interval);
        frameIntervals.max = Math.max(frameIntervals.max, interval);
      }
    }
    lastSubmittedTimestamp = frame.timestamp;
  };

  const markFrameWork = (frame, work) => {
    if (!frame?.startedDuringSuspension || frame.didWorkDuringSuspension) return;
    frame.didWorkDuringSuspension = true;
    totals.callbacksDoingWorkDuringSuspension++;
    diagnostics.count('gw.frame.callback-work-during-suspension');
    const event = rememberEvent({
      kind: 'callback-work-during-suspension',
      runtime,
      frame: frame.id,
      work,
      pendingFrames: frame.suspensionsAtStart.map((suspension) => ({
        frame: suspension.frame?.id ?? null,
        suspension: suspension.kind,
        ...suspension.detail,
      })),
    });
    if (loggedOverlappingWork >= MAX_EVENT_LOGS) return;
    loggedOverlappingWork++;
    log('[frame-audit-event]', JSON.stringify(event));
  };

  // A socket, timer, or other host callback is not necessarily inside an
  // animation-frame wrapper. If it reaches the renderer while another logical
  // frame is suspended, record the first operation against each pending
  // suspension. The original continuation cannot be mistaken for this: its
  // suspension is ended before Wasm resumes.
  const markOutsideWorkDuringSuspension = (browserFrame, work) => {
    if (browserFrame) return;
    const pending = [...pendingSuspensions]
      .filter((suspension) => !suspension.ended && !suspension.outsideWorkReported);
    if (pending.length === 0) return;
    for (const suspension of pending) suspension.outsideWorkReported = true;
    totals.outsideWorkDuringSuspension += pending.length;
    diagnostics.count('gw.frame.outside-work-during-suspension', pending.length);
    const event = rememberEvent({
      kind: 'outside-work-during-suspension',
      runtime,
      work,
      pendingFrames: pending.map((suspension) => ({
        frame: suspension.frame?.id ?? null,
        suspension: suspension.kind,
        ...suspension.detail,
      })),
      externalCallback: externalStack.at(-1)?.kind ?? null,
    });
    if (loggedOverlappingWork >= MAX_EVENT_LOGS) return;
    loggedOverlappingWork++;
    log('[frame-audit-event]', JSON.stringify(event));
  };

  const beginAnimationFrame = (timestamp) => {
    if (!enabled) return null;
    const activeFrameSuspensions = [...pendingSuspensions]
      .filter((suspension) => suspension.frame && !suspension.ended);
    if (activeFrameSuspensions.length > 0) {
      totals.callbacksStartedDuringSuspension++;
      diagnostics.count('gw.frame.callback-during-suspension');
      for (const suspension of activeFrameSuspensions) {
        suspension.callbacksStartedWhilePending++;
      }
    }
    const frame = {
      id: ++nextFrame,
      started: clock(),
      timestamp,
      draws: 0,
      clears: 0,
      swaps: 0,
      pendingSuspensionsAfterDraw: 0,
      suspensionsAfterDraw: [],
      interrupted: false,
      browserEnded: false,
      startedDuringSuspension: activeFrameSuspensions.length > 0,
      didWorkDuringSuspension: false,
      suspensionsAtStart: activeFrameSuspensions,
    };
    totals.animationCallbacks++;
    stack.push(frame);
    return frame;
  };

  const endAnimationFrame = (frame) => {
    if (!enabled || !frame) return;
    const position = stack.lastIndexOf(frame);
    if (position !== -1) stack.splice(position, 1);
    frame.browserEnded = true;
    reportInterruptedFrame(frame);
  };

  const beginExternalCallback = (kind) => {
    if (!enabled) return null;
    const callback = { kind };
    externalStack.push(callback);
    const pending = [...pendingSuspensions].filter((suspension) => !suspension.ended);
    if (pending.length === 0) return callback;
    totals.externalCallbacksDuringSuspension++;
    diagnostics.count('gw.frame.external-callback-during-suspension');
    const fresh = [];
    for (const suspension of pending) {
      suspension.externalCallbacksWhilePending++;
      if (suspension.externalKinds.has(kind)) continue;
      suspension.externalKinds.add(kind);
      fresh.push(suspension);
    }
    if (fresh.length === 0) return callback;
    const event = rememberEvent({
      kind: 'external-callback-during-suspension',
      runtime,
      callback: kind,
      pendingFrames: fresh.map((suspension) => ({
        frame: suspension.frame?.id ?? null,
        suspension: suspension.kind,
        ...suspension.detail,
      })),
    });
    if (loggedOverlappingWork < MAX_EVENT_LOGS) {
      loggedOverlappingWork++;
      log('[frame-audit-event]', JSON.stringify(event));
    }
    return callback;
  };

  const endExternalCallback = (callback) => {
    if (!enabled || !callback) return;
    const position = externalStack.lastIndexOf(callback);
    if (position !== -1) externalStack.splice(position, 1);
  };

  const draw = (importName = 'draw') => {
    if (!enabled) return;
    totals.drawCalls++;
    const isClear = /^glClear/.test(importName);
    if (isClear) totals.clearCalls++;
    const browserFrame = currentBrowserFrame();
    const frame = browserFrame ?? resumedFrame;
    markOutsideWorkDuringSuspension(browserFrame, importName);
    markFrameWork(browserFrame, importName);
    if (frame) {
      frame.draws++;
      if (isClear) frame.clears++;
    }
    if (!browserFrame) totals.drawsOutsideAnimationFrame++;
  };

  const swap = (ok) => {
    if (!ok) return undefined;
    markOutsideWorkDuringSuspension(currentBrowserFrame(), 'swap');
    const now = clock();
    lastSwapAt = now;
    if (awaitingActivationSwap && lastActivatedAt !== null) {
      const elapsed = Math.max(0, now - lastActivatedAt);
      activation.lastToFirstSwapMs = elapsed;
      activation.maxToFirstSwapMs = Math.max(activation.maxToFirstSwapMs, elapsed);
      diagnostics.count('gw.frame.activation.first-swap.ms.total', elapsed);
      diagnostics.peak('gw.frame.activation.first-swap.ms.max', elapsed);
      awaitingActivationSwap = false;
    }
    if (enabled) {
      totals.swaps++;
      const browserFrame = currentBrowserFrame();
      const frame = browserFrame ?? resumedFrame;
      markFrameWork(browserFrame, 'swap');
      submitFrame(frame);
      if (!browserFrame) {
        totals.swapsOutsideAnimationFrame++;
        diagnostics.count('gw.frame.swap.outside-animation');
      }
      if (frame && resumedFrame === frame) resumedFrame = null;
    }
    return now;
  };

  const deactivate = () => {
    totals.deactivations++;
  };

  const activate = () => {
    const now = clock();
    totals.activations++;
    lastActivatedAt = now;
    activation.lastFrameAgeMs = lastSwapAt === null ? null : Math.max(0, now - lastSwapAt);
    if (activation.lastFrameAgeMs !== null) {
      diagnostics.count('gw.frame.activation.stale-ms.total', activation.lastFrameAgeMs);
      diagnostics.peak('gw.frame.activation.stale-ms.max', activation.lastFrameAgeMs);
    }
    awaitingActivationSwap = true;
  };

  const beginSuspension = (kind, detail = {}) => {
    const browserFrame = currentBrowserFrame();
    const frame = enabled ? currentLogicalFrame() : null;
    markOutsideWorkDuringSuspension(browserFrame, `suspend-${kind}`);
    markFrameWork(browserFrame, `suspend-${kind}`);
    const afterDraw = Boolean(frame && frame.draws > 0 && frame.swaps === 0);
    let site = [];
    if (afterDraw) {
      try {
        site = suspensionSite();
      } catch {
        // Stack capture is supporting evidence. Never let it change a wait.
      }
    }
    const suspension = {
      kind,
      detail: site.length > 0 ? { ...detail, site } : detail,
      frame,
      afterDraw,
      started: clock(),
      ended: false,
      callbacksStartedWhilePending: 0,
      externalCallbacksWhilePending: 0,
      externalKinds: new Set(),
    };
    pendingSuspensions.add(suspension);
    activeSuspensions++;
    if (kind === 'read') totals.suspendingReads++;
    if (kind === 'wait') totals.suspendingWaits++;
    if (afterDraw) {
      totals.suspensionsStartedAfterDraw++;
      frame.pendingSuspensionsAfterDraw++;
      frame.suspensionsAfterDraw.push(suspension);
      diagnostics.count(`gw.frame.suspension-after-draw.${kind}`);
      if (frame.browserEnded) reportInterruptedFrame(frame);
    }
    return suspension;
  };

  const endSuspension = (suspension) => {
    if (!suspension || suspension.ended) return;
    suspension.ended = true;
    pendingSuspensions.delete(suspension);
    activeSuspensions = Math.max(0, activeSuspensions - 1);
    if (suspension.callbacksStartedWhilePending > 0) {
      totals.suspensionsResumedAfterLaterCallback++;
      diagnostics.count('gw.frame.suspension-resumed-after-later-callback');
    }
    if (suspension.afterDraw && suspension.frame) {
      suspension.frame.pendingSuspensionsAfterDraw = Math.max(
        0,
        suspension.frame.pendingSuspensionsAfterDraw - 1,
      );
      const elapsed = Math.max(0, clock() - suspension.started);
      diagnostics.count('gw.frame.suspension-after-draw.ms.total', elapsed);
      diagnostics.peak('gw.frame.suspension-after-draw.ms.max', elapsed);
      if (suspension.frame.interrupted) {
        const event = rememberEvent({
          kind: 'suspension-resolved-after-draw',
          runtime,
          frame: suspension.frame.id,
          suspension: suspension.kind,
          durationMs: round(elapsed),
          callbacksStartedWhilePending: suspension.callbacksStartedWhilePending,
          externalCallbacksWhilePending: suspension.externalCallbacksWhilePending,
          ...suspension.detail,
        });
        if (loggedSuspensionResumes < MAX_EVENT_LOGS) {
          loggedSuspensionResumes++;
          log('[frame-audit-event]', JSON.stringify(event));
        }
      }
    }
    if (suspension.frame?.browserEnded && suspension.frame.swaps === 0) {
      resumedFrame = suspension.frame;
    }
  };

  /**
   * ArenaNet's generated glue gives the synchronous image import a stable,
   * descriptive name.  Stack inspection is intentionally diagnostic-only and
   * happens once per demand read rather than once per draw.
   */
  const isSynchronousImageRead = () =>
    enabled
    && /__asyncjs__[^\n]*ImageReadSync/.test(new Error().stack ?? '');

  /**
   * @param {string} kind
   * @param {number} offset
   * @param {number} bytes
   * @param {boolean} suspends
   */
  const beginRead = (kind, offset, bytes, suspends = false) => {
    const frame = enabled ? currentLogicalFrame() : null;
    const afterDraw = Boolean(frame && frame.draws > 0 && frame.swaps === 0);
    const read = {
      kind,
      offset,
      bytes,
      started: clock(),
      frame,
      afterDraw,
      suspension: suspends ? beginSuspension('read', { offset, bytes }) : null,
      ended: false,
    };
    activeReads++;
    if (afterDraw) {
      totals.readsStartedAfterDraw++;
    }
    return read;
  };

  const endRead = (read, failed = false) => {
    if (!read || read.ended) return;
    read.ended = true;
    activeReads = Math.max(0, activeReads - 1);
    endSuspension(read.suspension);
    const elapsed = Math.max(0, clock() - read.started);
    totals.reads++;
    totals.readBytes += Number(read.bytes) || 0;
    totals.readMs += elapsed;
    totals.readMsMax = Math.max(totals.readMsMax, elapsed);
    if (failed) totals.readFailures++;
    diagnostics.count(`gw.snapshot.${read.kind}.reads`);
    diagnostics.count(`gw.snapshot.${read.kind}.bytes`, Number(read.bytes) || 0);
    diagnostics.count(`gw.snapshot.${read.kind}.ms.total`, elapsed);
    diagnostics.peak(`gw.snapshot.${read.kind}.ms.max`, elapsed);
    if (failed) diagnostics.count(`gw.snapshot.${read.kind}.failures`);
  };

  /**
   * `ImageReadAsync` stores a Promise in `Module.imageReads`; the generated
   * suspending `ImageWait` import later obtains it with Map#get. Observing only
   * that lookup identifies the moment the game waits, without labelling the
   * background read itself as a suspension. The generated async wrapper also
   * awaits a missing value, which is still a one-microtask suspension.
   *
   * @template T
   * @param {Promise<T> | T} promise
   * @param {number | undefined} id
   * @returns {Promise<T> | T}
   */
  const trackImageWait = (promise, id) => {
    if (!enabled) return promise;
    const pending = Boolean(promise && typeof promise.then === 'function');
    const suspension = beginSuspension('wait', {
      readId: id,
      ...(pending ? {} : { alreadyComplete: true }),
      ...imageReadsById.get(id),
    });
    if (!pending) {
      // The generated import is itself async and executes `await Map#get(id)`.
      // Even `await undefined` yields and suspends Wasm for a microtask. End
      // this observation separately without replacing the value or attaching
      // a reaction to the game's Promise.
      scheduleMicrotask(() => endSuspension(suspension));
      return promise;
    }
    const waits = imageWaits.get(id) ?? [];
    waits.push(suspension);
    imageWaits.set(id, waits);
    return promise;
  };

  /** @param {Promise<unknown> | unknown} promise @param {{ offset: number, bytes: number }} detail */
  const tagImageRead = (promise, detail) => {
    if (enabled && promise && typeof promise === 'object') {
      imageReadDetails.set(promise, detail);
    }
    return promise;
  };

  /** @param {number} id @param {Promise<unknown> | unknown} promise */
  const imageReadQueued = (id, promise) => {
    if (!enabled) return;
    const detail = imageReadDetails.get(promise) ?? {};
    imageReadsById.set(id, detail);
    recentImageReads.push({ readId: id, ...detail });
    if (recentImageReads.length > 32) recentImageReads.shift();
  };

  /**
   * The generated background-read completion deletes its Promise from
   * `Module.imageReads` before the awaiting Wasm continuation runs.  Observing
   * that existing synchronous operation avoids attaching a diagnostic Promise
   * reaction in front of or behind the game's own resume.
   *
   * @param {number | undefined} id
   */
  const imageReadResolved = (id) => {
    const waits = imageWaits.get(id);
    imageReadsById.delete(id);
    if (!waits) return;
    imageWaits.delete(id);
    for (const suspension of waits) endSuspension(suspension);
  };

  const findContext = () => {
    // getContext() creates a context when one does not exist.  Never let an
    // observer called before EGL setup claim the canvas with audit defaults;
    // the graphics bridge explicitly tells us after ArenaNet has created it.
    if (context || !contextWasCreated || !canvas?.getContext) return context;
    // Asking for an already-created context returns that context.  Trying both
    // names avoids assuming which WebGL generation ArenaNet requested.
    context = canvas.getContext('webgl2') ?? canvas.getContext('webgl');
    return context;
  };

  const contextCreated = () => {
    contextWasCreated = true;
    findContext();
  };

  canvas?.addEventListener?.('webglcontextlost', () => {
    totals.contextLost++;
    diagnostics.count('gw.webgl.context-lost');
    log('[warn] WebGL context lost');
  });
  canvas?.addEventListener?.('webglcontextrestored', () => {
    totals.contextRestored++;
    diagnostics.count('gw.webgl.context-restored');
    context = null;
    findContext();
    log('WebGL context restored');
  });

  const snapshot = () => {
    const gl = findContext();
    const intervalMean = frameIntervals.count
      ? frameIntervals.total / frameIntervals.count
      : null;
    let canvasState = null;
    if (canvas) {
      let css = null;
      try {
        const rect = canvas.getBoundingClientRect?.();
        if (rect) css = { width: round(rect.width), height: round(rect.height) };
      } catch {
        // A detached canvas can reject layout queries; its buffer still helps.
      }
      canvasState = {
        width: canvas.width,
        height: canvas.height,
        css,
      };
    }
    let pageState;
    try {
      pageState = page();
    } catch (error) {
      pageState = { auditError: String(error) };
    }
    return {
      runtime,
      detailed: enabled,
      page: pageState,
      canvas: canvasState,
      webgl: {
        type: gl?.constructor?.name ?? null,
        lost: gl?.isContextLost?.() ?? null,
        drawingBufferWidth: gl?.drawingBufferWidth ?? null,
        drawingBufferHeight: gl?.drawingBufferHeight ?? null,
        attributes: contextAttributes(gl),
      },
      activeReads,
      activeSuspensions,
      resumedFrame: resumedFrame ? {
        id: resumedFrame.id,
        draws: resumedFrame.draws,
        clears: resumedFrame.clears,
        swaps: resumedFrame.swaps,
        browserEnded: resumedFrame.browserEnded,
      } : null,
      recentImageReads: enabled ? [...recentImageReads] : [],
      recentEvents: enabled ? [...recentEvents] : [],
      activation: {
        lastFrameAgeMs: round(activation.lastFrameAgeMs),
        lastToFirstSwapMs: round(activation.lastToFirstSwapMs),
        maxToFirstSwapMs: round(activation.maxToFirstSwapMs),
        awaitingFirstSwap: awaitingActivationSwap,
      },
      totals: {
        ...totals,
        readMs: round(totals.readMs),
        readMsMax: round(totals.readMsMax),
      },
      submittedFrameIntervalMs: {
        count: frameIntervals.count,
        mean: round(intervalMean),
        min: round(frameIntervals.min),
        max: round(frameIntervals.max),
      },
    };
  };

  const mark = () => {
    const state = snapshot();
    log('[frame-audit]', JSON.stringify(state));
    return state;
  };

  return Object.freeze({
    enabled,
    beginAnimationFrame,
    endAnimationFrame,
    beginExternalCallback,
    endExternalCallback,
    draw,
    swap,
    beginRead,
    endRead,
    isSynchronousImageRead,
    trackImageWait,
    tagImageRead,
    imageReadQueued,
    imageReadResolved,
    contextCreated,
    activate,
    deactivate,
    snapshot,
    mark,
  });
}
