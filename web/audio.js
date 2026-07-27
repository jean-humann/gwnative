// Game audio: capture the client's audio contexts, and give the host a volume
// control it did not otherwise have.
//
// The client is Emscripten OpenAL, not SDL2. Its `AudioContext` is built inside
// the glue's `_alcCreateContext` and kept on a glue-local object that nothing
// exports. `Module.AL` is not a way in either — `AL` is in the glue's
// `unexportedSymbols` list, and reading an unexported symbol off `Module` does
// not return undefined, it calls `abort()` and takes the runtime with it.
//
// So the only safe way to reach the context is to be the thing that constructs
// it. `_alcCreateContext` reads `window.AudioContext || window.webkitAudioContext`
// at call time, which means replacing those globals any time before the glue
// starts playing is enough.

import * as diagnostics from './diagnostics.js';

/**
 * Live contexts, newest last. Ones already scheduled to close stay in here
 * until they do, because until then they can still be making noise.
 *
 * @type {{context: AudioContext, master: GainNode, closing: boolean}[]}
 */
const captured = [];
let muted = false;

/**
 * A ramp rather than a step. 20 ms is short enough to read as instant and long
 * enough that the jump to silence does not click.
 */
const RAMP_S = 0.02;

/**
 * Stale contexts are closed this long after being replaced, rather than at
 * once: the client swaps devices by building the new context before tearing the
 * old one down, and a sound already scheduled on the old one should be allowed
 * to finish.
 */
const STALE_CLOSE_MS = 2000;

const applyGain = ({ context, master }) => {
  if (context.state === 'closed') return;
  // setTargetAtTime rather than linearRampToValueAtTime: it needs no end time,
  // so repeated calls from both the native and the page trigger are idempotent
  // — the second one just re-aims at a target the gain is already heading for.
  master.gain.setTargetAtTime(muted ? 0 : 1, context.currentTime, RAMP_S);
};

/**
 * Silence or restore the game.
 *
 * Ducking a gain node rather than suspending the context, deliberately. The
 * client feeds OpenAL from a 25 ms interval that schedules against
 * `currentTime`; suspending freezes that clock while the interval keeps running
 * and keeps computing start times against it. And a context suspended here can
 * only be resumed programmatically — if WebKit ever refuses that resume, the
 * game is silent until relaunch. A gain of zero has neither problem.
 *
 * @param {boolean} next
 */
export function setGameAudioMuted(next) {
  if (muted === next) return;
  muted = next;
  diagnostics.gauge('gw.audio.muted', muted ? 1 : 0);
  diagnostics.count(muted ? 'gw.audio.muted.on' : 'gw.audio.muted.off');
  for (const entry of captured) applyGain(entry);
}

/**
 * Resume anything the platform parked.
 *
 * A context may be suspended because it was born that way — WebKit starts one
 * suspended until a gesture — or interrupted, which is a WebKit-only state a
 * phone call or another app taking the audio session produces. The client only
 * ever handles the first, and only once: the glue's `autoResumeAudioContext`
 * registers its listeners `{once: true}`, so after they have fired nothing in
 * the client will ever resume a context again.
 */
export function resumeGameAudio() {
  for (const { context } of captured) {
    if (context.state !== 'suspended' && context.state !== 'interrupted') continue;
    context
      .resume()
      .then(() => diagnostics.count('gw.audio.resumed'))
      .catch(() => diagnostics.count('gw.audio.resume-failed'));
  }
}

/** What the console command reports. */
export function gameAudioState() {
  return {
    muted,
    contexts: captured.map(({ context }) => ({
      state: context.state,
      sampleRate: context.sampleRate,
      baseLatencyMs: Math.round((context.baseLatency ?? 0) * 1e4) / 10,
      outputLatencyMs: Math.round((context.outputLatency ?? 0) * 1e4) / 10,
    })),
  };
}

/**
 * Close every context but the newest.
 *
 * The glue's `_alcDestroyContext` clears the feed interval and drops its
 * bookkeeping but never calls `close()`, so each device change — which is what
 * changing the output frequency is — leaves a live context and its render
 * thread behind forever. Browsers cap concurrent contexts per page; past the
 * cap `new AudioContext` throws, the glue swallows it into ALC_INVALID_DEVICE,
 * and the client gets no audio at all with nothing reported. Closing the ones
 * the client has walked away from is the whole fix.
 */
const closeStale = () => {
  const stale = captured.slice(0, -1).filter((entry) => !entry.closing);
  if (stale.length === 0) return;
  for (const entry of stale) entry.closing = true;
  setTimeout(() => {
    for (const entry of stale) {
      const index = captured.indexOf(entry);
      if (index !== -1) captured.splice(index, 1);
      if (entry.context.state === 'closed') continue;
      entry.context
        .close()
        .then(() => diagnostics.count('gw.audio.context.closed'))
        .catch(() => diagnostics.count('gw.audio.context.close-failed'));
    }
  }, STALE_CLOSE_MS);
};

/** @param {AudioContext} context */
const capture = (context) => {
  // Read the real speaker node before it is shadowed, or `master.connect` below
  // would connect the master to itself.
  const speakers = context.destination;
  const master = context.createGain();
  master.gain.value = muted ? 0 : 1;
  master.connect(speakers);
  // `destination` is an accessor on BaseAudioContext.prototype, so an own
  // property here wins. The client reaches for it exactly once, in the
  // `gain.connect(ac.destination)` that wires up its own mixer, so everything
  // it plays arrives through the master from then on.
  Object.defineProperty(context, 'destination', {
    get: () => master,
    configurable: true,
  });

  captured.push({ context, master, closing: false });
  closeStale();

  diagnostics.count('gw.audio.context.created');
  diagnostics.gauge('gw.audio.sampleRate', context.sampleRate);
  context.addEventListener('statechange', () => {
    diagnostics.count(`gw.audio.state.${context.state}`);
    // Latency is only meaningful once the context is actually running, and the
    // client never reports it. It is the number that decides whether a sound
    // lands with its animation.
    if (context.state !== 'running') return;
    diagnostics.gauge('gw.audio.baseLatency.ms', (context.baseLatency ?? 0) * 1000);
    diagnostics.gauge('gw.audio.outputLatency.ms', (context.outputLatency ?? 0) * 1000);
  });
};

/**
 * Replace the constructors the glue will reach for.
 *
 * Subclassing rather than wrapping in a function: the glue calls these with
 * `new`, and `class extends` keeps `instanceof` and the prototype chain intact
 * for anything else that looks.
 */
export function installGameAudio() {
  for (const name of ['AudioContext', 'webkitAudioContext']) {
    const Original = window[name];
    if (typeof Original !== 'function') continue;
    window[name] = class extends Original {
      constructor(...args) {
        super(...args);
        try {
          capture(this);
        } catch (error) {
          // Never let instrumentation be the reason the game has no sound.
          diagnostics.count('gw.audio.capture-failed');
          console.log('[warn] audio capture failed:', error);
        }
      }
    };
  }

  // An output device change is the other thing a player would call "changing
  // the frequency", and neither host handled it: the client keeps feeding a
  // context bound to a device that is gone.
  navigator.mediaDevices?.addEventListener?.('devicechange', () => {
    diagnostics.count('gw.audio.devicechange');
    resumeGameAudio();
  });

  return { setGameAudioMuted, resumeGameAudio, gameAudioState };
}
