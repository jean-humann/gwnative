// The panel behind ⌘, — the only way to reach the settings without a text
// editor.
//
// The host has owned a settings file since the render scale went in, and the
// page has read it at every boot, but nothing has ever offered to change it.
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
//
// The panel is in tabs because it outgrew the window. Eight controls, each with
// a sentence under it, is taller than the game window many people play in — and
// the old layout centred that column in the viewport, which meant the overflow
// went off the top as well as the bottom and the part off the top could not be
// scrolled to at all. The first setting was not merely hard to reach, it was
// unreachable. Tabs keep any one view short enough to fit; the dialog scrolls
// inside itself for the sizes where even that is not enough.

import {
  enhancementNotice,
  templateSaveNotice,
} from './compatibility.js';
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
 *   group: string,
 *   note?: string,
 *   live: boolean,
 *   when?: (host: Record<string, unknown>) => boolean,
 *   choices: Choice[],
 * }} Control
 * @typedef {{ id: string, label: string }} Group
 */

/**
 * The tabs, in the order they are shown.
 *
 * Four rather than one per topic. A tab per control would be a worse index than
 * the scrolling list it replaced — the point is to have few enough that a player
 * can see all of them at once and guess right the first time.
 *
 * `data` is where the game image lives, and the storage figure and the button
 * that deletes it move there with it: the setting and the gigabytes it governs
 * are the same subject, and they were two cards apart.
 *
 * @type {Group[]}
 */
export const GROUPS = [
  { id: 'general', label: 'General' },
  { id: 'tools', label: 'Tools' },
  { id: 'data', label: 'Game data' },
  { id: 'updates', label: 'Updates' },
];

