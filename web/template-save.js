// Page half of the derived-client bridge in `src/wasm.rs`.
//
// Four `Base/Os` file routines ship unimplemented in ArenaNet's build: creating
// a directory always fails, enumerating one does nothing, deriving a name from
// an entry writes nothing, and deleting a file aborts the client. A fifth is
// implemented but wrong — the probe that asks whether a name is taken opens the
// file, so it creates the file it is testing for. The derived module forwards
// all five to `__syscall_newfstatat` behind dirfd markers no real call can
// produce, and they are answered here against the mounted IDBFS.
//
// The markers arrive from the host rather than being restated here, so the side
// that writes them into the module and the side that answers them cannot drift.

import * as diagnostics from './diagnostics.js';

/**
 * The client's directory-entry record: a 24-byte header it never reads, then
 * WCHAR name[260]. Callers stride by the whole record and free the block, so it
 * has to come from the client's own allocator.
 */
const RECORD_BYTES = 544;
const RECORD_NAME_OFFSET = 24;
const NAME_LIMIT = 260;

const PATH_LIMIT = 260;
const ERROR_NOT_FOUND = 2;

// Entry kinds the client asks for. `Templates/Skills/*.txt` is requested with
// FILES and `Templates/Skills/*` with DIRECTORIES, so answering both with files
// fills the subdirectory list with phantom folders.
const FIND_FILES_FLAG = 1;
const FIND_DIRECTORIES_FLAG = 2;

/** @returns {import('./filesystem.js').EmscriptenFileSystem | null} */
const filesystem = () => globalThis.FS ?? null;

/**
 * Read a NUL-terminated UTF-16 string out of the client's heap.
 *
 * @param {{ HEAPU8?: Uint8Array }} module
 * @param {number} pointer
 */
function readWide(module, pointer) {
  const bytes = module.HEAPU8;
  if (!bytes || pointer <= 0 || (pointer & 1) !== 0) return null;
  const heap = new Uint16Array(bytes.buffer);
  const start = pointer >>> 1;
  const limit = Math.min(heap.length, start + PATH_LIMIT);
  let end = start;
  while (end < limit && heap[end] !== 0) end += 1;
  // No terminator inside the limit means this is not a path we can trust.
  if (end === limit) return null;
  let value = '';
  for (let index = start; index < end; index += 1) {
    value += String.fromCharCode(heap[index] ?? 0);
  }
  return value;
}

/**
 * @param {{ HEAPU8?: Uint8Array }} module
 * @param {number} pointer
 * @param {string} value
 * @param {number} limit Buffer size in characters, including the terminator.
 */
function writeWide(module, pointer, value, limit) {
  const bytes = module.HEAPU8;
  if (!bytes || pointer <= 0 || (pointer & 1) !== 0 || limit < 1) return false;
  const heap = new Uint16Array(bytes.buffer);
  const start = pointer >>> 1;
  const count = Math.min(value.length, limit - 1);
  if (start + count >= heap.length) return false;
  for (let index = 0; index < count; index += 1) {
    heap[start + index] = value.charCodeAt(index);
  }
  heap[start + count] = 0;
  return true;
}

/**
 * Everything the client passes is relative to the mount. `_wsplitpath` and
 * `_wmakepath` both keep the separator that ends a directory component, so a
 * directory always arrives as `Templates/Skills/`.
 *
 * Runs of separators collapse the way `PATH.normalize` collapses them: the
 * client joins its type directory with `/` onto a record name that already
 * begins with `\`, so every operation on a listed template arrives as
 * `Templates/Skills/\Test.txt` — a redundant separator, not a traversal.
 *
 * @param {string} value
 */
function normalize(value) {
  const trimmed = value
    .replaceAll('\\', '/')
    .replace(/\/{2,}/g, '/')
    .replace(/^\/+/, '')
    .replace(/\/+$/, '');
  const relative = trimmed.startsWith('app:/') ? trimmed.slice(5) : trimmed;
  if (!relative) return null;
  // Keep the client inside its own mount: no absolute paths, no traversal.
  const parts = relative.split('/');
  return parts.every((part) => part && part !== '.' && part !== '..')
    ? relative
    : null;
}

