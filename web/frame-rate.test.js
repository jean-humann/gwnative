import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { createFrameCadence } from './frame-rate.js';

const accepted = (displayHz, requestedHz, seconds = 10) => {
  const cadence = createFrameCadence(1000 / requestedHz);
  let count = 0;
  for (let frame = 0; frame < displayHz * seconds; frame += 1) {
    if (cadence.accept((frame * 1000) / displayHz)) count += 1;
  }
  return count;
};

describe('frame-rate cadence', () => {
  it('keeps the requested average on non-divisible display rates', () => {
    assert.ok(Math.abs(accepted(75, 60) - 600) <= 1);
    assert.ok(Math.abs(accepted(120, 50) - 500) <= 1);
  });

  it('keeps every frame when the ceiling exceeds the display', () => {
    assert.equal(accepted(60, 120), 600);
  });

  it('does not let a long stall create a burst of catch-up frames', () => {
    const cadence = createFrameCadence(1000 / 60);
    assert.equal(cadence.accept(0), true);
    assert.equal(cadence.accept(1_000), true);
    assert.equal(cadence.accept(1_001), false);
  });

  it('is transparent when no ceiling is requested', () => {
    const cadence = createFrameCadence(0);
    assert.equal(cadence.accept(0), true);
    assert.equal(cadence.accept(0), true);
  });

  it('keeps independent animation loops from consuming each other’s slots', () => {
    const game = createFrameCadence(1000 / 60);
    const widgets = createFrameCadence(1000 / 60);
    for (const timestamp of [0, 8.33, 16.67, 25, 33.33]) {
      assert.equal(
        game.accept(timestamp),
        widgets.accept(timestamp),
        `animation loops diverged at ${timestamp} ms`,
      );
    }
  });
});
