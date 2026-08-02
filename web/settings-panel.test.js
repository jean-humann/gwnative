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

  /** `dataLine` with the fields `__prefetch` always sends filled in. */
  const dataFor = (info) => panel.dataLine(info && { running: false, ...info });

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

  it('leaves Enter to the focused control instead of saving globally', () => {
    assert.equal(panel.overlayKeyAction('Enter'), null);
    assert.equal(panel.overlayKeyAction(' '), null);
    assert.equal(panel.overlayKeyAction('Escape'), 'close');
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

  it('does not turn a failed live action into a failed save', async () => {
    const saved = { ...current, dataStrategy: 'full' };
    const outcome = await panel.saveAndApply(
      ['dataStrategy'],
      { dataStrategy: 'full' },
      {
        save: async () => saved,
        showLog: () => {},
        sweep: async () => {
          throw new Error('not enough room');
        },
      },
    );
    assert.equal(outcome.saved, saved);
    assert.match(String(outcome.liveError), /not enough room/);

    await assert.rejects(
      panel.saveAndApply(['dataStrategy'], { dataStrategy: 'full' }, {
        save: async () => {
          throw new Error('disk is read-only');
        },
        showLog: () => {},
        sweep: async () => assert.fail('nothing live runs before the save succeeds'),
      }),
      /disk is read-only/,
    );
  });

  // Every control's choices have to be reachable from the settings the host
  // will accept, or the panel offers something that cannot be saved. The list
  // is `PATCHABLE` in src/settings.rs minus the ones with no sensible control —
  // there is none of those yet, so it is the whole of it.
  it('offers only values the host declares patchable', () => {
    const keys = panel.CONTROLS.map((control) => control.key);
    assert.deepEqual(keys.sort(), [
      'autoCheckUpdates',
      'autoInstallUpdates',
      'dataStrategy',
      'nativeCursor',
      'renderScale',
      'showDiagnostics',
      'targetReadout',
      'touchMode',
    ]);
    for (const control of panel.CONTROLS) {
      assert.ok(control.choices.length >= 2, `${control.key} has nothing to choose between`);
      const values = control.choices.map((choice) => JSON.stringify(choice.value));
      assert.equal(new Set(values).size, values.length, `${control.key} repeats a value`);
    }
  });

  // A build whose Cargo.toml declares no repository has nowhere to look, and
  // `menu::updates_offered` already hides the menu item on exactly this. A
  // switch the build cannot honour is a promise it cannot keep.
  it('hides the update check on a build that cannot check', () => {
    const keys = (host) => panel.offered(host).map((control) => control.key);
    assert.ok(keys({ __gwnativeUpdates: true }).includes('autoCheckUpdates'));
    assert.ok(!keys({ __gwnativeUpdates: false }).includes('autoCheckUpdates'));
    // An injection that never happened — a page opened outside the app, or a
    // host older than this file. Absent is not the same as true.
    assert.ok(!keys({}).includes('autoCheckUpdates'));
  });

  // The two are injected separately because they are two different questions.
  // A build that can only open a web page can honestly offer to look once a
  // day; it cannot offer to install anything, and a switch saying otherwise
  // would be the panel promising what the build cannot do.
  it('offers unattended installing only where something can install', () => {
    const keys = (host) => panel.offered(host).map((control) => control.key);
    const both = keys({ __gwnativeUpdates: true, __gwnativeAutoInstall: true });
    assert.ok(both.includes('autoCheckUpdates'));
    assert.ok(both.includes('autoInstallUpdates'));
    const checkOnly = keys({ __gwnativeUpdates: true, __gwnativeAutoInstall: false });
    assert.ok(checkOnly.includes('autoCheckUpdates'));
    assert.ok(!checkOnly.includes('autoInstallUpdates'));
    assert.ok(!keys({}).includes('autoInstallUpdates'));
  });

  // Everything without a `when` is unconditional, and has to stay that way:
  // hiding a control the player has already set would leave the value in force
  // with nothing to change it back.
  it('keeps every other control on every build', () => {
    const bare = panel.offered({}).map((control) => control.key);
    assert.deepEqual(bare.sort(), [
      'dataStrategy',
      'nativeCursor',
      'renderScale',
      'showDiagnostics',
      'targetReadout',
      'touchMode',
    ]);
  });

  // `changed` and `needsRelaunch` read the whole of CONTROLS on purpose. They
  // are handed keys that were actually offered and saved, so a hidden control
  // cannot reach them — and reading the filtered list would make them depend on
  // what the host injected, which is not something a diff should care about.
  it('reasons about a saved key whether or not this build offers it', () => {
    assert.deepEqual(
      panel.changed({ ...current, autoCheckUpdates: false }, { autoCheckUpdates: true }),
      ['autoCheckUpdates'],
    );
    assert.equal(panel.needsRelaunch(['autoCheckUpdates']), false);
  });

  // The figure is what a player decides on before deleting several gigabytes,
  // so it says the same thing at both ends rather than switching to a bare
  // percentage when the download finishes.
  it('says how much of the game image is on this Mac', () => {
    const chunkSize = 262_144;
    assert.match(dataFor({ cached: 0, total: 16_000, chunkSize }), /^0\.0 of 4\.2 GB/);
    assert.match(dataFor({ cached: 8000, total: 16_000, chunkSize }), /2\.1 of 4\.2 GB on this Mac \(50%\)$/);
    assert.match(dataFor({ cached: 16_000, total: 16_000, chunkSize }), /^All 4\.2 GB/);
  });

  // A running sweep is the difference between "this is all there will be" and
  // "this is going up while you read it", and only one of those is worth
  // clearing to get the disk space back.
  it('says when the download is still moving', () => {
    const info = { cached: 4000, total: 16_000, chunkSize: 262_144 };
    assert.match(dataFor({ ...info, running: true }), /· downloading$/);
    assert.doesNotMatch(dataFor({ ...info, running: false }), /downloading/);
  });

  // Both are ordinary: a launch with no snapshot store answers nothing at all,
  // and a first launch that has fetched nothing answers zeroes. Neither should
  // produce a division or a sentence with NaN in it.
  it('does not invent a figure it was not given', () => {
    assert.match(dataFor(null), /cannot be read/);
    assert.match(dataFor({ cached: 0, total: 0, chunkSize: 0 }), /No game data/);
  });

  // A control with no group, or one naming a tab that does not exist, is built
  // into a row that no tab ever shows: present in the DOM, hidden by every
  // `showTab`, and reachable by nothing. It would look exactly like the bug the
  // tabs were added to fix.
  it('puts every control on a tab that exists', () => {
    const known = new Set(panel.GROUPS.map((group) => group.id));
    for (const control of panel.CONTROLS) {
      assert.ok(known.has(control.group), `${control.key} is on no tab (${control.group})`);
    }
  });

  it('gives every tab something to show', () => {
    const used = new Set(panel.CONTROLS.map((control) => control.group));
    for (const group of panel.GROUPS) {
      assert.ok(used.has(group.id), `the ${group.label} tab has no controls`);
    }
  });

  // A `cargo run` build offers neither update control, and a tab that opens
  // onto nothing is worse than no tab — the same reasoning that hides the
  // controls themselves.
  it('drops a tab this build has nothing to put on', () => {
    const names = (host) => panel.tabs(host).map((group) => group.id);
    const full = names({ __gwnativeUpdates: true, __gwnativeAutoInstall: true });
    assert.deepEqual(full, ['general', 'tools', 'data', 'updates']);
    assert.deepEqual(names({}), ['general', 'tools', 'data']);
  });

  it('keeps a tab that kept one of its two controls', () => {
    const updates = panel.tabs({ __gwnativeUpdates: true, __gwnativeAutoInstall: false })
      .find((group) => group.id === 'updates');
    assert.deepEqual(updates.controls.map((control) => control.key), ['autoCheckUpdates']);
  });

  // The game image and the gigabytes it governs are the same subject, so the
  // storage card follows the setting onto its tab. Hard-coding `'data'` in the
  // panel and here is the point: if the setting moves, this fails.
  it('keeps the game image and the storage card on one tab', () => {
    const image = panel.CONTROLS.find((control) => control.key === 'dataStrategy');
    assert.equal(image.group, 'data');
  });

  it('does not present render scale as part of the game graphics preset', () => {
    const scale = panel.CONTROLS.find((control) => control.key === 'renderScale');
    assert.match(scale.note, /Separate from the game’s graphics preset/);
    assert.match(scale.note, /four times as many pixels/);
    assert.deepEqual(
      scale.choices.map((choice) => choice.label),
      ['1× — fastest', '1.5× — balanced', '2× — Retina sharpness'],
    );
  });

  // The sentence used to name the render scale and the gestures whatever had
  // changed, which has been wrong since the tools arrived: turning the game
  // cursor off and being told the render scale needs a restart reads as the
  // panel having saved something else.
  it('names the settings that are actually waiting for a restart', () => {
    assert.equal(
      panel.relaunchNotice(['nativeCursor']),
      'Saved. Game cursor takes effect when the app restarts.',
    );
    assert.equal(
      panel.relaunchNotice(['renderScale', 'touchMode']),
      'Saved. Render scale and Double-click take effect when the app restarts.',
    );
    assert.equal(
      panel.relaunchNotice(['renderScale', 'nativeCursor', 'targetReadout']),
      'Saved. Render scale, Game cursor and Target distance take effect when the app restarts.',
    );
  });

  // `apply` only reaches the notice when `needsRelaunch` is true, so a live-only
  // change never gets here — but the sentence must not claim a restart is
  // pending if one ever does.
  it('promises no restart for a change that already took effect', () => {
    assert.equal(panel.relaunchNotice(['showDiagnostics']), 'Saved.');
    assert.equal(panel.relaunchNotice([]), 'Saved.');
  });

  // Three of the four touch modes stop double-clicking from working, and one
  // of those also withholds every mouse click from the game. That is fine for
  // working out where a problem comes from and ruinous to land on by accident,
  // which is what happened when the labels described touch events rather than
  // consequences. Whatever the wording becomes, the working mode leads and the
  // three that break something say so.
  it('leads with the double-click mode and warns about the rest', () => {
    const touch = panel.CONTROLS.find((control) => control.key === 'touchMode');
    assert.equal(touch.choices[0].value, 'dbltap');
    assert.match(touch.choices[0].label, /^On\b/);
    for (const choice of touch.choices.slice(1)) {
      assert.match(choice.label, /not work|withheld|together/,
        `"${choice.label}" does not say what choosing it costs`);
    }
  });
});
