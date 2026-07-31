import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  asyncifyStateReader,
  createPassiveObserver,
} from './passive-observer.js';
import { decodeManifest } from './enhancements.js';

const FAMILY_ID = 'a'.repeat(64);

function manifest(overrides = {}) {
  return {
    snapshotAbi: 14,
    snapshotBytes: 59_776,
    cursorSnapshotAbi: 1,
    cursorSnapshotBytes: 4_160,
    configBytes: 928,
    familyId: FAMILY_ID,
    layoutWords: Array(232).fill(0),
    ...overrides,
  };
}

describe('signed companion manifest', () => {
  it('accepts the exact compiled snapshot and layout ABI', () => {
    const decoded = decodeManifest(manifest());
    assert.equal(decoded.familyId, FAMILY_ID);
    assert.equal(decoded.layoutWords.length, 232);
    assert(Object.isFrozen(decoded));
    assert(Object.isFrozen(decoded.layoutWords));
  });

  it('rejects stale or internally inconsistent layouts', () => {
    assert.equal(decodeManifest(manifest({ snapshotAbi: 13 })), null);
    assert.equal(decodeManifest(manifest({ layoutWords: Array(231).fill(0) })), null);
    assert.equal(decodeManifest(manifest({ configBytes: 924 })), null);
  });
});

describe('passive enhancement observer', () => {
  it('observes JSPI without requiring an Asyncify export', () => {
    let reads = 0;
    const observe = createPassiveObserver(null, () => { reads += 1; });
    assert.equal(observe(), true);
    assert.equal(reads, 1);
  });

  it('never enters the companion while Asyncify unwinds or rewinds', () => {
    let state = 1;
    let reads = 0;
    const observe = createPassiveObserver(
      () => state,
      () => { reads += 1; },
    );
    assert.equal(observe(), false);
    state = 2;
    assert.equal(observe(), false);
    assert.equal(reads, 0);
  });

  it('does not mistake a missing Asyncify state export for JSPI', () => {
    assert.throws(
      () => asyncifyStateReader({}, 'asyncify'),
      /does not export asyncify_get_state/,
    );
    assert.equal(asyncifyStateReader({}, 'jspi'), null);
    assert.throws(() => asyncifyStateReader({}, 'later-runtime'), /unknown client runtime/);
  });

  it('requires Asyncify to remain Normal across the read', () => {
    let state = 0;
    const observe = createPassiveObserver(
      () => state,
      () => { state = 1; },
    );
    assert.equal(observe(), false);
  });

  it('fails closed when a state getter or companion read traps', () => {
    assert.equal(
      createPassiveObserver(
        () => { throw new Error('state'); },
        () => {},
      )(),
      false,
    );
    assert.equal(
      createPassiveObserver(null, () => { throw new Error('read'); })(),
      false,
    );
  });
});
