// Complete-frame presentation for ArenaNet's implicit-swap WebGL build.
//
// A browser may composite the default drawing buffer whenever JavaScript
// yields. That is normally equivalent to a frame boundary, but it is not when
// JSPI or Asyncify suspends the Wasm stack after the game has started drawing.
// This barrier makes framebuffer zero private and copies it to the real default
// framebuffer only after ArenaNet's successful logical eglSwapBuffers call.

const FRAMEBUFFER = 0x8d40;
const READ_FRAMEBUFFER = 0x8ca8;
const DRAW_FRAMEBUFFER = 0x8ca9;
const DRAW_FRAMEBUFFER_BINDING = 0x8ca6;
const READ_FRAMEBUFFER_BINDING = 0x8caa;
const RENDERBUFFER = 0x8d41;
const RENDERBUFFER_BINDING = 0x8ca7;
const TEXTURE_2D = 0x0de1;
const TEXTURE_BINDING_2D = 0x8069;
const PIXEL_UNPACK_BUFFER = 0x88ec;
const PIXEL_UNPACK_BUFFER_BINDING = 0x88ef;
const COLOR_ATTACHMENT0 = 0x8ce0;
const RED_BITS = 0x0d52;
const GREEN_BITS = 0x0d53;
const BLUE_BITS = 0x0d54;
const ALPHA_BITS = 0x0d55;
const DEPTH_BITS = 0x0d56;
const STENCIL_BITS = 0x0d57;
const DEPTH_ATTACHMENT = 0x8d00;
const STENCIL_ATTACHMENT = 0x8d20;
const DEPTH_STENCIL_ATTACHMENT = 0x821a;
const RGB = 0x1907;
const RGBA = 0x1908;
const RGB8 = 0x8051;
const RGBA8 = 0x8058;
const UNSIGNED_BYTE = 0x1401;
const DEPTH_COMPONENT16 = 0x81a5;
const DEPTH_COMPONENT24 = 0x81a6;
const STENCIL_INDEX8 = 0x8d48;
const DEPTH24_STENCIL8 = 0x88f0;
const SAMPLES = 0x80a9;
const FRAMEBUFFER_COMPLETE = 0x8cd5;
const COLOR_BUFFER_BIT = 0x4000;
const NEAREST = 0x2600;
const SCISSOR_TEST = 0x0c11;
const TEXTURE_MIN_FILTER = 0x2801;
const TEXTURE_MAG_FILTER = 0x2800;
const TEXTURE_WRAP_S = 0x2802;
const TEXTURE_WRAP_T = 0x2803;
const CLAMP_TO_EDGE = 0x812f;
const FIRST_COMMIT_TIMEOUT_MS = 10_000;

const REQUIRED_IMPORTS = [
  'eglCreateContext',
  'eglSwapBuffers',
  'glBindFramebuffer',
  'emscripten_set_canvas_element_size',
];

// These APIs give framebuffer zero special attachment or buffer-enum
// semantics that differ from an ordinary FBO. The current ArenaNet artifacts
// import none of them. A future client that does should keep official direct
// rendering until the host translates that operation deliberately.
const SUPPORTED_FRAMEBUFFER_IMPORTS = new Set([
  'glBindFramebuffer',
  'glCheckFramebufferStatus',
  'glFramebufferTexture2D',
  'glGenFramebuffers',
  'glIsFramebuffer',
]);

const hasUnsupportedDefaultFramebufferSemantics = (name) =>
  // Binding, creation, identity, completeness, and the translated
  // Texture2D attachment operation are understood. Any other core or
  // extension API whose name exposes framebuffer semantics is not.
  (name.includes('Framebuffer') && !SUPPORTED_FRAMEBUFFER_IMPORTS.has(name))
  // These select default-only BACK versus FBO-only COLOR_ATTACHMENT enums,
  // even though their names do not contain "Framebuffer".
  || /^gl(?:DrawBuffers|ReadBuffer)/.test(name);

/** Restore independent WebGL2 read/draw bindings without assuming they match. */
function restoreFramebuffers(gl, draw, read) {
  gl.bindFramebuffer(READ_FRAMEBUFFER, read);
  gl.bindFramebuffer(DRAW_FRAMEBUFFER, draw);
}

