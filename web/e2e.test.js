import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  buttonNamed,
  executeE2EAction,
  installE2EBridge,
  prepareNativeE2EAction,
  restoreStorage,
  snapshotStorage,
} from './e2e.js';

class MemoryStorage {
  constructor(entries = []) {
    this.values = new Map(entries);
  }

  getItem(key) {
    return this.values.has(key) ? this.values.get(key) : null;
  }

  setItem(key, value) {
    this.values.set(key, String(value));
  }

  removeItem(key) {
    this.values.delete(key);
  }
}

describe('end-to-end helpers', () => {
  it('finds controls by exact normalized text', () => {
    const save = { textContent: ' Save ' };
    const restart = { textContent: 'Save and restart' };
    const root = { querySelectorAll: () => [save, restart] };
    assert.equal(buttonNamed(root, 'Save'), save);
    assert.throws(() => buttonNamed(root, 'Done'), /found 0/);
  });

  it('restores present, empty, and absent storage keys exactly', () => {
    const storage = new MemoryStorage([
      ['gwnative.overlay-layout.v1', ''],
      ['gwnative.build-library.v1', '{"format":1,"builds":[]}'],
    ]);
    const snapshot = snapshotStorage(storage);
    storage.setItem('gwnative.overlay-layout.v1', 'changed');
    storage.removeItem('gwnative.build-library.v1');

    restoreStorage(storage, snapshot);
    assert.equal(storage.getItem('gwnative.overlay-layout.v1'), '');
    assert.equal(
      storage.getItem('gwnative.build-library.v1'),
      '{"format":1,"builds":[]}',
    );

    const empty = new MemoryStorage();
    const absent = snapshotStorage(empty);
    empty.setItem('gwnative.overlay-layout.v1', 'temporary');
    restoreStorage(empty, absent);
    assert.equal(empty.getItem('gwnative.overlay-layout.v1'), null);
  });

  it('refuses to synthesize gameplay keyboard events in the page', async () => {
    const window = { Module: {} };
    await assert.rejects(
      executeE2EAction(
        { sequence: 1, action: 'activate', durationMs: 40 },
        { window, canvas: {} },
      ),
      /native-only/,
    );
    await assert.rejects(
      executeE2EAction(
        { sequence: 2, action: 'move-forward', durationMs: 800 },
        { window, canvas: {} },
      ),
      /native-only/,
    );
  });

  it('reports the active text proxy after the page-owned UI check', async () => {
    const canvas = {};
    const proxy = {};
    const window = {
      Module: { canvas, oskActiveInput: proxy, oskInput: { password: proxy } },
      gwRunAppE2E: async () => {},
    };
    assert.deepEqual(
      await executeE2EAction(
        { sequence: 1, action: 'test-ui', durationMs: 0 },
        { window, canvas },
      ),
      { target: 'app-ui', activeTarget: 'password-proxy' },
    );
  });

  it('prepares only the active text proxy or game canvas for native input', () => {
    let focused = '';
    const canvas = { focus: () => { focused = 'canvas'; } };
    const proxy = { focus: () => { focused = 'password'; } };
    const window = {
      Module: { canvas, oskActiveInput: proxy, oskInput: { password: proxy } },
    };
    assert.equal(
      prepareNativeE2EAction(
        { sequence: 1, action: 'activate', durationMs: 40 },
        { window, canvas },
      ),
      'password-proxy',
    );
    assert.equal(focused, 'password');
    assert.equal(
      prepareNativeE2EAction(
        { sequence: 2, action: 'move-forward', durationMs: 800 },
        { window, canvas },
      ),
      'canvas',
    );
    assert.equal(focused, 'canvas');
    assert.throws(
      () => prepareNativeE2EAction(
        { sequence: 3, action: 'type-password', durationMs: 40 },
        { window, canvas },
      ),
      /allowed vocabulary/,
    );
  });

  it('runs app UI checks without dispatching game input', async () => {
    let checks = 0;
    const window = {
      Module: {},
      gwRunAppE2E: async () => {
        checks += 1;
      },
    };
    assert.deepEqual(
      await executeE2EAction(
        { sequence: 1, action: 'test-ui', durationMs: 0 },
        { window, canvas: {}, sleep: async () => {} },
      ),
      { target: 'app-ui', activeTarget: 'canvas' },
    );
    assert.equal(checks, 1);
  });

  it('keeps the action channel dormant in normal launches', () => {
    let fetched = 0;
    const window = {
      __gwnativeE2E: false,
      fetch: async () => {
        fetched += 1;
      },
    };
    assert.equal(installE2EBridge({
      window,
      canvas: {},
      log() {},
    }), null);
    assert.equal(fetched, 0);
  });
});
