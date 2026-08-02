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

function benchmarkSceneHarness({ district2Agents = 111 } = {}) {
  let placement = { mapId: 449, district: 3, language: 0, instanceType: 0 };
  const agents = [
    {
      agentId: 43,
      isLiving: true,
      playerNumber: 5052,
      level: 24,
      allegiance: 6,
      x: -7_412,
      y: 14_475,
    },
    {
      agentId: 44,
      isLiving: true,
      playerNumber: 5052,
      level: 24,
      allegiance: 6,
      x: -7_671,
      y: 14_159,
    },
    {
      agentId: 45,
      isLiving: true,
      playerNumber: 5052,
      level: 24,
      allegiance: 6,
      x: -4_422,
      y: 10_452,
    },
  ];
  const state = {
    status: 'ready',
    playerX: -9_167,
    playerY: 13_147,
    agents: { total: 111, agents },
  };
  const commands = [];
  const window = {
    Module: {},
    gwCompanionState: state,
    gwCompanionRuntime: {
      benchmarkSceneState: () => placement,
      async benchmarkSceneCommand(command, argument) {
        commands.push([command, argument]);
        if (command === 'travel-america') {
          placement = { mapId: 449, district: argument, language: 0, instanceType: 0 };
          state.agents.total = argument === 2 ? district2Agents : 111;
        }
        if (command === 'interact-xunlai') {
          assert.equal(argument, 44);
          if (commands.filter(([name]) => name === 'interact-xunlai').length === 2) {
            state.playerX = agents[1].x;
            state.playerY = agents[1].y;
          }
        }
      },
    },
  };
  return { commands, window };
}

describe('end-to-end helpers', () => {
  it('reports character selection only after two certified ready frames', async () => {
    const frames = [];
    const readiness = [false, true, true];
    let reports = 0;
    const milestone = createCharacterSelectionMilestone({
      afterFrame: (callback) => frames.push(callback),
      selectorReady: () => readiness.shift(),
      report: async () => {
        reports += 1;
      },
      settleFrames: 2,
    });

    assert.equal(frames.length, 0);
    milestone.authenticationCommitted();
    assert.equal(frames.length, 1);
    frames.shift()();
    assert.equal(reports, 0);
    frames.shift()();
    assert.equal(reports, 0);
    frames.shift()();
    await Promise.resolve();
    assert.equal(reports, 1);
  });

  it('cancels character selection when certified gameplay is already ready', () => {
    const frames = [];
    let reports = 0;
    const milestone = createCharacterSelectionMilestone({
      afterFrame: (callback) => frames.push(callback),
      selectorReady: () => true,
      report: () => {
        reports += 1;
      },
      settleFrames: 1,
    });
    milestone.authenticationCommitted();
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
    assert.equal(
      prepareNativeE2EAction(
        { sequence: 4, action: 'focus-window', durationMs: 0 },
        { window, canvas },
      ),
      'canvas',
    );
    assert.equal(focused, 'canvas');
    assert.throws(
      () => prepareNativeE2EAction(
        { sequence: 5, action: 'type-password', durationMs: 40 },
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

  it('normalizes graphics, district, and Xunlai position through the finite client API', async () => {
    const { commands, window } = benchmarkSceneHarness();

    const result = await executeE2EAction(
      { sequence: 9, action: 'prepare-benchmark-scene', durationMs: 0 },
      { window, canvas: {}, sleep: async () => {} },
    );

    assert.deepEqual(commands, [
      ['high-graphics', 0],
      ['travel-america', 2],
      ['interact-xunlai', 44],
      ['interact-xunlai', 44],
    ]);
    assert.deepEqual(result.benchmarkScene, {
      actionSequence: 9,
      mapId: 449,
      district: 2,
      language: 0,
      playerX: -7_671,
      playerY: 14_159,
      anchorX: -7_671,
      anchorY: 14_159,
      anchorDistance: 0,
      agentCount: 111,
      graphicsPreset: 'high',
    });
  });

  it('falls back to District 1 when District 2 is too quiet', async () => {
    const { commands, window } = benchmarkSceneHarness({ district2Agents: 24 });
    const result = await executeE2EAction(
      { sequence: 10, action: 'prepare-benchmark-scene', durationMs: 0 },
      { window, canvas: {}, sleep: async () => {} },
    );

    assert.deepEqual(commands, [
      ['high-graphics', 0],
      ['travel-america', 2],
      ['travel-america', 1],
      ['interact-xunlai', 44],
      ['interact-xunlai', 44],
    ]);
    assert.equal(result.benchmarkScene.district, 1);
    assert.equal(result.benchmarkScene.agentCount, 111);
  });

  it('runs a bounded logical-frame sample without exposing a script hook', async () => {
    const sampled = {
      runtime: 'jspi',
      durationMs: 1_001,
      frames: 121,
      framesPerSecond: 120,
      intervalMs: { samples: 120, mean: 8.333, p50: 8.333, p95: 8.5, p99: 9, max: 9 },
      callbackToSwapMs: {
        samples: 121, unsampled: 0, mean: 5, p50: 5, p95: 6, p99: 7, max: 7,
      },
      canvas: { width: 2560, height: 1364, css: { width: 1280, height: 682 } },
      webgl: {
        type: 'WebGL2RenderingContext',
        lost: false,
        drawingBufferWidth: 2560,
        drawingBufferHeight: 1364,
        attributes: { alpha: false },
      },
      audit: {
        contextLost: 0,
        contextRestored: 0,
        framesInterruptedAfterDraw: 0,
        callbacksDoingWorkDuringSuspension: 0,
        outsideWorkDuringSuspension: 0,
      },
    };
    let slept = 0;
    const window = {
      Module: {},
      gwFrameAudit: { beginPerformanceSample: () => () => sampled },
    };
    const result = await executeE2EAction(
      { sequence: 7, action: 'sample-performance', durationMs: 1_000 },
      {
        window,
        canvas: {},
        sleep: async (duration) => { slept = duration; },
      },
    );
    assert.equal(slept, 1_000);
    assert.deepEqual(result, {
      target: 'app-ui',
      activeTarget: 'canvas',
      performanceSample: {
        actionSequence: 7,
        requestedDurationMs: 1_000,
        runtime: 'jspi',
        durationMs: 1_001,
        frames: 121,
        framesPerSecond: 120,
        intervalMs: sampled.intervalMs,
        callbackToSwapMs: sampled.callbackToSwapMs,
        canvas: {
          width: 2560,
          height: 1364,
          cssWidth: 1280,
          cssHeight: 682,
        },
        webgl: {
          type: 'WebGL2RenderingContext',
          lost: false,
          drawingBufferWidth: 2560,
          drawingBufferHeight: 1364,
        },
        audit: sampled.audit,
        gpuTiming: 'not-sampled',
      },
    });
    await assert.rejects(
      executeE2EAction(
        { sequence: 8, action: 'sample-performance', durationMs: 999 },
        { window, canvas: {}, sleep: async () => {} },
      ),
      /outside its bound/,
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
