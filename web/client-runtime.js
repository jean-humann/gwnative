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

const RUNTIME_STATE_DEADLINE_MS = 1_500;
const PROOF_RETRY_DELAYS_MS = Object.freeze([0, 50, 150, 350]);
const JSPI_PROBE_DEADLINE_MS = 1_500;
const JSPI_PROBE_TIMED_OUT = Symbol('JSPI probe timed out');

/**
 * Prove that this realm implements the JSPI operation the client needs.
 *
 * @param {typeof WebAssembly} wasm
 * @param {number} deadlineMs
 * @returns {Promise<boolean>}
 */
export async function supportsJspi(
  wasm = WebAssembly,
  deadlineMs = JSPI_PROBE_DEADLINE_MS,
) {
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
    let deadline;
    const result = await Promise.race([
      wasm.promising(instance.exports.g)(),
      new Promise((resolve) => {
        deadline = setTimeout(() => resolve(JSPI_PROBE_TIMED_OUT), deadlineMs);
      }),
    ]).finally(() => clearTimeout(deadline));
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
  plan = { failedOfficial: [] },
) {
  const failed = new Set(plan.failedOfficial ?? []);
  if (forced === 'asyncify') {
    if (failed.has('asyncify')) {
      throw new Error('The forced Asyncify runtime already failed for these exact official bytes.');
    }
    return CLIENTS.asyncify;
  }

  const jspi = await supportsJspi(wasm);
  if (forced === 'jspi' && !jspi) {
    throw new Error(
      'JSPI was requested for this test, but this WKWebView failed its suspend/resume probe.',
    );
  }
  if (jspi && !failed.has('jspi')) return CLIENTS.jspi;
  if (!failed.has('asyncify')) return CLIENTS.asyncify;
  throw new Error('No compatible official runtime remains after the exact recorded failures.');
}

/** Read the durable launch plan before selecting any glue in this realm. */
export async function readRuntimePlan(options = {}) {
  const send = options.fetch ?? fetch;
  const token = options.token ?? globalThis.__gwnativeToken ?? '';
  const deadlineMs = options.deadlineMs ?? RUNTIME_STATE_DEADLINE_MS;
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(), deadlineMs);
  try {
    const response = await send('__runtime-plan', {
      headers: { 'X-Gwnative-Token': token },
      signal: controller.signal,
    });
    if (!response.ok) {
      throw new Error((await response.text()) || `runtime plan failed: ${response.status}`);
    }
    const plan = await response.json();
    if (
      !plan
      || Object.keys(plan).some((key) => key !== 'failedOfficial')
      || !Array.isArray(plan.failedOfficial)
      || plan.failedOfficial.some((runtime) => runtime !== 'jspi' && runtime !== 'asyncify')
    ) {
      throw new Error('the host returned an invalid runtime plan');
    }
    return { failedOfficial: [...new Set(plan.failedOfficial)] };
  } finally {
    clearTimeout(deadline);
  }
}

/**
 * Apply the independently certified facts for the selected official runtime.
 *
 * The host prepares both modules before this realm can perform the JSPI probe.
 * Keeping the facts keyed by runtime prevents a macOS 26 Asyncify selection
 * from inheriting JSPI hashes, while preserving the JSPI certificate unchanged
 * on macOS 27.
 *
 * @param {(typeof CLIENTS)[keyof typeof CLIENTS]} client
 * @param {{ nativeCursor?: unknown, targetReadout?: unknown }} settings
 * @param {Record<string, unknown>} target
 */
export function applyClientLimits(client, settings, target = globalThis) {
  const selected = target.__gwnativeRuntimeCapabilities?.[client.mode];
  const wanted = settings.nativeCursor === true || settings.targetReadout === true;
  target.__gwnativeClientBuild = selected?.build ?? null;
  target.__gwnativeTemplateSave = selected?.templateSave ?? 'uncertified';
  target.__gwnativeEnhancements = selected?.enhancements ?? (wanted ? 'uncertified' : 'off');
  target.__gwnativeEnhancementManifest = selected?.enhancementManifest ?? null;
}

/**
 * Persist launch/fallback state without allowing an auxiliary loopback write
 * to hold the client boot indefinitely.
 *
 * @param {string} path
 * @param {object} body
 * @param {{ fetch?: typeof fetch, token?: string, deadlineMs?: number }} options
 */
export async function postRuntimeState(path, body, options = {}) {
  const send = options.fetch ?? fetch;
  const token = options.token ?? globalThis.__gwnativeToken ?? '';
  const deadlineMs = options.deadlineMs ?? RUNTIME_STATE_DEADLINE_MS;
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(), deadlineMs);
  try {
    const response = await send(path, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Gwnative-Token': token,
      },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!response.ok) {
      throw new Error((await response.text()) || `${path} failed: ${response.status}`);
    }
    return response.status === 204 ? null : response.json();
  } finally {
    clearTimeout(deadline);
  }
}

/**
 * Deliver an idempotent launch proof in the background. A reply can disappear
 * after the host has committed the proof, so retries send byte-for-byte the
 * same launch identity and let the host distinguish that from stale evidence.
 *
 * @param {string} path
 * @param {object} body
 * @param {{ post?: Function, wait?: Function, delays?: number[] }} options
 */
export async function deliverRuntimeProof(path, body, options = {}) {
  const post = options.post ?? ((proofPath, proofBody) =>
    postRuntimeState(proofPath, proofBody, options));
  const wait = options.wait ?? ((milliseconds) =>
    new Promise((resolve) => setTimeout(resolve, milliseconds)));
  const delays = options.delays ?? PROOF_RETRY_DELAYS_MS;
  let failure = new Error('proof delivery had no attempts');
  for (const delay of delays) {
    if (delay) await wait(delay);
    try {
      return await post(path, body);
    } catch (error) {
      failure = error;
    }
  }
  throw failure;
}

/**
 * Persist an exact original-runtime failure, then leave the contaminated
 * WKWebView behind. The host starts the successor before this page disappears.
 */
export async function transitionRuntimeFailure(launch, options = {}) {
  const post = options.post ?? postRuntimeState;
  const relaunch = options.relaunch;
  const result = await post('__runtime-failed', { launch });
  if (result?.outcome === 'exhausted') {
    throw new Error('Both usable official runtimes are exhausted; no predecessor was removed.');
  }
  if (
    result?.outcome !== 'predecessor-restored'
    && !(result?.outcome === 'try-runtime' && result.runtime === 'asyncify')
  ) {
    throw new Error('the host returned an invalid runtime failure transition');
  }
  if (typeof relaunch !== 'function') {
    throw new Error('runtime transition has no fresh-realm relaunch');
  }
  await relaunch();
  return result;
}
