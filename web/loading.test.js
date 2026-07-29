// Failure semantics for the recovery reset.

import assert from 'node:assert/strict';
import { afterEach, describe, it } from 'node:test';

import { wipe } from './loading.js';

const originalIndexedDB = globalThis.indexedDB;

afterEach(() => {
  globalThis.indexedDB = originalIndexedDB;
});

/** Install a one-database IndexedDB mock whose delete emits `outcome`. */
function database(outcome) {
  globalThis.indexedDB = {
    databases: async () => [{ name: 'app:' }],
    deleteDatabase: () => {
      const request = { error: null };
      queueMicrotask(() => {
        if (outcome === 'success') request.onsuccess();
        if (outcome === 'error') {
          request.error = new Error('storage refused deletion');
          request.onerror();
        }
        if (outcome === 'blocked') request.onblocked();
      });
      return request;
    },
  };
}

describe('failed-boot data reset', () => {
  it('reports a deletion error instead of reloading unchanged data', async () => {
    database('error');
    await assert.rejects(wipe(), /storage refused deletion/);
  });

  it('reports a blocked database instead of calling it deleted', async () => {
    database('blocked');
    await assert.rejects(wipe(), /still open/);
  });

  it('counts only databases the browser confirmed deleted', async () => {
    database('success');
    assert.equal(await wipe(), 1);
  });
});
