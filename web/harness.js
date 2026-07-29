// Host for Gw.jspi.wasm inside WKWebView.
//
// Module MUST be `var`: the generated glue does
// `var Module = typeof Module != 'undefined' ? Module : {}`, and a const/let
// here collides with it at parse time.
var Module;

(function () {
'use strict';

const LOG_LINES = 400;
const logBuf = [];

// WKWebView has no stdout, so log lines are batched back to the host to land in
// the terminal alongside its own. Batched because a chatty boot would otherwise
// be one request per line.
const pending = [];
let flushTimer = 0;
const forward = (line) => {
  pending.push(line);
  if (flushTimer) return;
  flushTimer = setTimeout(() => {
    flushTimer = 0;
    const body = pending.join('\n');
    pending.length = 0;
    fetch('__report', {
      method: 'POST',
      headers: { 'X-Gwnative-Token': window.__gwnativeToken ?? '' },
      body,
    }).catch(() => {});
  }, 50);
};

// The glue and the client log through console directly; mirror those, or the
// host terminal sees only the harness's own half of the story. Installed here,
// above the first caller, so `log` below can just use console and not forward a
// second copy of its own.
for (const level of ['log', 'warn', 'error']) {
  const original = console[level].bind(console);
  console[level] = (...values) => {
    original(...values);
    forward(level === 'log' ? values.map(String).join(' ')
                            : `[${level}] ${values.map(String).join(' ')}`);
  };
}
window.addEventListener('error', (e) => forward(`[uncaught] ${e.message} @ ${e.filename}:${e.lineno}`));
window.addEventListener('unhandledrejection', (e) => forward(`[unhandled] ${e.reason}`));

// Keep the main loop turning when the window is not on screen.
//
// WKWebView stops delivering requestAnimationFrame entirely while its window is
// fully occluded — not throttled, stopped. The client drives everything from
// that callback, so a covered window does not merely stop painting: networking,
// timers and the session all stop with it, and the server eventually drops the
// connection. It applies before the first paint, so launching the app behind
// another window would wedge it just after `runtime initialised` — and it
// applies mid-boot, so a first launch left to download while the player walks
// away froze the moment the screen locked. Measured: a blank-install boot
// stalled at 2,093 of 6,075 chunks, host idle in accept, the moment user input
// stopped; it never resumed.
//
// So until the first frame has been presented, arm a timer alongside every
// frame request and let whichever arrives first win. While the window is
// visible the real frame wins every race and the client sees stock timing;
// while it is covered, the timer keeps the loop — and with it the whole
// download — turning at frame rate. A frame, not something gentler, because
// the loop is what pulls the download: at 250 ms a covered first launch
// moved four beats a second and took twice as long as a visible one. Not
// something faster, either — 8 ms beats were tried and the covered boot got
// no quicker, so the pace of the walk under cover is set somewhere below the
// beat rate, and doubling the wake-ups just doubles their cost. Nothing
// audible is running before the first frame, so the substitute clock costs
// nothing here. The rescue ends at first frame, not at first callback: a
// boot is not survived until there is something on screen to come back to.
//
// After that, stock behaviour, deliberately: the timestamp
// requestAnimationFrame hands its callback is the clock the client drives
// animation and audio from, and substituting performance.now() for it on every
// frame is audible. Slowing the loop while a running game is covered would
// starve that same clock, so a covered *game* freezes exactly as it does under
// Chromium — the download is the phase that has to survive, and does.
//
// The glue re-reads globalThis.requestAnimationFrame on every call rather than
// capturing it, so both the swap in and the swap back reach the client without
// touching any vendored code. The handle returned while the wrapper is armed is
// not a usable frame id, which is safe only because the client never cancels a
// frame — cancelAnimationFrame appears nowhere in Gw.jspi.js.
const launchOptions =
  window.__gwnativeLaunch && typeof window.__gwnativeLaunch === 'object'
    ? window.__gwnativeLaunch
    : {};
const requestedFps = Number.isInteger(launchOptions.fps) && launchOptions.fps > 0
  ? launchOptions.fps
  : 0;
const requestedFrameMs = requestedFps ? 1000 / requestedFps : 0;
const BOOT_FRAME_MS = 16;
let bootRescueActive = true;
let makeFrameCadence = null;
const frameCadences = new WeakMap();
{
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (callback) => {
    if (!bootRescueActive) {
      if (!requestedFrameMs) {
        // First frame has been presented and no compatibility cap was asked
        // for; hand the native function back for good.
        window.requestAnimationFrame = raf;
        raf(callback);
        return 0;
      }
      let cadence = frameCadences.get(callback);
      if (!cadence) {
        cadence = makeFrameCadence?.() ?? { accept: () => true };
        frameCadences.set(callback, cadence);
      }
      const limited = (timestamp) => {
        if (cadence.accept(timestamp)) {
          callback(timestamp);
        } else {
          raf(limited);
        }
      };
      raf(limited);
      return 0;
    }
    let taken = false;
    const run = (timestamp) => {
      if (taken) return;
      taken = true;
      callback(timestamp);
    };
    const timer = setTimeout(() => run(performance.now()), BOOT_FRAME_MS);
    raf((timestamp) => {
      clearTimeout(timer);
      run(timestamp);
    });
    return 0;
  };
}

const statusEl = () => document.getElementById('status');
const log = (...values) => {
  console.log(...values);
  logBuf.push(values.map(String).join(' '));
  if (logBuf.length > LOG_LINES) logBuf.splice(0, logBuf.length - LOG_LINES);
  const el = document.getElementById('log');
  if (el && el.style.display !== 'none') {
    el.textContent = logBuf.join('\n');
    el.scrollTop = el.scrollHeight;
  }
};

/**
 * Show one line of boot state. `fraction` is 0..1, or null when the stage has
 * no total to divide by — the rail sweeps instead of parking at a made-up
 * number. `detail` carries rate and ETA when the client supplies them.
 */
const status = (text, fraction = null, detail = '', force = false) => {
  const el = statusEl();
  if (!el) return;
  if (launchOptions.noPatchUi && text !== null && !force) {
    el.hidden = true;
    return;
  }
  el.hidden = text === null;
  if (text === null) return;
  document.getElementById('status-label').textContent = text;
  document.getElementById('status-detail').textContent = detail;
  const bar = document.getElementById('bar');
  bar.classList.toggle('busy', fraction === null);
  if (fraction !== null) {
    document.getElementById('bar-fill').style.width =
      `${Math.max(0, Math.min(1, fraction)) * 100}%`;
  }
};

/**
 * A boot that cannot continue, and what the player can do about it.
 *
 * Most of these are transient — a range request that lost its connection, a
 * module fetch that raced the server coming up — so the overlay's first offer
 * is simply to try again. Before `loading.js` has loaded there is only the
 * status line, which is what this used to be in every case.
 */
const fail = (text) => {
  status(text, null, '', true);
  log('[err]', text);
  recovery?.showFailure(text, log);
};

// Long enough for a loopback round trip several times over, short enough that
// nobody is left looking at a frozen game while it runs out. This is already a
// failed boot: a host that has stopped answering must not turn the failure
// screen into a blank one.
const REASON_DEADLINE_MS = 1500;

/**
 * Why a read of the game data failed, in the player's terms.
 *
 * The client reports a fatal read by calling `handleFatalReadError` with no
 * argument, so the page is told that something went wrong and nothing at all
 * about what. This used to answer "no cached copy of the required game data is
 * available", which named a cause the page had not established and named the
 * least likely one — this host streams, so a read failing has far more to do
 * with the network or the disk than with the cache. Worse, the overlay under
 * the sentence offers to delete the player's game data, and a sentence about a
 * missing cache is exactly what would send someone to that button.
 *
 * So it asks. The host has the reason — every chunk fetch that failed left one
 * behind — and when it answers, the player reads what actually happened. When
 * it does not, they read the part that is true either way.
 */
const describeReadFailure = async () => {
  try {
    const response = await fetch('__diag', {
      headers: { 'X-Gwnative-Token': window.__gwnativeToken ?? '' },
      signal: AbortSignal.timeout(REASON_DEADLINE_MS),
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const reason = (await response.json()).lastFetchFailure;
    if (reason) return `The game data could not be read: ${reason}`;
  } catch (error) {
    log('[warn] the host could not say why the read failed:', error);
  }
  return 'The game data could not be read.';
};

window.gwLog = (on = true) => {
  const el = document.getElementById('log');
  if (!el) return false;
  el.style.display = on ? 'block' : 'none';
  if (on) {
    el.textContent = logBuf.join('\n');
    el.scrollTop = el.scrollHeight;
  }
  return on;
};

// The game's own file transfer runs at a few tens of bytes a second early on,
// which a fixed MB/s format renders as a flat "0.0 MB/s" — indistinguishable
// from stalled. Scaling the unit keeps a slow but live transfer legible.
const rate = (bytesPerSecond) => {
  if (bytesPerSecond >= 1048576) return `${(bytesPerSecond / 1048576).toFixed(1)} MB/s`;
  if (bytesPerSecond >= 1024) return `${(bytesPerSecond / 1024).toFixed(1)} kB/s`;
  return `${Math.round(bytesPerSecond)} B/s`;
};

const remaining = (seconds) => {
  if (seconds >= 3600) return `${Math.floor(seconds / 3600)} h ${Math.round((seconds % 3600) / 60)} min`;
  if (seconds >= 60) return `${Math.ceil(seconds / 60)} min`;
  return `${Math.ceil(seconds)} s`;
};

// The client's estimate of the seconds left, measured against the largest
// estimate it has yet given. That estimate falls to zero across the transfer,
// so the ratio starts at nothing and ends at everything; and a transfer that
// slows down revises the estimate up, which raises the ceiling rather than
// pulling the rail backwards. Monotonic on both counts, which a progress bar
// has to be to be worth showing.
let etaCeiling = 0;
let etaFraction = 0;
const downloadProgress = (seconds) => {
  etaCeiling = Math.max(etaCeiling, seconds);
  etaFraction = Math.max(etaFraction, 1 - seconds / etaCeiling);
  return etaFraction;
};

// A stage with nothing to divide by sweeps the rail, and a sweeping rail is
// indistinguishable from a stalled one in a screenshot — which is how a
// perfectly healthy boot came to be reported as a hang. Elapsed time tells the
// two apart, but the client calls in only when something changes, so a stage
// that is quiet for a minute would leave a frozen number sitting there. Drive
// it from here instead, so the one stage that cannot show progress can at
// least show that it is still running.
let stageClock = null;
let stageLabel = null;
/** Re-read the client's heap size. Set once the module's imports exist. */
let readHeap = null;

const releaseStage = () => {
  clearInterval(stageClock);
  stageClock = null;
  stageLabel = null;
};

const holdStage = (label) => {
  // Re-entering the same stage keeps its clock; restarting would reset the
  // count on every call and never read higher than zero.
  if (label === stageLabel) return;
  releaseStage();
  stageLabel = label;
  const started = performance.now();
  const tick = () =>
    status(label, null, `${Math.round((performance.now() - started) / 1000)} s`);
  tick();
  stageClock = setInterval(tick, 1000);
};

const credentials = (method, body) => {
  // Without the injected token the host answers 403, which reaches the client
  // as "saved login unavailable" with nothing to distinguish a broken keychain
  // from a script that never ran. Name the actual cause instead.
  if (!window.__gwnativeToken) {
    return Promise.reject(new Error('the host did not inject a credential token'));
  }
  return fetch('__credentials', {
    method,
    headers: {
      'X-Gwnative-Token': window.__gwnativeToken ?? '',
      ...(body ? { 'Content-Type': 'application/json' } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
};

/// The saved login, read once and held, or null when there is none.
///
/// The read is started when this file loads rather than when the client asks
/// for it, because the client does not wait: it asks for the saved login during
/// a startup step and builds the login screen when that step completes,
/// whichever of the two happens first. Opening the keychain item can cost about
/// 150 ms — long enough to lose that race — and the symptom is a login screen
/// with "Remember Account Name" ticked and nothing in the field. Started at load
/// it is minutes early instead.
let saved = launchOptions.credentials
  ? Promise.resolve({ ...launchOptions.credentials })
  : null;

const readSaved = () => {
  saved ??= credentials('GET').then((response) => {
    // Distinguished from a failure here rather than at the call: "nothing
    // saved" is a value worth caching, and a first launch legitimately has it.
    if (response.status === 404) return null;
    if (!response.ok) throw new Error(`credential read failed: ${response.status}`);
    return response.json();
  });
  return saved;
};

// Dropped on failure so the client's own call retries rather than inheriting a
// rejection from a fetch that ran before the origin was ready. Nothing is
// reported here: at this point no one has asked for a login, and a message
// about one would arrive before the window does.
readSaved().catch(() => {
  saved = null;
});

const STARTUP_LABELS = {
  connecting: 'Starting Guild Wars',
  downloading: 'Preparing files needed to start',
  decompressing: 'Preparing files needed to start',
  loading: 'Starting Guild Wars',
};

let imageSource = null;
// Overwritten from the settings the host injects, before the client's first
// call into the graphics host. 1 is only what it reads if that injection is
// missing entirely — the shape a page opened outside the app has.
let renderScale = 1;

// The supporting modules are ESM; this bootstrap is not, because the generated
// glue redeclares `var Module`. boot() imports them and holds them here, since
// instantiateWasm is called synchronously by the glue and cannot await.
// Reading `host` before boot() assigns it is a TypeError, not a silent skip.
let host;
let diag;
let recovery;
// The client's own allocator, reached through the instance rather than through
// `Module.wasmExports`: this build's glue does not export that name, and asking
// for it does not return undefined — it aborts.
let gameInstance;
let gameImports;
// Kept beside it because the enhancements read their manifest off the module
// rather than the instance — `WebAssembly.Module.customSections` — and by the
// time the runtime is initialised, `instantiateWasm`'s `result` is long gone.
let gameModule;

Module = {
  canvas: document.getElementById('canvas'),
  print: (text) => log(text),
  printErr: (text) => log('[err]', text),

  // Take over instantiation so the EGL imports can be patched before the
  // client ever calls them.
  instantiateWasm(imports, success) {
    gameImports = imports;
    host.installGraphics({
      env: imports.env,
      renderScale: () => renderScale,
      firstFrame: () => {
        performance.mark('gw.frame.first-submit');
        // The boot is survived; frame delivery goes back to stock. See the
        // requestAnimationFrame wrapper above.
        bootRescueActive = false;
        status(null);
        // Nothing is loading any more, and the startup clock has no reason to
        // keep ticking. The client does send `complete`, which already does
        // this, but a frame on screen is the stronger evidence of the two.
        releaseStage();
        log('first frame presented');
        window.__gwnativeFirstFrame = true;
        window.dispatchEvent(new Event('gwnative:first-frame'));
        void window.gwE2E?.report('first-frame').catch(() => {});
        // The heap the client settled on to get here, which is the number a
        // later reading has to be compared against to mean anything.
        readHeap?.();
        // Time to first frame, from the page's own origin rather than from
        // process start — the one launch number a change can be judged by.
        diag?.gauge('gw.boot.first-frame.ms', performance.now());
        // Everything the chunk store served up to here is what booting costs.
        // Telling the host now, rather than at some later milestone, is what
        // keeps the recorded list to the chunks that gate the first frame.
        fetch('__booted', {
          method: 'POST',
          headers: { 'X-Gwnative-Token': window.__gwnativeToken ?? '' },
        }).catch(() => {});
      },
      log,
    });

    // The client's linear memory lives in the web content process, which the
    // host's own sampler cannot see at all. This is the only way its size is
    // ever reported.
    readHeap = host.installMemorySensor({
      env: imports.env,
      heapBytes: () => Module.HEAPU8?.byteLength ?? 0,
      log,
    });

    // Answer the derived client's file bridge. Must be in place before the
    // module is instantiated, since the forwarders are reached through an
    // import the instance binds once.
    host.installTemplateSave({
      imports,
      module: Module,
      // The directory listing hands the client a block it frees itself, so it
      // has to come from the client's own allocator, which only exists once
      // instantiation below resolves.
      exports: () => gameInstance?.exports ?? null,
      log,
    });

    const url = 'Gw.jspi.wasm';
    performance.mark('gw.wasm.instantiate.begin');
    (async () => {
      let result;
      try {
        result = await WebAssembly.instantiateStreaming(fetch(url), imports);
      } catch (error) {
        log('[warn] streaming instantiate failed, falling back:', error);
        result = await WebAssembly.instantiate(
          await (await fetch(url)).arrayBuffer(),
          imports,
        );
      }
      performance.mark('gw.wasm.instantiate.end');
      // 8.2 MB to fetch and compile, and the largest single item in a launch.
      // Worth its own gauge: it is what a code cache would move, so a claim
      // about caching is checkable against this number rather than argued.
      diag?.gauge(
        'gw.boot.wasm.ms',
        performance.measure(
          'gw.wasm.instantiate',
          'gw.wasm.instantiate.begin',
          'gw.wasm.instantiate.end',
        ).duration,
      );
      log('wasm instantiated');
      gameInstance = result.instance;
      gameModule = result.module;
      success(result.instance, result.module);
    })().catch((error) => fail(`The game client could not start: ${error}`));

    return {};   // signals that instantiation is in flight
  },

  // Both builds share an output basename, so Gw.jspi.js also asks for
  // "Gw.wasm". Without this it silently pairs with the Asyncify binary.
  locateFile: (path) => (path === 'Gw.wasm' ? 'Gw.jspi.wasm' : path),

  // Module.image is assigned in boot(), once the snapshot size that makes
  // fileSize() answerable synchronously has arrived.

  // Module.dns and Module.socket are assigned in boot(), once the host module
  // that backs them has been imported.

  // All three must exist: the glue's missing-method branches call their
  // fallback without returning.
  // Backed by a keychain item on the host. The token comes from a script the
  // host injects, not from anything served over the loopback origin, so a local
  // process that can reach the same port still cannot read the saved password.
  secureStorage: {
    async getCredentials() {
      const asked = performance.now();
      let stored;
      try {
        stored = await readSaved();
      } catch (error) {
        saved = null;
        throw error;
      }
      // The client's contract for "nothing saved" is a rejection, and a first
      // launch legitimately has nothing.
      if (!stored) {
        log('secureStorage: nothing saved — the client should ask');
        throw new Error('no stored credentials');
      }
      // Said on this side as well as the host's, because "the keychain
      // answered" and "the client took the answer in time" are different facts
      // and only the second one puts an account on the login screen. The delay
      // is the one that matters, so it is on the line. Presence, never the
      // fields: this goes to a log the player can open.
      log(
        `secureStorage: returning the saved login (account ${stored.username ? 'set' : 'empty'},`,
        `password ${stored.password ? 'set' : 'empty'}) after`,
        `${Math.round(performance.now() - asked)} ms`,
      );
      void window.gwE2E?.report('credentials-offered', {
        accountSet: Boolean(stored.username),
        passwordSet: Boolean(stored.password),
      }).catch(() => {});
      return stored;
    },
    async storeCredentials(username, password) {
      const response = await credentials('PUT', { username, password });
      if (!response.ok) throw new Error(await response.text());
      // Held rather than re-read: the host now has exactly this, and a client
      // that signs out and back in within one session should not pay for the
      // keychain twice.
      saved = Promise.resolve({ username, password });
    },
    async clearCredentials() {
      const response = await credentials('DELETE');
      if (!response.ok) {
        throw new Error((await response.text()) || `credential deletion failed: ${response.status}`);
      }
      saved = Promise.resolve(null);
    },
  },

  // No federated auth: reporting no providers falls back to email/password.
  login: {
    hasProvider: () => false,
  },

  // onDemand streams chunks as the client asks for them, which is what the
  // ranged snapshot endpoint is for.
  getPatchMode: async () => 'onDemand',

  setStartupProgress(stage, a, b, c, d) {
    log(`[startup] ${stage}`, [a, b, c, d].filter((v) => v !== undefined).join(' '));
    const s = String(stage || '').toLowerCase();
    if (s === 'complete') {
      releaseStage();
      void window.gwE2E?.report('startup-complete').catch(() => {});
      return status(null);
    }
    // The client's four arguments for `downloading` are not what their shape
    // suggests. The first looks like a percent and is not one: over a complete
    // transfer it takes the values 0, 1, 2, 3, stepping once per ~1353 units of
    // the second, so it counts finished parts. Read as a percent it pins the
    // rail near zero for the entire ninety minutes and reads exactly like a
    // stall. The second is a rising count with no published total, so it cannot
    // be turned into a fraction either. The third and fourth — bytes per second
    // and seconds left — are what they appear to be, and the fourth is the only
    // one that describes the whole job, so progress comes from it.
    if (s === 'downloading' && typeof d === 'number' && d > 0) {
      releaseStage();
      const parts = [];
      if (typeof c === 'number' && c > 0) parts.push(rate(c));
      parts.push(`${remaining(d)} remaining`);
      return status('Preparing files needed to start', downloadProgress(d), parts.join(' · '));
    }
    holdStage(STARTUP_LABELS[s] ?? 'Loading…');
  },

  handleFatalReadError() {
    describeReadFailure().then(fail);
  },

  setBuildInfo(info) {
    window.gwBuildInfo = Object.freeze({
      programId: Number(info.programId),
      buildId: Number(info.buildId),
    });
    log(`build info: program=${info.programId} build=${info.buildId}`);
    void window.gwE2E?.report('client-build', {
      programId: Number(info.programId),
      buildId: Number(info.buildId),
    }).catch(() => {});
  },

  isMobile: launchOptions.mockSteamDeck === true,
  requestFullScreen: () => Module.canvas.requestFullscreen?.(),
  requestFullscreen: () => Module.canvas.requestFullscreen?.(),

  onRuntimeInitialized() {
    performance.mark('gw.runtime.initialized');
    log('runtime initialised');
    status('Starting Guild Wars');
    installTools();
    installMods();
  },

  onAbort(reason) {
    fail(`The game client stopped unexpectedly: ${reason}`);
  },

  onExit(code) {
    log('wasm exited:', code);
    if (code !== 0) {
      fail('The game client stopped unexpectedly.');
      return;
    }
    // The player chose Exit inside the game. Without this the window stays open
    // on the last frame it drew and they have to quit a second time, from the
    // menu, to close a game that has already ended. The host takes it from here
    // — including writing the files out, which is why this is a request to
    // terminate rather than a close.
    status('Closing…');
    fetch('__quit', {
      method: 'POST',
      headers: { 'X-Gwnative-Token': window.__gwnativeToken ?? '' },
    }).catch(() => {});
  },
};

/**
 * Install optional enhancements, if this launch is one that has them.
 *
 * Three things have to line up, and each one is a different party's decision:
 * the player turned a tool on, the host answered by deriving a client that has
 * the hook (`window.__gwnativeEnhancements`, `'ready'`), and the module the
 * page actually instantiated carries the manifest that proves it. The last is
 * checked rather than assumed because it is the only one of the three this
 * page can see for itself, and the cost of being wrong is a companion reading
 * whatever happens to be at those offsets.
 *
 * `enhancements.js` and everything under it is imported here rather than in
 * `boot` so that a launch with no tools on never fetches it at all.
 */
function installTools() {
  const settings = host.currentSettings();
  const selection = {
    nativeCursor: settings.nativeCursor === true,
    targetReadout: settings.targetReadout === true,
    stateApi: true,
  };
  if (window.__gwnativeEnhancements !== 'ready') {
    log(`[warn] enhancements are on but this client is ${window.__gwnativeEnhancements}`);
    return;
  }
  if (
    !gameInstance
    || !gameModule
    || WebAssembly.Module.customSections(gameModule, 'enhancement_manifest').length !== 1
  ) {
    log('[warn] enhancements: the client the page ran carries no manifest');
    return;
  }
  const instance = gameInstance;
  const module = gameModule;
  void import('./enhancements.js')
    .then(({ installEnhancements }) => installEnhancements(instance, module, selection))
    .catch((error) => log('[warn] enhancements:', error?.message ?? error));
}

function installMods() {
  if (!launchOptions.modsEnabled) return;
  if (!gameInstance || !gameImports) {
    log('[warn] mods: the running game instance is unavailable');
    return;
  }
  void import('./mod-runtime.js')
    .then(({ installModRuntime }) =>
      installModRuntime({
        game: gameInstance,
        gameImports,
        log,
        alert: (message) => log(`[mod alert] ${message}`),
      }),
    )
    .then((loaded) => log(`mods: ${loaded.length} module(s) loaded`))
    .catch((error) => log('[warn] mods:', error?.message ?? error));
}

function appendGlue() {
  const src = 'Gw.jspi.js';
  log('loading', src, '(wasm: Gw.jspi.wasm)…');
  const script = document.createElement('script');
  script.src = src;
  script.onerror = () => fail('The game client could not be loaded.');
  document.body.appendChild(script);
}

(async function boot() {
  status('Loading host…');

  // Loaded on its own, and first, because "the host contract could not be
  // loaded" is one of the failures it has to be able to report. Its own
  // failure is the one case left with nothing but the status line, which is
  // what every failure had before it existed.
  try {
    [recovery] = await Promise.all([import('./loading.js'), import('./commands.js')]);
  } catch (error) {
    log('[warn] no recovery UI:', error);
  }

  try {
    const [
      graphics, audio, memory, filesystem, image, sockets, platform, input, templates, prefs,
      frameRate, start, panel, data, compat, guide, gameApi, overlay, tools, hotkeys, e2e, metrics,
    ] = await Promise.all([
      import('./graphics.js'),
      import('./audio.js'),
      import('./memory.js'),
      import('./filesystem.js'),
      import('./image.js'),
      import('./sockets.js'),
      import('./platform-capabilities.js'),
      import('./input.js'),
      import('./template-save.js'),
      import('./settings.js'),
      import('./frame-rate.js'),
      import('./launcher.js'),
      import('./settings-panel.js'),
      import('./game-data.js'),
      import('./compatibility.js'),
      import('./guide.js'),
      import('./game-api.js'),
      import('./overlay.js'),
      import('./tools-panel.js'),
      import('./hotkeys.js'),
      import('./e2e.js'),
      import('./diagnostics.js'),
    ]);
    host = {
      ...graphics,
      ...audio,
      ...memory,
      ...filesystem,
      ...image,
      ...sockets,
      ...platform,
      ...input,
      ...templates,
      ...prefs,
      ...frameRate,
      ...start,
      ...panel,
      ...data,
      ...compat,
      ...guide,
      ...gameApi,
      ...overlay,
      ...tools,
      ...hotkeys,
      ...e2e,
    };
    // Kept out of the host bag: `count`, `gauge` and `peak` are names the game
    // contract could plausibly want for something else.
    diag = metrics;
  } catch (error) {
    return fail(`The game host contract could not be loaded: ${error}`);
  }

  // The settings that decide how the client is built, applied before it exists.
  // Synchronous on purpose — see settings.js for why they are injected rather
  // than fetched. Anything that changes one of these afterwards has to reload,
  // which is what the host already does for a client-module change.
  const settings = host.currentSettings();
  renderScale = settings.renderScale;
  makeFrameCadence = () => host.createFrameCadence(requestedFrameMs);
  if (settings.showDiagnostics || launchOptions.diagnostics || launchOptions.performance) {
    window.gwLog(true);
  }
  // Said out loud for the same reason input.js announces its touch mode: the
  // render scale is the one setting whose effect is a cost rather than a
  // control, so a session's log has to record which one it was paying.
  log(`settings: render scale ${settings.renderScale}, touch mode ${settings.touchMode}`);
  window.gwSettings = {
    current: host.currentSettings,
    read: host.readSettings,
    save: host.saveSettings,
  };
  window.gwGameApi = host.installGameApi({ log });
  window.gwOverlays = host.createOverlayManager({ document, log });
  window.gwOpenTools = host.installToolsPanel({
    document,
    overlays: window.gwOverlays,
    log,
  });
  window.gwHotkeys = host.createHotkeyEngine(window);
  window.gwHotkeys.register({
    id: 'companion-tools',
    chord: 'Command+Shift+T',
    run: () => window.gwOpenTools?.(),
  });
  window.gwHotkeys.register({
    id: 'overlay-layout',
    chord: 'Command+Shift+O',
    run: () => window.gwOverlays?.edit(!window.gwOverlays.editing()),
  });

  // The panel is wired before the client exists, so ⌘, answers from the first
  // moment the page is up rather than only once the game has booted — which is
  // exactly when a wrong render scale is worth changing.
  window.gwOpenSettings = host.installSettingsPanel({
    read: host.currentSettings,
    save: host.saveSettings,
    showLog: (on) => window.gwLog(on),
    sweep: host.sweepSnapshot,
    progress: host.snapshotProgress,
    clearData: host.clearGameData,
    relaunch: host.relaunchApp,
    log,
  });

  // Same moment and the same reason as the panel: Help → User Guide has to
  // answer from the first frame, and a player looking for the guide is often a
  // player whose game did not start.
  window.gwOpenGuide = host.installGuide({ log });

  if (window.__gwnativeE2E === true) {
    window.gwE2E = host.installE2EBridge({
      window,
      canvas: Module.canvas,
      log,
    });
    void window.gwE2E?.report('client-capabilities', {
      enhancements: String(window.__gwnativeEnhancements ?? 'off'),
      templateSave: String(window.__gwnativeTemplateSave ?? 'off'),
    }).catch(() => {});
    // The runner invokes this only after the client has either reached the
    // game or explicitly skipped gameplay. Exercising every overlay while the
    // single-threaded client is still booting makes the test itself a source
    // of startup stalls and closes the panel before a live widget can render.
    window.gwRunAppE2E = () => host.runAppE2E({
      window,
      document,
      storage: window.localStorage,
      overlays: window.gwOverlays,
      openTools: window.gwOpenTools,
      openSettings: window.gwOpenSettings,
      openGuide: window.gwOpenGuide,
      log,
    });
  }

  // The only failure here that will still be a failure tomorrow. Everything
  // else the overlay catches is transient, which is why its first offer is to
  // try again; this one is a WebKit that predates JSPI, so trying again does
  // the same thing forever and the offer beside it deletes the player's game
  // data for nothing. The remedy is not in this app at all — the bundle asks
  // for macOS 15.2 because that is the first release whose WebKit has this —
  // so the sentence says where it is rather than naming the API.
  if (!('Suspending' in WebAssembly)) {
    log('[err] WebAssembly.Suspending is missing; the client cannot be run here');
    return fail(
      'Guild Wars cannot run on this version of macOS. Updating macOS updates ' +
        'the web engine the game needs, which is missing here.',
    );
  }

  // ArenaNet's glue never dials its own API hosts. Outside Capacitor it folds
  // every request onto this origin, under a path equal to the first label of the
  // host it meant to reach — `https://webgate.ncplatform.net/x` becomes
  // `https://<location.hostname>/webgate/x`. That drops our port, so the URL
  // lands on :443 of the loopback address where nothing is listening. Putting it
  // back on this origin is what lets logging in reach NCSoft's gateway at all:
  // the login request is this XHR, not a packet on the game socket.
  const PROXY_LABELS = new Set(['webgate', 'account', 'help', 'store', 'www']);
  const open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...rest) {
    const routedOpen = (path, target) => {
      log(`api: ${method} ${path}`);
      this.addEventListener('loadend', () => {
        const length = this.getResponseHeader('content-length') ?? 'unknown';
        log(`api: ${method} ${path} -> ${this.status} (${length} bytes)`);
        if (window.__gwnativeE2E === true && this.response instanceof ArrayBuffer) {
          // Schema only: element names establish which logical response the
          // client received without putting credentials, tokens, or even
          // response values into an E2E log.
          const source = new TextDecoder().decode(this.response);
          const tags = [...source.matchAll(/<\s*\/?\s*([A-Za-z_][\w:.-]*)/g)]
            .map((match) => match[1])
            .filter((tag, index, all) => all.indexOf(tag) === index)
            .slice(0, 16);
          log(`api schema: ${path} <${tags.join('> <')}>`);
          if (path === '/webgate/my_account/token.xml') {
            void window.gwE2E?.report('login-response', {
              status: this.status,
              bytes: this.response.byteLength,
            }).catch(() => {});
          }
        }
      }, { once: true });
      return open.call(this, method, target, ...rest);
    };
    try {
      const target = new URL(url, location.href);
      const routed = target.pathname.replace(/^\/+/, '').split('/')[0] ?? '';
      const label = target.hostname.split('.')[0] ?? '';
      // Root-relative, so it resolves against this origin — port included.
      if (PROXY_LABELS.has(routed)) {
        return routedOpen(target.pathname, `${target.pathname}${target.search}`);
      }
      // A request the glue left alone because it was handed an explicit proxy
      // host. Same five names, so it belongs on the same route.
      if (PROXY_LABELS.has(label) && target.hostname !== location.hostname) {
        const path = `/${label}${target.pathname}`;
        return routedOpen(path, `${path}${target.search}`);
      }
    } catch {
      // Not a URL this can classify, so it is not one of the five.
    }
    return open.call(this, method, url, ...rest);
  };

  Module.dns = host.createDns({ log });
  Module.socket = host.createSockets({ log });

  // Two namespaces the client dereferences after deciding they are missing.
  // See platform-capabilities.js — neither is implemented, both must exist.
  Object.assign(Module, host.unavailablePlatformCapabilities(log));

  // Before the glue exists, which is the whole requirement: it reads
  // window.AudioContext when the client first opens a device, and this has to
  // be what it finds there.
  window.gwAudio = host.installGameAudio();
  if (launchOptions.mute) window.gwAudio.setGameAudioMuted(true);

  // preRun, so it only has to precede the glue that appendGlue() loads below.
  host.installGameFilesystem({
    module: Module,
    failed: (error) => fail(`Persistent storage is unavailable: ${error}`),
    log,
  });

  status('Preparing game data…');
  let snapshotBytes = 0;
  try {
    const meta = await host.loadSnapshotMetadata();
    const source = host.createImageSource({
      ...meta,
      writeBytes: (data, address) => Module.HEAPU8.set(data, address),
      log,
    });
    imageSource = source;
    snapshotBytes = meta.size;
    Module.image = source.image;
    window.gwStats = () => source.stats();
    log(
      `snapshot: ${(meta.size / 1e9).toFixed(2)} GB in ${meta.chunkSize / 1024} KiB chunks`,
    );
  } catch (error) {
    return fail(`Game data could not be prepared: ${error}`);
  }

  // Asked here because it is the last moment there is anywhere to ask: after
  // appendGlue() the client owns the canvas and the keyboard. It returns at once
  // unless there is a real choice to make — see launcher.js.
  window.gwResolveDataStrategy = (bytes) =>
    host.resolveDataStrategy(bytes, {
      log,
      save: host.saveSettings,
      strategy: host.currentSettings().dataStrategy,
    });
  try {
    await window.gwResolveDataStrategy(snapshotBytes);
  } catch (error) {
    // The launcher's own failure paths already end in "boot anyway"; this is
    // for the one it cannot catch, which deserves the same answer.
    log(`[warn] launcher: ${error}`);
    document.getElementById('launcher').hidden = true;
  }

  // After the launcher and before the glue, which is the only gap left: the
  // launcher owns the overlay until it is done with it, and once the client is
  // appended there is nowhere to say anything. Awaited because it is a sentence
  // a player is reading, not a step in the boot — see compatibility.js.
  if (window.__gwnativeE2E === true) {
    log('[e2e] compatibility acknowledgement is covered by module tests');
  } else {
    try {
      await host.announceCompatibility({
        log,
        save: host.saveSettings,
        seenFor: host.currentSettings().compatibilityNoticeSeenFor,
      });
    } catch (error) {
      log(`[warn] compatibility: ${error}`);
      document.getElementById('launcher').hidden = true;
    }
  }

  // The canvas is sized by the client, not here. It owns the drawing buffer —
  // it asks for the pixel ratio through emscripten_get_device_pixel_ratio and
  // sets width/height itself — and writing those from the host as well forces
  // WebGL to reallocate and clear the buffer on every resize event.
  const canvas = Module.canvas;
  canvas.focus();

  // Before the glue, so a corrected key event replaces the original rather than
  // arriving after the client has already acted on it.
  window.gwInput = host.installGameInput({
    canvas,
    touchMode: settings.touchMode,
    log,
  });

  // Text entry does not run through keydown on the canvas: the client focuses
  // one of these fields and reads the composed result back. Without them it
  // reports it cannot accept text and every login keystroke is dropped.
  Module.oskInput = {
    text: document.getElementById('osk-input-text'),
    email: document.getElementById('osk-input-email'),
    password: document.getElementById('osk-input-password'),
    number: document.getElementById('osk-input-number'),
    multiline: document.getElementById('osk-input-multiline'),
  };
  // Modal means "show the field and let it own the keys", which is for an
  // on-screen keyboard. A Mac has a real one, so the proxy stays out of sight
  // and the client keeps reading composition events from it.
  Module.oskIsModal = false;
  const oskFields = new Set(Object.values(Module.oskInput).filter(Boolean));

  // Focus moving to a text proxy is part of the game, not a loss of game focus;
  // letting the client's canvas-blur handler see it mutes audio mid-chat.
  canvas.addEventListener('blur', (event) => {
    if (oskFields.has(event.relatedTarget)) event.stopImmediatePropagation();
  }, true);

  // A field that took focus without the client asking would swallow keys meant
  // for the game, so bounce it back — after a microtask, because the client
  // sets oskActiveInput immediately after its own focus() call.
  for (const [type, field] of Object.entries(Module.oskInput)) {
    if (!field) {
      log(`[warn] missing text-entry field for "${type}"`);
      continue;
    }
    field.addEventListener('focus', () => {
      queueMicrotask(() => {
        if (Module.oskActiveInput !== field && document.activeElement === field) {
          field.blur();
        }
      });
    });
  }

  // Keyboard delivery has two failure points outside this file — the window may
  // never make the web view first responder, or focus may sit somewhere the
  // client is not listening. One line on the first key press tells the two
  // apart; after that the channel is proven and silence is correct.
  window.addEventListener('keydown', function first(event) {
    if (!event.isTrusted) return;
    window.removeEventListener('keydown', first, true);
    log(`keyboard reaching the page (target=${event.target?.id || event.target?.nodeName})`);
  }, true);

  // Audio contexts start suspended until a gesture, and the client asks only
  // once — the glue's own auto-resume listeners are `{once: true}`, so after
  // they have fired nothing in the client will resume a context again.
  for (const event of ['pointerdown', 'keydown']) {
    window.addEventListener(event, () => host.resumeGameAudio(), true);
  }

  // The client registers its blur callback on the canvas, while WKWebView fires
  // blur on the window when it resigns key. The client therefore never hears
  // about the transition on its own and keeps playing.
  //
  // The native side is the source of truth — see `src/commands.rs` for why
  // AppKit sees deactivations the page does not — and these are the fallback
  // for anything that reaches the page first. Both are gain ramps, so which of
  // them arrives, and how often, does not matter.
  window.addEventListener('blur', () => host.setGameAudioMuted(true));
  window.addEventListener('focus', () => {
    host.setGameAudioMuted(false);
    host.resumeGameAudio();
  });

  status('Starting the game…');
  appendGlue();
})();

})();
