// ArenaNet's EGL adapter.
//
// The client owns context creation and canvas sizing, so this only hooks what
// the host has to know about: when the first frame reaches the screen, and what
// device pixel ratio the client should render at.
//
// Unlike the Electron host this does NOT route frames through an OffscreenCanvas
// and transferToImageBitmap. That hop measured 0.20 ms p50 at 3840x2160 on this
// machine, which buys nothing when the client renders at the canvas's own size —
// EGL gets the visible canvas directly and the compositor presents it.

/**
 * @param {{
 *   env: Record<string, unknown>,
 *   renderScale(): number,
 *   firstFrame(): void,
 *   log(...values: unknown[]): void,
 * }} options
 */
export function installGraphics({ env, renderScale, firstFrame, log }) {
  if (!env || typeof env.eglCreateContext !== 'function') {
    log('[warn] no eglCreateContext import — nothing will be presented');
    return;
  }

  const createContext = env.eglCreateContext;
  env.eglCreateContext = (...args) => {
    const context = createContext(...args);
    if (!context) throw new Error('EGL could not create a WebGL context');
    log('egl context created');
    return context;
  };

  // Render scale is the density the client sees, not a second host-side resize
  // competing with emscripten's canvas owner.
  if (typeof env.emscripten_get_device_pixel_ratio === 'function') {
    env.emscripten_get_device_pixel_ratio = renderScale;
  }

  const swap = env.eglSwapBuffers;
  if (typeof swap === 'function') {
    let waiting = true;
    env.eglSwapBuffers = (...args) => {
      const ok = swap(...args);
      if (waiting && ok) {
        waiting = false;
        firstFrame();
      }
      return ok;
    };
  }
}
