// Choose the official client build that the WebKit in this WKWebView can run.
//
// Safari, Safari Technology Preview and WKWebView do not necessarily use the
// same WebKit. In particular, installing a browser with JSPI does not add JSPI
// to an application's system WKWebView. Capability detection has to happen in
// this realm, and presence alone is not enough: exercise a suspended import and
// its promised export before selecting ArenaNet's JSPI build.

const PROBE_WASM = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
  0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
  0x02, 0x07, 0x01, 0x01, 0x65, 0x01, 0x66, 0x00, 0x00,
  0x03, 0x02, 0x01, 0x00,
  0x07, 0x05, 0x01, 0x01, 0x67, 0x00, 0x01,
  0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
]);

const CLIENTS = Object.freeze({
  jspi: Object.freeze({
    mode: 'jspi',
    glue: 'Gw.jspi.js',
    wasm: 'Gw.jspi.wasm',
  }),
  asyncify: Object.freeze({
    mode: 'asyncify',
    glue: 'Gw.js',
    wasm: 'Gw.wasm',
  }),
});

/**
 * Prove that this realm implements the JSPI operation the client needs.
 *
 * @param {typeof WebAssembly} wasm
 * @returns {Promise<boolean>}
 */
export async function supportsJspi(wasm = WebAssembly) {
  if (
    typeof wasm?.Suspending !== 'function'
    || typeof wasm?.promising !== 'function'
  ) {
    return false;
  }
  try {
    const module = new wasm.Module(PROBE_WASM);
    const imports = {
      e: {
        f: new wasm.Suspending(async () => {
          await 0;
          return 42;
        }),
      },
    };
    const instance = new wasm.Instance(module, imports);
    const result = await wasm.promising(instance.exports.g)();
    return result === 42;
  } catch {
    return false;
  }
}

/**
 * Select the matching official glue and module pair.
 *
 * `forced` is a bring-up hook injected by the native host from
 * `GWNATIVE_CLIENT_RUNTIME`. It lets a runner exercise both paths, but forcing
 * JSPI still has to pass the functional probe: an override must not turn a
 * compatibility test into an avoidable crash.
 *
 * @param {typeof WebAssembly} wasm
 * @param {unknown} forced
 * @returns {Promise<(typeof CLIENTS)[keyof typeof CLIENTS]>}
 */
export async function selectClient(
  wasm = WebAssembly,
  forced = globalThis.__gwnativeClientRuntime,
) {
  if (forced === 'asyncify') return CLIENTS.asyncify;

  const jspi = await supportsJspi(wasm);
  if (forced === 'jspi' && !jspi) {
    throw new Error(
      'JSPI was requested for this test, but this WKWebView failed its suspend/resume probe.',
    );
  }
  return jspi ? CLIENTS.jspi : CLIENTS.asyncify;
}