/** @param {string} glob */
function globToRegExp(glob) {
  const source = glob
    .replaceAll(/[.+^${}()|[\]\\]/g, String.raw`\$&`)
    .replaceAll('*', '[^/]*')
    .replaceAll('?', '[^/]');
  return new RegExp(`^${source}$`, 'i');
}

/**
 * The client keys a template on its path below the type directory, in Windows
 * form with a leading separator: `\Test`, or `\Sub\Test` one level down. Its own
 * save path builds exactly that, and the list filter matches records against the
 * current subdirectory prefix — so a bare `Test` never matches.
 *
 * The extension comes off here rather than being left to the client's
 * `Path::RemoveExtension`, which is off by one in this build and takes the last
 * character of the name with it: `\Test.txt` becomes `\Tes`. Handing it a name
 * with no extension leaves it nothing to trim.
 *
 * @param {string} name
 */
function internalPath(name) {
  const relative = name.replaceAll('/', '\\').replace(/^\\+/, '');
  const separator = relative.lastIndexOf('\\');
  const dot = relative.lastIndexOf('.');
  const bare = dot > separator + 1 ? relative.slice(0, dot) : relative;
  return `\\${bare}`;
}

/**
 * @param {import('./filesystem.js').EmscriptenFileSystem} fs
 * @param {string} directory
 * @param {RegExp} pattern
 * @param {number} flags
 */
function matchingEntries(fs, directory, pattern, flags) {
  const wantFiles = (flags & FIND_FILES_FLAG) !== 0;
  const wantDirectories = (flags & FIND_DIRECTORIES_FLAG) !== 0;
  const found = [];
  for (const entry of fs.readdir(directory)) {
    if (entry === '.' || entry === '..') continue;
    if (!pattern.test(entry) || entry.length >= NAME_LIMIT) continue;
    try {
      const { mode } = fs.stat(`${directory}/${entry}`);
      if (fs.isDir(mode) ? wantDirectories : wantFiles) found.push(entry);
    } catch {
      // An entry the mount cannot describe is left out, the way a directory
      // read degrades. The client has no way to report it.
    }
  }
  return found.sort();
}

/**
 * Hook the carrier import so the derived module's forwarders are answered.
 *
 * A no-op when the module is not the derived one: the markers never arrive, so
 * the wrapper only ever forwards to the real `__syscall_newfstatat`.
 *
 * @param {{
 *   imports: { env?: Record<string, Function> },
 *   module: { HEAPU8?: Uint8Array },
 *   exports: () => { malloc?: (bytes: number) => number } | null | undefined,
 *   log: (...values: unknown[]) => void,
 * }} options
 */
