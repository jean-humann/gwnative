import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { buildCursorCss } from './enhancement-cursor.js';

describe('companion cursor presentation', () => {
  it('does not require a DOM until a cursor image is rendered', () => {
    assert.equal(typeof buildCursorCss, 'function');
  });
});
