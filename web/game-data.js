// The game image, as three things the page can ask the host about it.
//
// How much of it is on this Mac, start or stop fetching the rest, and throw it
// away. They live together rather than inside launcher.js because the launcher
// is only the first caller: the settings panel drives the same sweep after boot,
// and a player who wants the disk space back is asking about the same 4.2 GB the
// launcher offered to download.
//
// Nothing here decides anything. Each function is a round trip and a shape; who
// may ask, and what a refusal means, is the caller's business.

const headers = () => ({ 'X-Gwnative-Token': window.__gwnativeToken ?? '' });

/**
 * How much of the image is cached, and whether a sweep is running.
 *
 * `{ cached, total, fetched, running, chunkSize, free, needed, outstanding }`,
 * or a throw. A build with no snapshot store has no route to answer, so the
 * throw is the ordinary way of finding out there is nothing to report.
 *
 * @returns {Promise<Record<string, number | boolean | null>>}
 */
export async function snapshotProgress() {
  const response = await fetch('__prefetch', { headers: headers() });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

/**
 * Start or stop the background sweep. Returns the same progress shape.
 *
 * @param {'start' | 'stop'} action
 */
export async function sweepSnapshot(action) {
  const response = await fetch(action === 'stop' ? '__prefetch?stop' : '__prefetch', {
    method: 'POST',
    headers: headers(),
  });
  if (!response.ok) {
    // The host explains a refusal in the body — "not enough room" is the one
    // worth repeating to the player rather than reporting as a status code.
    const detail = await response.json().catch(() => null);
    throw new Error(detail?.error ?? `HTTP ${response.status}`);
  }
  return response.json();
}

/**
 * Ask for the cache to be deleted.
 *
 * It is armed rather than done. The store this launch is reading holds a
 * readahead thread, a prefetch thread and up to forty-eight fetches with open
 * descriptors; deleting the directory out from under all of that is a crash
 * with extra steps. The host writes a marker beside the directory and consumes
 * it at the next launch, before the store is opened — so this resolving means
 * the request is recorded, and the restart is what carries it out.
 *
 * @returns {Promise<void>}
 */
export async function clearGameData() {
  const response = await fetch('__data', { method: 'DELETE', headers: headers() });
  if (!response.ok) {
    throw new Error((await response.text()) || `the game data was not cleared (${response.status})`);
  }
}
