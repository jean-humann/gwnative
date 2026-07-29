import assert from 'node:assert/strict';
import { beforeEach, describe, it } from 'node:test';

globalThis.window = globalThis;

const steam = await import('./steam.js');

describe('Steam login host contract', () => {
  const calls = [];

  beforeEach(() => {
    calls.length = 0;
    window.__gwnativeToken = 'host-capability';
    globalThis.fetch = async (path, options) => {
      calls.push({ path, options });
      return {
        ok: true,
        status: 200,
        json: async () => ({ token: 'bearer-secret' }),
        text: async () => '',
      };
    };
  });

  it('advertises Steam case-insensitively and no other provider', () => {
    assert.equal(steam.hasSteamProvider('Steam'), true);
    assert.equal(steam.hasSteamProvider('steam'), true);
    assert.equal(steam.hasSteamProvider('Apple'), false);
    assert.equal(steam.hasSteamProvider(null), false);
  });

  it('treats only explicit false as an interactive request', () => {
    assert.equal(steam.steamRequestIsSilent({ silent: false }), false);
    for (const options of [undefined, null, {}, { silent: true }, { silent: 0 }]) {
      assert.equal(steam.steamRequestIsSilent(options), true);
    }
  });

  it('returns exactly the shape the generated client copies into wasm', async () => {
    const lines = [];
    const answer = await steam.getSteamAuthToken('Steam', { silent: true }, {
      log: (line) => lines.push(line),
    });
    assert.deepEqual(answer, {
      userId: '1',
      authCode: 'bearer-secret',
      refreshToken: '',
    });
    assert.deepEqual(JSON.parse(calls[0].options.body), { silent: true });
    assert.equal(calls[0].options.headers['X-Gwnative-Token'], 'host-capability');
    assert.doesNotMatch(lines.join(' '), /bearer-secret/);
  });

  it('relays a valid Date as epoch milliseconds and an invalid one as null', async () => {
    await steam.storeSteamAccountData('same-token', new Date(1234));
    assert.deepEqual(JSON.parse(calls[0].options.body), {
      token: 'same-token',
      expiry: 1234,
    });
    await steam.storeSteamAccountData('same-token', new Date(Number.NaN));
    assert.equal(JSON.parse(calls[1].options.body).expiry, null);
  });

  it('rejects absence without turning it into a credential-shaped value', async () => {
    globalThis.fetch = async () => ({
      ok: false,
      status: 404,
      text: async () => 'no Steam session available',
    });
    await assert.rejects(
      steam.getSteamAuthToken('Steam', { silent: true }),
      /no Steam session/,
    );
  });

  it('clears through the dedicated host route', async () => {
    await steam.clearSteamAccountData();
    assert.equal(calls[0].path, '__steam');
    assert.equal(calls[0].options.method, 'DELETE');
  });
});
