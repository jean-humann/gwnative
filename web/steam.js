// The page-side half of Steam account authentication.
//
// The bearer token crosses this realm because the official client copies it
// into wasm and redeems it in login.xml. Acquisition and persistence still
// belong to the native host: this file only preserves the exact three-string
// contract the generated client awaits.

const headers = () => ({
  'Content-Type': 'application/json',
  'X-Gwnative-Token': window.__gwnativeToken ?? '',
});

const request = (method, body) => {
  if (!window.__gwnativeToken) {
    return Promise.reject(new Error('the host did not inject a Steam session token'));
  }
  return fetch('__steam', {
    method,
    headers: headers(),
    body: body === undefined ? undefined : JSON.stringify(body),
  });
};

export const hasSteamProvider = (name) =>
  typeof name === 'string' && name.toLowerCase() === 'steam';

// Anything except an explicit `false` is the launch-time probe. Defaulting to
// silent makes a changed or malformed client contract fail back to the login
// screen instead of opening a sign-in window nobody requested.
export const steamRequestIsSilent = (options) =>
  typeof options !== 'object' || options === null || options.silent !== false;

/**
 * @param {unknown} name
 * @param {unknown} options
 * @param {{ log?: (...values: unknown[]) => void }} deps
 */
export async function getSteamAuthToken(name, options, { log = () => {} } = {}) {
  if (!hasSteamProvider(name)) throw new Error('provider not offered');
  const silent = steamRequestIsSilent(options);
  const response = await request('POST', { silent });
  if (response.status === 404) {
    log(`login.getAuthToken(silent=${silent}): no Steam session`);
    throw new Error('no Steam session available');
  }
  if (!response.ok) {
    throw new Error((await response.text()) || `Steam sign-in failed: ${response.status}`);
  }
  const { token } = await response.json();
  if (typeof token !== 'string' || !token) throw new Error('Steam sign-in returned no token');
  // Never log the bearer token. The client base64-encodes `authCode` into
  // <PasswordToken>; userId is a local profile slot, not the SteamID.
  log(`login.getAuthToken(silent=${silent}): Steam session vended`);
  return { userId: '1', authCode: token, refreshToken: '' };
}

export async function storeSteamAccountData(refreshToken, expirationDate) {
  const expiry = expirationDate instanceof Date && Number.isFinite(expirationDate.getTime())
    ? expirationDate.getTime()
    : null;
  const response = await request('PUT', {
    token: typeof refreshToken === 'string' ? refreshToken : '',
    expiry,
  });
  if (!response.ok) {
    throw new Error((await response.text()) || `Steam session update failed: ${response.status}`);
  }
}

export async function clearSteamAccountData() {
  const response = await request('DELETE');
  if (!response.ok) {
    throw new Error((await response.text()) || `Steam sign-out failed: ${response.status}`);
  }
}
