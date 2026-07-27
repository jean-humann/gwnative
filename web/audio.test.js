// Tests for the game audio control surface.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/`.
//
// What is worth testing here is not "does a gain node exist" but the three
// things that would silently break the game if they regressed: the destination
// override has to intercept without feeding the master back into itself, mute
// has to be a ramp on a gain rather than a suspend, and a context the client has
// abandoned has to actually get closed — the leak that ends in a client with no
// audio at all once the browser's per-page cap is reached.

import assert from 'node:assert/strict';
import { before, beforeEach, describe, it, mock } from 'node:test';

/**
 * The parts of the Web Audio API the module touches, and no more.
 *
 * `destination` is defined on the prototype on purpose: that is where the real
 * one lives, and the override under test only works because an own property on
 * the instance shadows it.
 */
class FakeAudioContext extends EventTarget {
  static built = [];

  constructor() {
    super();
    this.state = 'running';
    this.sampleRate = 48000;
    this.currentTime = 0;
    this.baseLatency = 0.005;
    this.outputLatency = 0.01;
    this.speakers = { id: 'destination' };
    this.closed = false;
    this.resumed = 0;
    FakeAudioContext.built.push(this);
  }

  get destination() {
    return this.speakers;
  }

  createGain() {
    const node = {
      connectedTo: null,
      gain: {
        value: 1,
        targets: [],
        setTargetAtTime(value, when, constant) {
          node.gain.targets.push({ value, when, constant });
        },
      },
      connect(target) {
        node.connectedTo = target;
      },
    };
    return node;
  }

  async resume() {
    this.resumed += 1;
    this.state = 'running';
  }

  async close() {
    this.closed = true;
    this.state = 'closed';
  }
}

/** The one context the client is currently using. */
const current = () => FakeAudioContext.built.at(-1);

describe('game audio', () => {
  let audio;

  before(async () => {
    // The flush timer in diagnostics.js would otherwise hold the test runner
    // open for as long as the process lives.
    const interval = globalThis.setInterval;
    globalThis.setInterval = (...args) => interval(...args).unref();
    // Node's global object is not an EventTarget, and `diagnostics.js` registers
    // a `pagehide` listener the moment it is imported.
    globalThis.addEventListener ??= () => {};
    globalThis.window = globalThis;
    globalThis.navigator ??= {};
    globalThis.AudioContext = FakeAudioContext;

    audio = await import('./audio.js');
    audio.installGameAudio();
  });

  beforeEach(() => {
    FakeAudioContext.built.length = 0;
    audio.setGameAudioMuted(false);
  });

  it('routes the client through a master gain without feeding it back', () => {
    const context = new globalThis.AudioContext();
    const master = context.destination;

    assert.notEqual(master, context.speakers, 'destination should be the master gain');
    assert.equal(master.connectedTo, context.speakers, 'master should reach the speakers');
    assert.equal(master.gain.value, 1);
  });

  it('keeps instanceof intact, because the glue and the platform both look', () => {
    assert.ok(new globalThis.AudioContext() instanceof FakeAudioContext);
  });

  it('ramps to silence and back rather than stepping', () => {
    const master = new globalThis.AudioContext().destination;

    audio.setGameAudioMuted(true);
    assert.deepEqual(
      master.gain.targets.map((t) => t.value),
      [0],
    );
    assert.ok(master.gain.targets[0].constant > 0, 'a step would click');

    audio.setGameAudioMuted(false);
    assert.deepEqual(
      master.gain.targets.map((t) => t.value),
      [0, 1],
    );
  });

  it('never suspends the context, which would freeze the clock the client schedules against', () => {
    const context = new globalThis.AudioContext();
    audio.setGameAudioMuted(true);
    assert.equal(context.state, 'running');
  });

  it('ignores a repeated mute, so the host and the page can both send one', () => {
    const master = new globalThis.AudioContext().destination;

    audio.setGameAudioMuted(true);
    audio.setGameAudioMuted(true);
    assert.equal(master.gain.targets.length, 1);
  });

  it('starts a context built while muted already silent', () => {
    audio.setGameAudioMuted(true);
    const master = new globalThis.AudioContext().destination;
    assert.equal(master.gain.value, 0, 'a device change while away must not blare');
  });

  it('closes the context the client walked away from', (t) => {
    t.mock.timers.enable({ apis: ['setTimeout'] });

    const abandoned = new globalThis.AudioContext();
    const replacement = new globalThis.AudioContext();

    assert.equal(abandoned.closed, false, 'a sound already scheduled should finish');
    t.mock.timers.tick(5000);

    assert.equal(abandoned.closed, true, 'the glue never closes it, so this must');
    assert.equal(replacement.closed, false);
  });

  it('mutes every context the client is still holding', (t) => {
    t.mock.timers.enable({ apis: ['setTimeout'] });

    const first = new globalThis.AudioContext().destination;
    const second = new globalThis.AudioContext().destination;
    audio.setGameAudioMuted(true);

    // The old one is only closed after the grace period, and until then it can
    // still be making noise.
    assert.deepEqual(first.gain.targets.map((target) => target.value), [0]);
    assert.deepEqual(second.gain.targets.map((target) => target.value), [0]);
  });

  it('resumes a context the platform parked, in either of its parked states', async () => {
    const context = current() ?? new globalThis.AudioContext();
    for (const state of ['suspended', 'interrupted']) {
      context.state = state;
      audio.resumeGameAudio();
      await Promise.resolve();
      assert.equal(context.state, 'running', `${state} should be resumed`);
    }
    assert.equal(context.resumed, 2);
  });

  it('leaves a running context alone', async () => {
    const context = new globalThis.AudioContext();
    audio.resumeGameAudio();
    await Promise.resolve();
    assert.equal(context.resumed, 0);
  });

  it('reports what the host would want to see', () => {
    new globalThis.AudioContext();
    const state = audio.gameAudioState();

    assert.equal(state.muted, false);
    assert.equal(state.contexts.at(-1).sampleRate, 48000);
    assert.equal(state.contexts.at(-1).outputLatencyMs, 10);
  });

  it('survives a context it cannot instrument', () => {
    const broken = mock.method(FakeAudioContext.prototype, 'createGain', () => {
      throw new Error('no gain for you');
    });
    // The alternative is a game with no sound because the instrumentation
    // threw inside a constructor the glue called.
    assert.doesNotThrow(() => new globalThis.AudioContext());
    broken.mock.restore();
  });
});
