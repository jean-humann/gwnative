import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  asyncifyStateReader,
  createPassiveObserver,
} from './passive-observer.js';

describe('passive enhancement observer', () => {
  it('observes JSPI without requiring an Asyncify export', () => {
    let reads = 0;
    const observe = createPassiveObserver({}, () => { reads += 1; });
    assert.equal(observe(), true);
    assert.equal(reads, 1);
  });

  it('never enters the companion while Asyncify unwinds or rewinds', () => {
    let state = 1;
    let reads = 0;
    const observe = createPassiveObserver(
      { asyncify_get_state: () => state },
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
      { asyncify_get_state: () => state },
      () => { state = 1; },
    );
    assert.equal(observe(), false);
  });

  it('fails closed when a state getter or companion read traps', () => {
    assert.equal(
      createPassiveObserver(
        { asyncify_get_state: () => { throw new Error('state'); } },
        () => {},
      )(),
      false,
    );
    assert.equal(
      createPassiveObserver({}, () => { throw new Error('read'); })(),
      false,
    );
  });
});
