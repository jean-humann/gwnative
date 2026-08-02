import assert from 'node:assert/strict';
import { before, describe, it } from 'node:test';

let installGraphics;

before(async () => {
  const interval = globalThis.setInterval;
  globalThis.setInterval = (...args) => interval(...args).unref();
  globalThis.addEventListener ??= () => {};
  globalThis.window = globalThis;
  ({ installGraphics } = await import('./graphics.js'));
});

const environment = () => ({
  eglCreateContext: () => 1,
  eglSwapBuffers: () => 1,
});

describe('graphics bridge', () => {
  it('leaves the game context attributes unchanged by default', () => {
    const attributes = { alpha: false };
    const canvas = {
      getContext: (_kind, actual) => actual,
    };
    const env = environment();
    installGraphics({
      env,
      canvas,
      renderScale: () => 1,
      firstFrame() {},
      log() {},
    });

    assert.equal(canvas.getContext('webgl2', attributes), attributes);
    assert.equal(attributes.preserveDrawingBuffer, undefined);
  });

  it('can preserve the game canvas for a controlled flash comparison', () => {
    const attributes = { alpha: false };
    const canvas = {
      getContext: (_kind, actual) => actual,
    };
    const env = environment();
    installGraphics({
      env,
      canvas,
      renderScale: () => 1,
      firstFrame() {},
      preserveDrawingBuffer: true,
      log() {},
    });

    assert.equal(canvas.getContext('webgl2', attributes), attributes);
    assert.equal(attributes.preserveDrawingBuffer, true);
  });

  it('audits every framebuffer write without counting draw-buffer selection', () => {
    const writes = [];
    const env = {
      ...environment(),
      glClear() {},
      glClearBufferfv() {},
      glDrawElementsInstanced() {},
      glBlitFramebuffer() {},
      glDrawBuffers() {},
    };
    installGraphics({
      env,
      renderScale: () => 1,
      firstFrame() {},
      audit: {
        enabled: true,
        contextCreated() {},
        draw(name) { writes.push(name); },
        swap() {},
      },
      log() {},
    });

    env.glClear();
    env.glClearBufferfv();
    env.glDrawElementsInstanced();
    env.glBlitFramebuffer();
    env.glDrawBuffers();

    assert.deepEqual(writes, [
      'glClear',
      'glClearBufferfv',
      'glDrawElementsInstanced',
      'glBlitFramebuffer',
    ]);
  });

  it('acknowledges only the first successful presentation after native activation', () => {
    const messages = [];
    globalThis.__gwnativeActivationCoverArmed = '41';
    globalThis.webkit = {
      messageHandlers: {
        gwnativeActivationCover: {
          postMessage(value) {
            messages.push(value);
          },
        },
      },
    };
    const outcomes = [0, 1, 1];
    const env = {
      eglCreateContext: () => 1,
      eglSwapBuffers: () => outcomes.shift(),
    };
    try {
      installGraphics({
        env,
        renderScale: () => 1,
        firstFrame() {},
        log() {},
      });

      assert.equal(env.eglSwapBuffers(), 0);
      assert.equal(globalThis.__gwnativeActivationCoverArmed, '41');
      assert.deepEqual(messages, []);

      assert.equal(env.eglSwapBuffers(), 1);
      assert.equal(globalThis.__gwnativeActivationCoverArmed, null);
      assert.deepEqual(messages, ['41']);

      assert.equal(env.eglSwapBuffers(), 1);
      assert.deepEqual(messages, ['41']);
    } finally {
      delete globalThis.__gwnativeActivationCoverArmed;
      delete globalThis.webkit;
    }
  });

  it('does not break presentation when the optional native bridge throws', () => {
    globalThis.__gwnativeActivationCoverArmed = '42';
    globalThis.webkit = {
      messageHandlers: {
        gwnativeActivationCover: {
          postMessage() {
            throw new Error('bridge closed');
          },
        },
      },
    };
    const env = environment();
    try {
      installGraphics({
        env,
        renderScale: () => 1,
        firstFrame() {},
        log() {},
      });
      assert.equal(env.eglSwapBuffers(), 1);
      assert.equal(globalThis.__gwnativeActivationCoverArmed, null);
    } finally {
      delete globalThis.__gwnativeActivationCoverArmed;
      delete globalThis.webkit;
    }
  });

  it('runs the finite E2E command only after Guild Wars reaches its logical swap', () => {
    const order = [];
    const env = {
      eglCreateContext: () => 1,
      eglSwapBuffers: () => {
        order.push('swap');
        return 1;
      },
    };
    installGraphics({
      env,
      renderScale: () => 1,
      firstFrame() {},
      command: () => order.push('command'),
      log() {},
    });

    assert.equal(env.eglSwapBuffers(), 1);
    assert.deepEqual(order, ['swap', 'command']);
  });
});
