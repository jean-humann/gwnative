// Opt-in end-to-end checks for the interface gwnative owns.
//
// Page-owned buttons, forms and overlays are exercised by accessible name. The
// Guild Wars canvas gets only the finite action vocabulary below, delivered
// over a sleeping long-poll channel; no screenshot or coordinate is involved.

export const E2E_STORAGE_KEYS = Object.freeze([
  'gwnative.overlay-layout.v1',
  'gwnative.build-library.v1',
]);

const normal = (value) => String(value ?? '').trim().replace(/\s+/g, ' ');

/**
 * Find one button by its accessible text.
 *
 * Exact matching is deliberate. "Save" must never silently become "Save and
 * restart" after a copy change and leave an E2E run performing a different
 * action from the one it reports.
 *
 * @param {{ querySelectorAll(selector: string): Iterable<HTMLButtonElement> }} root
 * @param {string} name
 * @returns {HTMLButtonElement}
 */
export function buttonNamed(root, name) {
  const matches = [...root.querySelectorAll('button')]
    .filter((candidate) => normal(candidate.textContent) === normal(name));
  if (matches.length !== 1) {
    throw new Error(`expected one ${JSON.stringify(name)} button, found ${matches.length}`);
  }
  return matches[0];
}

/**
 * Preserve exact storage bytes, including the distinction between an absent
 * key and a key whose value is the empty string.
 *
 * @param {Storage} storage
 * @param {readonly string[]} [keys]
 */
export function snapshotStorage(storage, keys = E2E_STORAGE_KEYS) {
  return Object.freeze(
    Object.fromEntries(keys.map((key) => [
      key,
      Object.freeze({ present: storage.getItem(key) !== null, value: storage.getItem(key) }),
    ])),
  );
}

/**
 * @param {Storage} storage
 * @param {ReturnType<typeof snapshotStorage>} snapshot
 */
export function restoreStorage(storage, snapshot) {
  for (const [key, entry] of Object.entries(snapshot)) {
    if (entry.present) storage.setItem(key, entry.value);
    else storage.removeItem(key);
  }
}

const assert = (condition, message) => {
  if (!condition) throw new Error(message);
};

const waitForFirstFrame = (window, timeout) => {
  if (window.__gwnativeFirstFrame === true) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      window.removeEventListener('gwnative:first-frame', arrived);
      reject(new Error(`first frame did not arrive within ${timeout} ms`));
    }, timeout);
    const arrived = () => {
      clearTimeout(timer);
      resolve();
    };
    window.addEventListener('gwnative:first-frame', arrived, { once: true });
  });
};

const cleanMessage = (value) =>
  String(value ?? 'unknown failure').replace(/[\p{Cc}\p{Cf}]+/gu, ' ').trim().slice(0, 160);

const wait = (milliseconds) =>
  new Promise((resolve) => setTimeout(resolve, milliseconds));

/**
 * Deliver one action from the native E2E vocabulary to the generated client.
 *
 * These are DOM input events, which is the interface Emscripten itself wires
 * to the game. The native API cannot name arbitrary keys or coordinates, and
 * this side checks the same bounds again before anything is dispatched.
 *
 * @param {{ sequence: number, action: string, durationMs: number }} action
 * @param {{
 *   window: Window,
 *   canvas: HTMLCanvasElement,
 *   KeyboardEvent?: typeof globalThis.KeyboardEvent,
 *   sleep?: (milliseconds: number) => Promise<void>,
 * }} options
 * @returns {Promise<'canvas' | 'text-proxy'>}
 */
export async function executeE2EAction(action, {
  window,
  canvas,
  KeyboardEvent = window.KeyboardEvent,
  sleep = wait,
}) {
  assert(Number.isSafeInteger(action?.sequence) && action.sequence > 0,
    'E2E action has no valid sequence');
  const spec = action.action === 'activate'
    ? { key: 'Enter', code: 'Enter', keyCode: 13, minimum: 40, maximum: 40 }
    : action.action === 'move-forward'
      ? { key: 'ArrowUp', code: 'ArrowUp', keyCode: 38, minimum: 50, maximum: 1_000 }
      : null;
  assert(spec, 'E2E action is not in the allowed vocabulary');
  assert(
    Number.isSafeInteger(action.durationMs)
      && action.durationMs >= spec.minimum
      && action.durationMs <= spec.maximum,
    `E2E ${action.action} duration is outside its bound`,
  );

  const activeInput = window.Module?.oskActiveInput;
  const target = action.action === 'activate' && activeInput ? activeInput : canvas;
  const targetName = target === canvas ? 'canvas' : 'text-proxy';
  target.focus?.();
  const keyboard = (type) => {
    const event = new KeyboardEvent(type, {
      bubbles: true,
      cancelable: true,
      composed: true,
      key: spec.key,
      code: spec.code,
      location: 0,
    });
    // WebKit leaves the legacy fields at zero on a constructed KeyboardEvent.
    // Emscripten copies all three into its C event, and the generated client
    // still reads them, so the test event must carry the same values as a real
    // key from the window server.
    for (const name of ['keyCode', 'which']) {
      Object.defineProperty(event, name, { value: spec.keyCode });
    }
    return event;
  };
  target.dispatchEvent(keyboard('keydown'));
  await sleep(action.durationMs);
  target.dispatchEvent(keyboard('keyup'));
  return targetName;
}

