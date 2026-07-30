import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { FEATURES, formatDuration, setPanelVisible } from './tools-panel.js';

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
    assert.equal(FEATURES.find((feature) => feature.id === 'party').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'skillbar').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'effects').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'agents').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'quests').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'inventory').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'completion').status, 'needs-layout');
  });

  it('makes inline flex panels genuinely hide and reopen', () => {
    const attributes = new Map();
    const overlay = {
      hidden: false,
      style: { display: 'flex' },
      setAttribute: (name, value) => attributes.set(name, value),
    };

    setPanelVisible(overlay, false);
    assert.equal(overlay.hidden, true);
    assert.equal(overlay.style.display, 'none');
    assert.equal(attributes.get('aria-hidden'), 'true');

    setPanelVisible(overlay, true);
    assert.equal(overlay.hidden, false);
    assert.equal(overlay.style.display, 'flex');
    assert.equal(attributes.get('aria-hidden'), 'false');

    setPanelVisible(overlay, false);
    assert.equal(overlay.hidden, true);
    assert.equal(overlay.style.display, 'none');
  });
});
