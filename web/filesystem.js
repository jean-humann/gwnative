// Persistent game filesystem.
//
// The client mounts IDBFS at `app:` itself, but only from main(), after preRun.
// By then the WASM can open and write files while having no mkdir import and a
// working directory still pointing at the ephemeral MEMFS root. So the mount is
// taken over here, before main(), which makes every relative game file durable
// and guarantees the template directories exist up front.

const MOUNT = 'app:';
const DEPENDENCY = 'gw-persistent-filesystem';
const SYNC_TIMEOUT_MS = 30_000;
const REQUIRED_DIRECTORIES = [
  `${MOUNT}/Templates/Skills`,
  `${MOUNT}/Templates/Equipment`,
];

/** The client is a Windows program at heart and hands down backslash paths. */
const normalize = (file) =>
  typeof file === 'string' && file.includes('\\')
    ? file.replace(/^\\+/, '').replaceAll('\\', '/')
    : file;

/**
 * Normalize at each public operation rather than only at lookupPath: rename and
 * unlink take a basename from their original argument after the lookup, which
 * would otherwise put the backslashes straight back.
 */
function installPathNormalization(fs) {
  const wrapFirst = (name) => {
    const original = fs[name].bind(fs);
    fs[name] = (file, ...rest) => original(normalize(file), ...rest);
  };
  const wrapPair = (name) => {
    const original = fs[name].bind(fs);
    fs[name] = (from, to) => original(normalize(from), normalize(to));
  };
  for (const name of ['lookupPath', 'open', 'unlink', 'mknod', 'mkdir', 'mkdirTree', 'rmdir']) {
    wrapFirst(name);
  }
  for (const name of ['rename', 'symlink']) {
    wrapPair(name);
  }
}

/**
 * IDBFS persists a file when the client closes it, so anything still open when
 * the app goes away — the account record in `Gw.dat` above all — is only in
 * memory. One last sync on the way out is the difference between remembering a
 * login and asking for it again.
 *
 * The host drives this, not the page. Nothing gwnative does unloads the
 * document: closing the window does not deallocate it, and quitting sends
 * `terminate:`, which kills the web content process rather than unloading it.
 * So `pagehide` is registered because it costs nothing and covers a reload, but
 * it is not the path that matters — `applicationShouldTerminate:` in `src/app.rs`
 * is, and it calls the function published here and waits for the answer.
 */
function installShutdownFlush(sync, log) {
  let running = null;
  const flush = () => {
    // Idempotent: the host asks once, but a reload arriving mid-quit would
    // otherwise start a second sync against the same IndexedDB transaction.
    running ??= sync(false)
      .then(() => true)
      .catch((error) => {
        log('fs: the closing sync did not finish:', error);
        return false;
      })
      .finally(() => {
        running = null;
      });
    return running;
  };
  globalThis.gwFlushFilesystem = flush;
  globalThis.addEventListener('pagehide', flush);
}

/**
 * @param {{
 *   module: { addRunDependency(name: string): void,
 *             removeRunDependency(name: string): void,
 *             preRun?: () => void },
 *   failed(error: unknown): void,
 *   log(...values: unknown[]): void,
 * }} options
 */
export function installGameFilesystem({ module, failed, log }) {
  module.preRun = () => {
    module.addRunDependency(DEPENDENCY);
    const fs = globalThis.FS;
    const idbfs = globalThis.IDBFS;
    let finished = false;

    const stop = (error) => {
      if (finished) return;
      finished = true;
      failed(error);
    };
    const ready = () => {
      if (finished) return;
      finished = true;
      log('persistent filesystem ready');
      module.removeRunDependency(DEPENDENCY);
    };

    // syncfs can hang on a wedged IndexedDB, and a boot that never fails is
    // worse than one that reports why.
    const sync = (populate) =>
      new Promise((resolve, reject) => {
        const timer = setTimeout(
          () => reject(new Error('persistent filesystem sync timed out')),
          SYNC_TIMEOUT_MS,
        );
        fs.syncfs(populate, (error) => {
          clearTimeout(timer);
          error ? reject(error) : resolve();
        });
      });

    (async () => {
      if (fs.analyzePath(MOUNT).error) {
        fs.mkdir(MOUNT);
        fs.mount(idbfs, { autoPersist: true }, MOUNT);
      }
      await sync(true);
      for (const directory of REQUIRED_DIRECTORIES) fs.mkdirTree(directory);
      fs.chdir(MOUNT);
      // Persist the directory invariant before the game can write a template,
      // screenshot, or chat log beneath it.
      await sync(false);
      installPathNormalization(fs);
      installShutdownFlush(sync, log);
      ready();
    })().catch(stop);
  };
}
