// The panel behind ⌘, — the only way to reach the settings without a text
// editor.
//
// The host has owned a settings file since the render scale went in, and the
// page has read it at every boot, but nothing has ever offered to change it.
// The Electron build has had a settings window all along; this is the same four
// settings with the same effects.
//
// Two of them cannot take effect until the next launch, and the panel says so
// rather than pretending. The render scale is handed to the client through an
// import it reads when it recomputes the canvas, and the gesture translation is
// a set of listeners installed once at boot around a mode that was captured by
// value. Both are fixable — and both are a change to the boot path to fix,
// which is not a change worth making inside a settings panel.
//
// So the panel offers the launch instead. Saying "next launch" and leaving the
// player to arrange one was honest but not much use: the setting they just
// asked for is on disk and inert, and the only thing standing between them and
// it is a quit they have to think of themselves.

import { templateSaveNotice } from './compatibility.js';
import * as diagnostics from './diagnostics.js';

/** How often the game-image figure is re-read while the panel is open. */
const DATA_POLL_MS = 2000;

/**
 * @typedef {{
 *   value: unknown,
 *   label: string,
 * }} Choice
 * @typedef {{
 *   key: string,
 *   label: string,
 *   note?: string,
 *   live: boolean,
 *   when?: (host: Record<string, unknown>) => boolean,
 *   choices: Choice[],
 * }} Control
 */

/**
 * What the panel offers, in the order it offers it.
 *
 * Exported because it is the whole specification: the tests read it rather than
 * a rendered panel, and `live` is what decides whether the footer warns about a
 * relaunch. A control added here appears with no other change.
 *
 * @type {Control[]}
 */
export const CONTROLS = [
  {
    key: 'renderScale',
    label: 'Render scale',
    note: 'Lower is faster. 2× is one game pixel per display pixel on a Retina screen.',
    live: false,
    choices: [
      { value: 1, label: '1×' },
      { value: 1.5, label: '1.5×' },
      { value: 2, label: '2×' },
    ],
  },
  // Every label here used to describe the *mechanism* — "Double-tap to drag",
  // "Instead of the mouse" — and a player choosing between them was choosing
  // between four descriptions of touch events, none of which mentions the thing
  // that changes: whether double clicking works. It reads as a trackpad
  // preference, so picking the one that sounds most like "I use a trackpad"
  // silently turns the mouse off. Each option now says what it costs.
  {
    key: 'touchMode',
    label: 'Double-click',
    note: 'The game has no double-click of its own; it is built from taps. '
      + 'Leave this on unless you are working out where a problem comes from.',
    live: false,
    choices: [
      { value: 'dbltap', label: 'On (recommended)' },
      { value: 'off', label: 'Off — double-clicking will not work' },
      { value: 'translate', label: 'Touch only — mouse clicks are withheld' },
      { value: 'augment', label: 'Touch and mouse together' },
    ],
  },
  {
    key: 'showDiagnostics',
    label: 'Diagnostics overlay',
    note: 'The same log the Diagnostics menu item shows.',
    live: true,
    choices: [
      { value: false, label: 'Hidden' },
      { value: true, label: 'Shown' },
    ],
  },
  {
    key: 'dataStrategy',
    label: 'Game image',
    note: 'Streaming fetches each piece as the game asks for it. A full download is 4.2 GB.',
    live: true,
    choices: [
      { value: null, label: 'Ask at the next launch' },
      { value: 'quick', label: 'Stream on demand' },
      { value: 'full', label: 'Download in full' },
    ],
  },
  {
    key: 'autoCheckUpdates',
    label: 'Update check',
    note: 'Asks once a day, at launch, and stays quiet unless there is a newer release.',
    live: true,
    // Not offered on a build with nowhere to look. The Help menu hides "Check
    // for Updates…" on the same question — see `menu::updates_offered` — and a
    // switch for a check that cannot happen is worse than no switch, because it
    // is a promise the build cannot keep.
    when: (host) => host.__gwnativeUpdates === true,
    choices: [
      { value: false, label: 'Only when I ask' },
      { value: true, label: 'Once a day, at launch' },
    ],
  },
  // The GWonMac Tools. Both are relaunch-required for the same reason and it
  // is not a UI limitation: turning either on changes which client module the
  // host builds and serves, and that is decided before this page exists. There
  // is deliberately no master switch above them — "are the tools on" is
  // derived from "is any tool on", so there is nothing that can disagree with
  // the two controls it would be speaking for.
  {
    key: 'nativeCursor',
    label: 'Game cursor',
    note: 'Draws the game\'s own cursor as the pointer, so it moves with the '
      + 'mouse instead of with the frame rate.',
    live: false,
    choices: [
      { value: true, label: 'Follow the mouse (recommended)' },
      { value: false, label: 'Leave it to the game' },
    ],
  },
  {
    key: 'targetReadout',
    label: 'Target distance',
    note: 'Shows how far away your target is, and which range band that is, '
      + 'above the game.',
    live: false,
    choices: [
      { value: false, label: 'Hidden' },
      { value: true, label: 'Shown' },
    ],
  },
];

