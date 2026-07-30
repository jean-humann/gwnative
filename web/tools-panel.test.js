import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  FEATURES,
  formatDuration,
  formatMerchantSummary,
  formatProgressionSummary,
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
    assert.equal(FEATURES.find((feature) => feature.id === 'merchant').status, 'available');
    assert.equal(FEATURES.find((feature) => feature.id === 'progression').status, 'available');
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

  it('summarises the bounded merchant item-ID page', () => {
    assert.equal(formatMerchantSummary(null), 'Unavailable for this client build');
    assert.equal(
      formatMerchantSummary({ truncated: false, total: 0, itemIds: [] }),
      'No merchant item IDs published',
    );
    assert.equal(
      formatMerchantSummary({ truncated: false, total: 2, itemIds: [900, 901] }),
      '2/2 merchant item IDs',
    );
    assert.equal(
      formatMerchantSummary({
        truncated: true,
        total: 200,
        itemIds: Array(128).fill(900),
      }),
      '128/200 merchant item IDs · first 128',
    );
  });

  it('summarises character progression without inventing names or ranks', () => {
    assert.equal(formatProgressionSummary(null), 'Unavailable for this client build');
    assert.equal(
      formatProgressionSummary({
        hardModeUnlocked: true,
        level: 20,
        experience: 1_337_500,
        factions: {
          kurzick: { current: 1_000, maximum: 10_000 },
          luxon: { current: 2_000, maximum: 10_000 },
          imperial: { current: 100, maximum: 15_000 },
          balthazar: { current: 500, maximum: 10_000 },
        },
        skillPoints: { current: 5, totalEarned: 125 },
      }),
      'Level 20 · 1337500 XP · 5/125 skill points · HM unlocked · '
        + 'factions K 1000/10000, L 2000/10000, I 100/15000, B 500/10000',
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
