import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

const context = {
  createImageData(width, height) {
    return { data: new Uint8ClampedArray(width * height * 4) };
  },
  putImageData() {},
};

globalThis.document = {
  createElement(name) {
    assert.equal(name, 'canvas');
    return {
      width: 0,
      height: 0,
      getContext(kind) {
        assert.equal(kind, '2d');
        return context;
      },
      toDataURL(kind) {
        assert.equal(kind, 'image/png');
        return 'data:image/png;base64,cursor';
      },
    };
  },
};

const { buildCursorCss } = await import('./enhancement-cursor.js');

describe('game cursor presentation', () => {
  it('gives WebKit one stable cursor URL rather than a rebuilt image-set', () => {
    const css = buildCursorCss(new Uint8ClampedArray(32 * 32 * 4), 3, 7);

    assert.equal(
      css,
      'url("data:image/png;base64,cursor") 3 7, default',
    );
    assert.equal(css.includes('image-set('), false);
  });
});
