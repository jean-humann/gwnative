// Commands the host pushes into the page.
//
// Everything else the host tells the page arrives as an answer to a request the
// page made. A few things do not work that way, because only AppKit knows they
// happened: the window losing key status, an input source change, a menu item.
// Those come in here.
//
// The host delivers them by evaluating a one-line dispatch into this realm —
// see `src/commands.rs`. Naming them rather than having the host evaluate the
// action itself keeps the vocabulary in one file on each side, so the host
// never has to know what an event listener in `input.js` is called.

import * as diagnostics from './diagnostics.js';

const handlers = new Map();

/**
 * @param {string} name
 * @param {(detail: unknown) => void} run
 */
export function onCommand(name, run) {
  handlers.set(name, run);
}

window.addEventListener('gw:command', (event) => {
  const { name, ...detail } = event.detail ?? {};
  const run = handlers.get(name);
  // Counted by name, so the channel can be shown to work — and so an unhandled
  // command leaves a trace rather than vanishing.
  diagnostics.count(`gw.command.${run ? name : 'unhandled'}`);
  // A host that is newer than the page will name commands this build has never
  // heard of. That is not an error worth surfacing to a player mid-session.
  if (run) run(detail);
});

// Release every held key and button.
//
// The page's own `blur` handler covers most of this, but not all of it: ⌘Tab
// away while a movement key is down and the keyup is delivered to whatever
// took focus, so the client keeps walking into a wall until the key is pressed
// and released again. AppKit sees the window resign key in every one of those
// cases, which the page does not.
onCommand('input-reset', () => {
  window.dispatchEvent(new Event('gw:input-reset'));
});
