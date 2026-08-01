import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { installPresentationBarrier } from './presentation-barrier.js';

const FRAMEBUFFER = 0x8d40;
const READ_FRAMEBUFFER = 0x8ca8;
const DRAW_FRAMEBUFFER = 0x8ca9;
const DRAW_FRAMEBUFFER_BINDING = 0x8ca6;
const READ_FRAMEBUFFER_BINDING = 0x8caa;
const RENDERBUFFER_BINDING = 0x8ca7;
const TEXTURE_BINDING_2D = 0x8069;
const PIXEL_UNPACK_BUFFER = 0x88ec;
const PIXEL_UNPACK_BUFFER_BINDING = 0x88ef;
const RED_BITS = 0x0d52;
const GREEN_BITS = 0x0d53;
const BLUE_BITS = 0x0d54;
const ALPHA_BITS = 0x0d55;
const DEPTH_BITS = 0x0d56;
const STENCIL_BITS = 0x0d57;
const SAMPLES = 0x80a9;
const FRAMEBUFFER_COMPLETE = 0x8cd5;
const SCISSOR_TEST = 0x0c11;

function fakeWebGL({
  alpha = false,
  antialias = false,
  depth = true,
  stencil = true,
  redBits = 8,
  greenBits = 8,
  blueBits = 8,
  alphaBits = alpha ? 8 : 0,
  depthBits = depth ? 24 : 0,
  stencilBits = stencil ? 8 : 0,
  samples = 0,
  status = FRAMEBUFFER_COMPLETE,
} = {}) {
  let serial = 0;
  const parameters = new Map([
    [DRAW_FRAMEBUFFER_BINDING, null],
    [READ_FRAMEBUFFER_BINDING, null],
    [RENDERBUFFER_BINDING, null],
    [TEXTURE_BINDING_2D, null],
    [PIXEL_UNPACK_BUFFER_BINDING, null],
    [RED_BITS, redBits],
    [GREEN_BITS, greenBits],
    [BLUE_BITS, blueBits],
    [ALPHA_BITS, alphaBits],
    [DEPTH_BITS, depthBits],
    [STENCIL_BITS, stencilBits],
    [SAMPLES, samples],
  ]);
  const enabled = new Set();
  const calls = [];
  const object = (kind) => ({ kind, serial: ++serial });
  return {
    drawingBufferWidth: 640,
    drawingBufferHeight: 360,
    calls,
    getContextAttributes: () => ({ alpha, antialias, depth, stencil }),
    getParameter: (name) => parameters.get(name) ?? null,
    createFramebuffer: () => object('framebuffer'),
    createTexture: () => object('texture'),
    createRenderbuffer: () => object('renderbuffer'),
    bindFramebuffer(target, value) {
      calls.push(['bindFramebuffer', target, value]);
      if (target === FRAMEBUFFER) {
        parameters.set(DRAW_FRAMEBUFFER_BINDING, value);
        parameters.set(READ_FRAMEBUFFER_BINDING, value);
      } else if (target === DRAW_FRAMEBUFFER) {
        parameters.set(DRAW_FRAMEBUFFER_BINDING, value);
      } else if (target === READ_FRAMEBUFFER) {
        parameters.set(READ_FRAMEBUFFER_BINDING, value);
      }
    },
    bindTexture(_target, value) {
      parameters.set(TEXTURE_BINDING_2D, value);
    },
    bindRenderbuffer(_target, value) {
      parameters.set(RENDERBUFFER_BINDING, value);
    },
    bindBuffer(target, value) {
      if (target === PIXEL_UNPACK_BUFFER) {
        parameters.set(PIXEL_UNPACK_BUFFER_BINDING, value);
      }
    },
    texParameteri() {},
    texImage2D(...args) {
      calls.push(['texImage2D', ...args]);
    },
    framebufferTexture2D() {},
    renderbufferStorage(...args) {
      calls.push(['renderbufferStorage', ...args]);
    },
    framebufferRenderbuffer() {},
    checkFramebufferStatus: () => status,
    deleteFramebuffer(value) {
      calls.push(['deleteFramebuffer', value]);
    },
    deleteTexture() {},
    deleteRenderbuffer() {},
    isEnabled: (name) => enabled.has(name),
    enable(name) {
      enabled.add(name);
    },
    disable(name) {
      enabled.delete(name);
    },
    blitFramebuffer(...args) {
      calls.push(['blitFramebuffer', ...args]);
    },
  };
}

