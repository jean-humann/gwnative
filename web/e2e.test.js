import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  buttonNamed,
  executeE2EAction,
  installE2EBridge,
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

  it('maps only bounded semantic actions onto complete key pairs', async () => {
    const events = [];
    const canvas = {
      focus() {},
      dispatchEvent(event) {
        events.push(event);
        return true;
      },
    };
    class KeyboardEvent {
      constructor(type, values) {
        this.type = type;
        Object.assign(this, values);
      }
    }
    const window = { KeyboardEvent, Module: { canvas } };
    const target = await executeE2EAction(
      { sequence: 1, action: 'activate', durationMs: 40 },
      { window, canvas, sleep: async () => {} },
    );
    assert.equal(target, 'canvas');
    assert.deepEqual(events.map((event) => [
      event.type,
      event.key,
      event.code,
      event.keyCode,
      event.which,
    ]), [
      ['keydown', 'Enter', 'Enter', 13, 13],
      ['keyup', 'Enter', 'Enter', 13, 13],
    ]);
    await assert.rejects(
      executeE2EAction(
        { sequence: 2, action: 'javascript', durationMs: 40 },
        { window, canvas, sleep: async () => {} },
      ),
      /allowed vocabulary/,
    );
    await assert.rejects(
      executeE2EAction(
        { sequence: 3, action: 'move-forward', durationMs: 5_000 },
        { window, canvas, sleep: async () => {} },
      ),
      /outside its bound/,
    );
  });

  it('uses the active text proxy for activation', async () => {
    const targets = [];
    class KeyboardEvent {
      constructor(type, values) {
        this.type = type;
        Object.assign(this, values);
      }
    }
    const makeTarget = (name) => ({
      focus() {},
      dispatchEvent() {
        targets.push(name);
        return true;
      },
    });
    const canvas = makeTarget('canvas');
    const proxy = makeTarget('text-proxy');
    const window = { KeyboardEvent, Module: { canvas, oskActiveInput: proxy } };
    assert.equal(
      await executeE2EAction(
        { sequence: 1, action: 'activate', durationMs: 40 },
        { window, canvas, sleep: async () => {} },
      ),
      'text-proxy',
    );
    assert.deepEqual(targets, ['text-proxy', 'text-proxy']);
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
