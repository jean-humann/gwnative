// The page's half of the settings file.
//
// The host injects the current values at document start as
// `window.__gwnativeSettings`, so the two settings the boot path needs — the
// render scale the client is handed and the gesture translation `input.js`
// installs — are readable synchronously, before anything the page could await.
// That is the whole reason this is not simply a fetch: a boot that awaited its
// settings would either draw one frame at the wrong scale or add a round trip
// in front of every launch to avoid it.
//
// Writes go to the host, which owns the file. It answers with the merged whole
// rather than an acknowledgement, so `current()` is never a guess about what a
// patch turned into.

/** @typedef {'off' | 'dbltap' | 'translate' | 'augment'} TouchMode */
/** @typedef {'quick' | 'full' | null} DataStrategy */
/**
 * @typedef {{
 *   renderScale: number,
 *   touchMode: TouchMode,
 *   showDiagnostics: boolean,
 *   dataStrategy: DataStrategy,
 * }} Settings
 */

/**
 * What this file falls back to when the host injected nothing — a page opened
 * outside the app, or a build whose injection failed. Deliberately the same
 * values as the host's own defaults: the two disagreeing would show up as a
 * setting that appears to change nothing.
 *
 * @type {Settings}
 */
const FALLBACK = {
  renderScale: 2,
  touchMode: 'off',
  showDiagnostics: false,
  dataStrategy: null,
};

/** @type {Settings} */
let settings = { ...FALLBACK, ...(window.__gwnativeSettings ?? null) };

const headers = () => ({
  'Content-Type': 'application/json',
  'X-Gwnative-Token': window.__gwnativeToken ?? '',
});

/**
 * The values in force, without a round trip. Correct at boot by construction,
 * and correct afterwards because every write goes through `save`.
 *
 * @returns {Settings}
 */
export const currentSettings = () => ({ ...settings });

/**
 * Re-read from the host.
 *
 * Only worth calling where staleness is possible — a settings window opened
 * long after launch, where another window may have written in between. The boot
 * path does not need it.
 *
 * @returns {Promise<Settings>}
 */
export async function readSettings() {
  const response = await fetch('__settings', { headers: headers() });
  if (!response.ok) throw new Error(`settings unavailable (${response.status})`);
  settings = { ...FALLBACK, ...(await response.json()) };
  return currentSettings();
}

/**
 * Write some of the settings and take back the merged whole.
 *
 * The host refuses an unknown field rather than ignoring it, so a misspelled
 * name fails here instead of becoming a control that silently does nothing.
 *
 * @param {Partial<Settings>} patch
 * @returns {Promise<Settings>}
 */
/**
 * Ask the host to quit and come back.
 *
 * The two settings that only take effect on the boot path are saved long before
 * this is called; this is only the launch that reads them. It resolves when the
 * host has started the successor and is about to go, which is a moment before
 * this page stops existing — so there is nothing useful to do with the answer
 * beyond not treating a failure as a success.
 *
 * @returns {Promise<void>}
 */
export async function relaunchApp() {
  const response = await fetch('__relaunch', { method: 'POST', headers: headers() });
  if (!response.ok) {
    throw new Error((await response.text()) || `the app did not restart (${response.status})`);
  }
}

export async function saveSettings(patch) {
  const response = await fetch('__settings', {
    method: 'PUT',
    headers: headers(),
    body: JSON.stringify(patch),
  });
  if (!response.ok) {
    throw new Error((await response.text()) || `settings not saved (${response.status})`);
  }
  settings = { ...FALLBACK, ...(await response.json()) };
  return currentSettings();
}
