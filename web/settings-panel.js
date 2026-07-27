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
// which is not a change worth making inside a settings panel. Saying "next
// launch" is honest, and a relaunch is one ⌘Q away.

import * as diagnostics from './diagnostics.js';

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
  {
    key: 'touchMode',
    label: 'Trackpad gestures',
    note: 'The client was built for a mouse. This decides what a trackpad is turned into.',
    live: false,
    choices: [
      { value: 'off', label: 'Off' },
      { value: 'dbltap', label: 'Double-tap to drag' },
      { value: 'translate', label: 'Instead of the mouse' },
      { value: 'augment', label: 'Alongside the mouse' },
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
];

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
 *   log: (...args: unknown[]) => void,
 * }} deps
 * @returns {() => void} opens the panel
 */
export function installSettingsPanel({ read, save, showLog, sweep, log }) {
  const overlay = document.getElementById('settings');
  const rows = document.getElementById('settings-rows');
  const note = document.getElementById('settings-note');
  const actions = document.getElementById('settings-actions');
  if (!overlay || !rows || !note || !actions) {
    log('[warn] settings: the panel is not in this page');
    return () => {};
  }

  /** The `<select>` for each control, by key. */
  const fields = new Map();

  // The value round-trips through an index rather than through the option's
  // value attribute, which is a string: `false` and `null` would both come back
  // as text and the host would refuse the patch by type.
  const chosen = (control) => control.choices[Number(fields.get(control.key).value)]?.value;

  for (const control of CONTROLS) {
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

  const show = () => {
    const settings = read();
    for (const control of CONTROLS) {
      const index = control.choices.findIndex((choice) => Object.is(choice.value, settings[control.key]));
      // A value the panel has no choice for is a file written by a later build,
      // or by hand. Showing the first choice would silently offer to overwrite
      // it, so nothing is selected and only a deliberate change writes.
      fields.get(control.key).selectedIndex = index;
    }
    note.textContent = '';
    overlay.hidden = false;
    fields.get(CONTROLS[0].key).focus();
    diagnostics.count('gw.settings.opened');
  };

  const close = () => {
    overlay.hidden = true;
    // The client stops hearing keys the moment something else takes focus, and
    // the panel took it. Giving it back is what makes the game playable again
    // rather than merely visible.
    document.getElementById('canvas')?.focus();
  };

  const apply = async () => {
    const before = read();
    const after = Object.fromEntries(CONTROLS.map((c) => [c.key, chosen(c)]).filter(([, v]) => v !== undefined));
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
        log('settings: saved; the render scale and gestures apply at the next launch');
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

  const button = (label, run, primary) => {
    const element = document.createElement('button');
    element.textContent = label;
    if (primary) element.classList.add('primary');
    element.addEventListener('click', run);
    return element;
  };
  actions.append(button('Cancel', close), button('Save', apply, true));

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
