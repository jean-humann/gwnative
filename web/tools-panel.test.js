import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  FEATURES,
  formatDuration,
  formatTradeSummary,
  formatUiSummary,
  setPanelVisible,
} from './tools-panel.js';

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
    assert.equal(FEATURES.find((feature) => feature.id === 'social').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'completion').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'camera').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'trade').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'ui').status, 'available');
  });

  it('summarises the bounded UI frame inventory', () => {
    assert.equal(formatUiSummary(null), 'Unavailable for this client build');
    assert.equal(
      formatUiSummary({
        visibleTotal: 12,
        createdTotal: 18,
        total: 20,
        truncated: false,
      }),
      '12/18 locally visible · 20 frames',
    );
    assert.equal(
      formatUiSummary({
        visibleTotal: 90,
        createdTotal: 150,
        total: 180,
        truncated: true,
      }),
      '90/150 locally visible · 180 frames · first 128',
    );
  });

  it('summarises closed and bounded trade offers', () => {
    assert.equal(formatTradeSummary(null), 'Unavailable for this client build');
    assert.equal(
      formatTradeSummary({
        open: false,
        player: { items: [], itemsTruncated: false, gold: 0 },
        partner: { items: [], itemsTruncated: false, gold: 0 },
      }),
      'Closed · no active offer',
    );
    assert.equal(
      formatTradeSummary({
        open: true,
        statusName: 'OfferSent',
        player: { items: [{}, {}], itemsTruncated: false, gold: 2_222 },
        partner: { items: [{}], itemsTruncated: true, gold: 3_333 },
      }),
      'OfferSent · you 2 items + 2222g · partner 1 item + 3333g · truncated',
    );
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
