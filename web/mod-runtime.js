// Opt-in WebAssembly module loader.
//
// Rust has already validated and flattened the selected .gwmod dependency
// graph. This side does the realm-specific work: compile each module, wire its
// imports to the running game and earlier modules, run constructors, then call
// mod_init. A selected mod shares the game's memory and is therefore trusted
// code; there is no pretend sandbox around an object that can write that memory.

const text = new TextDecoder();

const headers = () => ({
  'X-Gwnative-Token': window.__gwnativeToken ?? '',
});

export function validateCatalog(value) {
  if (!value || value.format !== 1 || typeof value.name !== 'string') {
    throw new Error('the host returned an unsupported mod catalog');
  }
  if (!Array.isArray(value.modules) || value.modules.length < 1 || value.modules.length > 64) {
    throw new Error('the host returned an invalid module list');
  }
  const seen = new Set();
  for (const [position, module] of value.modules.entries()) {
    if (
      !module
      || module.index !== position
      || typeof module.name !== 'string'
      || !module.name.endsWith('.wasm')
      || typeof module.url !== 'string'
      || !/^__mods\/\d+$/.test(module.url)
      || typeof module.sha256 !== 'string'
      || !/^[0-9a-f]{64}$/.test(module.sha256)
      || !Number.isSafeInteger(module.size)
      || module.size < 8
    ) {
      throw new Error(`the host returned invalid module metadata at index ${position}`);
    }
    if (seen.has(module.sha256)) throw new Error('the host returned a duplicate module');
    seen.add(module.sha256);
  }
  return value;
}

function findExport(exports, name) {
  return exports[name] ?? exports[`_${name}`] ?? null;
}

function findMemory(instance, gameImports) {
  for (const value of Object.values(instance.exports)) {
    if (value instanceof WebAssembly.Memory) return value;
  }
  for (const namespace of Object.values(gameImports ?? {})) {
    for (const value of Object.values(namespace ?? {})) {
      if (value instanceof WebAssembly.Memory) return value;
    }
  }
  throw new Error('the running game exposes no linear memory');
}

function findTable(instance, gameImports) {
  for (const value of Object.values(instance.exports)) {
    if (value instanceof WebAssembly.Table) return value;
  }
  for (const namespace of Object.values(gameImports ?? {})) {
    for (const value of Object.values(namespace ?? {})) {
      if (value instanceof WebAssembly.Table) return value;
    }
  }
  return null;
}

function readString(memory, pointer, length) {
  if (!Number.isSafeInteger(pointer) || !Number.isSafeInteger(length) || pointer < 0 || length < 0) {
    return '<invalid mod string>';
  }
  const end = pointer + length;
  if (end < pointer || end > memory.buffer.byteLength) return '<invalid mod string>';
  return text.decode(new Uint8Array(memory.buffer, pointer, length));
}

function hostFunctions(memory, state, gameTable, log, alert) {
  const sleep = (milliseconds) =>
    new Promise((resolve) => setTimeout(resolve, Math.max(0, Number(milliseconds) || 0)));
  return {
    mod_log: (pointer, length) => log(`[mod] ${readString(memory, pointer, length)}`),
    mod_alert: (pointer, length) => alert(readString(memory, pointer, length)),
    emscripten_sleep:
      typeof WebAssembly.Suspending === 'function' ? new WebAssembly.Suspending(sleep) : () => {},
    abort: () => {
      throw new Error('mod called abort');
    },
    table_publish: (localIndex) => {
      const localTable = state.instance && Object.values(state.instance.exports)
        .find((value) => value instanceof WebAssembly.Table);
      if (!localTable || !gameTable) return -1;
      const funcref = localTable.get(localIndex);
      if (typeof funcref !== 'function') return -1;
      const slot = gameTable.length;
      try {
        gameTable.grow(1);
        gameTable.set(slot, funcref);
        return slot;
      } catch {
        return -1;
      }
    },
    table_adopt: (gameIndex) => {
      const localTable = state.instance && Object.values(state.instance.exports)
        .find((value) => value instanceof WebAssembly.Table);
      if (!localTable || !gameTable || gameIndex < 0 || gameIndex >= gameTable.length) return -1;
      const funcref = gameTable.get(gameIndex);
      if (typeof funcref !== 'function') return -1;
      const slot = localTable.length;
      try {
        localTable.grow(1);
        localTable.set(slot, funcref);
        return slot;
      } catch {
        return -1;
      }
    },
  };
}

