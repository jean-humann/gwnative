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

import * as diagnostics from './diagnostics.js';

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

  // Every presented frame passes through here, which makes it the one place
  // frame pacing can be measured. Reading performance.now() per frame is not
  // the thing harness.js warns about: what is audible is *substituting* it for
  // the timestamp handed to the client, because that is the clock the client
  // drives animation and audio from. Nothing here touches that clock.
  const swap = env.eglSwapBuffers;
  if (typeof swap === 'function') {
    let waiting = true;
    let previous = 0;
    env.eglSwapBuffers = (...args) => {
      const ok = swap(...args);
      if (waiting && ok) {
        waiting = false;
        firstFrame();
      } else if (ok) {
        const now = performance.now();
        // The first interval after a stall spans the whole stall, so it is a
        // real datum rather than an outlier to drop — a covered window is
        // exactly what it should show.
        if (previous) {
          const ms = now - previous;
          // A total rather than a gauge: a gauge keeps only the last value
          // written, so it would report whichever frame happened to draw last
          // before the sample. Divided by the frame count it is the mean, and
          // the peak beside it keeps the stall the mean hides.
          diagnostics.count('gw.frames', 1);
          diagnostics.count('gw.frame.ms.total', ms);
          diagnostics.peak('gw.frame.ms.max', ms);
        }
        previous = now;
      }
      return ok;
    };
  }
}
