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
    fetch('__report', { method: 'POST', body }).catch(() => {});
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
// connection. It also applies before the first paint, so launching the app
// behind another window leaves it wedged just after `runtime initialised`,
// having loaded the runtime and then never taken a single frame.
//
// So arm a timer alongside the frame request and let whichever arrives first
// win, then get out of the way: the moment a real frame lands, put the native
// function back. From then on the client sees stock frame timing exactly, which
// matters because the timestamp requestAnimationFrame hands its callback is the
// clock the client drives animation and audio from — substituting
// performance.now() for it on every frame is audible.
//
// So this only rescues the boot, and deliberately does not throttle a running
// game. Slowing the loop while the window is covered does save CPU, but it
// starves the same audio clock and stutters the sound, which is a bad trade for
// idle watts. A covered window therefore freezes exactly as it does under
// Chromium, which is what the Electron build does too.
//
// The glue re-reads globalThis.requestAnimationFrame on every call rather than
// capturing it, so both the swap in and the swap back reach the client without
// touching any vendored code. The handle returned while the timer is armed is
// not a usable frame id, which is safe only because the client never cancels a
// frame — cancelAnimationFrame appears nowhere in Gw.jspi.js.
const BOOT_FRAME_MS = 250;
{
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (callback) => {
    let taken = false;
    const run = (timestamp) => {
      if (taken) return;
      taken = true;
      callback(timestamp);
    };
    const timer = setTimeout(() => run(performance.now()), BOOT_FRAME_MS);
    raf((timestamp) => {
      clearTimeout(timer);
      // Frames are flowing; nothing above needs to intercept them any more.
      window.requestAnimationFrame = raf;
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
const status = (text, fraction = null, detail = '') => {
  const el = statusEl();
  if (!el) return;
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

const fail = (text) => {
  status(text);
  log('[err]', text);
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

const STARTUP_LABELS = {
  connecting: 'Starting Guild Wars',
  downloading: 'Preparing files needed to start',
  decompressing: 'Preparing files needed to start',
  loading: 'Starting Guild Wars',
};

let imageSource = null;
let renderScale = 1;

// The supporting modules are ESM; this bootstrap is not, because the generated
// glue redeclares `var Module`. boot() imports them and holds them here, since
// instantiateWasm is called synchronously by the glue and cannot await.
// Reading `host` before boot() assigns it is a TypeError, not a silent skip.
let host;

Module = {
  canvas: document.getElementById('canvas'),
  print: (text) => log(text),
  printErr: (text) => log('[err]', text),

  // Take over instantiation so the EGL imports can be patched before the
  // client ever calls them.
  instantiateWasm(imports, success) {
    host.installGraphics({
      env: imports.env,
      renderScale: () => renderScale,
      firstFrame: () => {
        performance.mark('gw.frame.first-submit');
        status(null);
        log('first frame presented');
      },
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
      log('wasm instantiated');
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
      const response = await credentials('GET');
      // The client's contract for "nothing saved" is a rejection, and a first
      // launch legitimately has nothing.
      if (response.status === 404) throw new Error('no stored credentials');
      if (!response.ok) throw new Error(`credential read failed: ${response.status}`);
      return response.json();
    },
    async storeCredentials(username, password) {
      const response = await credentials('PUT', { username, password });
      if (!response.ok) throw new Error(await response.text());
    },
    async clearCredentials() {
      await credentials('DELETE');
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
    if (s === 'complete') return status(null);
    // The client's own argument contract for `downloading`: percent, an opaque
    // counter, bytes per second, seconds remaining. Only the first, third and
    // fourth are meaningful to a person.
    if (s === 'downloading' && typeof a === 'number') {
      const parts = [`${a.toFixed(0)}%`];
      if (typeof c === 'number' && c > 0) parts.push(rate(c));
      if (typeof d === 'number' && d > 0) parts.push(`${remaining(d)} remaining`);
      return status('Preparing files needed to start', a / 100, parts.join(' · '));
    }
    status(STARTUP_LABELS[s] ?? 'Loading…');
  },

  handleFatalReadError() {
    fail('No cached copy of the required game data is available.');
  },

  setBuildInfo(info) {
    window.gwBuildInfo = Object.freeze({
      programId: Number(info.programId),
      buildId: Number(info.buildId),
    });
    log(`build info: program=${info.programId} build=${info.buildId}`);
  },

  isMobile: false,
  requestFullScreen: () => Module.canvas.requestFullscreen?.(),
  requestFullscreen: () => Module.canvas.requestFullscreen?.(),

  onRuntimeInitialized() {
    performance.mark('gw.runtime.initialized');
    log('runtime initialised');
    status('Starting Guild Wars');
  },

  onAbort(reason) {
    fail(`The game client stopped unexpectedly: ${reason}`);
  },

  onExit(code) {
    log('wasm exited:', code);
    if (code !== 0) fail('The game client stopped unexpectedly.');
  },
};

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
  try {
    const [graphics, filesystem, image, sockets] = await Promise.all([
      import('./graphics.js'),
      import('./filesystem.js'),
      import('./image.js'),
      import('./sockets.js'),
    ]);
    host = { ...graphics, ...filesystem, ...image, ...sockets };
  } catch (error) {
    return fail(`The game host contract could not be loaded: ${error}`);
  }

  if (!('Suspending' in WebAssembly)) {
    return fail('This WebView lacks WebAssembly JSPI (WebAssembly.Suspending).');
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
    try {
      const target = new URL(url, location.href);
      const routed = target.pathname.replace(/^\/+/, '').split('/')[0] ?? '';
      const label = target.hostname.split('.')[0] ?? '';
      // Root-relative, so it resolves against this origin — port included.
      if (PROXY_LABELS.has(routed)) {
        log(`api: ${method} ${target.pathname}`);
        return open.call(this, method, `${target.pathname}${target.search}`, ...rest);
      }
      // A request the glue left alone because it was handed an explicit proxy
      // host. Same five names, so it belongs on the same route.
      if (PROXY_LABELS.has(label) && target.hostname !== location.hostname) {
        log(`api: ${method} /${label}${target.pathname}`);
        return open.call(
          this, method, `/${label}${target.pathname}${target.search}`, ...rest);
      }
    } catch {
      // Not a URL this can classify, so it is not one of the five.
    }
    return open.call(this, method, url, ...rest);
  };

  Module.dns = host.createDns({ log });
  Module.socket = host.createSockets({ log });

  // preRun, so it only has to precede the glue that appendGlue() loads below.
  host.installGameFilesystem({
    module: Module,
    failed: (error) => fail(`Persistent storage is unavailable: ${error}`),
    log,
  });

  status('Preparing game data…');
  try {
    const meta = await host.loadSnapshotMetadata();
    const source = host.createImageSource({
      ...meta,
      writeBytes: (data, address) => Module.HEAPU8.set(data, address),
      log,
    });
    imageSource = source;
    Module.image = source.image;
    window.gwStats = () => source.stats();
    log(
      `snapshot: ${(meta.size / 1e9).toFixed(2)} GB in ${meta.chunkSize / 1024} KiB chunks`,
    );
  } catch (error) {
    return fail(`Game data could not be prepared: ${error}`);
  }

  // The canvas is sized by the client, not here. It owns the drawing buffer —
  // it asks for the pixel ratio through emscripten_get_device_pixel_ratio and
  // sets width/height itself — and writing those from the host as well forces
  // WebGL to reallocate and clear the buffer on every resize event.
  const canvas = Module.canvas;
  canvas.focus();

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

  // Audio contexts start suspended until a gesture; the client never asks.
  const resumeAudio = () => {
    const ctx = Module.SDL2?.audioContext || Module.audioContext;
    if (ctx?.state === 'suspended') ctx.resume().catch(() => {});
  };
  for (const event of ['pointerdown', 'keydown']) {
    window.addEventListener(event, resumeAudio, true);
  }

  status('Starting the game…');
  appendGlue();
})();

})();