/**
 * Install an explicit presentation barrier around the stable browser imports.
 *
 * The return value is intentionally small: graphics.js owns the logical EGL
 * swap wrapper and calls commit only after ArenaNet's adapter accepted a swap.
 * Everything else is installed here before Wasm instantiation.
 *
 * @param {{
 *   enabled?: boolean,
 *   env: Record<string, unknown>,
 *   canvas?: HTMLCanvasElement | null,
 *   log(...values: unknown[]): void,
 * }} options
 */
export function installPresentationBarrier({ enabled = false, env, canvas, log }) {
  if (!enabled) return null;

  const missing = REQUIRED_IMPORTS.filter((name) => typeof env?.[name] !== 'function');
  const unsupported = Object.keys(env ?? {}).filter(hasUnsupportedDefaultFramebufferSemantics);
  if (!canvas || missing.length || unsupported.length) {
    const reason = missing.length
      ? `missing ${missing.join(', ')}`
      : unsupported.length
        ? `unsupported default-framebuffer import ${unsupported.join(', ')}`
        : 'missing canvas';
    log(`[warn] frame isolation unavailable: ${reason}`);
    return null;
  }

  let gl = null;
  let context = null;
  let framebuffer = null;
  let color = null;
  let depthStencil = null;
  let colorInternalFormat = RGBA8;
  let colorFormat = RGBA;
  let depthStencilInternalFormat = null;
  let depthStencilAttachment = null;
  let defaultFormat = null;
  let width = 0;
  let height = 0;
  let active = false;
  let lost = false;
  let reportedFailure = false;
  let firstCommitTimer = null;
  // WebGL starts with framebuffer zero bound. Track the names the game asked
  // for separately from the raw objects, because resizing the canvas may reset
  // browser bindings while the game's logical default remains zero.
  let logicalDraw = 0;
  let logicalRead = 0;
  // The raw bindings last established through the wrapped import. Comparing
  // them at commit catches changed generated glue that binds behind the
  // wrapper, including while a nonzero game FBO is logically selected.
  let expectedDraw = null;
  let expectedRead = null;
  // ArenaNet exposes both state changes as imports. Tracking the one capability
  // commit temporarily changes avoids a synchronous WebGL query every frame;
  // a future artifact missing either wrapper falls back to `isEnabled`.
  const enable = env.glEnable;
  const disable = env.glDisable;
  const tracksScissor = typeof enable === 'function' && typeof disable === 'function';
  let scissorEnabled = false;

  const rememberRawBindings = () => {
    expectedDraw = gl?.getParameter(DRAW_FRAMEBUFFER_BINDING) ?? null;
    expectedRead = gl?.getParameter(READ_FRAMEBUFFER_BINDING) ?? null;
  };

  const restoredBinding = (logical, previous, previousPrivate) => {
    if (active && logical === 0) return framebuffer;
    if (!active && previous === previousPrivate) return null;
    return previous;
  };

  const clearFirstCommitTimer = () => {
    if (firstCommitTimer === null) return;
    clearTimeout(firstCommitTimer);
    firstCommitTimer = null;
  };

  const dispose = () => {
    clearFirstCommitTimer();
    if (!gl || !framebuffer || lost) {
      framebuffer = null;
      color = null;
      depthStencil = null;
      width = 0;
      height = 0;
      active = false;
      return;
    }
    // A failed allocation can leave the private target bound. Replace only
    // bindings owned by this barrier; never disturb a game-created FBO.
    const draw = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);
    const read = gl.getParameter(READ_FRAMEBUFFER_BINDING);
    if (read === framebuffer) gl.bindFramebuffer(READ_FRAMEBUFFER, null);
    if (draw === framebuffer) gl.bindFramebuffer(DRAW_FRAMEBUFFER, null);
    gl.deleteFramebuffer(framebuffer);
    if (color) gl.deleteTexture(color);
    if (depthStencil) gl.deleteRenderbuffer(depthStencil);
    framebuffer = null;
    color = null;
    depthStencil = null;
    width = 0;
    height = 0;
    active = false;
    rememberRawBindings();
  };

  const failOpen = (reason) => {
    dispose();
    if (!reportedFailure) {
      reportedFailure = true;
      log(`[warn] frame isolation disabled; using official direct rendering: ${reason}`);
    }
  };

  const allocate = (force = false) => {
    if (!gl || lost) return false;
    const nextWidth = gl.drawingBufferWidth;
    const nextHeight = gl.drawingBufferHeight;
    if (nextWidth <= 0 || nextHeight <= 0) return false;
    if (!force && active && width === nextWidth && height === nextHeight) return true;

    const previousDraw = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);
    const previousRead = gl.getParameter(READ_FRAMEBUFFER_BINDING);
    const previousTexture = gl.getParameter(TEXTURE_BINDING_2D);
    const previousRenderbuffer = gl.getParameter(RENDERBUFFER_BINDING);
    const previousUnpackBuffer = gl.getParameter(PIXEL_UNPACK_BUFFER_BINDING);
    const previousPrivate = framebuffer;
    const previousWidth = width;
    const previousHeight = height;

    try {
      framebuffer ??= gl.createFramebuffer();
      color ??= gl.createTexture();
      if (!framebuffer || !color) throw new Error('WebGL resource allocation failed');

      gl.bindFramebuffer(FRAMEBUFFER, framebuffer);
      gl.bindTexture(TEXTURE_2D, color);
      // With a WebGL2 pixel-unpack buffer bound, texImage2D's final `null`
      // selects the buffer-offset overload rather than storage-only allocation.
      // Private attachment creation must be invisible to that game state.
      gl.bindBuffer(PIXEL_UNPACK_BUFFER, null);
      gl.texParameteri(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
      gl.texParameteri(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
      gl.texParameteri(TEXTURE_2D, TEXTURE_WRAP_S, CLAMP_TO_EDGE);
      gl.texParameteri(TEXTURE_2D, TEXTURE_WRAP_T, CLAMP_TO_EDGE);
      gl.texImage2D(
        TEXTURE_2D,
        0,
        colorInternalFormat,
        nextWidth,
        nextHeight,
        0,
        colorFormat,
        UNSIGNED_BYTE,
        null,
      );
      gl.framebufferTexture2D(FRAMEBUFFER, COLOR_ATTACHMENT0, TEXTURE_2D, color, 0);

      if (depthStencilInternalFormat !== null) {
        depthStencil ??= gl.createRenderbuffer();
        if (!depthStencil) throw new Error('depth/stencil allocation failed');
        gl.bindRenderbuffer(RENDERBUFFER, depthStencil);
        gl.renderbufferStorage(
          RENDERBUFFER,
          depthStencilInternalFormat,
          nextWidth,
          nextHeight,
        );
        gl.framebufferRenderbuffer(
          FRAMEBUFFER,
          depthStencilAttachment,
          RENDERBUFFER,
          depthStencil,
        );
      }

      const status = gl.checkFramebufferStatus(FRAMEBUFFER);
      if (status !== FRAMEBUFFER_COMPLETE) {
        throw new Error(`private framebuffer incomplete (0x${status.toString(16)})`);
      }
      width = nextWidth;
      height = nextHeight;
      active = true;
      if (previousWidth && (previousWidth !== width || previousHeight !== height)) {
        log(
          'graphics: resized isolated frame',
          `${previousWidth}x${previousHeight}`,
          '→',
          `${width}x${height}`,
        );
      }
      return true;
    } catch (error) {
      failOpen(error?.message ?? error);
      return false;
    } finally {
      // Allocation is invisible to the game. In particular, an Emscripten
      // glGetIntegerv immediately afterwards must see its previous logical FBO.
      gl.bindTexture(TEXTURE_2D, previousTexture);
      gl.bindRenderbuffer(RENDERBUFFER, previousRenderbuffer);
      gl.bindBuffer(PIXEL_UNPACK_BUFFER, previousUnpackBuffer);
      restoreFramebuffers(
        gl,
        restoredBinding(logicalDraw, previousDraw, previousPrivate),
        restoredBinding(logicalRead, previousRead, previousPrivate),
      );
      rememberRawBindings();
    }
  };

  const initialise = () => {
    dispose();
    lost = false;
    logicalDraw = 0;
    logicalRead = 0;
    context = canvas.GLctxObject;
    gl = context?.GLctx ?? null;
    const attributes = gl?.getContextAttributes?.();
    if (!gl || context?.version < 2 || typeof gl.blitFramebuffer !== 'function') {
      failOpen('a WebGL2 context with framebuffer blits is required');
      return;
    }
    scissorEnabled = gl.isEnabled(SCISSOR_TEST);
    // Emscripten itself uses a shader-copy fallback for antialiased contexts.
    // The current client requests antialias:false; changing quality silently
    // for a future build would be worse than declining the barrier.
    if (!attributes || attributes.antialias) {
      failOpen('antialiased or unreadable context attributes are unsupported');
      return;
    }
    defaultFormat = {
      red: gl.getParameter(RED_BITS),
      green: gl.getParameter(GREEN_BITS),
      blue: gl.getParameter(BLUE_BITS),
      alpha: gl.getParameter(ALPHA_BITS),
      depth: gl.getParameter(DEPTH_BITS),
      stencil: gl.getParameter(STENCIL_BITS),
      samples: gl.getParameter(SAMPLES),
    };
    const formatLabel = [
      `rgba=${defaultFormat.red}/${defaultFormat.green}/${defaultFormat.blue}/${defaultFormat.alpha}`,
      `depth=${defaultFormat.depth}`,
      `stencil=${defaultFormat.stencil}`,
      `samples=${defaultFormat.samples}`,
    ].join(' ');
    if (
      defaultFormat.red !== 8
      || defaultFormat.green !== 8
      || defaultFormat.blue !== 8
      || ![0, 8].includes(defaultFormat.alpha)
      || defaultFormat.samples !== 0
      || attributes.alpha !== (defaultFormat.alpha > 0)
      || attributes.depth !== (defaultFormat.depth > 0)
      || attributes.stencil !== (defaultFormat.stencil > 0)
    ) {
      failOpen(`unsupported default framebuffer format (${formatLabel})`);
      return;
    }
    // An opaque default framebuffer reads alpha as one after blending. Match
    // its actual attachment bits rather than merely trusting creation flags.
    if (defaultFormat.alpha === 0) {
      colorInternalFormat = RGB8;
      colorFormat = RGB;
    } else {
      colorInternalFormat = RGBA8;
      colorFormat = RGBA;
    }
    if (defaultFormat.depth === 0 && defaultFormat.stencil === 0) {
      depthStencilInternalFormat = null;
      depthStencilAttachment = null;
    } else if (defaultFormat.depth === 16 && defaultFormat.stencil === 0) {
      depthStencilInternalFormat = DEPTH_COMPONENT16;
      depthStencilAttachment = DEPTH_ATTACHMENT;
    } else if (defaultFormat.depth === 24 && defaultFormat.stencil === 0) {
      depthStencilInternalFormat = DEPTH_COMPONENT24;
      depthStencilAttachment = DEPTH_ATTACHMENT;
    } else if (defaultFormat.depth === 0 && defaultFormat.stencil === 8) {
      depthStencilInternalFormat = STENCIL_INDEX8;
      depthStencilAttachment = STENCIL_ATTACHMENT;
    } else if (defaultFormat.depth === 24 && defaultFormat.stencil === 8) {
      depthStencilInternalFormat = DEPTH24_STENCIL8;
      depthStencilAttachment = DEPTH_STENCIL_ATTACHMENT;
    } else {
      failOpen(`unsupported default depth/stencil format (${formatLabel})`);
      return;
    }
    if (!allocate()) return;
    firstCommitTimer = setTimeout(() => {
      firstCommitTimer = null;
      if (active) failOpen(`no successful logical swap within ${FIRST_COMMIT_TIMEOUT_MS} ms`);
    }, FIRST_COMMIT_TIMEOUT_MS);
    // Node's test timer must not hold the process open; browsers return a
    // numeric handle and simply skip this optional method.
    firstCommitTimer?.unref?.();
    log(
      'graphics: frame isolation active',
      `${width}x${height}`,
      `colour=${colorInternalFormat === RGB8 ? 'rgb8' : 'rgba8'}`,
      `depth=${defaultFormat.depth}`,
      `stencil=${defaultFormat.stencil}`,
    );
  };

  const createContext = env.eglCreateContext;
  env.eglCreateContext = (...args) => {
    const result = createContext(...args);
    if (result) initialise();
    return result;
  };

  const bindFramebuffer = env.glBindFramebuffer;
  env.glBindFramebuffer = (target, name) => {
    if (target === FRAMEBUFFER) {
      logicalDraw = name;
      logicalRead = name;
    } else if (target === DRAW_FRAMEBUFFER) {
      logicalDraw = name;
    } else if (target === READ_FRAMEBUFFER) {
      logicalRead = name;
    }
    if (name !== 0 || !active || !allocate()) {
      const result = bindFramebuffer(target, name);
      rememberRawBindings();
      return result;
    }
    // The private object deliberately has no Emscripten `name`. Generated
    // glGetIntegerv therefore maps it back to logical framebuffer zero.
    const result = gl.bindFramebuffer(target, framebuffer);
    rememberRawBindings();
    return result;
  };

  if (tracksScissor) {
    env.glEnable = (capability) => {
      const result = enable(capability);
      if (capability === SCISSOR_TEST) scissorEnabled = true;
      return result;
    };
    env.glDisable = (capability) => {
      const result = disable(capability);
      if (capability === SCISSOR_TEST) scissorEnabled = false;
      return result;
    };
  }

  // Unlike framebuffer binds, attachment calls are invalid on the browser's
  // default framebuffer. A private FBO must not accidentally make them valid.
  // Run the current artifact's one attachment import against raw default when
  // the game logically has zero bound, then restore its virtual binding.
  const framebufferTexture2D = env.glFramebufferTexture2D;
  if (typeof framebufferTexture2D === 'function') {
    env.glFramebufferTexture2D = (target, ...args) => {
      const logical = target === READ_FRAMEBUFFER ? logicalRead : logicalDraw;
      if (!active || logical !== 0) return framebufferTexture2D(target, ...args);
      const previousDraw = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);
      const previousRead = gl.getParameter(READ_FRAMEBUFFER_BINDING);
      try {
        gl.bindFramebuffer(target, null);
        return framebufferTexture2D(target, ...args);
      } finally {
        restoreFramebuffers(gl, previousDraw, previousRead);
      }
    };
  }

  const setCanvasSize = env.emscripten_set_canvas_element_size;
  env.emscripten_set_canvas_element_size = (...args) => {
    const result = setCanvasSize(...args);
    // Setting a canvas dimension resets its drawing buffer even when the
    // numeric size did not change. Reinitialize the private attachments and
    // restore the logical default binding in both cases.
    if (result === 0 && active) allocate(true);
    return result;
  };

  canvas.addEventListener?.('webglcontextlost', () => {
    lost = true;
    active = false;
    framebuffer = null;
    color = null;
    depthStencil = null;
    expectedDraw = null;
    expectedRead = null;
  });
  canvas.addEventListener?.('webglcontextrestored', initialise);

  return {
    commit(ok) {
      if (!ok || !active) return false;
      const rawDraw = gl.getParameter(DRAW_FRAMEBUFFER_BINDING);
      const rawRead = gl.getParameter(READ_FRAMEBUFFER_BINDING);
      if (rawDraw !== expectedDraw || rawRead !== expectedRead) {
        failOpen('raw framebuffer binding escaped the wrapped import');
        return false;
      }
      if (!allocate()) return false;
      clearFirstCommitTimer();
      // `allocate` is a no-op at the normal steady-state size, and restores
      // the raw bindings if a resize raced the explicit canvas-size import.
      // Reuse the two values already read for the escape check instead of
      // synchronously asking WebGL for the same state twice more.
      const previousDraw = rawDraw;
      const previousRead = rawRead;
      const scissor = tracksScissor ? scissorEnabled : gl.isEnabled(SCISSOR_TEST);
      const previousPrivate = framebuffer;
      try {
        if (scissor) gl.disable(SCISSOR_TEST);
        gl.bindFramebuffer(READ_FRAMEBUFFER, framebuffer);
        gl.bindFramebuffer(DRAW_FRAMEBUFFER, null);
        gl.blitFramebuffer(
          0,
          0,
          width,
          height,
          0,
          0,
          width,
          height,
          COLOR_BUFFER_BIT,
          NEAREST,
        );
        return true;
      } catch (error) {
        failOpen(error?.message ?? error);
        return false;
      } finally {
        restoreFramebuffers(
          gl,
          restoredBinding(logicalDraw, previousDraw, previousPrivate),
          restoredBinding(logicalRead, previousRead, previousPrivate),
        );
        if (scissor) gl.enable(SCISSOR_TEST);
      }
    },
    get active() {
      return active;
    },
  };
}
