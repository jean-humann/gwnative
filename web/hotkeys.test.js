import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { matchesChord, parseChord } from './hotkeys.js';

describe('companion hotkeys', () => {
  it('normalises Mac and cross-platform modifier names', () => {
    assert.deepEqual(parseChord('Command+Shift+T'), {
      key: 't',
      metaKey: true,
      ctrlKey: false,
      altKey: false,
      shiftKey: true,
    });
    assert.deepEqual(parseChord('Ctrl+Alt+Space'), {
      key: ' ',
      metaKey: false,
      ctrlKey: true,
      altKey: true,
      shiftKey: false,
    });
  });

  it('requires an exact modifier match', () => {
    const chord = parseChord('Command+T');
    assert(matchesChord({ key: 't', metaKey: true }, chord));
    assert(!matchesChord({ key: 't', metaKey: true, shiftKey: true }, chord));
    assert(!matchesChord({ key: 'r', metaKey: true }, chord));
  });

  it('refuses ambiguous or keyless chords', () => {
    assert.throws(() => parseChord('Command+Shift'), /no key/);
    assert.throws(() => parseChord('A+B'), /more than one key/);
  });
});
