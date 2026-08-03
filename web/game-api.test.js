import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  installGameApi,
  publicState,
  publishGameStateUnavailable,
} from './game-api.js';

describe('public game state transport', () => {
  it('keeps only the supported telemetry fields', () => {
    const state = publicState({
      status: 'ready',
      mapId: 55,
      playerId: 2,
      targetValid: false,
      rangeName: 'None',
      sequence: 99,
      credentials: { password: 'never' },
    });
    assert.deepEqual(state, {
      status: 'ready',
      mapId: 55,
      playerId: 2,
      targetValid: false,
      rangeName: 'None',
    });
    assert(Object.isFrozen(state));
  });

  it('removes fixed target slots when no target is valid', () => {
    assert.deepEqual(
      publicState({
        status: 'ready',
        mapId: 55,
        playerId: 2,
        targetValid: false,
        targetId: 0,
        targetKind: 'None',
        targetX: 0,
        targetY: 0,
        distance: 0,
        rangeName: 'None',
      }),
      {
        status: 'ready',
        mapId: 55,
        playerId: 2,
        targetValid: false,
        targetKind: 'None',
        rangeName: 'None',
      },
    );
  });

  it('cannot carry stale telemetry in an unavailable state', () => {
    assert.deepEqual(
      publicState({ status: 'unsupported', reason: 'stopped', mapId: 55, playerId: 2 }),
      { status: 'unsupported', reason: 'stopped' },
    );
    let published = null;
    const target = { gwGameApi: { publish: (state) => { published = state; } } };
    const state = publishGameStateUnavailable('stopped', target);
    assert.deepEqual(published, { status: 'unsupported', reason: 'stopped' });
    assert.equal(target.gwGameState, state);
  });

  it('publishes with the injection-only token rather than the browser token', async () => {
    const originalWindow = globalThis.window;
    const originalFetch = globalThis.fetch;
    let offered = null;
    globalThis.window = {
      __gwnativeToken: 'browser-secret',
      __gwnativeGamePublisherToken: 'publisher-secret',
    };
    globalThis.fetch = async (_url, options) => {
      offered = options.headers['X-Gwnative-Token'];
      return { ok: true };
    };
    try {
      let scheduled = null;
      installGameApi({
        log() {},
        now: () => 1_000,
        setTimer(callback, delay) {
          scheduled = { callback, delay };
          return 1;
        },
      }).publish({ status: 'waiting' });
      assert.equal(scheduled.delay, 0);
      scheduled.callback();
      assert.equal(offered, 'publisher-secret');
    } finally {
      globalThis.window = originalWindow;
      globalThis.fetch = originalFetch;
    }
  });

  it('limits repeated failed publication attempts to four per second', async () => {
    const originalWindow = globalThis.window;
    const originalFetch = globalThis.fetch;
    globalThis.window = { __gwnativeGamePublisherToken: 'publisher-secret' };
    let current = 1_000;
    let settle = null;
    const attempts = [];
    const timers = [];
    const logs = [];
    globalThis.fetch = async () => {
      attempts.push(current);
      return new Promise((resolve) => { settle = resolve; });
    };
    const api = installGameApi({
      log: (message) => logs.push(message),
      now: () => current,
      setTimer(callback, delay) {
        timers.push({ callback, delay });
        return timers.length;
      },
    });

    try {
      api.publish({ status: 'waiting' });
      const first = timers.shift();
      assert.equal(first.delay, 0);
      first.callback();
      assert.deepEqual(attempts, [1_000]);
      // Start the request, then keep producing while that request is in flight.
      api.publish({ status: 'waiting' });
      api.publish({ status: 'waiting' });
      assert.equal(timers.length, 0);
      settle({ ok: false, status: 503 });
      await new Promise((resolve) => setImmediate(resolve));

      const second = timers.shift();
      assert.equal(second.delay, 250);
      current += second.delay;
      second.callback();
      assert.deepEqual(attempts, [1_000, 1_250]);

      api.publish({ status: 'waiting' });
      settle({ ok: false, status: 503 });
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(timers.length, 1);
      assert.equal(timers[0].delay, 250);
      assert.equal(logs.length, 1, 'the repeated failure should be logged once');
    } finally {
      globalThis.window = originalWindow;
      globalThis.fetch = originalFetch;
    }
  });
});
