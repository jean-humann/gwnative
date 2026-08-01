// ArenaNet's EGL adapter.
//
// The client owns context creation and canvas sizing, so this only hooks what
// the host has to know about: when the first frame reaches the screen, and what
// device pixel ratio the client should render at.
//
// Frames are not routed through an OffscreenCanvas and transferToImageBitmap.
// That hop measured 0.20 ms p50 at 3840x2160 on this machine and buys nothing
// when the client renders at the canvas's own size — EGL gets the visible canvas
// directly and the compositor presents it.

import * as diagnostics from './diagnostics.js';
import { installPresentationBarrier } from './presentation-barrier.js';

// These imports can change pixels in the current draw framebuffer. A clear is
// just as capable as a triangle draw of exposing an incomplete default buffer
// if Wasm suspends before its logical swap. `glDrawBuffers`, by contrast, only
// selects render targets and must not manufacture a write signature.
const FRAMEBUFFER_WRITE_IMPORT = /^(?:glClear(?:Buffer(?:fi|fv|iv|uiv))?|glDraw(?:Arrays|Elements|RangeElements)(?:Instanced(?:ANGLE|ARB|EXT)?)?|glBlitFramebuffer(?:ANGLE)?)$/;

// The native activation cover arms this only while a retained
// complete frame is above WebKit. Disarm before crossing the process boundary
// so one activation costs one message rather than one message per frame. Any
// absent or broken native bridge is deliberately invisible to the game; the
// native side has its own bounded fail-safe for removing the cover.
const notifyActivationCover = (presented) => {
  const generation = globalThis.__gwnativeActivationCoverArmed;
  if (!presented || typeof generation !== 'string') return;
  globalThis.__gwnativeActivationCoverArmed = null;
  try {
    globalThis.webkit?.messageHandlers?.gwnativeActivationCover?.postMessage(generation);
  } catch {
    // A diagnostics aid must never turn a healthy logical swap into a failure.
  }
};

/**
 * @param {{
 *   env: Record<string, unknown>,
 *   canvas?: HTMLCanvasElement | null,
 *   renderScale(): number,
 *   firstFrame(): void,
 *   preserveDrawingBuffer?: boolean,
 *   frameIsolation?: boolean,
 *   audit?: { enabled?: boolean, draw(): void, swap(ok: unknown): number | undefined, contextCreated(): void },
 *   log(...values: unknown[]): void,
 * }} options
 */
export function installGraphics({
  env,
  canvas,
  renderScale,
  firstFrame,
  preserveDrawingBuffer = false,
  frameIsolation = false,
  audit,
  log,
}) {
  if (!env || typeof env.eglCreateContext !== 'function') {
    log('[warn] no eglCreateContext import — nothing will be presented');
    return;
  }

  // A non-preserved WebGL drawing buffer may be cleared after compositing.
  // Keeping it is deliberately an experiment rather than a default: it can
  // cost an extra full-resolution surface and disable compositor fast paths.
  // Hook only this canvas and only before ArenaNet asks for its one context.
  if (preserveDrawingBuffer && typeof canvas?.getContext === 'function') {
    const getContext = canvas.getContext;
    let reported = false;
    canvas.getContext = function (kind, attributes) {
      if ((kind === 'webgl' || kind === 'webgl2') && attributes) {
        attributes.preserveDrawingBuffer = true;
      }
      const context = getContext.call(this, kind, attributes);
      if (!reported && context) {
        reported = true;
        log(
          'graphics: actual WebGL context attributes',
          JSON.stringify(context.getContextAttributes?.() ?? null),
        );
      }
      return context;
    };
    log('graphics: preserving WebGL drawing buffer for comparison');
  }

  // Browser callback return is normally an implicit WebGL presentation
  // boundary. It is not safe when the Wasm stack can suspend after drawing,
  // so complete-frame presentation redirects framebuffer zero and submits it
  // only at the client's logical swap. The module validates its browser/import
  // seam and otherwise leaves ArenaNet's official direct path in place.
  const presentation = installPresentationBarrier({
    enabled: frameIsolation,
    env,
    canvas,
    log,
  });

  const createContext = env.eglCreateContext;
  env.eglCreateContext = (...args) => {
    const context = createContext(...args);
    if (!context) throw new Error('EGL could not create a WebGL context');
    audit?.contextCreated();
    log('egl context created');
    return context;
  };

  // Correlating a suspension with a partially drawn frame requires knowing
  // whether any draw happened before the async read.  This is deliberately a
  // diagnostic-build hook: a game can issue thousands of draw calls per frame,
  // and an always-on wrapper would turn the observer into a performance cost.
  if (audit?.enabled) {
    for (const [name, draw] of Object.entries(env)) {
      if (!FRAMEBUFFER_WRITE_IMPORT.test(name) || typeof draw !== 'function') continue;
      env[name] = (...args) => {
        audit.draw(name);
        return draw(...args);
      };
    }
  }

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
      const wasIsolated = presentation?.active === true;
      // Detailed timing reads the clock twice per frame and exists only in the
      // opt-in audit. Normal builds retain a cheap submit count without making
      // the observer part of the cost it is measuring.
      const measureIsolation = wasIsolated && audit?.enabled === true;
      const isolationStarted = measureIsolation ? performance.now() : 0;
      const committed = presentation?.commit(ok) === true;
      const isolationEnded = measureIsolation ? performance.now() : 0;
      const auditedAt = audit?.swap(ok);
      if (wasIsolated) {
        diagnostics.count('gw.frame.isolation.submits', 1);
        if (measureIsolation) {
          // This is CPU submission time, not GPU execution time: WebGL queues
          // the blit. Frame cadence and process CPU beside it reveal
          // backpressure; this catches an expensive host call itself.
          const elapsed = isolationEnded - isolationStarted;
          diagnostics.count('gw.frame.isolation.submit.ms.total', elapsed);
          diagnostics.peak('gw.frame.isolation.submit.ms.max', elapsed);
        }
      }
      // If a commit fails, its fail-open transition discarded
      // that private frame. Do not call it the first presented frame; the next
      // direct-rendered swap will satisfy this condition instead.
      const presented = ok && (!wasIsolated || committed);
      notifyActivationCover(presented);
      if (waiting && presented) {
        waiting = false;
        firstFrame();
      } else if (ok) {
        // The always-on audit already needs this timestamp to age the last
        // frame across activation. Reuse it rather than reading the same clock
        // twice on every presented frame.
        const now = auditedAt ?? performance.now();
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