/**
 * The controls this build can honestly offer.
 *
 * Filtering here rather than at each use keeps `CONTROLS` the specification: a
 * control that is hidden is still the same declaration, still tested, and still
 * the thing `changed` and `needsRelaunch` reason about. Those two deliberately
 * keep reading the whole list — they are given keys that were actually saved,
 * and a key that could not be shown was never among them.
 *
 * @param {Record<string, unknown>} [host] where the host's injections landed
 * @returns {Control[]}
 */
export function offered(host = globalThis) {
  return CONTROLS.filter((control) => !control.when || control.when(host));
}

/**
 * How much of the game image is on this Mac, in a sentence.
 *
 * The figure is chunks × chunk size rather than a byte count, so the last
 * partial chunk rounds it up by at most a quarter of a megabyte in four
 * gigabytes — below the first decimal place it is shown to.
 *
 * @param {Record<string, number | boolean | null> | null} progress from `__prefetch`
 * @returns {string}
 */
export function dataLine(progress) {
  if (!progress) return 'The game data cannot be read right now.';
  const cached = Number(progress.cached ?? 0);
  const total = Number(progress.total ?? 0);
  const chunk = Number(progress.chunkSize ?? 0);
  if (!total || !chunk) return 'No game data is stored on this Mac yet.';
  const gb = (chunks) => ((chunks * chunk) / 1e9).toFixed(1);
  if (cached >= total) return `All ${gb(total)} GB of the game image is on this Mac.`;
  const share = Math.floor((cached / total) * 100);
  const line = `${gb(cached)} of ${gb(total)} GB on this Mac (${share}%)`;
  return progress.running ? `${line} · downloading` : line;
}

/**
 * The keys whose values differ.
 *
 * A patch is built from this rather than from the whole form, so a panel that
 * was opened and closed writes nothing — and the host, which persists on every
 * accepted patch, is not asked to rewrite a file that has not changed.
 *
 * @param {Record<string, unknown>} before
 * @param {Record<string, unknown>} after
 * @returns {string[]}
 */
export function changed(before, after) {
  return CONTROLS.map((control) => control.key).filter(
    (key) => key in after && !Object.is(before[key], after[key]),
  );
}

/**
 * Whether any of `keys` only takes effect at the next launch.
 *
 * @param {string[]} keys
 * @returns {boolean}
 */
export function needsRelaunch(keys) {
  return keys.some((key) => CONTROLS.some((control) => control.key === key && !control.live));
}

/**
 * Do the part of a saved change that can be done now.
 *
 * Separated from the save because the two fail independently: a page that
 * showed the overlay and could not write the file has still done what was asked
 * for this session.
 *
 * The game image is the interesting one. The launcher asks the question before
 * the client exists and then never again, which left "download the rest of it"
 * as something a player could only decide at a launch they had already got
 * past. Changing it here starts or stops the same host-side sweep the launcher
 * drives, so the setting is a switch rather than a note for next time. `null`
 * is neither: it is the request to be asked again, and asking is the launcher's
 * job at the next boot.
 *
 * @param {string[]} keys
 * @param {Record<string, unknown>} settings
 * @param {{ showLog: (on: boolean) => void,
 *           sweep: (action: 'start' | 'stop') => Promise<unknown> }} page
 */
export async function applyLive(keys, settings, page) {
  if (keys.includes('showDiagnostics')) page.showLog(Boolean(settings.showDiagnostics));
  if (keys.includes('dataStrategy')) {
    if (settings.dataStrategy === 'full') await page.sweep('start');
    if (settings.dataStrategy === 'quick') await page.sweep('stop');
  }
}

/**
 * Wire the panel to the document and publish the opener.
 *
 * @param {{
 *   read: () => Record<string, unknown>,
 *   save: (patch: object) => Promise<Record<string, unknown>>,
 *   showLog: (on: boolean) => void,
 *   sweep: (action: 'start' | 'stop') => Promise<unknown>,
 *   progress: () => Promise<Record<string, unknown>>,
 *   clearData: () => Promise<void>,
 *   relaunch: () => Promise<void>,
 *   log: (...args: unknown[]) => void,
 * }} deps
 * @returns {() => void} opens the panel
 */
