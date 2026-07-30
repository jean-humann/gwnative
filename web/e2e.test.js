import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  buttonNamed,
  createCharacterSelectionMilestone,
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
  it('reports character selection only after authenticated traffic settles', async () => {
    const frames = [];
    let reports = 0;
    const milestone = createCharacterSelectionMilestone({
      afterFrame: (callback) => frames.push(callback),
      report: async () => {
        reports += 1;
      },
      settleFrames: 2,
    });

    milestone.receive();
    assert.equal(frames.length, 0);
    milestone.authenticationCommitted();
    milestone.receive();
    frames.shift()();
    milestone.receive();
    frames.shift()();
    assert.equal(reports, 0);
    frames.shift()();
    frames.shift()();
    await Promise.resolve();
    assert.equal(reports, 1);
  });

  it('cancels character selection when certified gameplay is already ready', () => {
    const frames = [];
    let reports = 0;
    const milestone = createCharacterSelectionMilestone({
      afterFrame: (callback) => frames.push(callback),
      report: () => {
        reports += 1;
      },
      settleFrames: 1,
    });
    milestone.authenticationCommitted();
    milestone.receive();
    milestone.gameReady();
    frames.shift()();
    assert.equal(reports, 0);
  });

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
    assert.equal(
      prepareNativeE2EAction(
        { sequence: 3, action: 'target-next', durationMs: 40 },
        { window, canvas },
      ),
      'canvas',
    );
    assert.throws(
      () => prepareNativeE2EAction(
        { sequence: 4, action: 'type-password', durationMs: 40 },
        { window, canvas },
      ),
      /allowed vocabulary/,
    );
  });

  it('uses an isolated password proxy for the fixed secure-input probe', () => {
    let focused = '';
    const canvas = { focus: () => { focused = 'canvas'; } };
    const secureInput = {
      value: 'must be cleared',
      focus: () => { focused = 'secure'; },
    };
    const window = { Module: { canvas } };

    assert.equal(
      prepareNativeE2EAction(
        { sequence: 1, action: 'probe-secure-input', durationMs: 40 },
        { window, canvas, secureInput },
      ),
      'password-proxy',
    );
    assert.equal(secureInput.value, '');
    assert.equal(focused, 'secure');
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

  it('runs only the bounded companion layout probe in the page', async () => {
    const result = Object.freeze({
      radiusBytes: 2048,
      contextDeltas: [-48],
      agentDeltas: [-48],
      commonDeltas: [-48],
      quest: {
        worldAvailable: true,
        activeQuestId: 0,
        questCapacity: 0,
        questCount: 0,
        questInvalidIndex: 0xffff_ffff,
        questInvalidMask: 0,
        objectiveCapacity: 0,
        objectiveCount: 0,
        questRecordsValid: true,
        activeQuestPresent: true,
        objectiveRecordsValid: true,
      },
    });
    const window = {
      Module: {},
      gwCompanionRuntime: { probeLayout: () => result },
    };
    assert.deepEqual(
      await executeE2EAction(
        { sequence: 1, action: 'probe-layout', durationMs: 0 },
        { window, canvas: {} },
      ),
      { target: 'app-ui', activeTarget: 'canvas', layoutProbe: result },
    );
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
