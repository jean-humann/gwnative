// Tests for the build-compatibility notice.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// The decision is what is tested — whether this launch says anything, and about
// which build. `announceCompatibility` around it is a promise and six DOM
// writes, and the part of it that can be wrong in a way a player would notice is
// `announcement`: a notice that never appears leaves a Save button silently
// doing nothing, and one that appears every launch is one nobody reads.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { announcement, templateSaveNotice } from './compatibility.js';

// A plausible hash — the client build is a SHA-256 of the wasm this launch was
// handed, and the shape matters because it is what the setting is keyed on.
const BUILD = 'a'.repeat(64);
const OTHER = 'b'.repeat(64);

describe('compatibility', () => {
  // The state the host injects is the only way the page can know, and the three
  // values are the three outcomes of `wasm::prepare`. Anything else is a host
  // and a page that have drifted, and saying nothing is the safe end of that: a
  // sentence about a missing feature that is not missing is worse than no
  // sentence at all.
  it('speaks up about build templates only when they are actually unavailable', () => {
    assert.equal(templateSaveNotice('ready'), null);
    assert.equal(templateSaveNotice(undefined), null);
    assert.equal(templateSaveNotice('something later'), null);
    assert.match(templateSaveNotice('uncertified'), /cannot be saved/);
    assert.match(templateSaveNotice('failed'), /cannot be saved/);
  });

  // Both sentences have to say what still works, because "templates cannot be
  // saved" on its own reads as "the game is broken" — which it is not.
  it('says what is unaffected in the same breath', () => {
    for (const state of ['uncertified', 'failed']) {
      assert.match(templateSaveNotice(state), /Everything else works/);
    }
  });

  it('interrupts a launch that met a client build this release does not patch', () => {
    const say = announcement({ state: 'uncertified', build: BUILD, seenFor: null });
    assert.equal(say.build, BUILD);
    assert.match(say.sentence, /cannot be saved/);
  });

  it('says nothing at all on a build it does patch', () => {
    assert.equal(announcement({ state: 'ready', build: BUILD, seenFor: null }), null);
  });

  // The whole point of keying it: acknowledged once, quiet from then on.
  it('stays quiet about a build the player has already acknowledged', () => {
    assert.equal(announcement({ state: 'uncertified', build: BUILD, seenFor: BUILD }), null);
  });

  // And the other half of the point: ArenaNet ships a new client and the app is
  // behind it again, which is news even to a player who dismissed the last one.
  it('asks again when ArenaNet ships a different client', () => {
    const say = announcement({ state: 'uncertified', build: OTHER, seenFor: BUILD });
    assert.equal(say.build, OTHER);
  });

  // A local failure to prepare has no hash to be keyed on, so a notice about it
  // could never be turned off. It is in the log and in the settings panel, which
  // is where a state belongs; interrupting every launch forever is not.
  it('does not interrupt for a preparation that failed on this Mac', () => {
    assert.equal(announcement({ state: 'failed', build: null, seenFor: null }), null);
    assert.equal(announcement({ state: 'failed', build: BUILD, seenFor: null }), null);
  });

  // Same reasoning, reached the other way: an uncertified build the host could
  // not name. Nothing to remember it by means nothing to dismiss it with.
  it('does not interrupt for a build it cannot name', () => {
    for (const build of [null, undefined, '', 42]) {
      assert.equal(announcement({ state: 'uncertified', build, seenFor: null }), null);
    }
  });
});
