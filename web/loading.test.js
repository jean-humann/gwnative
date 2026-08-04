// Failure semantics for the recovery reset.

import assert from 'node:assert/strict';
import { afterEach, describe, it } from 'node:test';

import { showFailure, wipe } from './loading.js';

const originalIndexedDB = globalThis.indexedDB;
const originalDocument = globalThis.document;
const originalWindow = globalThis.window;

afterEach(() => {
  globalThis.indexedDB = originalIndexedDB;
  globalThis.document = originalDocument;
  globalThis.window = originalWindow;
});

function failureDocument() {
  const elements = new Map(
    ['failure', 'failure-text', 'failure-note', 'failure-actions'].map((id) => [
      id,
      { id, hidden: true, textContent: '', children: [] },
    ]),
  );
  const row = elements.get('failure-actions');
  row.append = (button) => row.children.push(button);
  Object.defineProperty(row, 'textContent', {
    get: () => '',
    set: () => { row.children = []; },
  });
  globalThis.document = {
    getElementById: (id) => elements.get(id) ?? null,
    createElement: () => {
      const button = { textContent: '', className: '', run: null };
      button.addEventListener = (_event, run) => { button.run = run; };
      return button;
    },
  };
  globalThis.window = {};
  return elements;
}

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

  it('turns an initial restart rejection into an actionable message', async () => {
    const elements = failureDocument();
    showFailure('boot failed', null, async () => { throw new Error('spawn refused'); });
    const retry = elements.get('failure-actions').children.find((button) =>
      button.textContent === 'Try again');
    await retry.run();
    assert.match(elements.get('failure-note').textContent, /could not be restarted.*spawn refused/);
    assert.doesNotMatch(elements.get('failure-note').textContent, /could not be deleted/);
  });

  it('does not call a successful deletion failed when only restart rejects', async () => {
    database('success');
    const elements = failureDocument();
    const lines = [];
    showFailure('boot failed', (line) => lines.push(line), async () => {
      throw new Error('successor unavailable');
    });
    elements.get('failure-actions').children
      .find((button) => button.textContent === 'Reset game data…').run();
    await elements.get('failure-actions').children
      .find((button) => button.textContent === 'Delete and restart').run();
    assert.deepEqual(lines, ['reset 1 database(s); restarting']);
    assert.match(elements.get('failure-note').textContent, /could not be restarted/);
    assert.doesNotMatch(elements.get('failure-note').textContent, /could not be deleted/);
  });
});