/**
 * Join the page to the test-only loopback control plane.
 *
 * The GET is a bounded long poll: on a healthy run this is one sleeping server
 * thread and no work in WebKit until a command arrives. Reconnecting after the
 * server's bound is the only periodic activity.
 *
 * @param {{
 *   window: Window,
 *   canvas: HTMLCanvasElement,
 *   log(...values: unknown[]): void,
 *   fetch?: typeof globalThis.fetch,
 *   waitMs?: number,
 * }} options
 */
export function installE2EBridge({
  window,
  canvas,
  log,
  fetch = window.fetch.bind(window),
  waitMs = 15_000,
}) {
  if (window.__gwnativeE2E !== true) return null;
  const headers = {
    'X-Gwnative-Token': window.__gwnativeToken ?? '',
    'Content-Type': 'application/json',
  };
  let after = 0;
  let stopped = false;
  let controller = null;
  let lastFailure = '';
  let trafficAction = 0;
  const trafficReported = new Set();

  const report = async (kind, detail = {}) => {
    const response = await fetch('__e2e/v1/events', {
      method: 'POST',
      headers,
      body: JSON.stringify({ kind, detail }),
    });
    if (!response.ok) throw new Error(`E2E event ${kind} was refused (${response.status})`);
    return response.json();
  };

  const pump = async () => {
    while (!stopped) {
      try {
        controller = new AbortController();
        const response = await fetch(
          `__e2e/v1/actions?after=${after}&waitMs=${Math.min(15_000, Math.max(0, waitMs))}`,
          { headers, signal: controller.signal },
        );
        if (!response.ok) throw new Error(`E2E action channel failed (${response.status})`);
        const envelope = await response.json();
        for (const action of envelope.actions ?? []) {
          if (!Number.isSafeInteger(action.sequence) || action.sequence <= after) continue;
          after = action.sequence;
          trafficAction = action.sequence;
          trafficReported.clear();
          let target = 'canvas';
          try {
            target = await executeE2EAction(action, { window, canvas });
            await report('action-complete', {
              actionSequence: action.sequence,
              action: action.action,
              target,
            });
          } catch (error) {
            await report('action-fail', {
              actionSequence: action.sequence,
              action: action.action,
              target,
              message: cleanMessage(error instanceof Error ? error.message : error),
            });
          }
        }
        lastFailure = '';
      } catch (error) {
        if (stopped || error?.name === 'AbortError') break;
        const message = cleanMessage(error instanceof Error ? error.message : error);
        if (message !== lastFailure) log(`[e2e] bridge: ${message}`);
        lastFailure = message;
        await wait(1_000);
      }
    }
  };

  let gameReadyReported = false;
  const gameState = (event) => {
    if (!gameReadyReported && event.detail?.status === 'ready') {
      gameReadyReported = true;
      void report('game-ready').catch(() => {});
    }
  };
  window.addEventListener('gwnative:state', gameState);

  const stop = () => {
    stopped = true;
    controller?.abort();
    window.removeEventListener('gwnative:state', gameState);
  };
  window.addEventListener('pagehide', stop, { once: true });
  void report('bridge-ready').catch((error) => {
    log(`[e2e] bridge: ${cleanMessage(error instanceof Error ? error.message : error)}`);
  });

  const traffic = (direction, socketId, bytes) => {
    if (
      trafficAction <= 0
      || !['send', 'receive'].includes(direction)
      || trafficReported.has(direction)
    ) return;
    trafficReported.add(direction);
    void report('client-traffic', {
      actionSequence: trafficAction,
      direction,
      socketId,
      bytes,
    }).catch(() => {});
  };

  void pump();

  return Object.freeze({ report, traffic, stop });
}

const setValue = (field, value) => {
  field.value = value;
  field.dispatchEvent(new Event('input', { bubbles: true }));
  field.dispatchEvent(new Event('change', { bubbles: true }));
};

