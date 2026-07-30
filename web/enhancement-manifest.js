// Decode the build-pinned contract carried inside an enhanced client module.
//
// Kept independent of the DOM-backed cursor consumer so the whole host/page/
// companion ABI can be tested without constructing a browser surface.

import {
  COMPANION_CURSOR_ABI,
  COMPANION_CURSOR_BYTES,
  COMPANION_SNAPSHOT_ABI,
  COMPANION_SNAPSHOT_BYTES,
} from './companion-snapshot.js';

const ENHANCEMENT_TRANSFORM_ABI = 16;
const ENHANCEMENT_LAYOUT_WORDS = 232;
const COMPANION_RELOCATION_ABI = 1;
const COMPANION_STACK_BYTES = 1_048_576;
const MAX_COMPANION_WORKSPACE_BYTES = 2 * 1_048_576;

/**
 * The manifest the host wrote into the module, or `null` if it is not one this
 * page can act on.
 *
 * @param {WebAssembly.Module} module
 */
export function decodeEnhancementManifest(module) {
  const sections = WebAssembly.Module.customSections(module, 'enhancement_manifest');
  if (sections.length !== 1) return null;
  try {
    const value = JSON.parse(new TextDecoder().decode(sections[0]));
    if (
      value?.transformAbi !== ENHANCEMENT_TRANSFORM_ABI
      || value?.snapshotAbi !== COMPANION_SNAPSHOT_ABI
      || value?.snapshotBytes !== COMPANION_SNAPSHOT_BYTES
      || value?.cursorSnapshotAbi !== COMPANION_CURSOR_ABI
      || value?.cursorSnapshotBytes !== COMPANION_CURSOR_BYTES
      || !Number.isSafeInteger(value?.buildId)
      || value.buildId <= 0
      || !Number.isSafeInteger(value?.programId)
      || value.programId <= 0
      || !Number.isSafeInteger(value?.tableSlot)
      || value.tableSlot < 0
      || !Array.isArray(value?.layoutWords)
      || value.layoutWords.length !== ENHANCEMENT_LAYOUT_WORDS
      || value.layoutWords.some(
        (/** @type {unknown} */ word) =>
          !Number.isInteger(word)
          || Number(word) < 0
          || Number(word) > 0xffff_ffff,
      )
      // The companion is handed a byte count and reads that many words out of
      // it, so the two have to be the same statement.
      || value?.configBytes !== value.layoutWords.length * Uint32Array.BYTES_PER_ELEMENT
    ) {
      return null;
    }
    return Object.freeze(value);
  } catch {
    return null;
  }
}

/**
 * The allocation contract carried by the relocatable companion module.
 *
 * The kernel imports its stack, memory and data bases. These bounds are decoded
 * before the game allocates anything for it, so a stale or ordinary
 * `--import-memory` module is refused before instantiation can initialise data
 * over the client's own address zero.
 *
 * @param {WebAssembly.Module} module
 */
export function decodeCompanionManifest(module) {
  const sections = WebAssembly.Module.customSections(module, 'companion_manifest');
  if (sections.length !== 1) return null;
  try {
    const value = JSON.parse(new TextDecoder().decode(sections[0]));
    if (
      value?.relocationAbi !== COMPANION_RELOCATION_ABI
      || value?.stackBytes !== COMPANION_STACK_BYTES
      || value?.dataOffset !== value.stackBytes
      || !Number.isSafeInteger(value?.workspaceBytes)
      || value.workspaceBytes <= value.stackBytes
      || value.workspaceBytes > MAX_COMPANION_WORKSPACE_BYTES
      || value.workspaceBytes % 16 !== 0
      || !Number.isSafeInteger(value?.dataBytes)
      || value.dataBytes <= 0
      || value.dataOffset + value.dataBytes > value.workspaceBytes
    ) {
      return null;
    }
    return Object.freeze(value);
  } catch {
    return null;
  }
}