export function installSettingsPanel({
  read, save, showLog, sweep, progress, clearData, relaunch, log,
}) {
  const overlay = document.getElementById('settings');
  const rows = document.getElementById('settings-rows');
  const note = document.getElementById('settings-note');
  const actions = document.getElementById('settings-actions');
  // Not required: a page without it loses the restart offer and keeps every
  // other thing the panel does, which is the right way round for a line of
  // explanatory text.
  const pending = document.getElementById('settings-pending');
  if (!overlay || !rows || !note || !actions) {
    log('[warn] settings: the panel is not in this page');
    return () => {};
  }

  const notice = document.getElementById('settings-compat');
  if (notice) {
    const sentence = templateSaveNotice(globalThis.__gwnativeTemplateSave);
    notice.textContent = sentence ?? '';
    notice.hidden = sentence === null;
  }

  /** The `<select>` for each control, by key. */
  const fields = new Map();

  // Read once. What the host injected does not change while the page is up, and
  // a panel whose rows appeared and disappeared between openings would be worse
  // than one that is simply shorter on some builds.
  const controls = offered();

  // The value round-trips through an index rather than through the option's
  // value attribute, which is a string: `false` and `null` would both come back
  // as text and the host would refuse the patch by type.
  const chosen = (control) => control.choices[Number(fields.get(control.key).value)]?.value;

  for (const control of controls) {
    const row = document.createElement('div');
    row.className = 'settings-row';

    const label = document.createElement('label');
    label.textContent = control.label;
    label.htmlFor = `settings-${control.key}`;

    const select = document.createElement('select');
    select.id = `settings-${control.key}`;
    for (const [index, choice] of control.choices.entries()) {
      const option = document.createElement('option');
      option.value = String(index);
      option.textContent = choice.label;
      select.append(option);
    }
    fields.set(control.key, select);

    const hint = document.createElement('div');
    hint.className = 'settings-hint';
    hint.textContent = control.note ?? '';

    row.append(label, select, hint);
    rows.append(row);
  }

  const say = (sentence) => {
    if (!pending) return;
    pending.textContent = sentence;
    pending.hidden = sentence === '';
  };

  const show = () => {
    const settings = read();
    for (const control of controls) {
      const index = control.choices.findIndex((choice) => Object.is(choice.value, settings[control.key]));
      // A value the panel has no choice for is a file written by a later build,
      // or by hand. Showing the first choice would silently offer to overwrite
      // it, so nothing is selected and only a deliberate change writes.
      fields.get(control.key).selectedIndex = index;
    }
    note.textContent = '';
    say('');
    offerSaving();
    offerClearing();
    watchData();
    overlay.hidden = false;
    fields.get(controls[0].key).focus();
    diagnostics.count('gw.settings.opened');
  };

  const close = () => {
    stopWatching();
    overlay.hidden = true;
    // The client stops hearing keys the moment something else takes focus, and
    // the panel took it. Giving it back is what makes the game playable again
    // rather than merely visible.
    document.getElementById('canvas')?.focus();
  };

  const apply = async () => {
    const before = read();
    const after = Object.fromEntries(controls.map((c) => [c.key, chosen(c)]).filter(([, v]) => v !== undefined));
    const keys = changed(before, after);
    if (keys.length === 0) {
      close();
      return;
    }
    const patch = Object.fromEntries(keys.map((key) => [key, after[key]]));
    try {
      const saved = await save(patch);
      // Awaited inside the same try: a sweep the host refuses for want of disk
      // space says so in the body, and that refusal is the one the player most
      // needs to see rather than a silent no-op.
      await applyLive(keys, saved, { showLog, sweep });
      diagnostics.count('gw.settings.saved');
      for (const key of keys) diagnostics.count(`gw.settings.changed.${key}`);
      if (needsRelaunch(keys)) {
        // Deliberately not closed. The change is saved and doing nothing, and
        // the panel is where the player is looking; closing it and mentioning
        // the restart in the log would be telling the one place they are not.
        offerRestart();
        return;
      }
      close();
    } catch (error) {
      // Left open, deliberately: the player's choice is still on screen and a
      // closed panel would have thrown it away along with the explanation.
      note.textContent = `Not saved: ${error}`;
      diagnostics.count('gw.settings.save-failed');
      log(`[warn] settings: ${error}`);
    }
  };

  const button = (label, run, primary, danger) => {
    const element = document.createElement('button');
    element.textContent = label;
    if (primary) element.classList.add('primary');
    if (danger) element.classList.add('danger');
    element.addEventListener('click', run);
    return element;
  };

  const offerSaving = () => {
    actions.replaceChildren(button('Cancel', close), button('Save', apply, true));
  };

  // The game image, as a figure and a way to be rid of it.
  //
  // Deliberately below the Save row and not part of it. Everything above is a
  // preference that a Cancel takes back; this deletes gigabytes and cannot be
  // taken back, so it does not share a button row with a form — and it acts the
  // moment it is confirmed rather than waiting to be saved, because there is no
  // version of "clear the cache" that should sit pending until Save is pressed.
  const section = document.getElementById('settings-data');
  const line = document.getElementById('settings-data-line');
  const dataActions = document.getElementById('settings-data-actions');
  const dataOffered = Boolean(section && line && dataActions && progress && clearData);

  /** The poll that keeps the figure moving while the panel sits open. */
  let watching = null;

  const refreshData = async () => {
    let info = null;
    try {
      info = await progress();
    } catch (error) {
      // A build with no snapshot store, or a host that stopped answering. The
      // section is about data that may not exist, so it goes away rather than
      // offering to clear something nothing can report on.
      stopWatching();
      section.hidden = true;
      log(`settings: no game data to report on (${error})`);
      return;
    }
    line.textContent = dataLine(info);
    section.hidden = false;
  };

  const watchData = () => {
    if (!dataOffered || watching) return;
    void refreshData();
    // A download moves by tens of megabytes a second, and a figure that only
    // updated on open would be wrong within a second of being read.
    watching = setInterval(refreshData, DATA_POLL_MS);
  };

  function stopWatching() {
    if (watching !== null) clearInterval(watching);
    watching = null;
  }

  const offerClearing = () => {
    if (!dataOffered) return;
    dataActions.replaceChildren(button('Clear Game Data…', confirmClearing));
  };

  // The poll is stopped for the length of the question, because the next tick
  // would overwrite the sentence being answered with a progress figure.
  const confirmClearing = () => {
    stopWatching();
    line.textContent =
      'The downloaded game data will be deleted when the app restarts, and fetched ' +
      'again as the game asks for it. Characters, settings and storage are held by ' +
      'ArenaNet and are not touched.';
    dataActions.replaceChildren(
      button('Keep It', () => {
        offerClearing();
        watchData();
      }),
      button('Clear and Restart', clearAndRestart, false, true),
    );
  };

  const clearAndRestart = async () => {
    dataActions.replaceChildren();
    line.textContent = 'Clearing…';
    try {
      await clearData();
    } catch (error) {
      line.textContent = `Not cleared: ${error}`;
      diagnostics.count('gw.settings.game-data-clear-failed');
      log(`[warn] settings: ${error}`);
      offerClearing();
      return;
    }
    diagnostics.count('gw.settings.game-data-cleared');
    line.textContent = 'The game data will be cleared when the app restarts.';
    try {
      await relaunch();
    } catch (error) {
      // The request is recorded either way, so this is a slower path to the
      // same place rather than a failure to say sorry for.
      note.textContent = `Not restarted: ${error}`;
      say('The game data will be cleared the next time the app starts.');
      diagnostics.count('gw.settings.relaunch-failed');
      log(`[warn] settings: ${error}`);
    }
  };

  /**
   * What is offered once a saved setting is waiting for a launch to read it.
   *
   * "Later" is not "Cancel": there is nothing left to cancel, the file is
   * written either way, and the only question still open is when it starts
   * counting.
   */
  const offerRestart = () => {
    say('Saved. The render scale and gestures take effect when the app restarts.');
    actions.replaceChildren(button('Later', close), button('Restart Now', restart, true));
  };

  const restart = async () => {
    actions.replaceChildren();
    say('Restarting…');
    try {
      await relaunch();
      // The host answers before it goes, so this page has a moment left and
      // nothing to do in it. Leaving "Restarting…" up is the honest frame to
      // spend it on.
      diagnostics.count('gw.settings.relaunched');
    } catch (error) {
      say('');
      note.textContent = `Not restarted: ${error}`;
      diagnostics.count('gw.settings.relaunch-failed');
      log(`[warn] settings: ${error}`);
      // Back to the offer rather than to Save: the setting is still saved and
      // still waiting, which is exactly the state this was reached from.
      offerRestart();
    }
  };

  offerSaving();

  // Escape closes and Return saves, which is what every other panel on the
  // system does. Scoped to the overlay so neither reaches the client.
  overlay.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.stopPropagation();
      close();
    }
    if (event.key === 'Enter') {
      event.stopPropagation();
      void apply();
    }
  });

  return show;
}
