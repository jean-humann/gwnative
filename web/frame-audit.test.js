import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { createFrameAudit } from './frame-audit.js';

function fakeCanvas() {
  const listeners = new Map();
  const context = {
    constructor: { name: 'WebGL2RenderingContext' },
    drawingBufferWidth: 1600,
    drawingBufferHeight: 900,
    getContextAttributes: () => ({ alpha: false, antialias: true }),
    isContextLost: () => false,
  };
  return {
    width: 1600,
    height: 900,
    addEventListener: (name, run) => listeners.set(name, run),
    dispatch: (name) => listeners.get(name)?.(),
    getBoundingClientRect: () => ({ width: 800, height: 450 }),
    getContext: (name) => name === 'webgl2' ? context : null,
  };
}

describe('frame audit', () => {
  it('recognises an ordinary submitted frame and its native refresh interval', () => {
    let now = 0;
    const audit = createFrameAudit({ enabled: true, clock: () => now });

    let frame = audit.beginAnimationFrame(100);
    audit.draw();
    audit.swap(true);
    audit.endAnimationFrame(frame);

    now = 8.333;
    frame = audit.beginAnimationFrame(108.333);
    audit.draw();
    audit.swap(true);
    audit.endAnimationFrame(frame);

    const state = audit.snapshot();
    assert.equal(state.totals.submittedLogicalFrames, 2);
    assert.equal(state.totals.framesInterruptedAfterDraw, 0);
    assert.equal(state.totals.swapsOutsideAnimationFrame, 0);
    assert.equal(state.submittedFrameIntervalMs.mean, 8.333);
  });

  it('treats a clear before suspension as potentially exposed frame work', () => {
    const lines = [];
    const audit = createFrameAudit({
      enabled: true,
      log: (...parts) => lines.push(parts.join(' ')),
    });
    const frame = audit.beginAnimationFrame(16);
    audit.draw('glClear');
    audit.trackImageWait(new Promise(() => {}), 4);
    audit.endAnimationFrame(frame);

    const state = audit.snapshot();
    assert.equal(state.totals.drawCalls, 1);
    assert.equal(state.totals.clearCalls, 1);
    assert.equal(state.totals.framesInterruptedAfterDraw, 1);
    assert.equal(state.recentEvents[0].clears, 1);
    assert.match(lines.join('\n'), /frame-interrupted-after-draw/);
  });

  it('records the partial-frame signature of a read that suspends after drawing', () => {
    let now = 10;
    const metrics = [];
    const lines = [];
    const audit = createFrameAudit({
      enabled: true,
      runtime: 'asyncify',
      clock: () => now,
      suspensionSite: () => ['wasm-function[227]:0xabc'],
      diagnostics: {
        count: (name, value = 1) => metrics.push(['count', name, value]),
        peak: (name, value) => metrics.push(['peak', name, value]),
      },
      log: (...parts) => lines.push(parts.join(' ')),
    });

    const frame = audit.beginAnimationFrame(10);
    audit.draw();
    const read = audit.beginRead('read', 4096, 8192, true);
    audit.endAnimationFrame(frame);
    now = 27;
    audit.endRead(read);
    // A JSPI continuation or Asyncify rewind can resume outside the browser's
    // original animation-frame callback.
    audit.draw();
    audit.swap(true);

    const state = audit.snapshot();
    assert.equal(state.runtime, 'asyncify');
    assert.equal(state.totals.readsStartedAfterDraw, 1);
    assert.equal(state.totals.suspendingReads, 1);
    assert.equal(state.totals.suspensionsStartedAfterDraw, 1);
    assert.equal(state.totals.framesInterruptedAfterDraw, 1);
    assert.equal(state.totals.drawsOutsideAnimationFrame, 1);
    assert.equal(state.totals.swapsOutsideAnimationFrame, 1);
    assert.equal(state.totals.readMsMax, 17);
    assert.deepEqual(
      state.recentEvents.map((event) => event.kind),
      ['frame-interrupted-after-draw', 'suspension-resolved-after-draw'],
    );
    assert.ok(metrics.some(([, name]) => name === 'gw.snapshot.read.ms.max'));
    assert.ok(metrics.some(([, name]) => name === 'gw.frame.suspension-after-draw.read'));
    assert.ok(metrics.some(([, name]) => name === 'gw.frame.interrupted-after-draw'));
    assert.ok(metrics.some(([, name]) => name === 'gw.frame.swap.outside-animation'));
    assert.equal(lines.length, 2);
    assert.match(lines[0], /^\[frame-audit-event\] /);
    assert.deepEqual(
      JSON.parse(lines[0].slice(lines[0].indexOf('{'))).pendingSuspensions,
      [{
        kind: 'read',
        ageMs: 0,
        offset: 4096,
        bytes: 8192,
        site: ['wasm-function[227]:0xabc'],
      }],
    );
  });

  it('distinguishes a background image read from a later suspending wait', async () => {
    let resolve;
    const pending = new Promise((done) => { resolve = done; });
    const audit = createFrameAudit({ enabled: true });
    audit.tagImageRead(pending, { offset: 8192, bytes: 4096 });
    audit.imageReadQueued(7, pending);
    const frame = audit.beginAnimationFrame(10);
    audit.draw();
    const read = audit.beginRead('read', 0, 4096);
    const wait = audit.trackImageWait(pending, 7);
    assert.equal(wait, pending, 'the audit must not add a Promise to the resume path');
    audit.endAnimationFrame(frame);

    let state = audit.snapshot();
    assert.equal(state.totals.suspendingReads, 0);
    assert.equal(state.totals.suspendingWaits, 1);
    assert.equal(state.totals.framesInterruptedAfterDraw, 1);
    assert.equal(state.activeSuspensions, 1);

    resolve();
    audit.imageReadResolved(7);
    await wait;
    audit.endRead(read);
    state = audit.snapshot();
    assert.equal(state.activeSuspensions, 0);
    assert.deepEqual(state.recentImageReads, [
      { readId: 7, offset: 8192, bytes: 4096 },
    ]);
  });

  it('tracks the generated await when an image wait is already complete', () => {
    const microtasks = [];
    const audit = createFrameAudit({
      enabled: true,
      scheduleMicrotask: (callback) => microtasks.push(callback),
    });
    const frame = audit.beginAnimationFrame(10);
    audit.draw('glClear');
    assert.equal(audit.trackImageWait(undefined, 9), undefined);
    audit.endAnimationFrame(frame);

    let state = audit.snapshot();
    assert.equal(state.activeSuspensions, 1);
    assert.equal(state.totals.suspendingWaits, 1);
    assert.equal(state.totals.framesInterruptedAfterDraw, 1);
    assert.equal(state.recentEvents[0].pendingSuspensions[0].alreadyComplete, true);

    microtasks.shift()();
    state = audit.snapshot();
    assert.equal(state.activeSuspensions, 0);
    assert.equal(state.recentEvents.at(-1).kind, 'suspension-resolved-after-draw');
  });

  it('detects a later animation callback entering before an interrupted wait resumes', () => {
    let now = 0;
    const metrics = [];
    const lines = [];
    const pending = new Promise(() => {});
    const audit = createFrameAudit({
      enabled: true,
      clock: () => now,
      diagnostics: {
        count: (name, value = 1) => metrics.push(['count', name, value]),
        peak: (name, value) => metrics.push(['peak', name, value]),
      },
      log: (...parts) => lines.push(parts.join(' ')),
    });

    const interrupted = audit.beginAnimationFrame(0);
    audit.draw();
    audit.trackImageWait(pending, 12);
    audit.endAnimationFrame(interrupted);
    now = 8;
    const later = audit.beginAnimationFrame(8);
    audit.draw();
    audit.endAnimationFrame(later);
    now = 15;
    audit.imageReadResolved(12);

    const state = audit.snapshot();
    assert.equal(state.totals.callbacksStartedDuringSuspension, 1);
    assert.equal(state.totals.callbacksDoingWorkDuringSuspension, 1);
    assert.equal(state.totals.suspensionsResumedAfterLaterCallback, 1);
    assert.deepEqual(
      state.recentEvents.map((event) => event.kind),
      [
        'frame-interrupted-after-draw',
        'callback-work-during-suspension',
        'suspension-resolved-after-draw',
      ],
    );
    assert.ok(metrics.some(([, name]) => name === 'gw.frame.callback-during-suspension'));
    assert.ok(metrics.some(([, name]) =>
      name === 'gw.frame.callback-work-during-suspension'));
    assert.ok(metrics.some(([, name]) =>
      name === 'gw.frame.suspension-resumed-after-later-callback'));
    const resolved = lines
      .map((line) => JSON.parse(line.slice(line.indexOf('{'))))
      .find((event) => event.kind === 'suspension-resolved-after-draw');
    assert.equal(resolved.durationMs, 15);
    assert.equal(resolved.callbacksStartedWhilePending, 1);
    assert.equal(resolved.readId, 12);
  });

  it('detects renderer work from a non-animation callback during suspension', () => {
    const lines = [];
    const pending = new Promise(() => {});
    const audit = createFrameAudit({
      enabled: true,
      log: (...parts) => lines.push(parts.join(' ')),
    });
    const interrupted = audit.beginAnimationFrame(0);
    audit.draw();
    audit.trackImageWait(pending, 14);
    audit.endAnimationFrame(interrupted);

    // This has no animation-frame boundary: a WebSocket callback is entering
    // a Wasm export independently.
    const socketCallback = audit.beginExternalCallback('socket-message');
    audit.draw('glClear');
    audit.draw('glDrawElementsInstanced');
    audit.endExternalCallback(socketCallback);

    const state = audit.snapshot();
    assert.equal(state.totals.externalCallbacksDuringSuspension, 1);
    assert.equal(state.totals.outsideWorkDuringSuspension, 1);
    assert.equal(state.totals.drawsOutsideAnimationFrame, 2);
    assert.equal(state.recentEvents.at(-1).kind, 'outside-work-during-suspension');
    assert.equal(state.recentEvents.at(-1).work, 'glClear');
    assert.equal(state.recentEvents.at(-1).externalCallback, 'socket-message');
    assert.equal(state.recentEvents.at(-2).kind, 'external-callback-during-suspension');
    assert.match(lines.join('\n'), /outside-work-during-suspension/);
  });

  it('keeps the logical frame across a no-draw unwind and catches a later outside wait', () => {
    const lines = [];
    const pending = new Promise(() => {});
    const audit = createFrameAudit({
      enabled: true,
      runtime: 'asyncify',
      log: (...parts) => lines.push(parts.join(' ')),
      suspensionSite: () => ['wasm-function[536]:0x1784f'],
    });

    const frame = audit.beginAnimationFrame(20);
    audit.trackImageWait(pending, 1);
    audit.endAnimationFrame(frame);
    assert.equal(audit.snapshot().totals.framesInterruptedAfterDraw, 0);

    // The generated completion deletes read 1 before Asyncify rewinds. The
    // continuation then draws and reaches a second wait without re-entering a
    // browser animation callback.
    audit.imageReadResolved(1);
    audit.draw();
    audit.trackImageWait(pending, 2);

    let state = audit.snapshot();
    assert.equal(state.totals.drawsOutsideAnimationFrame, 1);
    assert.equal(state.totals.framesInterruptedAfterDraw, 1);
    assert.equal(state.resumedFrame.id, 1);
    const interrupted = lines
      .map((line) => JSON.parse(line.slice(line.indexOf('{'))))
      .find((event) => event.kind === 'frame-interrupted-after-draw');
    assert.equal(interrupted.browserCallbackEnded, true);
    assert.equal(interrupted.draws, 1);
    assert.equal(interrupted.pendingSuspensions[0].readId, 2);

    audit.imageReadResolved(2);
    audit.swap(true);
    state = audit.snapshot();
    assert.equal(state.resumedFrame, null);
    assert.equal(state.totals.submittedLogicalFrames, 1);
    assert.equal(state.totals.swapsOutsideAnimationFrame, 1);
  });

  it('keeps cheap read and context evidence when detailed hooks are disabled', () => {
    let now = 0;
    const canvas = fakeCanvas();
    const audit = createFrameAudit({ canvas, clock: () => now });
    audit.contextCreated();
    const frame = audit.beginAnimationFrame(0);
    audit.draw();
    audit.swap(true);
    audit.endAnimationFrame(frame);
    const read = audit.beginRead('warm', 0, 262144);
    now = 4;
    audit.endRead(read);
    canvas.dispatch('webglcontextlost');
    canvas.dispatch('webglcontextrestored');

    const state = audit.snapshot();
    assert.equal(state.detailed, false);
    assert.equal(state.totals.animationCallbacks, 0);
    assert.equal(state.totals.reads, 1);
    assert.equal(state.totals.contextLost, 1);
    assert.equal(state.totals.contextRestored, 1);
    assert.equal(state.canvas.width, 1600);
    assert.equal(state.webgl.type, 'WebGL2RenderingContext');
  });

  it('does not claim the canvas before ArenaNet creates its EGL context', () => {
    let getContextCalls = 0;
    const audit = createFrameAudit({
      canvas: {
        width: 1,
        height: 1,
        addEventListener() {},
        getContext() {
          getContextCalls++;
          return {};
        },
      },
    });

    assert.equal(audit.snapshot().webgl.type, null);
    assert.equal(getContextCalls, 0);
    audit.contextCreated();
    assert.equal(getContextCalls, 1);
  });

  it('measures stale-frame age and time to the first swap after activation', () => {
    let now = 10;
    const metrics = [];
    const audit = createFrameAudit({
      clock: () => now,
      diagnostics: {
        count: (name, value = 1) => metrics.push(['count', name, value]),
        peak: (name, value) => metrics.push(['peak', name, value]),
      },
    });
    audit.swap(true);
    audit.deactivate();
    now = 510;
    audit.activate();
    now = 518.5;
    audit.swap(true);

    const state = audit.snapshot();
    assert.equal(state.totals.deactivations, 1);
    assert.equal(state.totals.activations, 1);
    assert.equal(state.activation.lastFrameAgeMs, 500);
    assert.equal(state.activation.lastToFirstSwapMs, 8.5);
    assert.equal(state.activation.awaitingFirstSwap, false);
    assert.deepEqual(
      metrics.filter(([, name]) => name.startsWith('gw.frame.activation.')),
      [
        ['count', 'gw.frame.activation.stale-ms.total', 500],
        ['peak', 'gw.frame.activation.stale-ms.max', 500],
        ['count', 'gw.frame.activation.first-swap.ms.total', 8.5],
        ['peak', 'gw.frame.activation.first-swap.ms.max', 8.5],
      ],
    );
  });

  it('writes one self-contained structured line at a user mark', () => {
    const lines = [];
    const audit = createFrameAudit({
      runtime: 'jspi',
      log: (...parts) => lines.push(parts.join(' ')),
      page: () => ({ visibility: 'visible' }),
    });
    const state = audit.mark();
    assert.equal(state.runtime, 'jspi');
    assert.equal(state.page.visibility, 'visible');
    assert.equal(lines.length, 1);
    assert.match(lines[0], /^\[frame-audit\] \{"runtime":"jspi"/);
  });

  it('samples logical swaps without enabling detailed draw hooks', () => {
    let now = 100;
    const canvas = fakeCanvas();
    const audit = createFrameAudit({ runtime: 'jspi', canvas, clock: () => now });
    audit.contextCreated();
    const finish = audit.beginPerformanceSample();
    for (const interval of [0, 8, 9, 8, 17]) {
      now += interval;
      const frame = audit.beginAnimationFrame(now);
      audit.swap(true);
      audit.endAnimationFrame(frame);
    }
    now = 150;

    assert.deepEqual(finish(), {
      runtime: 'jspi',
      durationMs: 50,
      frames: 5,
      framesPerSecond: 100,
      intervalMs: {
        samples: 4,
        mean: 10.5,
        p50: 8,
        p95: 17,
        p99: 17,
        max: 17,
      },
      callbackToSwapMs: {
        samples: 5,
        unsampled: 0,
        mean: 0,
        p50: 0,
        p95: 0,
        p99: 0,
        max: 0,
      },
      canvas: {
        width: 1600,
        height: 900,
        css: { width: 800, height: 450 },
      },
      webgl: {
        type: 'WebGL2RenderingContext',
        lost: false,
        drawingBufferWidth: 1600,
        drawingBufferHeight: 900,
        attributes: { alpha: false, antialias: true },
      },
      audit: {
        contextLost: 0,
        contextRestored: 0,
        framesInterruptedAfterDraw: 0,
        callbacksDoingWorkDuringSuspension: 0,
        outsideWorkDuringSuspension: 0,
      },
    });
  });

  it('allows only one bounded performance sample at a time', () => {
    const audit = createFrameAudit();
    const finish = audit.beginPerformanceSample();
    assert.throws(() => audit.beginPerformanceSample(), /already running/);
    finish();
    assert.throws(() => finish(), /no longer active/);
  });

  it('keeps a multi-second stall in the sampled frame distribution', () => {
    let now = 0;
    const audit = createFrameAudit({ clock: () => now });
    const finish = audit.beginPerformanceSample();
    audit.swap(true);
    now = 2_000;
    audit.swap(true);
    const sample = finish();
    assert.equal(sample.framesPerSecond, 1);
    assert.equal(sample.intervalMs.p95, 2_000);
    assert.equal(sample.intervalMs.max, 2_000);
  });
});
