/**
 * The one decision a player makes before the client exists: stream the game
 * image on demand, or fetch all of it first.
 *
 * It has to be asked here — after the snapshot's size is known, before the glue
 * is appended — for two reasons. Once the client boots it owns the canvas and
 * the keyboard, so there is nowhere left to ask; and the answer changes what
 * playing the next hour feels like, which is not something to discover after
 * the first zone starts hitching.
 *
 * The choice is remembered in settings (`dataStrategy`), so it is asked once.
 * Until it is answered the field is `null`, which is deliberately distinct from
 * either answer: "not asked yet" is a state the launcher acts on.
 *
 * Nothing here can block a boot. Every failure path — a progress endpoint that
 * is not there, a poll that stops answering, a setting that will not save —
 * ends in the game starting anyway, because streaming on demand works whether
 * or not any of this does.
 */

const POLL_MS = 1000;
// A poll that fails is usually a transient loopback hiccup. Three in a row is
// something worse, and the response to "the download cannot be watched" is to
// let the player play, not to hold the screen on a number that stopped moving.
const POLL_FAILURES_TOLERATED = 3;

const el = (id) => document.getElementById(id);
const headers = () => ({ 'X-Gwnative-Token': window.__gwnativeToken ?? '' });
const gb = (bytes) => (bytes / 1e9).toFixed(1);

/** `{ cached, total, fetched, running, chunkSize }`, or a throw. */
async function poll() {
  const response = await fetch('__prefetch', { headers: headers() });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

/** Start or stop the background sweep. Returns the same progress shape. */
async function sweep(action) {
  const response = await fetch(action === 'stop' ? '__prefetch?stop' : '__prefetch', {
    method: 'POST',
    headers: headers(),
  });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

/** Replace the action row, returning a promise for whichever button is pressed. */
function choose(buttons) {
  const actions = el('launcher-actions');
  actions.replaceChildren();
  return new Promise((resolve) => {
    for (const [index, { label, value, primary, danger }] of buttons.entries()) {
      const button = document.createElement('button');
      button.textContent = label;
      if (primary) button.classList.add('primary');
      if (danger) button.classList.add('danger');
      button.addEventListener('click', () => resolve(value));
      actions.append(button);
      // Focus the first, so the choice is reachable without the trackpad and
      // Return means the recommended answer rather than nothing.
      if (index === 0) button.focus();
    }
  });
}

/**
 * Ask, if there is anything to ask, and resolve once the client may boot.
 *
 * @param {number} snapshotBytes total size of the game image
 * @param {{ log: (...args: unknown[]) => void, save: (patch: object) => Promise<object>,
 *           strategy: 'quick' | 'full' | null }} deps
 */
export async function resolveDataStrategy(snapshotBytes, { log, save, strategy }) {
  let info;
  try {
    info = await poll();
  } catch (error) {
    // No snapshot store, or a host that cannot report on one. Streaming is the
    // only thing on offer, so there is no choice to present.
    log(`launcher: no download progress available (${error}); streaming on demand`);
    return;
  }

  if (info.cached >= info.total) {
    log(`launcher: the full ${gb(snapshotBytes)} GB image is already cached`);
    return;
  }

  const overlay = el('launcher');
  const done = () => {
    overlay.hidden = true;
    el('launcher-rail').hidden = true;
  };

  const remember = async (value) => {
    try {
      await save({ dataStrategy: value });
    } catch (error) {
      // Worth a line but not a stop: the choice still applies to this session,
      // it just gets asked again next time.
      log(`[warn] launcher: could not remember the data choice: ${error}`);
    }
  };

  let choice = strategy;
  if (choice === null) {
    const cachedBytes = Math.min(info.cached * info.chunkSize, snapshotBytes);
    el('launcher-title').textContent = 'How should Guild Wars load?';
    el('launcher-text').textContent =
      `The game is ${gb(snapshotBytes)} GB. It can be streamed as the client asks ` +
      'for it, which starts in seconds and fetches each area the first time you ' +
      'visit it, or downloaded in full first, which takes a while once and then ' +
      'never touches the network for game data again.';
    el('launcher-detail').textContent = info.cached
      ? `${gb(cachedBytes)} GB already cached from earlier sessions.`
      : '';
    overlay.hidden = false;
    choice = await choose([
      { label: 'Play now, stream as needed', value: 'quick', primary: true },
      { label: 'Download everything first', value: 'full' },
    ]);
    log(`launcher: data strategy set to ${choice}`);
    await remember(choice);
  }

  if (choice !== 'full') {
    done();
    return;
  }

  // Downloading. The sweep is a host-side background walk at Utility QoS, so it
  // yields to whatever the client is doing — which is what makes "play now"
  // honest rather than a way of quietly abandoning the download.
  el('launcher-title').textContent = 'Downloading Guild Wars';
  el('launcher-text').textContent =
    'This runs in the background. You can start playing at any point and it ' +
    'will keep going while you do.';
  el('launcher-rail').hidden = false;
  overlay.hidden = false;

  const fill = el('launcher-rail-fill');
  const detail = el('launcher-detail');
  const show = (progress) => {
    const bytes = Math.min(progress.cached * progress.chunkSize, snapshotBytes);
    fill.style.width = `${((progress.cached / progress.total) * 100).toFixed(1)}%`;
    detail.textContent = `${gb(bytes)} of ${gb(snapshotBytes)} GB`;
  };
  show(info);

  try {
    info = await sweep('start');
    show(info);
    log(`launcher: downloading, ${info.cached}/${info.total} chunks already cached`);
  } catch (error) {
    log(`[warn] launcher: the download could not be started (${error}); streaming instead`);
    done();
    return;
  }

  const pressed = choose([
    { label: 'Play now', value: 'play', primary: true },
    { label: 'Stop downloading', value: 'stop', danger: true },
  ]);

  // Resolves when the download finishes, when it can no longer be watched, or
  // when the player stops waiting for it — whichever comes first.
  const watched = (async () => {
    let failures = 0;
    for (;;) {
      await new Promise((resume) => setTimeout(resume, POLL_MS));
      try {
        info = await poll();
        failures = 0;
      } catch (error) {
        if (++failures >= POLL_FAILURES_TOLERATED) {
          log(`[warn] launcher: lost sight of the download (${error})`);
          return 'play';
        }
        continue;
      }
      show(info);
      if (info.cached >= info.total) {
        log(`launcher: the ${gb(snapshotBytes)} GB image is fully cached`);
        return 'play';
      }
      if (!info.running) {
        // The sweep ended without reaching every chunk: some fetch failed hard.
        // Streaming will pick those up on demand, so this is a note, not a stop.
        log(`[warn] launcher: the download stopped at ${info.cached}/${info.total} chunks`);
        return 'play';
      }
    }
  })();

  if ((await Promise.race([pressed, watched])) === 'stop') {
    log('launcher: download stopped; streaming on demand from here');
    await sweep('stop').catch((error) => log(`[warn] launcher: ${error}`));
    await remember('quick');
  }
  done();
}
