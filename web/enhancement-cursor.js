// Drawing the game's own cursor as the page's cursor.
//
// Guild Wars renders its cursors itself, into a texture it composites over the
// scene. In a browser that texture is behind the compositor: it arrives a
// frame late, it lags the pointer by however long the game took to draw, and
// under a WebView it can be drawn twice — once by the game and once by the
// system arrow on top of it.
//
// So the companion reads that texture out of the game's memory each frame and
// publishes it, and this file turns it into a CSS `cursor`. The pointer is
// then the operating system's own, moving at pointer rate with nothing to
// lag behind, wearing the picture the game meant it to wear.
//
// Everything here is presentation. No pixels and no pointer leave this file,
// and it has no way to affect what the game does.

import {
  readCompanionCursorHeader,
  readCompanionCursorPixels,
} from './companion-snapshot.js';

/** The companion refuses any texture that is not exactly this square. */
const EDGE = 32;
/** Bounded because the game composes drag cursors at runtime, one per item. */
const CACHE_LIMIT = 64;

/** @param {number} edge */
function createSurface(edge) {
  const canvas = document.createElement('canvas');
  canvas.width = edge;
  canvas.height = edge;
  const context = canvas.getContext('2d');
  if (context === null) throw new Error('the cursor canvas is unavailable');
  return { canvas, context };
}

const source = createSurface(EDGE);
/** @type {Map<string, string>} */
const cssCache = new Map();

/**
 * Turn one published bitmap into a `cursor` declaration.
 *
 * The authoring grid stays at 32 — that is the size the game's own cursor
 * textures are, and what the companion checks before publishing one. Hotspots
 * stay in that authoring grid too.
 *
 * Data URLs rather than blobs because the canvas is the only source and a blob
 * would need revoking; `img-src 'self' data: blob:` in the host's policy would
 * allow either. The trailing `default` is mandatory — a `cursor`
 * declaration with no keyword fallback is dropped whole.
 *
 * @param {Uint8ClampedArray} pixels RGBA8, row-major, 32x32
 * @param {number} hotspotX
 * @param {number} hotspotY
 */
export function buildCursorCss(pixels, hotspotX, hotspotY) {
  const image = source.context.createImageData(EDGE, EDGE);
  image.data.set(pixels);
  source.context.putImageData(image, 0, 0);
  // Chromium (and therefore GWOnMac) keeps the platform cursor behind an
  // `image-set` stable. WKWebView 27 instead resolves its candidates again as
  // it constructs a fresh NSCursor for mouse-move hit tests, even while this
  // declaration and the selected image are unchanged. During that resolution
  // AppKit briefly exposes the fallback arrow. One URL removes the candidate
  // handoff and remains stable even when WebKit recreates the platform object.
  // The source is already the game's canonical 32-pixel artwork; macOS scales
  // cursor imagery for the display just as it does its own cursors.
  return `url("${source.canvas.toDataURL('image/png')}") ${hotspotX} ${hotspotY}, default`;
}

/**
 * @param {number} pixelHash
 * @param {number} hotspotX
 * @param {number} hotspotY
 */
function cacheKey(pixelHash, hotspotX, hotspotY) {
  return `${pixelHash}:${hotspotX}:${hotspotY}`;
}

/** @param {string} key */
function cacheGet(key) {
  const css = cssCache.get(key);
  if (css === undefined) return undefined;
  // Re-inserting is what makes the eviction below least-recently-used: the
  // cursors a player is actually using stay, and the drag cursor for an item
  // they picked up once does not.
  cssCache.delete(key);
  cssCache.set(key, css);
  return css;
}

/**
 * @param {string} key
 * @param {string} css
 */
function cacheSet(key, css) {
  cssCache.set(key, css);
  while (cssCache.size > CACHE_LIMIT) {
    const oldest = cssCache.keys().next().value;
    if (oldest === undefined) break;
    cssCache.delete(oldest);
  }
}