function fixture({ install = true, scissorImports = false, ...options } = {}) {
  const listeners = new Map();
  const canvas = {
    GLctxObject: null,
    addEventListener(name, listener) {
      listeners.set(name, listener);
    },
    dispatch(name) {
      listeners.get(name)?.();
    },
  };
  const gl = fakeWebGL(options);
  const gameFramebuffers = new Map([
    [7, { name: 7 }],
    [8, { name: 8 }],
  ]);
  const env = {
    eglCreateContext() {
      canvas.GLctxObject = { version: 2, GLctx: gl };
      return 3;
    },
    eglSwapBuffers: () => 1,
    glBindFramebuffer(target, name) {
      gl.bindFramebuffer(target, gameFramebuffers.get(name) ?? null);
    },
    glFramebufferTexture2D(target) {
      gl.calls.push([
        'gameFramebufferTexture2D',
        target,
        gl.getParameter(DRAW_FRAMEBUFFER_BINDING),
      ]);
    },
    emscripten_set_canvas_element_size(_target, width, height) {
      gl.drawingBufferWidth = width;
      gl.drawingBufferHeight = height;
      return 0;
    },
  };
  if (scissorImports) {
    env.glEnable = (capability) => gl.enable(capability);
    env.glDisable = (capability) => gl.disable(capability);
  }
  const logs = [];
  const barrier = install
    ? installPresentationBarrier({
      enabled: true,
      env,
      canvas,
      log: (...values) => logs.push(values.join(' ')),
    })
    : null;
  return { barrier, canvas, env, gl, logs, gameFramebuffers };
}

