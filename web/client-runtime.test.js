import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  applyClientLimits,
  deliverRuntimeProof,
  postRuntimeState,
  readRuntimePlan,
  selectClient,
  supportsJspi,
  transitionRuntimeFailure,
} from './client-runtime.js';

const workingJspi = {
  Module: class {},
  Instance: class {
    constructor(_module, imports) {
      this.exports = { g: () => imports.e.f.operation() };
    }
  },
  Suspending: class {
    constructor(operation) {
      this.operation = operation;
    }
  },
  promising: (operation) => async () => operation(),
};

describe('client runtime selection', () => {
  it('uses Asyncify when the WKWebView has no JSPI', async () => {
    assert.equal(await supportsJspi({}), false);
    assert.deepEqual(
      await selectClient({}),
      { mode: 'asyncify', glue: 'Gw.js', wasm: 'Gw.wasm' },
    );
  });

  it('uses JSPI only after a functional suspend/resume round trip', async () => {
    assert.equal(await supportsJspi(workingJspi), true);
    assert.deepEqual(
      await selectClient(workingJspi),
      { mode: 'jspi', glue: 'Gw.jspi.js', wasm: 'Gw.jspi.wasm' },
    );
  });

  it('falls back when JSPI is present but does not work', async () => {
    const broken = { ...workingJspi, promising: () => async () => 41 };
    assert.equal(await supportsJspi(broken), false);
    assert.equal((await selectClient(broken)).mode, 'asyncify');
  });

  it('falls back when a partial JSPI implementation never resumes', async () => {
    const stuck = {
      ...workingJspi,
      promising: () => () => new Promise(() => {}),
    };
    assert.equal(await supportsJspi(stuck, 1), false);
  });

  it('can force Asyncify for runner coverage', async () => {
    assert.equal((await selectClient({}, 'asyncify')).mode, 'asyncify');
  });

  it('refuses to force JSPI in an incompatible WKWebView', async () => {
    await assert.rejects(
      selectClient({}, 'jspi'),
      /failed its suspend\/resume probe/,
    );
  });

  it('selects Asyncify in a fresh realm after the exact JSPI runtime failed', async () => {
    assert.equal(
      (await selectClient(workingJspi, undefined, { failedOfficial: ['jspi'] })).mode,
      'asyncify',
    );
    assert.equal(
      (await selectClient(workingJspi, 'jspi', { failedOfficial: ['jspi'] })).mode,
      'asyncify',
      'a bring-up preference cannot create a persisted crash loop',
    );
  });

  it('refuses an exhausted or force-selected failed official runtime', async () => {
    await assert.rejects(
      selectClient(workingJspi, undefined, { failedOfficial: ['jspi', 'asyncify'] }),
      /No compatible official runtime remains/,
    );
    await assert.rejects(
      selectClient(workingJspi, 'asyncify', { failedOfficial: ['asyncify'] }),
      /forced Asyncify runtime already failed/,
    );
  });

  it('validates the host runtime plan before using it', async () => {
    const response = (status) => ({
      ok: true,
      status,
    });
    assert.deepEqual(
      await readRuntimePlan({
        fetch: async () => response(221),
      }),
      { failedOfficial: ['jspi'] },
    );
    await assert.rejects(
      readRuntimePlan({ fetch: async () => response(227) }),
      /invalid runtime plan/,
    );
  });

  it('decodes bodyless runtime transition acknowledgements', async () => {
    const send = (status) => async () => ({ ok: true, status });
    assert.deepEqual(
      await postRuntimeState('__runtime-failed', {}, { fetch: send(224) }),
      { outcome: 'try-runtime', runtime: 'asyncify' },
    );
    assert.deepEqual(
      await postRuntimeState('__runtime-failed', {}, { fetch: send(225) }),
      { outcome: 'predecessor-restored' },
    );
    assert.deepEqual(
      await postRuntimeState('__runtime-failed', {}, { fetch: send(226) }),
      { outcome: 'exhausted' },
    );
  });

  it('persists an official failure before requesting a fresh WKWebView', async () => {
    const order = [];
    const launch = { runtime: 'jspi', mode: 'original', nonce: 'exact' };
    const result = await transitionRuntimeFailure(launch, {
      post: async (path, body) => {
        order.push(['persist', path, body]);
        return { outcome: 'try-runtime', runtime: 'asyncify' };
      },
      relaunch: async () => order.push(['relaunch']),
    });
    assert.deepEqual(result, { outcome: 'try-runtime', runtime: 'asyncify' });
    assert.deepEqual(order, [
      ['persist', '__runtime-failed', { launch }],
      ['relaunch'],
    ]);
  });

  it('retries the exact runtime failure after a lost acknowledgement', async () => {
    const launch = { runtime: 'jspi', transformed: false, nonce: 'exact' };
    const claims = [];
    let relaunched = 0;
    await transitionRuntimeFailure(launch, {
      delays: [0, 0],
      post: async (path, body) => {
        claims.push([path, body]);
        if (claims.length === 1) throw new Error('acknowledgement lost');
        return { outcome: 'try-runtime', runtime: 'asyncify' };
      },
      relaunch: async () => { relaunched += 1; },
    });
    assert.equal(claims.length, 2);
    assert.ok(claims.every(([path, body]) => path === '__runtime-failed' && body.launch === launch));
    assert.equal(relaunched, 1);
  });

  it('does not relaunch when both runtimes are exhausted without a predecessor', async () => {
    let relaunched = false;
    await assert.rejects(
      transitionRuntimeFailure({}, {
        post: async () => ({ outcome: 'exhausted' }),
        relaunch: async () => { relaunched = true; },
      }),
      /no predecessor was removed/,
    );
    assert.equal(relaunched, false);
  });

  it('applies only the independently selected JSPI certificate', () => {
    const state = {
      __gwnativeRuntimeCapabilities: {
        jspi: {
          build: 'certified-jspi-build',
          templateSave: 'ready',
          enhancements: 'ready',
          enhancementManifest: { familyId: 'jspi-asyncify-pair' },
        },
        asyncify: {
          build: 'certified-asyncify-build',
          templateSave: 'ready',
          enhancements: 'ready',
          enhancementManifest: { familyId: 'jspi-asyncify-pair' },
        },
      },
    };
    applyClientLimits(
      { mode: 'jspi', glue: 'Gw.jspi.js', wasm: 'Gw.jspi.wasm' },
      { nativeCursor: true, targetReadout: true },
      state,
    );
    assert.equal(state.__gwnativeTemplateSave, 'ready');
    assert.equal(state.__gwnativeClientBuild, 'certified-jspi-build');
    assert.equal(state.__gwnativeEnhancements, 'ready');
    assert.deepEqual(state.__gwnativeEnhancementManifest, {
      familyId: 'jspi-asyncify-pair',
    });
  });

  it('does not inherit JSPI facts when Asyncify is selected', () => {
    const state = {
      __gwnativeRuntimeCapabilities: {
        jspi: {
          build: 'jspi',
          templateSave: 'ready',
          enhancements: 'ready',
          enhancementManifest: { runtime: 'jspi' },
        },
        asyncify: {
          build: 'asyncify',
          templateSave: 'ready',
          enhancements: 'ready',
          enhancementManifest: { runtime: 'asyncify' },
        },
      },
    };
    applyClientLimits(
      { mode: 'asyncify', glue: 'Gw.js', wasm: 'Gw.wasm' },
      { nativeCursor: true, targetReadout: false },
      state,
    );
    assert.equal(state.__gwnativeTemplateSave, 'ready');
    assert.equal(state.__gwnativeClientBuild, 'asyncify');
    assert.equal(state.__gwnativeEnhancements, 'ready');
    assert.deepEqual(state.__gwnativeEnhancementManifest, { runtime: 'asyncify' });
  });

  it('fails closed when the selected artifact has no certificate', () => {
    const state = {};
    applyClientLimits(
      { mode: 'asyncify', glue: 'Gw.js', wasm: 'Gw.wasm' },
      { nativeCursor: true, targetReadout: false },
      state,
    );
    assert.equal(state.__gwnativeTemplateSave, 'uncertified');
    assert.equal(state.__gwnativeEnhancements, 'uncertified');
    assert.equal(state.__gwnativeEnhancementManifest, null);
  });

  it('does not let runtime-state persistence hold client startup', async () => {
    const neverAnswers = (_path, { signal }) => new Promise((_resolve, reject) => {
      signal.addEventListener('abort', () => {
        reject(new DOMException('timed out', 'AbortError'));
      });
    });
    await assert.rejects(
      postRuntimeState('__runtime', {}, {
        fetch: neverAnswers,
        token: 'test',
        deadlineMs: 1,
      }),
      { name: 'AbortError' },
    );
  });

  it('retries a lost proof with identical data and bounded backoff', async () => {
    const calls = [];
    const waits = [];
    const body = { launch: { nonce: 'exact' } };
    const result = await deliverRuntimeProof('__booted', body, {
      delays: [0, 10, 20],
      wait: async (delay) => waits.push(delay),
      post: async (path, sent) => {
        calls.push({ path, sent });
        if (calls.length < 3) throw new Error('reply lost');
        return null;
      },
    });
    assert.equal(result, null);
    assert.deepEqual(waits, [10, 20]);
    assert.equal(calls.length, 3);
    assert.ok(calls.every(({ path, sent }) => path === '__booted' && sent === body));
  });

  it('retries a durably recorded initial attempt before executing glue', async () => {
    const claim = { runtime: 'jspi', build: null, transformed: false, nonce: 'exact' };
    const calls = [];
    await deliverRuntimeProof('__runtime', claim, {
      delays: [0, 0],
      post: async (path, body) => {
        calls.push([path, body]);
        if (calls.length === 1) throw new Error('204 lost');
        return null;
      },
    });
    assert.equal(calls.length, 2);
    assert.ok(calls.every(([path, body]) => path === '__runtime' && body === claim));
  });

  it('stops proof retry after the bounded attempt set', async () => {
    let attempts = 0;
    await assert.rejects(
      deliverRuntimeProof('__booted', {}, {
        delays: [0, 0, 0],
        post: async () => {
          attempts += 1;
          throw new Error('offline');
        },
      }),
      /offline/,
    );
    assert.equal(attempts, 3);
  });
});
