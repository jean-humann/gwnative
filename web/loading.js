// What the page offers once booting has failed.
//
// A failure used to be the end: one line of red text in the status bar and no
// way forward except quitting and relaunching, which is the same thing the
// player had already tried. Most of these failures are transient — a range
// request that lost its connection, a module fetch that raced the server
// coming up — so a fresh-process restart fixes them, and the player has no way
// to know that.
//
// The overlay's markup lives in index.html rather than being built here,
// because one of the failures this has to survive is "a module could not be
// loaded".

/** The client's persistent files. Reset means: forget these and start over. */
const MOUNT = 'app:';

const el = (id) => document.getElementById(id);

/** Replace the action row, so a second failure does not stack buttons. */
const actions = (buttons) => {
  const row = el('failure-actions');
  if (!row) return;
  row.textContent = '';
  for (const { label, danger, run } of buttons) {
    const button = document.createElement('button');
    button.textContent = label;
    if (danger) button.className = 'danger';
    button.addEventListener('click', run);
    row.append(button);
  }
};

const note = (text) => {
  const el_ = el('failure-note');
  if (el_) el_.textContent = text ?? '';
};

/**
 * Delete this origin's IndexedDB databases, which is where the client's
 * persistent files live — `IDBFS` mounted at `app:` by `filesystem.js`.
 *
 * Enumerated rather than named: emscripten derives the database name from the
 * mount point, and hardcoding it here would leave data behind the day either
 * side changes. Deletion blocks while a connection is open, so this only runs
 * from a failed boot, where the client is not holding one.
 */
export const wipe = async () => {
  const names = (await indexedDB.databases?.())?.map((db) => db.name) ?? [MOUNT];
  const targets = names.filter(Boolean);
  await Promise.all(
    targets.map(
      (name) =>
        new Promise((resolve, reject) => {
          let request;
          try {
            request = indexedDB.deleteDatabase(name);
          } catch (error) {
            reject(error);
            return;
          }
          request.onsuccess = () => resolve();
          request.onerror = () =>
            reject(request.error ?? new Error(`IndexedDB refused to delete ${name}`));
          // A live connection elsewhere would park this forever. It is not a
          // successful reset: tell the player so instead of reloading into the
          // same database and the same failure.
          request.onblocked = () =>
            reject(new Error(`${name} is still open and could not be deleted`));
        }),
    ),
  );
  return targets.length;
};

const RESET_WARNING =
  'This deletes the game data stored on this Mac — settings, skill and ' +
  'equipment templates, and chat logs. Your account is not affected, and ' +
  'downloaded game files are kept. This cannot be undone.';

/**
 * Show a terminal failure with a way out of it.
 *
 * @param {string} text What went wrong, in the player's terms.
 * @param {(...values: unknown[]) => void} log
 * @param {() => unknown} restart
 */
export function showFailure(text, log, restart = () => location.reload()) {
  const panel = el('failure');
  const message = el('failure-text');
  // No overlay in the document means an older index.html; the status line the
  // caller already set is then the whole of the report, which is what this
  // replaced and still better than throwing here.
  if (!panel || !message) return false;

  message.textContent = text;
  note('');
  panel.hidden = false;

  const retry = async () => {
    actions([]);
    note('Restarting Guild Wars…');
    try {
      await restart();
    } catch (error) {
      note(`Guild Wars could not be restarted: ${error}`);
      actions([{ label: 'Try again', run: retry }]);
    }
  };

  const offer = () =>
    actions([
      { label: 'Try again', run: retry },
      {
        label: 'Reset game data…',
        danger: true,
        run: () => {
          // Two steps rather than a confirm() dialog: a native modal over a
          // game canvas blocks the whole WebView until it is answered, and
          // this way the warning stays on screen while the player decides.
          note(RESET_WARNING);
          actions([
            { label: 'Cancel', run: offer },
            { label: 'Delete and restart', danger: true, run: reset },
          ]);
        },
      },
      { label: 'Show log', run: () => window.gwLog?.(true) },
    ]);

  const reset = async () => {
    actions([]);
    note('Deleting game data…');
    let count;
    try {
      count = await wipe();
    } catch (error) {
      note(`The game data could not be deleted: ${error}`);
      actions([{ label: 'Try again', run: retry }]);
      return;
    }
    log?.(`reset ${count} database(s); restarting`);
    await retry();
  };

  offer();
  return true;
}
