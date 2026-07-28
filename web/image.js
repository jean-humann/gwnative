// Module.image — the client's view of Gw.snapshot.
//
// The host serves the snapshot as a ranged file over the loopback origin and
// owns chunk caching, deduplication, and concurrency. So this is only an
// adapter: translate the client's read into a Range request and write the
// result into the WASM heap. The chunk bookkeeping an in-page cache would need
// lives in Rust instead.

/** Only the snapshot is backed. `image` is nominally a filesystem over the whole
 *  manifest, and handing back a handle for anything else makes `fileSize` answer
 *  4.2 GB for a small .ini, which the client then tries to allocate for. */
const SNAPSHOT = /(^|[/\\])Gw\.snapshot$/i;

/**
 * @param {{
 *   size: number,
 *   chunkSize: number,
 *   resident: Uint8Array,
 *   writeBytes(data: Uint8Array, address: number): void,
 *   log(...values: unknown[]): void,
 * }} options
 */
export function createImageSource({ size, chunkSize, resident, writeBytes, log }) {
  const handles = new Set();
  let nextHandle = 1;
  const stats = { reads: 0, bytes: 0, warmed: 0 };

  const chunkRange = (offset, length) => [
    Math.floor(offset / chunkSize),
    Math.floor((offset + Math.max(length, 1) - 1) / chunkSize),
  ];
  const isResident = (i) => (resident[i >> 3] & (1 << (i & 7))) !== 0;
  const markResident = (i) => { resident[i >> 3] |= 1 << (i & 7); };

  async function fetchRange(offset, bytes) {
    const response = await fetch('Gw.snapshot', {
      headers: { Range: `bytes=${offset}-${offset + bytes - 1}` },
    });
    if (response.status !== 206) {
      throw new Error(`snapshot read ${offset}+${bytes}: HTTP ${response.status}`);
    }
    const data = new Uint8Array(await response.arrayBuffer());
    if (data.length !== bytes) {
      throw new Error(`snapshot read ${offset}+${bytes}: got ${data.length}`);
    }
    const [first, last] = chunkRange(offset, bytes);
    for (let i = first; i <= last; i++) markResident(i);
    return data;
  }

  /**
   * Warming asks the host to have a chunk, and wants nothing back.
   *
   * It used to be `fetchRange` with the result dropped, which is the same
   * request with 256 KiB attached: over the loopback socket, through
   * `arrayBuffer()`, into a `Uint8Array`, and straight into the garbage. A quick
   * launch warms some 1,800 chunks in twenty seconds, faster than the collector
   * reclaims them, and that is most of the gap between a renderer that settles
   * at 400 MB and one whose footprint peaks at 1.7 GB on the way there.
   */
  async function warmRange(offset, bytes) {
    const response = await fetch(`__warm?offset=${offset}&bytes=${bytes}`, {
      headers: { 'X-Gwnative-Token': window.__gwnativeToken ?? '' },
    });
    if (!response.ok) throw new Error(`snapshot warm ${offset}+${bytes}: HTTP ${response.status}`);
    const [first, last] = chunkRange(offset, bytes);
    for (let i = first; i <= last; i++) markResident(i);
  }

  const image = {
    open(path) {
      if (!SNAPSHOT.test(path)) return 0;
      const handle = nextHandle++;
      handles.add(handle);
      return handle;
    },

    // Synchronous by contract, which is why the size is fetched before the glue
    // loads rather than discovered on first read.
    fileSize(handle) {
      if (!handles.has(handle)) {
        log('[warn] image.fileSize on unknown handle', handle);
        return 0;
      }
      return size;
    },

    close(handle) {
      handles.delete(handle);
    },

    async readAsync(handle, offset, _unused, buffer, bytes) {
      if (!handles.has(handle)) throw new Error(`bad image handle ${handle}`);
      const data = await fetchRange(offset, bytes);
      stats.reads++;
      stats.bytes += bytes;
      writeBytes(data, buffer);
    },

    // Answered from the host's on-disk residency, seeded at boot. Being wrong
    // in the conservative direction only costs a redundant prefetch, which the
    // host serves from cache anyway.
    isCached(handle, offset, length) {
      const [first, last] = chunkRange(offset, length);
      for (let i = first; i <= last; i++) if (!isResident(i)) return 0;
      return 1;
    },

    async cacheAsync(handle, offset, length, progress) {
      const [first, last] = chunkRange(offset, length);
      for (let i = first; i <= last; i++) {
        if (isResident(i)) continue;
        const start = i * chunkSize;
        stats.warmed++;
        await warmRange(start, Math.min(chunkSize, size - start));
        try {
          progress(1);
        } catch (error) {
          log('[warn] cache progress callback threw:', error);
        }
      }
    },
  };

  return { image, stats: () => ({ ...stats }) };
}

/** Size and chunk layout have to be known before the glue loads. */
export async function loadSnapshotMetadata() {
  const head = await fetch('Gw.snapshot', { method: 'HEAD' });
  if (!head.ok) throw new Error(`snapshot metadata: HTTP ${head.status}`);
  const size = Number(head.headers.get('Content-Length'));
  if (!Number.isSafeInteger(size) || size <= 0) {
    throw new Error('snapshot metadata: no usable Content-Length');
  }

  const residency = await fetch('__resident', {
    headers: { 'X-Gwnative-Token': window.__gwnativeToken ?? '' },
  });
  if (!residency.ok) throw new Error(`snapshot residency: HTTP ${residency.status}`);
  const chunkSize = Number(residency.headers.get('X-Chunk-Size'));
  const resident = new Uint8Array(await residency.arrayBuffer());
  return { size, chunkSize, resident };
}
