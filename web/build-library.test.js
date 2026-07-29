import assert from 'node:assert/strict';
import { beforeEach, describe, it } from 'node:test';

import { createBuildLibrary, parseLibrary } from './build-library.js';

describe('build library', () => {
  let values;
  let storage;

  beforeEach(() => {
    values = new Map();
    storage = {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
    };
  });

  it('stores single and team templates in profile-local storage', () => {
    const library = createBuildLibrary(storage);
    library.add({
      name: 'Fissure team',
      members: [
        { name: 'Tank', code: 'OQATE5Z' },
        { name: 'Support', code: 'OwUTM5Y' },
      ],
    });
    assert.equal(library.list().length, 1);
    assert.equal(library.list()[0].members.length, 2);
    assert.equal(createBuildLibrary(storage).list()[0].name, 'Fissure team');
  });

  it('searches names, roles and codes', () => {
    const library = createBuildLibrary(storage);
    library.add({
      name: 'General',
      members: [{ name: 'Mesmer', code: 'OQhkAs' }],
    });
    assert.equal(library.list('mesm').length, 1);
    assert.equal(library.list('oqhk').length, 1);
    assert.equal(library.list('monk').length, 0);
  });

  it('imports valid entries without trusting malformed neighbours', () => {
    const parsed = parseLibrary({
      format: 1,
      builds: [
        {
          id: '0123456789abcdef',
          name: 'Valid',
          members: [{ name: 'Player', code: 'OQAA' }],
          createdAt: 1,
        },
        { id: 'bad', name: '', members: [] },
      ],
    });
    assert.equal(parsed.builds.length, 1);
    assert.equal(parsed.builds[0].id, '0123456789abcdef');
  });

  it('removes by stable identifier', () => {
    const library = createBuildLibrary(storage);
    const entry = library.add({
      name: 'Delete me',
      members: [{ name: 'Player', code: 'OQAA' }],
    });
    assert(library.remove(entry.id));
    assert.equal(library.list().length, 0);
    assert(!library.remove(entry.id));
  });
});
