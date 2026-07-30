// Tests for the build-compatibility notice.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// The decision is what is tested — whether this launch records anything, and
// about which build. The durable player-facing notice is rendered in Settings;
// this path must remain free of DOM or boot-flow dependencies.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  announceCompatibility,
  announcement,
  enhancementNotice,
  templateSaveNotice,
} from './compatibility.js';

// A plausible hash — the client build is a domain-separated SHA-256 of the
// runtime/artifacts/transform/output combination, and the shape matters because
// it is what the setting is keyed on.
const BUILD = 'a'.repeat(64);
const OTHER = 'b'.repeat(64);

describe('compatibility', () => {
  // The state the host injects is the only way the page can know. These are the
  // outcomes of `wasm::prepare`. Anything else is a host and a page that have
  // drifted, and saying nothing is the safe end of that: a
  // sentence about a missing feature that is not missing is worse than no
  // sentence at all.
  it('speaks up about build templates only when they are actually unavailable', () => {
    assert.equal(templateSaveNotice('ready'), null);
    assert.equal(templateSaveNotice(undefined), null);
    assert.equal(templateSaveNotice('something later'), null);
    assert.equal(templateSaveNotice('asyncify'), null);
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

  it('separates a pending live layout review from template certification', () => {
    assert.match(enhancementNotice('uncertified'), /read-only layout/);
    assert.match(enhancementNotice('uncertified'), /build templates still work/);
    assert.match(enhancementNotice('failed'), /Diagnostics/);
    assert.equal(enhancementNotice('ready'), null);
    assert.equal(enhancementNotice('off'), null);
  });

  it('records a client build this release does not patch', () => {
    const say = announcement({ state: 'uncertified', build: BUILD, seenFor: null });
    assert.equal(say.build, BUILD);
    assert.match(say.sentence, /cannot be saved/);
  });

  it('records once when enabled tools await both live runtime checks', () => {
    const say = announcement({
      state: 'ready',
      enhancements: 'uncertified',
      build: BUILD,
      seenFor: null,
    });
    assert.equal(say.build, BUILD);
    assert.match(say.sentence, /read-only layout/);
    assert.equal(
      announcement({
        state: 'ready',
        enhancements: 'uncertified',
        build: BUILD,
        seenFor: BUILD,
      }),
      null,
    );
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
  // is where a state belongs; recording it every launch forever is not.
  it('does not create a build record for a preparation that failed on this Mac', () => {
    assert.equal(announcement({ state: 'failed', build: null, seenFor: null }), null);
    assert.equal(announcement({ state: 'failed', build: BUILD, seenFor: null }), null);
  });

  // Same reasoning, reached the other way: an uncertified build the host could
  // not name. Nothing to remember it by means nothing to dismiss it with.
  it('does not create a record for a build it cannot name', () => {
    for (const build of [null, undefined, '', 42]) {
      assert.equal(announcement({ state: 'uncertified', build, seenFor: null }), null);
    }
  });

  it('logs and persists without depending on launcher DOM', async () => {
    const logged = [];
    const saved = [];
    await announceCompatibility({
      state: 'uncertified',
      enhancements: 'off',
      build: BUILD,
      seenFor: null,
      log: (...parts) => logged.push(parts.join(' ')),
      save: async (patch) => {
        saved.push(patch);
        return patch;
      },
    });
    assert.equal(logged.length, 1);
    assert.match(logged[0], /optional features disabled/);
    assert.deepEqual(saved, [{ compatibilityNoticeSeenFor: BUILD }]);
  });
});
