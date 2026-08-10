// Tests for the page's settings transport.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.

import assert from 'node:assert/strict';
import { before, describe, it } from 'node:test';

describe('settings transport', () => {
  let host;
  let originalFetch;

  before(async () => {
    globalThis.window = globalThis;
    globalThis.__gwnativeToken = 'browser-token';
    globalThis.__gwnativeSettings = {
      renderScale: 2,
      touchMode: 'dbltap',
      showDiagnostics: false,
      dataStrategy: 'quick',
      autoCheckUpdates: true,
      autoInstallUpdates: true,
      lastUpdateCheckAt: 123,
      compatibilityNoticeSeenFor: 'a'.repeat(64),
      nativeCursor: true,
      targetReadout: false,
    };
    originalFetch = globalThis.fetch;
    host = await import('./settings.js');
  });

  it('cannot turn an accepted write into a reported failure', async () => {
    let parsed = false;
    globalThis.fetch = async (path, init) => {
      assert.equal(path, '__settings');
      assert.equal(init.method, 'PUT');
      assert.equal(init.body, JSON.stringify({ renderScale: 1 }));
      return {
        ok: true,
        status: 204,
        json: async () => {
          parsed = true;
          throw new SyntaxError('The string did not match the expected pattern.');
        },
      };
    };

    try {
      const saved = await host.saveSettings({ renderScale: 1 });
      assert.equal(saved.renderScale, 1);
      assert.equal(saved.compatibilityNoticeSeenFor, 'a'.repeat(64));
      assert.equal(parsed, false, 'an accepted response body was inspected');
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it('still reports a host-refused write and keeps the prior value', async () => {
    globalThis.fetch = async () => ({
      ok: false,
      status: 400,
      text: async () => 'renderScale is not supported',
    });

    try {
      await assert.rejects(
        host.saveSettings({ renderScale: 9 }),
        /renderScale is not supported/,
      );
      assert.equal(host.currentSettings().renderScale, 1);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});
