import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { validateCatalog } from './mod-runtime.js';

describe('mod runtime catalog', () => {
  const module = {
    index: 0,
    name: 'hello.wasm',
    sha256: 'ab'.repeat(32),
    size: 8,
    url: '__mods/0',
  };

  it('accepts the host-owned ordered shape', () => {
    const catalog = { format: 1, name: 'hello', modules: [module] };
    assert.equal(validateCatalog(catalog), catalog);
  });

  it('refuses URLs, hashes and ordering the page could redirect', () => {
    for (const changed of [
      { ...module, index: 1 },
      { ...module, url: 'https://example.test/mod.wasm' },
      { ...module, sha256: 'not-a-hash' },
      { ...module, size: 7 },
    ]) {
      assert.throws(
        () => validateCatalog({ format: 1, name: 'bad', modules: [changed] }),
        /invalid module metadata/,
      );
    }
  });

  it('refuses duplicate content even under a second name', () => {
    assert.throws(
      () => validateCatalog({
        format: 1,
        name: 'bad',
        modules: [module, { ...module, index: 1, name: 'copy.wasm', url: '__mods/1' }],
      }),
      /duplicate module/,
    );
  });
});
