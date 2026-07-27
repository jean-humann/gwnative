// Tests for the settings panel.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// The panel's decisions are tested rather than its markup: what a change to a
// `<select>` turns into is the part that can be wrong in a way a player would
// notice, and `installSettingsPanel` is a thin wiring layer over these three.

import assert from 'node:assert/strict';
import { before, describe, it } from 'node:test';

describe('settings panel', () => {
  let panel;

  before(async () => {
    // `diagnostics.js` starts reporting on import and would hold the runner
    // open past the last assertion. Same unref the other page tests use.
    const interval = globalThis.setInterval;
    globalThis.setInterval = (...args) => interval(...args).unref();
    globalThis.window = globalThis;
    globalThis.addEventListener ??= () => {};
    panel = await import('./settings-panel.js');
  });

  const current = {
    renderScale: 2,
    touchMode: 'off',
    showDiagnostics: false,
    dataStrategy: null,
  };

  it('writes nothing when nothing was touched', () => {
    assert.deepEqual(panel.changed(current, { ...current }), []);
  });

  it('names only the settings that moved', () => {
    const after = { ...current, renderScale: 1, showDiagnostics: true };
    assert.deepEqual(panel.changed(current, after).sort(), ['renderScale', 'showDiagnostics']);
  });

  // `null` is a value here, not an absence: it is how the launcher's question
  // gets asked again. `!==` would call it a change every time the panel opened
  // on a profile that had never answered.
  it('treats a null game-image choice as a value rather than a gap', () => {
    assert.deepEqual(panel.changed(current, { ...current, dataStrategy: null }), []);
    assert.deepEqual(panel.changed(current, { ...current, dataStrategy: 'full' }), ['dataStrategy']);
  });

  it('ignores keys the panel does not offer', () => {
    assert.deepEqual(panel.changed(current, { ...current, formatVersion: 99 }), []);
  });

  it('warns about a relaunch only for the settings that need one', () => {
    assert.equal(panel.needsRelaunch(['renderScale']), true);
    assert.equal(panel.needsRelaunch(['touchMode']), true);
    assert.equal(panel.needsRelaunch(['showDiagnostics', 'dataStrategy']), false);
    assert.equal(panel.needsRelaunch([]), false);
  });

  /** A page that records what the panel asked it to do. */
  const page = () => {
    const shown = [];
    const swept = [];
    return {
      shown,
      swept,
      showLog: (on) => shown.push(on),
      sweep: async (action) => swept.push(action),
    };
  };

  it('shows and hides the overlay from the saved value, not the requested one', async () => {
    // What the host answered with is what the page acts on: a patch it clamped
    // or refused in part would otherwise leave the overlay disagreeing with the
    // file it came from.
    const it = page();
    await panel.applyLive(['showDiagnostics'], { showDiagnostics: true }, it);
    await panel.applyLive(['showDiagnostics'], { showDiagnostics: false }, it);
    assert.deepEqual(it.shown, [true, false]);
  });

  it('leaves the overlay alone when it was not among the changes', async () => {
    const it = page();
    await panel.applyLive(['renderScale'], { showDiagnostics: true }, it);
    assert.deepEqual(it.shown, []);
  });

  // The whole point of making the game image live: before this, choosing to
  // download the rest of it did nothing until a launch the player had already
  // got past.
  it('starts and stops the download when the game image changes', async () => {
    const it = page();
    await panel.applyLive(['dataStrategy'], { dataStrategy: 'full' }, it);
    await panel.applyLive(['dataStrategy'], { dataStrategy: 'quick' }, it);
    assert.deepEqual(it.swept, ['start', 'stop']);
  });

  // `null` is "ask me again at the next launch", which is the launcher's job.
  // Treating it as an answer would either start a download nobody asked for or
  // stop one that was running.
  it('neither starts nor stops when the question is being reopened', async () => {
    const it = page();
    await panel.applyLive(['dataStrategy'], { dataStrategy: null }, it);
    assert.deepEqual(it.swept, []);
  });

  // A host that refuses the sweep for want of disk space says so in the body,
  // and that refusal has to reach the caller — the panel keeps itself open on
  // it and shows the reason.
  it('lets a refused download surface rather than swallowing it', async () => {
    await assert.rejects(
      panel.applyLive(['dataStrategy'], { dataStrategy: 'full' }, {
        showLog: () => {},
        sweep: async () => {
          throw new Error('not enough room');
        },
      }),
      /not enough room/,
    );
  });

  // Every control's choices have to be reachable from the settings the host
  // will accept, or the panel offers something that cannot be saved.
  it('offers only values the host declares patchable', () => {
    const keys = panel.CONTROLS.map((control) => control.key);
    assert.deepEqual(keys.sort(), [
      'dataStrategy',
      'renderScale',
      'showDiagnostics',
      'touchMode',
    ]);
    for (const control of panel.CONTROLS) {
      assert.ok(control.choices.length >= 2, `${control.key} has nothing to choose between`);
      const values = control.choices.map((choice) => JSON.stringify(choice.value));
      assert.equal(new Set(values).size, values.length, `${control.key} repeats a value`);
    }
  });
});
