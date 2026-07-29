import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { validateLayout } from './overlay.js';

describe('overlay layout', () => {
  it('keeps bounded positions and visibility', () => {
    assert.deepEqual(
      validateLayout({
        format: 1,
        widgets: {
          target: { x: 10.4, y: 20.7, visible: false },
        },
      }),
      {
        format: 1,
        widgets: {
          target: { x: 10, y: 21, visible: false },
        },
      },
    );
  });

  it('drops malformed identifiers and coordinates', () => {
    assert.deepEqual(
      validateLayout({
        format: 1,
        widgets: {
          '../bad': { x: 0, y: 0 },
          huge: { x: Number.MAX_VALUE, y: 0 },
          good: { x: 5, y: 6 },
        },
      }),
      {
        format: 1,
        widgets: {
          good: { x: 5, y: 6, visible: true },
        },
      },
    );
  });

  it('falls back as a whole for an unknown format', () => {
    assert.deepEqual(validateLayout({ format: 2, widgets: {} }), {
      format: 1,
      widgets: {},
    });
  });
});
