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

const statusEl = () => document.getElementById('status');
const log = (...values) => {
  console.log(...values);
  forward(values.map(String).join(' '));
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

function sizeCanvasToWindow(canvas) {
  const ratio = window.devicePixelRatio || 1;
  canvas.width = Math.round(canvas.clientWidth * ratio * renderScale);
  canvas.height = Math.round(canvas.clientHeight * ratio * renderScale);
}

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
  secureStorage: {
    async getCredentials() {
      throw new Error('no stored credentials');
    },
    async storeCredentials() {
      throw new Error('credential storage is not wired up yet');
    },
    async clearCredentials() {},
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

  const canvas = Module.canvas;
  sizeCanvasToWindow(canvas);
  canvas.focus();
  window.addEventListener('resize', () => sizeCanvasToWindow(canvas));

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

// The glue and the client log through console directly; mirror those too, or
// the host terminal sees only the harness's own half of the story.
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

})();
