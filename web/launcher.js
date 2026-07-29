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

import { snapshotProgress as poll, sweepSnapshot } from './game-data.js';

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

/** One action-row button. */
function control(label, { primary, danger } = {}) {
  const button = document.createElement('button');
  button.textContent = label;
  if (primary) button.classList.add('primary');
  if (danger) button.classList.add('danger');
  return button;
}

/** Replace the action row, returning a promise for whichever button is pressed. */
function choose(buttons) {
  const actions = el('launcher-actions');
  actions.replaceChildren();
  return new Promise((resolve) => {
    for (const [index, { label, value, primary, danger }] of buttons.entries()) {
      const button = control(label, { primary, danger });
      button.addEventListener('click', () => resolve(value));
      actions.append(button);
      // Focus the first, so the choice is reachable without the trackpad and
      // Return means the recommended answer rather than nothing.
      if (index === 0) button.focus();
    }
  });
}

/**
 * Read the cache back and prove it, drawing the pass; resolve with how many
 * chunks the host had to throw away.
 *
 * Only a Full Game launch that believes it is complete runs this, and that is
 * the point: it is the one launch that was promised the network is done with,
 * so it is the one where a filename standing in for a hash is a promise the app
 * cannot keep. A Quick Start launch verifies as it reads, which costs nothing
 * because it was going to read the chunk anyway.
 *
 * Every failure path answers 0 — the same rule the rest of this file follows.
 * A check that cannot be run is not a reason to keep a player out of the game;
 * the demand reads verify lazily regardless, and the worst case is that a bad
 * chunk is caught a few seconds later than it could have been.
 *
 * @param {(...args: unknown[]) => void} log
 * @returns {Promise<number>} chunks discarded, so the caller knows to refetch
 */
