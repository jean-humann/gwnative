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

// A rate is averaged over several seconds rather than taken from the last poll.
// Chunks land in bursts of sixteen parallel fetches finishing at once, so a
// one-second difference swings between 0 and 200 MB/s while the actual download
// is steady — and a number that flickers like that is one nobody can read.
const RATE_WINDOW_MS = 12_000;
// Below this the window is too short for the burst pattern to average out, so
// there is no honest rate to show yet.
const RATE_FLOOR_MS = 3_000;

const el = (id) => document.getElementById(id);
const headers = () => ({ 'X-Gwnative-Token': window.__gwnativeToken ?? '' });
const gb = (bytes) => (bytes / 1e9).toFixed(1);

/**
 * Bytes per second across a window of samples, or null when the samples cannot
 * support a figure — too short a span, or nothing moved in it.
 *
 * A stall answers null rather than 0. Zero is a number, and a number invites
 * dividing by it; "no rate" is the truth and it leaves the line alone.
 *
 * @param {{ at: number, bytes: number }[]} samples oldest first
 */
export function rate(samples) {
  const first = samples[0];
  const last = samples[samples.length - 1];
  if (!first || !last) return null;
  const span = last.at - first.at;
  if (span < RATE_FLOOR_MS) return null;
  const moved = last.bytes - first.bytes;
  return moved > 0 ? (moved / span) * 1000 : null;
}

/**
 * How long the rest will take, in the words a person would use.
 *
 * Rounded hard on purpose: an estimate from a rate that swings is accurate to
 * a couple of minutes at best, and "about 12 minutes left" claims exactly that
 * much where "11:47" would claim to the second and be wrong every second.
 */
export function remaining(bytesLeft, bytesPerSecond) {
  if (!bytesPerSecond || bytesLeft <= 0) return null;
  const minutes = Math.round(bytesLeft / bytesPerSecond / 60);
  if (minutes < 1) return 'less than a minute left';
  if (minutes === 1) return 'about a minute left';
  if (minutes < 60) return `about ${minutes} minutes left`;
  const hours = Math.round(minutes / 60);
  return hours === 1 ? 'about an hour left' : `about ${hours} hours left`;
}

/**
 * The line under the bar: how far along, how fast, how much longer.
 *
 * Everything after the first clause is dropped when it cannot be said honestly,
 * so the line shortens at the start of a download and at a stall rather than
 * showing a placeholder where a figure should be.
 */
export function progressLine(bytes, totalBytes, samples) {
  const line = [`${gb(bytes)} of ${gb(totalBytes)} GB`];
  const speed = rate(samples);
  if (speed) line.push(`${(speed / 1e6).toFixed(0)} MB/s`);
  const left = remaining(totalBytes - bytes, speed);
  if (left) line.push(left);
  return line.join(' · ');
}

/** `{ cached, total, fetched, running, chunkSize }`, or a throw. */
async function poll() {
  const response = await fetch('__prefetch', { headers: headers() });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

/**
 * Start or stop the background sweep. Returns the same progress shape.
 *
 * Exported because the settings panel drives the same sweep after boot: the
 * launcher asks the question once, and changing the answer later has to reach
 * the same route with the same token rather than a second copy of both.
 *
 * @param {'start' | 'stop'} action
 */
export async function sweepSnapshot(action) {
  const response = await fetch(action === 'stop' ? '__prefetch?stop' : '__prefetch', {
    method: 'POST',
    headers: headers(),
  });
  if (!response.ok) {
    // The host explains a refusal in the body — "not enough room" is the one
    // worth repeating to the player rather than reporting as a status code.
    const detail = await response.json().catch(() => null);
    throw new Error(detail?.error ?? `HTTP ${response.status}`);
  }
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

  // Whether the volume could take the rest of the image, headroom included. A
  // host that will not say is treated as room enough: not knowing is not the
  // same as knowing there is a problem, and the host refuses the start anyway
  // if it turns out there is.
  const room = info.free === null || info.free >= info.needed;

  let choice = strategy;
  if (choice === null) {
    const cachedBytes = Math.min(info.cached * info.chunkSize, snapshotBytes);
    el('launcher-title').textContent = 'How should Guild Wars load?';
    el('launcher-text').textContent =
      `The game is ${gb(snapshotBytes)} GB. It can be streamed as the client asks ` +
      'for it, which starts in seconds and fetches each area the first time you ' +
      'visit it, or downloaded in full first, which takes a while once and then ' +
      'never touches the network for game data again.';
    const cached = info.cached ? `${gb(cachedBytes)} GB already cached from earlier sessions.` : '';
    // Offering a button the host is going to refuse would be worse than not
    // offering it, so when there is no room the option is replaced by the
    // reason there is none.
    el('launcher-detail').textContent = room
      ? cached
      : `${cached} Downloading it all would need ${gb(info.outstanding)} GB more, ` +
        `and this disk has ${gb(info.free)} GB free.`;
    overlay.hidden = false;
    choice = await choose(
      room
        ? [
            { label: 'Play now, stream as needed', value: 'quick', primary: true },
            { label: 'Download everything first', value: 'full' },
          ]
        : [{ label: 'Play now, stream as needed', value: 'quick', primary: true }],
    );
    log(`launcher: data strategy set to ${choice}`);
    await remember(choice);
  }

  if (choice !== 'full') {
    done();
    return;
  }

  // Downloading. Nothing below this point boots the client until the player
  // presses Play now or the sweep finishes, so for as long as this bar is the
  // only thing on screen the host runs the sweep as the interactive path. The
  // moment a first frame lands it drops to Utility and yields to the client —
  // which is what makes "play now" honest rather than a way of quietly
  // abandoning the download. See `ChunkStore::fetch_class`.
  el('launcher-title').textContent = 'Downloading Guild Wars';
  el('launcher-text').textContent =
    'This runs in the background. You can start playing at any point and it ' +
    'will keep going while you do.';
  el('launcher-rail').hidden = false;
  overlay.hidden = false;

  const fill = el('launcher-rail-fill');
  const detail = el('launcher-detail');
  // Trimmed to the window on every sample, so the rate is always over the last
  // few seconds of this download rather than over all of it: a player who
  // walks into another room and comes back wants to know what the connection
  // is doing now, not what it averaged while they were away.
  const samples = [];
  const show = (progress, at = performance.now()) => {
    const bytes = Math.min(progress.cached * progress.chunkSize, snapshotBytes);
    samples.push({ at, bytes });
    while (samples.length > 1 && at - samples[0].at > RATE_WINDOW_MS) samples.shift();
    fill.style.width = `${((progress.cached / progress.total) * 100).toFixed(1)}%`;
    detail.textContent = progressLine(bytes, snapshotBytes, samples);
  };
  show(info);

  try {
    info = await sweepSnapshot('start');
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
    await sweepSnapshot('stop').catch((error) => log(`[warn] launcher: ${error}`));
    await remember('quick');
  }
  done();
}