export function installTemplateSave({ imports, module, exports, log }) {
  const env = imports.env;
  const carrier = env?.__syscall_newfstatat;
  const markers = window.__gwnativeBridgeMarkers;
  if (!env || typeof carrier !== 'function' || !markers) {
    log('[warn] template save unavailable: no bridge carrier');
    return;
  }

  /** @param {number} path */
  const ensureDirectory = (path) => {
    const raw = readWide(module, path);
    const directory = raw === null ? null : normalize(raw);
    const fs = filesystem();
    if (!directory || !fs) return ERROR_NOT_FOUND;
    try {
      fs.mkdirTree(directory);
      return 0;
    } catch (error) {
      return typeof error?.errno === 'number' ? error.errno : ERROR_NOT_FOUND;
    }
  };

  /**
   * @param {number} path Wildcard such as `Templates/Skills/*`.
   * @param {number} out Zeroed list header: entries at +0, count at +8.
   * @param {number} flags Which entry kinds the caller wants.
   */
  const findFiles = (path, out, flags) => {
    const bytes = module.HEAPU8;
    const raw = readWide(module, path);
    const pattern = raw === null ? null : normalize(raw);
    const fs = filesystem();
    if (!bytes || !pattern || !fs || out <= 0 || (out & 3) !== 0) return 0;

    const cut = pattern.lastIndexOf('/');
    const directory = cut < 0 ? '.' : pattern.slice(0, cut);
    const glob = cut < 0 ? pattern : pattern.slice(cut + 1);
    let names;
    try {
      names = matchingEntries(fs, directory, globToRegExp(glob), flags);
    } catch {
      return 0;
    }
    if (names.length === 0) return 0;

    const malloc = exports()?.malloc;
    if (typeof malloc !== 'function') return 0;
    const entries = malloc(names.length * RECORD_BYTES);
    if (!entries) return 0;

    // malloc can grow memory, so every view is taken after it returns.
    const heap = module.HEAPU8;
    if (!heap) return 0;
    heap.fill(0, entries, entries + names.length * RECORD_BYTES);
    names.forEach((name, index) => {
      writeWide(
        module,
        entries + index * RECORD_BYTES + RECORD_NAME_OFFSET,
        name,
        NAME_LIMIT,
      );
    });
    const words = new Uint32Array(heap.buffer);
    words[out >>> 2] = entries;
    words[(out >>> 2) + 2] = names.length;
    diagnostics.count('gw.template.listed', names.length);
    return 0;
  };

  /**
   * @param {number} path A directory entry name from findFiles.
   * @param {number} destination
   * @param {number} limit
   */
  const fileBaseName = (path, destination, limit) => {
    const raw = readWide(module, path);
    if (raw === null) return 0;
    const written = writeWide(
      module,
      destination,
      internalPath(raw),
      Math.min(limit, PATH_LIMIT),
    );
    return written ? 1 : 0;
  };

  /**
   * The client's own delete is `assert("not implemented")` followed by
   * `unreachable`, so this is the difference between deleting a build and
   * aborting the client. Its caller treats a non-zero result as success.
   *
   * @param {number} path
   */
  const deleteFile = (path) => {
    const raw = readWide(module, path);
    const file = raw === null ? null : normalize(raw);
    const fs = filesystem();
    if (!file || !fs) return 0;
    try {
      fs.unlink(file);
      diagnostics.count('gw.template.deleted');
      return 1;
    } catch {
      return 0;
    }
  };

  /**
   * `File::Open` mode 1 is meant to open an existing file, but in this build it
   * opens `O_RDWR | O_CREAT` like the write mode — so the client's own "is this
   * name taken?" probe creates the file it is testing for and every rename is
   * refused. Answer the question without touching the file.
   *
   * An undecidable path answers "taken", which refuses a rename rather than
   * silently overwriting a template.
   *
   * @param {number} path
   */
  const fileExists = (path) => {
    const raw = readWide(module, path);
    const file = raw === null ? null : normalize(raw);
    const fs = filesystem();
    if (!file || !fs) return 1;
    return fs.analyzePath(file).exists ? 1 : 0;
  };

  const answer = new Map([
    [markers.ensureDirectory, (path) => ensureDirectory(path)],
    [markers.findFiles, (path, buffer, flags) => findFiles(path, buffer, flags)],
    [markers.fileBaseName, (path, buffer, flags) => fileBaseName(path, buffer, flags)],
    [markers.deleteFile, (path) => deleteFile(path)],
    [markers.fileExists, (path) => fileExists(path)],
  ]);

  env.__syscall_newfstatat = function (dirfd, path, buffer, flags) {
    const handler = answer.get(dirfd);
    if (!handler) return carrier.call(this, dirfd, path, buffer, flags);
    try {
      return handler(path, buffer, flags);
    } catch (error) {
      // Never let this throw back into the client: an exception crossing the
      // WebAssembly boundary aborts it, which is the failure this whole
      // transform exists to remove.
      log('[warn] template save bridge failed:', error);
      diagnostics.count('gw.template.failed');
      return 0;
    }
  };

  log('template save bridge installed');
}
