// Tests for the target readout.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// Only `targetReadout` is exercised: it is the whole of what the readout
// decides, and `createTargetReadout` is a few spans and a string comparison
// around it. That split is the reason the decision is a separate export.
//
// The sibling cursor module has no test here on purpose — every path in it
// runs through a canvas, and a fake one would be testing the fake.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { targetReadout } from './enhancement-readout.js';

/** A decoded snapshot with a target, as the decoder hands one over. */
const ready = Object.freeze({
  status: 'ready',
  targetValid: true,
  distance: 312.4,
  rangeName: 'Earshot',
});

describe('target readout', () => {
  it('shows the distance the game already has, rounded to a unit', () => {
    assert.deepEqual({ ...targetReadout(ready) }, { distance: '312', range: 'Earshot' });
    // Rounded rather than truncated, and no unit invented: the game counts in
    // whole units and a fractional one on screen would be a precision the
    // number does not have.
    assert.equal(targetReadout({ ...ready, distance: 311.5 }).distance, '312');
    assert.equal(targetReadout({ ...ready, distance: 0 }).distance, '0');
  });

  // Every waiting state the decoder can be in, plus a ready one with nothing
  // targeted. The readout is not a status line — it either has a number to
  // show or it is not there.
  it('says nothing about a state the decoder did not certify', () => {
    for (const state of [
      undefined,
      null,
      {},
      { status: 'waiting', reason: 'writing' },
      { status: 'waiting', reason: 'loading', tickCount: 12 },
      { status: 'ready', targetValid: false },
      // Absent is not false: a shape from some other version of the decoder
      // must not read as a target.
      { status: 'ready' },
    ]) {
      assert.equal(targetReadout(state), null, JSON.stringify(state));
    }
  });

  // The decoder guarantees these for a ready target, so reaching them means
  // something upstream changed. Showing `NaN` over somebody's game is a worse
  // way to find that out than showing nothing.
  it('shows nothing rather than a number it cannot render', () => {
    for (const broken of [
      { distance: Number.NaN },
      { distance: Number.POSITIVE_INFINITY },
      { distance: '300' },
      { rangeName: undefined },
      { rangeName: 4 },
    ]) {
      assert.equal(targetReadout({ ...ready, ...broken }), null, JSON.stringify(broken));
    }
  });

  it('hands back a frozen line', () => {
    assert.ok(Object.isFrozen(targetReadout(ready)));
  });
});
