// Tests for what the launcher says while the game image downloads.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// The arithmetic and the lifetime of its poller are tested: a rate taken from
// too short a window misleads the player, while a poller that survives the
// overlay keeps taxing the game after the player has stopped watching it.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  launchStrategy,
  progressLine,
  rate,
  remaining,
  watchSweep,
} from './launcher.js';

/** A window of samples `mb` megabytes apart, one per second. */
const steady = (seconds, mbPerSecond, from = 0) =>
  Array.from({ length: seconds }, (_, i) => ({
    at: i * 1000,
    bytes: from + i * mbPerSecond * 1e6,
  }));

describe('download rate', () => {
  it('says nothing until the window is long enough to average a burst', () => {
    assert.equal(rate([]), null);
    assert.equal(rate([{ at: 0, bytes: 0 }]), null);
    assert.equal(rate(steady(3, 40)), null, '2s of span is below the floor');
    assert.equal(rate(steady(4, 40)), 40e6, '3s of span is not');
  });

  it('divides by the span rather than by the sample count', () => {
    // Two samples ten seconds apart, one of them missed: the rate is still
    // what the connection did, not a tenth of it.
    assert.equal(rate([{ at: 0, bytes: 0 }, { at: 10_000, bytes: 500e6 }]), 50e6);
  });

  it('treats a stall as no rate rather than as zero', () => {
    const stalled = [{ at: 0, bytes: 900e6 }, { at: 12_000, bytes: 900e6 }];
    assert.equal(rate(stalled), null);
    // And the estimate built on it disappears with it, instead of dividing by
    // zero into an eternity.
    assert.equal(remaining(3.3e9, rate(stalled)), null);
  });
});

describe('time remaining', () => {
  it('rounds to the units someone would wait in', () => {
    assert.equal(remaining(20e6, 50e6), 'less than a minute left');
    assert.equal(remaining(3e9, 50e6), 'about a minute left');
    assert.equal(remaining(30e9, 50e6), 'about 10 minutes left');
    assert.equal(remaining(180e9, 50e6), 'about an hour left');
    assert.equal(remaining(360e9, 50e6), 'about 2 hours left');
  });

  it('has nothing to say about a download that is already done', () => {
    assert.equal(remaining(0, 50e6), null);
    assert.equal(remaining(-1e6, 50e6), null);
  });
});

describe('the line under the bar', () => {
  it('shortens to what it can stand behind', () => {
    assert.equal(
      progressLine(0, 4.2e9, [{ at: 0, bytes: 0 }]),
      '0.0 of 4.2 GB',
      'no window yet: no rate and no estimate',
    );
    assert.equal(
      progressLine(400e6, 4.2e9, steady(11, 40)),
      '0.4 of 4.2 GB · 40 MB/s · about 2 minutes left',
    );
  });

  it('keeps the total in view once the rest is measured in seconds', () => {
    assert.equal(
      progressLine(4.19e9, 4.2e9, steady(11, 40, 4.15e9)),
      '4.2 of 4.2 GB · 40 MB/s · less than a minute left',
    );
  });
});

describe('background sweep watcher', () => {
  it('stops before another poll when Play now cancels it', async () => {
    const controller = new AbortController();
    let polls = 0;
    const watched = watchSweep({
      poll: async () => {
        polls += 1;
        return { cached: 0, total: 1, chunkSize: 1, running: true };
      },
      show: () => assert.fail('a cancelled watcher must not redraw'),
      log: () => {},
      paused: () => false,
      signal: controller.signal,
      totalBytes: 1,
    });

    controller.abort();
    assert.equal(await watched, null);
    assert.equal(polls, 0);
  });

  it('still resolves when the visible download completes', async () => {
    const controller = new AbortController();
    const shown = [];
    const outcome = await watchSweep({
      poll: async () => ({ cached: 2, total: 2, chunkSize: 10, running: false }),
      show: (progress) => shown.push(progress.cached),
      log: () => {},
      paused: () => false,
      signal: controller.signal,
      totalBytes: 20,
      wait: async () => true,
    });
    assert.equal(outcome, 'play');
    assert.deepEqual(shown, [2]);
  });
});

describe('launch strategy', () => {
  it('uses Quick Start only for an unattended unconfigured profile', () => {
    assert.equal(launchStrategy(null, true), 'quick');
    assert.equal(launchStrategy(null, false), null);
    assert.equal(launchStrategy('full', true), 'full');
    assert.equal(launchStrategy('quick', true), 'quick');
  });
});
