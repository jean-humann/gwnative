import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  decodeCompanionManifest,
  decodeEnhancementManifest,
} from './enhancement-manifest.js';

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

function moduleWithCustomSection(nameValue, manifest) {
  const name = encoder.encode(nameValue);
  const json = encoder.encode(JSON.stringify(manifest));
  const body = Uint8Array.from([...uleb(name.length), ...name, ...json]);
  return new WebAssembly.Module(Uint8Array.from([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    0x00, ...uleb(body.length), ...body,
  ]));
}

function moduleWithManifest(overrides = {}) {
  return moduleWithCustomSection('enhancement_manifest', {
    transformAbi: 16,
    snapshotAbi: 15,
    snapshotBytes: 60_576,
    cursorSnapshotAbi: 1,
    cursorSnapshotBytes: 4_160,
    configBytes: 928,
    programId: 1,
    buildId: 38_795,
    tableSlot: 0,
    layoutWords: Array(232).fill(0),
    ...overrides,
  });
}

function moduleWithCompanionManifest(overrides = {}) {
  return moduleWithCustomSection('companion_manifest', {
    relocationAbi: 1,
    workspaceBytes: 1_053_184,
    stackBytes: 1_048_576,
    dataOffset: 1_048_576,
    dataBytes: 2_044,
    ...overrides,
  });
}

describe('enhancement manifest', () => {
  it('accepts exactly the host and companion ABI', () => {
    const manifest = decodeEnhancementManifest(moduleWithManifest());
    assert.equal(manifest.transformAbi, 16);
    assert.equal(manifest.snapshotAbi, 15);
    assert.equal(manifest.snapshotBytes, 60_576);
    assert.equal(manifest.configBytes, 928);
    assert.equal(manifest.layoutWords.length, 232);
    assert(Object.isFrozen(manifest));
  });

  it('requires the certified shared table slot', () => {
    assert.equal(
      decodeEnhancementManifest(moduleWithManifest({ tableSlot: -1 })),
      null,
    );
    assert.equal(
      decodeEnhancementManifest(moduleWithManifest({ tableSlot: 1.5 })),
      null,
    );
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

describe('companion relocation manifest', () => {
  it('accepts a bounded stack, data and BSS allocation', () => {
    const manifest = decodeCompanionManifest(moduleWithCompanionManifest());
    assert.equal(manifest.relocationAbi, 1);
    assert.equal(manifest.workspaceBytes, 1_053_184);
    assert.equal(manifest.dataBytes, 2_044);
    assert(Object.isFrozen(manifest));
  });

  it('refuses a fixed-base or out-of-bounds companion before instantiation', () => {
    assert.equal(decodeCompanionManifest(moduleWithManifest()), null);
    assert.equal(
      decodeCompanionManifest(moduleWithCompanionManifest({
        dataBytes: 8_192,
      })),
      null,
    );
    assert.equal(
      decodeCompanionManifest(moduleWithCompanionManifest({
        workspaceBytes: 3 * 1_048_576,
      })),
      null,
    );
  });
});