/**
 * Own the cursor on `element` for as long as the returned handle lives.
 *
 * `poll` is meant to be called once per animation frame. It reads the header
 * only — forty bytes — and returns immediately unless something changed, so
 * the four-kilobyte read and canvas draw happen on the handful of
 * frames where the cursor is actually different.
 *
 * @param {{
 *   element: HTMLElement,
 *   memory: WebAssembly.Memory,
 *   cursorPointer: number,
 *   fallback?: string,
 * }} options
 */
export function createCursorConsumer({
  element,
  memory,
  cursorPointer,
  fallback = '',
}) {
  let applied = fallback;
  let generation = -1;
  let flags = -1;
  let pixelHash = 0;
  let hidden = false;
  let valid = false;

  // The engine coalesces cursor changes onto a timer rather than pushing each
  // one to the window server as it happens, so a changed `cursor` reaches the
  // screen some tens of milliseconds late on its own. A real layout
  // invalidation makes it recompute sooner, which is what this is: one pixel
  // wide, out of flow, unpainted and untouchable, so dirtying it cannot reach
  // the game surface — and it is only ever dirtied when the cursor has in fact
  // changed, which is a few times a second at worst.
  const beacon = document.createElement('div');
  beacon.style.cssText =
    'position:fixed;top:0;left:0;height:1px;pointer-events:none;opacity:0';
  document.body.append(beacon);

  /** @param {string} css */
  const apply = (css) => {
    applied = css;
    // An inline style beats the stylesheet; the empty string hands it back.
    element.style.cursor = css;
    beacon.style.width = beacon.style.width === '1px' ? '2px' : '1px';
    void beacon.offsetWidth;
  };

  // macOS implements `cursor: none` as a transparent 1x1 NSCursor, and native
  // chrome — a pointer lock ending, the window taking focus — can put the
  // system arrow back behind our back. Re-assigning the same inline value does
  // not invalidate style, hence clearing it first.
  const reassert = () => {
    element.style.cursor = '';
    element.style.cursor = applied;
  };

  const poll = () => {
    const header = readCompanionCursorHeader(memory.buffer, cursorPointer);
    if (header.status === 'waiting') return;
    // The companion bumps the generation only when it republishes pixels, so a
    // show/hide arrives as a flags change with the generation standing still.
    if (header.generation === generation && header.flags === flags) return;
    if (header.status !== 'ready') {
      generation = header.generation;
      flags = header.flags;
      pixelHash = 0;
      hidden = false;
      valid = false;
      apply(fallback);
      return;
    }
    let css = 'none';
    if (!header.hidden) {
      const key = cacheKey(header.pixelHash, header.hotspotX, header.hotspotY);
      const cached = cacheGet(key);
      if (cached === undefined) {
        const full = readCompanionCursorPixels(memory.buffer, cursorPointer);
        // A publish landed between the header read and the pixel read. Change
        // nothing and try again next frame — the state below is deliberately
        // not advanced, so the next poll still sees a difference.
        if (full === null || full.generation !== header.generation) return;
        css = buildCursorCss(full.pixels, full.hotspotX, full.hotspotY);
        cacheSet(key, css);
      } else {
        css = cached;
      }
    }
    generation = header.generation;
    flags = header.flags;
    pixelHash = header.pixelHash;
    hidden = header.hidden;
    valid = true;
    apply(css);
  };

  document.addEventListener('pointerlockchange', reassert);
  window.addEventListener('focus', reassert);

  return {
    poll,
    get state() {
      return Object.freeze({
        generation: Math.max(generation, 0),
        pixelHash,
        hidden,
        valid,
        cssLength: applied.length,
      });
    },
    dispose() {
      document.removeEventListener('pointerlockchange', reassert);
      window.removeEventListener('focus', reassert);
      beacon.remove();
      element.style.cursor = '';
    },
  };
}
