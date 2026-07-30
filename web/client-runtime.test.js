import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { selectClient, supportsJspi } from './client-runtime.js';

const workingJspi = {
  Module: class {},
  Instance: class {
    constructor(_module, imports) {
      this.exports = { g: () => imports.e.f.operation() };
    }
  },
  Suspending: class {
    constructor(operation) {
      this.operation = operation;
    }
  },
  promising: (operation) => async () => operation(),
};

describe('client runtime selection', () => {
  it('uses Asyncify when the WKWebView has no JSPI', async () => {
    assert.equal(await supportsJspi({}), false);
    assert.deepEqual(
      await selectClient({}),
      { mode: 'asyncify', glue: 'Gw.js', wasm: 'Gw.wasm' },
    );
  });

  it('uses JSPI only after a functional suspend/resume round trip', async () => {
    assert.equal(await supportsJspi(workingJspi), true);
    assert.deepEqual(
      await selectClient(workingJspi),
      { mode: 'jspi', glue: 'Gw.jspi.js', wasm: 'Gw.jspi.wasm' },
    );
  });

  it('falls back when JSPI is present but does not work', async () => {
    const broken = { ...workingJspi, promising: () => async () => 41 };
    assert.equal(await supportsJspi(broken), false);
    assert.equal((await selectClient(broken)).mode, 'asyncify');
  });

  it('can force Asyncify for runner coverage', async () => {
    assert.equal((await selectClient({}, 'asyncify')).mode, 'asyncify');
  });

  it('refuses to force JSPI in an incompatible WKWebView', async () => {
    await assert.rejects(
      selectClient({}, 'jspi'),
      /failed its suspend\/resume probe/,
    );
  });
});