async function checkImage(log) {
  el('launcher-title').textContent = 'Checking Guild Wars';
  el('launcher-text').textContent =
    'Making sure the downloaded game is still intact. This happens once at ' +
    'each launch of a full install and takes a few seconds.';
  el('launcher-detail').textContent = '';
  el('launcher-actions').replaceChildren();
  el('launcher-rail').hidden = false;
  el('launcher').hidden = false;

  const fill = el('launcher-rail-fill');
  let state;
  try {
    state = await sweepSnapshot('verify');
  } catch (error) {
    log(`[warn] launcher: the check could not be started (${error}); playing anyway`);
    return 0;
  }

  let failures = 0;
  for (;;) {
    // `verifyTotal` counts distinct chunks and `total` counts indices, so the
    // bar has to be drawn against the former — the same number the host warned
    // is not the one beside it.
    const total = state.verifyTotal || 1;
    fill.style.width = `${((state.verified / total) * 100).toFixed(1)}%`;
    el('launcher-detail').textContent = `${state.verified} of ${state.verifyTotal} pieces checked`;
    // Read after drawing, so the finished pass gets its full bar before the
    // screen moves on.
    if (!state.verifying) return state.discarded ?? 0;
    await new Promise((resume) => setTimeout(resume, POLL_MS));
    try {
      state = await poll();
      failures = 0;
    } catch (error) {
      if (++failures >= POLL_FAILURES_TOLERATED) {
        log(`[warn] launcher: lost sight of the check (${error}); playing anyway`);
        return 0;
      }
    }
  }
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

  if (info.cached >= info.total) {
    log(`launcher: the full ${gb(snapshotBytes)} GB image is already cached`);
    // A player who never asked for the full image has nothing to check: they
    // are streaming, and streaming verifies each chunk on the read that wants
    // it. Residency here is a happy accident of having walked the whole world.
    if (strategy !== 'full') return;
    const discarded = await checkImage(log);
    if (discarded === 0) {
      done();
      return;
    }
    // The host has already unlinked what failed, which drops those chunks out
    // of residency — so the ordinary download screen below is now looking at an
    // incomplete image and refetches exactly the damage. Re-polled because the
    // `info` above still says complete, and a bar drawn from it would open at
    // 100% and go backwards.
    log(`launcher: ${discarded} damaged chunks discarded; refetching them`);
    info = await poll().catch(() => info);
  }

  // Whether the volume could take the rest of the image, headroom included. A
  // host that will not say is treated as room enough: not knowing is not the
  // same as knowing there is a problem, and the host refuses the start anyway
  // if it turns out there is.
  const room = info.free === null || info.free >= info.needed;

  let choice = strategy;
  if (choice === null) {
    const cachedBytes = Math.min(info.cached * info.chunkSize, snapshotBytes);
    el('launcher-title').textContent = 'How should Guild Wars load?';
    // Both answers are named here, and the Settings row names them the same
    // way. The prose used to describe the two without naming either, so the
    // setting that undoes this screen had nothing in common with it but the
    // subject.
    el('launcher-text').textContent =
      `The game is ${gb(snapshotBytes)} GB. Quick Start begins in seconds and ` +
      'fetches each area the first time you go there. Full Game downloads all ' +
      'of it first, which takes a while once and then never touches the network ' +
      'for game data again. Either can be changed later in Settings.';
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
            { label: 'Quick Start', value: 'quick', primary: true },
            { label: 'Full Game', value: 'full' },
          ]
        : [{ label: 'Quick Start', value: 'quick', primary: true }],
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

  // Three buttons rather than two, because "stop" was answering two questions
  // with one word. A player who wants their evening back and a player who wants
  // their bandwidth back for ten minutes both pressed it, and both got the
  // second one's answer written to disk. Pausing leaves `dataStrategy` alone,
  // so the next launch picks the sweep back up where this one left it; only
  // Quick Start rewrites the choice.
  const play = control('Play now', { primary: true });
  const pause = control('Pause');
  const quick = control('Switch to Quick Start', { danger: true });
  el('launcher-actions').replaceChildren(play, pause, quick);
  play.focus();

  const running = el('launcher-text').textContent;
  let paused = false;
  /** @type {(outcome: 'play' | 'quick') => void} */
  let settle = () => {};
  const pressed = new Promise((resolve) => {
    settle = resolve;
  });
  play.addEventListener('click', () => settle('play'));
  quick.addEventListener('click', () => settle('quick'));

  pause.addEventListener('click', async () => {
    pause.disabled = true;
    const pausing = !paused;
    // Set before stopping and cleared after starting, both so the watcher below
    // never sees `running: false` without knowing it was asked for. Either
    // ordering the other way around is a race that boots the game on a pause.
    if (pausing) paused = true;
    try {
      info = await sweepSnapshot(pausing ? 'stop' : 'start');
      if (!pausing) paused = false;
      pause.textContent = pausing ? 'Resume' : 'Pause';
      el('launcher-text').textContent = pausing
        ? 'Paused. Resume when you like, or start playing — anything not yet '
          + 'downloaded is streamed as the game asks for it.'
        : running;
      show(info);
    } catch (error) {
      if (pausing) paused = false;
      log(`[warn] launcher: the download could not be ${pausing ? 'paused' : 'resumed'} (${error})`);
    } finally {
      pause.disabled = false;
    }
  });

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
      // A pause is the one way this screen stays up with nothing moving, so it
      // is the one `running: false` that is not the sweep having given up.
      if (!info.running && !paused) {
        // The sweep ended without reaching every chunk: some fetch failed hard.
        // Streaming will pick those up on demand, so this is a note, not a stop.
        log(`[warn] launcher: the download stopped at ${info.cached}/${info.total} chunks`);
        return 'play';
      }
    }
  })();

  const outcome = await Promise.race([pressed, watched]);
  if (outcome === 'quick') {
    log('launcher: switched to Quick Start; streaming on demand from here');
    await sweepSnapshot('stop').catch((error) => log(`[warn] launcher: ${error}`));
    await remember('quick');
  } else if (paused) {
    // Play now resumes, which is what the Electron build does and the only
    // reading of three buttons that holds together: Pause is "not while I am
    // watching this bar", and the way to stop for good is beside it.
    log('launcher: playing now; the paused download picks back up in the background');
    await sweepSnapshot('start').catch((error) => log(`[warn] launcher: ${error}`));
  }
  done();
}