function wireImports(compiled, context) {
  const imports = {};
  const unresolved = [];
  const state = { instance: null };
  const host = hostFunctions(
    context.memory,
    state,
    context.gameTable,
    context.log,
    context.alert,
  );

  for (const descriptor of WebAssembly.Module.imports(compiled)) {
    const namespace = imports[descriptor.module] ??= {};
    if (descriptor.kind === 'memory') {
      namespace[descriptor.name] = context.memory;
      continue;
    }
    if (descriptor.kind === 'table' && context.gameTable) {
      namespace[descriptor.name] = context.gameTable;
      continue;
    }
    if (descriptor.kind === 'global' && descriptor.module === 'GOT.mem') {
      const exported = findExport(context.game.exports, descriptor.name);
      const value = exported instanceof WebAssembly.Global ? Number(exported.value) : 0;
      namespace[descriptor.name] = new WebAssembly.Global({ value: 'i32', mutable: true }, value);
      continue;
    }
    if (descriptor.kind !== 'function') {
      unresolved.push(`${descriptor.module}.${descriptor.name} (${descriptor.kind})`);
      continue;
    }

    let value = null;
    if (descriptor.module === 'env') {
      value =
        host[descriptor.name]
        ?? findExport(context.game.exports, descriptor.name)
        ?? context.previous.get(descriptor.name)
        ?? null;
    } else if (descriptor.module === 'wasi_snapshot_preview1') {
      // Freestanding mods occasionally retain harmless libc probes. Returning
      // ENOSYS is safer than claiming an operation happened.
      value = () => 52;
    }
    if (typeof value === 'function') {
      namespace[descriptor.name] = value;
    } else {
      unresolved.push(`${descriptor.module}.${descriptor.name}`);
    }
  }
  if (unresolved.length) {
    throw new Error(`unresolved imports: ${unresolved.join(', ')}`);
  }
  return { imports, state };
}

async function verifiedBytes(module) {
  const response = await fetch(module.url, { headers: headers() });
  if (!response.ok) throw new Error(`${module.name} could not be read (${response.status})`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength !== module.size) {
    throw new Error(`${module.name} changed size while loading`);
  }
  const digest = [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))]
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');
  if (digest !== module.sha256) throw new Error(`${module.name} failed its SHA-256 check`);
  return bytes;
}

async function gameBytes(game, memory) {
  const allocate = findExport(game.exports, 'malloc');
  if (typeof allocate !== 'function') return { pointer: 0, length: 0, release: () => {} };
  const response = await fetch('Gw.jspi.wasm');
  if (!response.ok) return { pointer: 0, length: 0, release: () => {} };
  const bytes = new Uint8Array(await response.arrayBuffer());
  const pointer = Number(allocate(bytes.byteLength));
  if (!pointer || pointer + bytes.byteLength > memory.buffer.byteLength) {
    return { pointer: 0, length: 0, release: () => {} };
  }
  new Uint8Array(memory.buffer, pointer, bytes.byteLength).set(bytes);
  const free = findExport(game.exports, 'free');
  return {
    pointer,
    length: bytes.byteLength,
    release: () => {
      if (typeof free === 'function') free(pointer);
    },
  };
}

/**
 * Load the one catalog validated by the native host.
 *
 * @param {{
 *   game: WebAssembly.Instance,
 *   gameImports: WebAssembly.Imports,
 *   log: (...values: unknown[]) => void,
 *   alert?: (message: string) => void,
 * }} options
 */
export async function loadSelectedMods({ game, gameImports, log, alert = window.alert }) {
  const response = await fetch('__mods', { headers: headers() });
  if (response.status === 404) return Object.freeze([]);
  if (!response.ok) throw new Error(`mod catalog unavailable (${response.status})`);
  const catalog = validateCatalog(await response.json());
  const memory = findMemory(game, gameImports);
  const gameTable = findTable(game, gameImports);
  const previous = new Map();
  const loaded = [];
  const source = await gameBytes(game, memory);

  try {
    for (const metadata of catalog.modules) {
      const bytes = await verifiedBytes(metadata);
      const compiled = await WebAssembly.compile(bytes);
      const context = { game, memory, gameTable, previous, log, alert };
      const { imports, state } = wireImports(compiled, context);
      const instance = await WebAssembly.instantiate(compiled, imports);
      state.instance = instance;
      if (typeof instance.exports._initialize === 'function') instance.exports._initialize();
      const initialise = findExport(instance.exports, 'mod_init');
      if (typeof initialise !== 'function') {
        throw new Error(`${metadata.name} exports no mod_init`);
      }
      const callable =
        typeof WebAssembly.promising === 'function' ? WebAssembly.promising(initialise) : initialise;
      await callable(source.pointer, source.length);
      for (const [name, value] of Object.entries(instance.exports)) {
        if (!previous.has(name)) previous.set(name, value);
      }
      loaded.push(Object.freeze({
        name: metadata.name,
        sha256: metadata.sha256,
        instance,
      }));
      log(`mod loaded: ${metadata.name} (${metadata.sha256.slice(0, 12)})`);
    }
  } finally {
    source.release();
  }
  return Object.freeze(loaded);
}

export function installModRuntime(options) {
  let loaded = null;
  const load = () => loaded ??= loadSelectedMods(options);
  window.GwInject = Object.freeze({
    load,
    selected: () => loaded,
  });
  return load();
}
