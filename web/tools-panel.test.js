import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { FEATURES, formatDuration } from './tools-panel.js';

describe('companion tools', () => {
  it('formats timer lengths without wrapping at one hour', () => {
    assert.equal(formatDuration(0), '0:00');
    assert.equal(formatDuration(65_000), '1:05');
    assert.equal(formatDuration(3_661_000), '1:01:01');
  });

  it('classifies every feature instead of presenting unavailable work as ready', () => {
    const statuses = new Set(['available', 'needs-layout', 'research', 'blocked']);
    assert(FEATURES.length > 8);
    assert(FEATURES.every((feature) => statuses.has(feature.status)));
    assert.equal(FEATURES.find((feature) => feature.id === 'automation').status, 'blocked');
    assert.equal(FEATURES.find((feature) => feature.id === 'builds').status, 'available');
  });
});
