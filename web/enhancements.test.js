import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { decodeEnhancementManifest } from './enhancement-manifest.js';

const encoder = new TextEncoder();

function uleb(value) {
  const bytes = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value !== 0) byte |= 0x80;
    bytes.push(byte);
  } while (value !== 0);
  return bytes;
}

function moduleWithManifest(overrides = {}) {
  const manifest = {
    transformAbi: 12,
    snapshotAbi: 9,
    snapshotBytes: 49_064,
    cursorSnapshotAbi: 1,
    cursorSnapshotBytes: 4_160,
    configBytes: 728,
    programId: 1,
    buildId: 38_795,
    tableSlot: 0,
    layoutWords: Array(182).fill(0),
    ...overrides,
  };
  const name = encoder.encode('enhancement_manifest');
  const json = encoder.encode(JSON.stringify(manifest));
  const body = Uint8Array.from([...uleb(name.length), ...name, ...json]);
  return new WebAssembly.Module(Uint8Array.from([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    0x00, ...uleb(body.length), ...body,
  ]));
}

describe('enhancement manifest', () => {
  it('accepts exactly the host and companion ABI', () => {
    const manifest = decodeEnhancementManifest(moduleWithManifest());
    assert.equal(manifest.transformAbi, 12);
    assert.equal(manifest.snapshotAbi, 9);
    assert.equal(manifest.snapshotBytes, 49_064);
    assert.equal(manifest.configBytes, 728);
    assert.equal(manifest.layoutWords.length, 182);
    assert(Object.isFrozen(manifest));
  });

  it('refuses a stale layout shape before the companion is installed', () => {
    assert.equal(
      decodeEnhancementManifest(moduleWithManifest({
        configBytes: 516,
        layoutWords: Array(129).fill(0),
      })),
      null,
    );
  });
});