describe('explicit presentation barrier', () => {
  it('does not touch imports when the barrier is disabled', () => {
    const env = {
      eglCreateContext() {},
      eglSwapBuffers() {},
      glBindFramebuffer() {},
      emscripten_set_canvas_element_size() {},
    };
    const originals = { ...env };
    const barrier = installPresentationBarrier({ enabled: false, env, canvas: {}, log() {} });

    assert.equal(barrier, null);
    assert.deepEqual(env, originals);
  });

  it('fails open when a future artifact adds unvirtualized framebuffer semantics', () => {
    const { canvas, env } = fixture({ install: false });
    env.glReadBuffer = () => {};
    const originalBind = env.glBindFramebuffer;
    const logs = [];

    const barrier = installPresentationBarrier({
      enabled: true,
      env,
      canvas,
      log: (...values) => logs.push(values.join(' ')),
    });

    assert.equal(barrier, null);
    assert.equal(env.glBindFramebuffer, originalBind);
    assert.match(logs.join('\n'), /unsupported default-framebuffer import glReadBuffer/);
  });

  it('rejects every future framebuffer-attachment variant until it is translated', () => {
    const { canvas, env } = fixture({ install: false });
    env.glFramebufferRenderbuffer = () => {};
    const originalBind = env.glBindFramebuffer;
    const logs = [];

    const barrier = installPresentationBarrier({
      enabled: true,
      env,
      canvas,
      log: (...values) => logs.push(values.join(' ')),
    });

    assert.equal(barrier, null);
    assert.equal(env.glBindFramebuffer, originalBind);
    assert.match(logs.join('\n'), /unsupported default-framebuffer import glFramebufferRenderbuffer/);
  });

  it('rejects framebuffer deletion until bound-object semantics are translated', () => {
    const { canvas, env } = fixture({ install: false });
    env.glDeleteFramebuffers = () => {};
    const originalBind = env.glBindFramebuffer;
    const logs = [];

    const barrier = installPresentationBarrier({
      enabled: true,
      env,
      canvas,
      log: (...values) => logs.push(values.join(' ')),
    });

    assert.equal(barrier, null);
    assert.equal(env.glBindFramebuffer, originalBind);
    assert.match(logs.join('\n'), /unsupported default-framebuffer import glDeleteFramebuffers/);
  });

  it('redirects only logical framebuffer zero after context creation', () => {
    const { barrier, env, gl, gameFramebuffers } = fixture();
    assert.equal(env.eglCreateContext(), 3);
    assert.equal(barrier.active, true);
    // Framebuffer zero is the initial GL binding; draws before the game's first
    // explicit bind must already be isolated.
    assert.notEqual(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);

    env.glBindFramebuffer(FRAMEBUFFER, 0);
    const privateFramebuffer = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);
    assert.notEqual(privateFramebuffer, null);
    assert.equal(privateFramebuffer.name | 0, 0);

    env.glBindFramebuffer(FRAMEBUFFER, 7);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), gameFramebuffers.get(7));
    assert.equal(gl.getParameter(READ_FRAMEBUFFER_BINDING), gameFramebuffers.get(7));

    env.glBindFramebuffer(FRAMEBUFFER, 0);
    assert.equal(barrier.active, true);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), privateFramebuffer);
    assert.equal(gl.getParameter(READ_FRAMEBUFFER_BINDING), privateFramebuffer);
  });

  it('matches an opaque default framebuffer with an RGB private attachment', () => {
    const { env, gl } = fixture({ alpha: false });
    env.eglCreateContext();

    const allocation = gl.calls.findLast(([name]) => name === 'texImage2D');
    assert.equal(allocation[3], 0x8051);
    assert.equal(allocation[7], 0x1907);
  });

  it('retains RGBA when the requested default framebuffer has alpha', () => {
    const { env, gl } = fixture({ alpha: true });
    env.eglCreateContext();

    const allocation = gl.calls.findLast(([name]) => name === 'texImage2D');
    assert.equal(allocation[3], 0x8058);
    assert.equal(allocation[7], 0x1908);
  });

  it('matches a 24-bit depth-only default attachment', () => {
    const { env, gl } = fixture({ depth: true, stencil: false, depthBits: 24 });
    env.eglCreateContext();

    const allocation = gl.calls.findLast(([name]) => name === 'renderbufferStorage');
    assert.equal(allocation[2], 0x81a6);
  });

  it('fails open rather than approximating an unknown default format', () => {
    const { barrier, env, logs } = fixture({ redBits: 10 });
    env.eglCreateContext();

    assert.equal(barrier.active, false);
    assert.match(logs.join('\n'), /official direct rendering.*unsupported default framebuffer/);
  });

  it('blits a complete frame and restores independent read/draw state', () => {
    const { barrier, env, gl, gameFramebuffers } = fixture();
    env.eglCreateContext();
    env.glBindFramebuffer(READ_FRAMEBUFFER, 7);
    env.glBindFramebuffer(DRAW_FRAMEBUFFER, 8);
    gl.enable(SCISSOR_TEST);

    assert.equal(barrier.commit(1), true);
    assert.equal(gl.getParameter(READ_FRAMEBUFFER_BINDING), gameFramebuffers.get(7));
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), gameFramebuffers.get(8));
    assert.equal(gl.isEnabled(SCISSOR_TEST), true);
    assert.deepEqual(
      gl.calls.findLast(([name]) => name === 'blitFramebuffer').slice(1),
      [0, 0, 640, 360, 0, 0, 640, 360, 0x4000, 0x2600],
    );
  });

  it('tracks scissor imports and performs only two synchronous binding queries per commit', () => {
    const { barrier, env, gl } = fixture({ scissorImports: true });
    let parameterQueries = 0;
    let enabledQueries = 0;
    const getParameter = gl.getParameter;
    const isEnabled = gl.isEnabled;
    gl.getParameter = (name) => {
      parameterQueries += 1;
      return getParameter(name);
    };
    gl.isEnabled = (name) => {
      enabledQueries += 1;
      return isEnabled(name);
    };
    env.eglCreateContext();
    env.glEnable(SCISSOR_TEST);
    parameterQueries = 0;
    enabledQueries = 0;

    assert.equal(barrier.commit(1), true);

    assert.equal(parameterQueries, 2);
    assert.equal(enabledQueries, 0);
    assert.equal(gl.isEnabled(SCISSOR_TEST), true);
  });

  it('resizes private attachments without changing the logical binding', () => {
    const { barrier, env, gl } = fixture();
    env.eglCreateContext();
    env.glBindFramebuffer(FRAMEBUFFER, 0);
    const privateFramebuffer = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);

    assert.equal(env.emscripten_set_canvas_element_size(0, 1280, 720), 0);
    assert.equal(barrier.active, true);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), privateFramebuffer);
    const resize = gl.calls.findLast(([name]) => name === 'texImage2D');
    assert.equal(resize[4], 1280);
    assert.equal(resize[5], 720);
  });

  it('preserves a pixel-unpack buffer across private attachment resize', () => {
    const { env, gl } = fixture();
    env.eglCreateContext();
    const unpackBuffer = { name: 'game-unpack-buffer' };
    gl.bindBuffer(PIXEL_UNPACK_BUFFER, unpackBuffer);
    const texImage2D = gl.texImage2D;
    let unpackDuringAllocation = unpackBuffer;
    gl.texImage2D = (...args) => {
      unpackDuringAllocation = gl.getParameter(PIXEL_UNPACK_BUFFER_BINDING);
      return texImage2D(...args);
    };

    env.emscripten_set_canvas_element_size(0, 1280, 720);

    assert.equal(unpackDuringAllocation, null);
    assert.equal(gl.getParameter(PIXEL_UNPACK_BUFFER_BINDING), unpackBuffer);
  });

  it('reinitializes attachments on an equal-size canvas reset', () => {
    const { env, gl } = fixture();
    env.eglCreateContext();
    const before = gl.calls.filter(([name]) => name === 'texImage2D').length;

    env.emscripten_set_canvas_element_size(0, 640, 360);

    assert.equal(gl.calls.filter(([name]) => name === 'texImage2D').length, before + 1);
    assert.notEqual(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);
  });

  it('preserves a game-created framebuffer across private attachment resize', () => {
    const { env, gl, gameFramebuffers } = fixture();
    env.eglCreateContext();
    env.glBindFramebuffer(FRAMEBUFFER, 7);

    env.emscripten_set_canvas_element_size(0, 800, 600);

    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), gameFramebuffers.get(7));
    assert.equal(gl.getParameter(READ_FRAMEBUFFER_BINDING), gameFramebuffers.get(7));
  });

  it('preserves default-framebuffer attachment error semantics', () => {
    const { env, gl } = fixture();
    env.eglCreateContext();
    const privateFramebuffer = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);

    env.glFramebufferTexture2D(FRAMEBUFFER, 0, 0, 0, 0);

    const call = gl.calls.findLast(([name]) => name === 'gameFramebufferTexture2D');
    assert.equal(call[2], null);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), privateFramebuffer);
    assert.equal(gl.getParameter(READ_FRAMEBUFFER_BINDING), privateFramebuffer);
  });

  it('fails open before stale private pixels can cover direct future-glue rendering', () => {
    const { barrier, env, gl, logs } = fixture();
    env.eglCreateContext();
    // Bypass the wrapped import, as changed generated glue could do.
    gl.bindFramebuffer(FRAMEBUFFER, null);

    assert.equal(barrier.commit(1), false);

    assert.equal(barrier.active, false);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);
    assert.match(logs.join('\n'), /official direct rendering.*escaped the wrapped import/);
  });

  it('detects a raw-binding escape while a nonzero game framebuffer is selected', () => {
    const { barrier, env, gl, logs } = fixture();
    env.eglCreateContext();
    env.glBindFramebuffer(FRAMEBUFFER, 7);
    // Changed generated glue could issue this raw bind without updating the
    // logical name tracked by the wrapped import.
    gl.bindFramebuffer(FRAMEBUFFER, null);

    assert.equal(barrier.commit(1), false);

    assert.equal(barrier.active, false);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);
    assert.match(logs.join('\n'), /official direct rendering.*escaped the wrapped import/);
  });

  it('falls back to the official direct path on an incomplete framebuffer', () => {
    const { barrier, env, gl, logs } = fixture({ status: 0x8cd6 });
    assert.doesNotThrow(() => env.eglCreateContext());
    assert.equal(barrier.active, false);

    env.glBindFramebuffer(FRAMEBUFFER, 0);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);
    assert.match(logs.join('\n'), /official direct rendering.*0x8cd6/);
  });

  it('fails open when a future client never reaches its logical swap', (context) => {
    context.mock.timers.enable({ apis: ['setTimeout'] });
    const { barrier, env, gl, logs } = fixture();
    env.eglCreateContext();
    assert.equal(barrier.active, true);

    context.mock.timers.tick(10_000);

    assert.equal(barrier.active, false);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);
    assert.match(logs.join('\n'), /official direct rendering.*no successful logical swap/);
  });

  it('keeps isolation after a successful first logical swap', (context) => {
    context.mock.timers.enable({ apis: ['setTimeout'] });
    const { barrier, env } = fixture();
    env.eglCreateContext();

    assert.equal(barrier.commit(1), true);
    context.mock.timers.tick(10_000);

    assert.equal(barrier.active, true);
  });

  it('declines an antialiased context instead of silently changing quality', () => {
    const { barrier, env, logs } = fixture({ antialias: true });
    env.eglCreateContext();

    assert.equal(barrier.active, false);
    assert.match(logs.join('\n'), /antialiased.*unsupported/);
  });

  it('recreates private resources after a context restoration event', () => {
    const { barrier, canvas, env, gl } = fixture();
    env.eglCreateContext();
    env.glBindFramebuffer(FRAMEBUFFER, 0);
    const before = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);

    canvas.dispatch('webglcontextlost');
    env.glBindFramebuffer(FRAMEBUFFER, 0);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);
    canvas.dispatch('webglcontextrestored');
    env.glBindFramebuffer(FRAMEBUFFER, 0);

    assert.equal(barrier.active, true);
    assert.notEqual(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), before);
  });

  it('fails open if a browser blit throws', () => {
    const { barrier, env, gl, logs } = fixture();
    env.eglCreateContext();
    env.glBindFramebuffer(FRAMEBUFFER, 0);
    gl.blitFramebuffer = () => {
      throw new Error('synthetic blit failure');
    };

    assert.equal(barrier.commit(1), false);
    assert.equal(barrier.active, false);
    assert.equal(gl.getParameter(DRAW_FRAMEBUFFER_BINDING), null);
    assert.match(logs.join('\n'), /official direct rendering.*synthetic blit failure/);
  });
});
