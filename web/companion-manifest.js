import {
  COMPANION_CURSOR_ABI,
  COMPANION_CURSOR_BYTES,
  COMPANION_SNAPSHOT_ABI,
  COMPANION_SNAPSHOT_BYTES,
} from './companion-snapshot.js';

const COMPANION_LAYOUT_WORDS = 232;

/**
 * Validate the signed manifest the native host selected.
 *
 * The page and host are versioned together. A disagreement is therefore an
 * unsupported build, never a field to default: guessing would point the
 * companion at uncertified offsets in live game memory.
 *
 * @param {unknown} candidate
 */
export function decodeCompanionManifest(candidate) {
  try {
    const value = candidate;
    if (
      value?.snapshotAbi !== COMPANION_SNAPSHOT_ABI
      || value?.snapshotBytes !== COMPANION_SNAPSHOT_BYTES
      || value?.cursorSnapshotAbi !== COMPANION_CURSOR_ABI
      || value?.cursorSnapshotBytes !== COMPANION_CURSOR_BYTES
      || typeof value?.familyId !== 'string'
      || !/^[0-9a-f]{64}$/.test(value.familyId)
      || !Array.isArray(value?.layoutWords)
      || value.layoutWords.length !== COMPANION_LAYOUT_WORDS
      || value.layoutWords.some(
        (/** @type {unknown} */ word) =>
          !Number.isInteger(word)
          || Number(word) < 0
          || Number(word) > 0xffff_ffff,
      )
      || value?.configBytes !== value.layoutWords.length * Uint32Array.BYTES_PER_ELEMENT
    ) {
      return null;
    }
    return Object.freeze({
      ...value,
      layoutWords: Object.freeze([...value.layoutWords]),
    });
  } catch {
    return null;
  }
}