/** The tab the game image, the storage figure and Clear Game Data share. */
const DATA_GROUP = 'data';

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
    group: 'general',
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
    group: 'general',
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
    group: 'general',
    label: 'Diagnostics overlay',
    note: 'The same log the Diagnostics menu item shows.',
    live: true,
    choices: [
      { value: false, label: 'Hidden' },
      { value: true, label: 'Shown' },
    ],
  },
  // Named for the launcher's two answers rather than described afresh. The
  // panel used to say "Stream on demand" and "Download in full" for the two
  // things the launcher calls Quick Start and Full Game, which left the setting
  // looking like a different subject from the question it answers — the row is
  // the only place that choice can be revisited, and it read as unrelated to
  // the one screen it undoes.
  {
    key: 'dataStrategy',
    group: 'data',
    label: 'Mode',
    note: 'Quick Start fetches each area the first time you go there. Full Game '
      + 'downloads all 4.2 GB first and then never touches the network for game '
      + 'data again.',
    live: true,
    choices: [
      { value: null, label: 'Ask at the next launch' },
      { value: 'quick', label: 'Quick Start — stream as needed' },
      { value: 'full', label: 'Full Game — download everything' },
    ],
  },
  {
    key: 'autoCheckUpdates',
    group: 'updates',
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
  {
    key: 'autoInstallUpdates',
    group: 'updates',
    label: 'Installing updates',
    note: 'An update installs the next time you quit. Nothing is downloaded '
      + 'while you are playing unless the check above is on.',
    live: true,
    // A narrower question than the one above, and a different injection.
    // Checking can be answered by either update path; installing on its own is
    // the packaged build's alone, so a `cargo run` build is offered the check
    // and not this.
    when: (host) => host.__gwnativeAutoInstall === true,
    choices: [
      { value: false, label: 'Ask me first' },
      { value: true, label: 'Install on its own' },
    ],
  },
  // The optional enhancements. Both require a relaunch for the same reason, and
  // it is not a UI limitation: turning either on changes which client module the
  // host builds and serves, and that is decided before this page exists. There
  // is deliberately no master switch above them — "are the tools on" is derived
  // from "is any tool on", so there is nothing that can disagree with the two
  // controls it would be speaking for.
  {
    key: 'nativeCursor',
    group: 'tools',
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
    group: 'tools',
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
 * The tabs this build shows, each with the controls that belong to it.
 *
 * Empty ones are dropped rather than shown disabled. A `cargo run` build offers
 * neither update control, and an Updates tab that opens onto nothing is a worse
 * answer than no tab at all — the same reasoning that hides the controls
 * themselves.
 *
 * @param {Record<string, unknown>} [host] where the host's injections landed
 * @returns {(Group & { controls: Control[] })[]}
 */
export function tabs(host = globalThis) {
  const shown = offered(host);
  return GROUPS
    .map((group) => ({ ...group, controls: shown.filter((c) => c.group === group.id) }))
    .filter((group) => group.controls.length > 0);
}

/**
 * What the footer says once a saved change is waiting for a launch.
 *
 * Built from the changed keys rather than written out. The fixed sentence named
 * the render scale and the gestures, which was true when those were the only two
 * settings that needed a launch and has been wrong since the tools arrived —
 * turning the game cursor off and being told the render scale needs a restart
 * reads as the panel having saved something else.
 *
 * @param {string[]} keys the keys that changed, as `changed` returns them
 * @returns {string}
 */
export function relaunchNotice(keys) {
  const names = CONTROLS
    .filter((control) => !control.live && keys.includes(control.key))
    .map((control) => control.label);
  if (names.length === 0) return 'Saved.';
  const list = names.length === 1
    ? names[0]
    : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
  const verb = names.length === 1 ? 'takes' : 'take';
  return `Saved. ${list} ${verb} effect when the app restarts.`;
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
 * The shortcuts owned by the overlay rather than its focused control.
 *
 * Enter deliberately has no overlay action: buttons already turn it into one
 * click and selects use it to choose an option. Treating it as a global Save
 * duplicates button actions and saves as a side effect of Cancel or Restart.
 *
 * @param {string} key
 * @returns {'close' | null}
 */
export function overlayKeyAction(key) {
  return key === 'Escape' ? 'close' : null;
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
 * Persist a patch, then attempt the independent live side effect.
 *
 * A rejected save still rejects. Once persistence succeeds, a refused live
 * action is data rather than a save failure: the caller must say that the
 * setting is on disk even though this session could not apply it.
 *
 * @param {string[]} keys
 * @param {Record<string, unknown>} patch
 * @param {{ save: (patch: object) => Promise<Record<string, unknown>>,
 *           showLog: (on: boolean) => void,
 *           sweep: (action: 'start' | 'stop') => Promise<unknown> }} page
 */
export async function saveAndApply(keys, patch, page) {
  const saved = await page.save(patch);
  try {
    await applyLive(keys, saved, page);
    return { saved, liveError: null };
  } catch (liveError) {
    return { saved, liveError };
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
  const body = document.getElementById('settings-body');
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
    const sentences = [
      templateSaveNotice(globalThis.__gwnativeTemplateSave),
      enhancementNotice(globalThis.__gwnativeEnhancements),
    ].filter((sentence) => sentence !== null);
    notice.textContent = sentences.join(' ');
    notice.hidden = sentences.length === 0;
  }

  /** The `<select>` for each control, by key. */
  const fields = new Map();
  /** The row element for each control, by key, so a tab can hide it. */
  const lines = new Map();

  // Read once. What the host injected does not change while the page is up, and
  // a panel whose rows appeared and disappeared between openings would be worse
  // than one that is simply shorter on some builds.
  const controls = offered();
  const groups = tabs();

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
    lines.set(control.key, row);
  }

  // Every row is built once and shown or hidden, rather than rebuilt per tab.
  // A `<select>` carries the player's unsaved choice, and rebuilding would throw
  // that away every time they looked at another tab — Cancel is the only thing
  // that should discard a pending change.
  const tabList = document.getElementById('settings-tabs');
  /** The tab currently shown, remembered across openings. */
  let active = groups[0]?.id ?? '';
  /** The `<button>` for each tab, by group id. */
  const chips = new Map();

  const showTab = (id) => {
    active = id;
    for (const control of controls) lines.get(control.key).hidden = control.group !== id;
    for (const [group, chip] of chips) {
      chip.setAttribute('aria-selected', String(group === id));
      // Only the selected tab is in the tab order, which is how a tablist is
      // meant to behave: Tab moves past the strip to the form, and the arrow
      // keys below move within it.
      chip.tabIndex = group === id ? 0 : -1;
    }
    syncData();
    // A tab whose rows are shorter than the last one leaves the body scrolled
    // into space that no longer exists.
    body?.scrollTo(0, 0);
  };

  if (tabList) {
    for (const group of groups) {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.textContent = group.label;
      chip.setAttribute('role', 'tab');
      chip.addEventListener('click', () => showTab(group.id));
      chips.set(group.id, chip);
      tabList.append(chip);
    }
    // One tab is not a choice, and a strip showing it would only take room from
    // a panel that is short of it.
    tabList.hidden = groups.length < 2;
    tabList.addEventListener('keydown', (event) => {
      const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
      if (step === 0) return;
      event.preventDefault();
      const at = groups.findIndex((group) => group.id === active);
      const next = groups[(at + step + groups.length) % groups.length];
      showTab(next.id);
      chips.get(next.id)?.focus();
    });
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
    showTab(active);
    overlay.hidden = false;
    // The first control of the tab being shown, not of the panel: the tab is
    // remembered between openings, and focusing a row on another one would
    // scroll a hidden part of the form into view and leave the keyboard
    // somewhere the player is not looking.
    first(active)?.focus();
    diagnostics.count('gw.settings.opened');
  };

  /** The first field on a tab, if it has one. */
  const first = (id) => {
    const control = controls.find((c) => c.group === id);
    return control && fields.get(control.key);
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
    let outcome;
    try {
      outcome = await saveAndApply(keys, patch, { save, showLog, sweep });
    } catch (error) {
      // Left open, deliberately: the player's choice is still on screen and a
      // closed panel would have thrown it away along with the explanation.
      note.textContent = `Not saved: ${error}`;
      diagnostics.count('gw.settings.save-failed');
      log(`[warn] settings: ${error}`);
      return;
    }

    diagnostics.count('gw.settings.saved');
    for (const key of keys) diagnostics.count(`gw.settings.changed.${key}`);
    if (outcome.liveError !== null) {
      note.textContent = `Saved, but could not apply now: ${outcome.liveError}`;
      diagnostics.count('gw.settings.apply-failed');
      log(`[warn] settings: saved, but the live action failed: ${outcome.liveError}`);
      if (needsRelaunch(keys)) offerRestart(keys);
      return;
    }
    if (needsRelaunch(keys)) {
      // Deliberately not closed. The change is saved and doing nothing, and
      // the panel is where the player is looking; closing it and mentioning
      // the restart in the log would be telling the one place they are not.
      offerRestart(keys);
      return;
    }
    close();
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
  // On the tab that owns the setting it belongs to, and still below the form
  // rather than inside it: everything in the grid is a preference that a Cancel
  // takes back; this deletes gigabytes and cannot be taken back, so it does not
  // share a button row with a form — and it acts the moment it is confirmed
  // rather than waiting to be saved, because there is no version of "clear the
  // cache" that should sit pending until Save is pressed.
  const section = document.getElementById('settings-data');
  const line = document.getElementById('settings-data-line');
  const dataActions = document.getElementById('settings-data-actions');
  const dataOffered = Boolean(section && line && dataActions && progress && clearData);

  /** The poll that keeps the figure moving while the panel sits open. */
  let watching = null;
  /** Whether the host has answered about the game image at all. */
  let dataKnown = true;

  /**
   * Show the card only where it belongs, and poll only while it is on screen.
   *
   * Two conditions, one place. The card is hidden both by being on another tab
   * and by there being nothing to report, and having each caller decide left
   * `refreshData` un-hiding it on top of a tab that does not own it.
   */
  function syncData() {
    if (!dataOffered) return;
    if (active !== DATA_GROUP) {
      section.hidden = true;
      stopWatching();
      return;
    }
    // Every visit to the tab is a fresh attempt, which is why `dataKnown` gates
    // the card and not the poll. A host that failed to answer once may answer
    // now, and latching the card shut for the rest of the session would report
    // a loopback hiccup as a build with no game data at all.
    section.hidden = !dataKnown;
    watchData();
  }

  const refreshData = async () => {
    let info = null;
    try {
      info = await progress();
    } catch (error) {
      // A build with no snapshot store, or a host that stopped answering. The
      // card is about data that may not exist, so it goes away rather than
      // offering to clear something nothing can report on.
      dataKnown = false;
      stopWatching();
      section.hidden = true;
      log(`settings: no game data to report on (${error})`);
      return;
    }
    dataKnown = true;
    line.textContent = dataLine(info);
    // A tab switch can land while a poll is in flight, so where the card
    // belongs is asked again rather than assumed from the host having answered.
    section.hidden = active !== DATA_GROUP;
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
  let waiting = [];
  const offerRestart = (keys = waiting) => {
    waiting = keys;
    say(relaunchNotice(keys));
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

  // Escape belongs to the panel. Return belongs to the focused native control:
  // a button activates once, and a select keeps its platform behaviour.
  overlay.addEventListener('keydown', (event) => {
    if (overlayKeyAction(event.key) === 'close') {
      event.stopPropagation();
      close();
    }
  });

  return show;
}