/**
 * Exercise the page-owned controls against the real WKWebView.
 *
 * The two localStorage surfaces are restored byte-for-byte in `finally`, and
 * every overlay is put back as it was. The process is then stopped by the
 * native runner, so the in-memory build-library copy cannot outlive the test.
 *
 * @param {{
 *   window: Window,
 *   document: Document,
 *   storage: Storage,
 *   overlays: ReturnType<import('./overlay.js').createOverlayManager>,
 *   openTools(): void,
 *   openSettings(): void,
 *   openGuide(): void,
 *   log(...values: unknown[]): void,
 *   timeout?: number,
 * }} options
 */
export async function runAppE2E({
  window,
  document,
  storage,
  overlays,
  openTools,
  openSettings,
  openGuide,
  log,
  timeout = 120_000,
}) {
  await waitForFirstFrame(window, timeout);
  const stored = snapshotStorage(storage);
  const widgets = [...document.querySelectorAll('#gwnative-overlays [data-widget]')];
  const originalVisibility = new Map(widgets.map((widget) => [widget.dataset.widget, !widget.hidden]));
  const originalEditMode = overlays.editing();
  const dummyName = `gwnative e2e ${Date.now()}`;
  const tools = () => document.querySelector('[data-surface="companion-tools"]');

  try {
    openTools();
    const surface = tools();
    assert(surface && !surface.hidden && surface.style.display === 'flex',
      'Companion Tools did not open');

    const widgetButtons = new Map([
      ['clock', 'Clock'],
      ['session-timer', 'Session timer'],
      ['target-details', 'Target details'],
      ['performance', 'Performance'],
    ]);
    for (const widget of widgets) {
      const id = widget.dataset.widget;
      if (!widgetButtons.has(id)) continue;
      buttonNamed(surface, widgetButtons.get(id)).click();
      assert(widget.hidden === originalVisibility.get(id),
        `${id} did not toggle visibility`);
      buttonNamed(surface, widgetButtons.get(id)).click();
      assert(!widget.hidden === originalVisibility.get(id),
        `${id} did not return to its original visibility`);
    }

    buttonNamed(surface, 'Edit layout').click();
    assert(overlays.editing() !== originalEditMode, 'layout edit mode did not toggle');
    buttonNamed(surface, 'Edit layout').click();
    assert(overlays.editing() === originalEditMode, 'layout edit mode did not restore');

    const form = surface.querySelector('form');
    const buildName = surface.querySelector('input[placeholder="Build name"]');
    const memberName = surface.querySelector('input[placeholder="Character or role"]');
    const code = surface.querySelector('input[placeholder="Template code"]');
    assert(form && buildName && memberName && code, 'build-library form is incomplete');
    setValue(buildName, dummyName);
    setValue(memberName, 'E2E role');
    setValue(code, 'OQAAE2E');
    form.requestSubmit();
    const row = [...surface.querySelectorAll('div')]
      .find((candidate) => normal(candidate.textContent).startsWith(`${dummyName} ·`));
    assert(row, 'build-library entry was not rendered');
    buttonNamed(row, 'Delete').click();
    assert(!surface.textContent.includes(dummyName), 'build-library entry was not deleted');

    buttonNamed(surface, 'Done').click();
    assert(surface.hidden && surface.style.display === 'none',
      'Done did not dismiss Companion Tools');

    openTools();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    assert(surface.hidden && surface.style.display === 'none',
      'Escape did not dismiss Companion Tools');

    openTools();
    surface.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    assert(surface.hidden && surface.style.display === 'none',
      'the backdrop did not dismiss Companion Tools');

    openSettings();
    const settings = document.getElementById('settings');
    assert(settings && !settings.hidden, 'Settings did not open');
    buttonNamed(settings, 'Cancel').click();
    assert(settings.hidden, 'Cancel did not dismiss Settings');

    openGuide();
    const guide = document.getElementById('guide');
    assert(guide && !guide.hidden, 'User Guide did not open');
    buttonNamed(guide, 'Done').click();
    assert(guide.hidden, 'Done did not dismiss User Guide');

    log('[e2e] app PASS');
  } finally {
    overlays.edit(originalEditMode);
    const surface = tools();
    if (surface) {
      if (surface.hidden) openTools();
      for (const widget of widgets) {
        const id = widget.dataset.widget;
        const wanted = originalVisibility.get(id);
        const label = new Map([
          ['clock', 'Clock'],
          ['session-timer', 'Session timer'],
          ['target-details', 'Target details'],
          ['performance', 'Performance'],
        ]).get(id);
        if (label && !widget.hidden !== wanted) buttonNamed(surface, label).click();
      }
      if (!surface.hidden) buttonNamed(surface, 'Done').click();
    }
    const settings = document.getElementById('settings');
    if (settings && !settings.hidden) buttonNamed(settings, 'Cancel').click();
    const guide = document.getElementById('guide');
    if (guide && !guide.hidden) buttonNamed(guide, 'Done').click();
    restoreStorage(storage, stored);
  }
}
