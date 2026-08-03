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

const NATIVE_ACTION_CODES = Object.freeze({
  activate: 'enter',
  'focus-window': 'focus-window',
  'move-forward': 'arrow-up',
  'move-backward': 'arrow-down',
  'turn-left': 'arrow-left',
  'turn-right': 'arrow-right',
  'target-next': 'tab',
  interact: 'space',
  cancel: 'escape',
  'skill-1': 'digit-1',
  'probe-secure-input': 'digit-1',
});

const EVENT_CODES = Object.freeze({
  Enter: 'enter',
  ArrowUp: 'arrow-up',
  ArrowDown: 'arrow-down',
  ArrowLeft: 'arrow-left',
  ArrowRight: 'arrow-right',
  Tab: 'tab',
  Space: 'space',
  Escape: 'escape',
  Digit1: 'digit-1',
});

const PAGE_ACTIONS = new Set([
  'test-ui',
  'probe-layout',
  'probe-benchmark-ui',
  'prepare-benchmark-scene',
  'sample-performance',
]);

const KAMADAN_MAP_ID = 449;
const ENGLISH_LANGUAGE_ID = 0;
const XUNLAI_AGENT_PLAYER_NUMBER = 5052;
const XUNLAI_AGENT_LEVEL = 24;
const XUNLAI_AGENT_ALLEGIANCE = 6;
const XUNLAI_DISTANCE = 180;
const XUNLAI_PAIR_MAX_DISTANCE = 600;
const XUNLAI_CROWD_RADIUS = 2_500;
const XUNLAI_INTERACTION_ATTEMPTS = 5;
const XUNLAI_POLLS_PER_ATTEMPT = 50;
const BENCHMARK_MIN_AGENT_COUNT = 80;
const BENCHMARK_POPULATION_POLLS = 20;

const gameState = (window) => window.gwCompanionState?.status === 'ready'
  ? window.gwCompanionState
  : null;

const waitFor = async (predicate, sleep, timeout, message) => {
  const deadline = performance.now() + timeout;
  while (performance.now() < deadline) {
    const result = predicate();
    if (result) return result;
    await sleep(100);
  }
  throw new Error(message);
};

const loadBenchmarkDistrict = async (window, sleep, district) => {
  const runtime = window.gwCompanionRuntime;
  const placement = () => runtime?.benchmarkSceneState?.();
  // Always issue the America packet. Language 0 is shared by America and
  // Europe-English, so map/language/district alone cannot distinguish them
  // when the saved character happens to start in Europe-English District 2.
  await runtime.benchmarkSceneCommand('travel-america', district);
  return waitFor(
    () => {
      const state = placement();
      return state?.mapId === KAMADAN_MAP_ID
        && state.language === ENGLISH_LANGUAGE_ID
        && state.district === district
        && state.instanceType === 0
        && gameState(window)
        ? state
        : null;
    },
    sleep,
    20_000,
    `Kamadan America-English District ${district} did not load`,
  );
};

const waitForBenchmarkPopulation = async (window, sleep) => {
  let consecutive = 0;
  for (let poll = 0; poll < BENCHMARK_POPULATION_POLLS; poll += 1) {
    const state = gameState(window);
    const total = state?.agents?.total;
    consecutive = Number.isSafeInteger(total) && total >= BENCHMARK_MIN_AGENT_COUNT
      ? consecutive + 1
      : 0;
    if (consecutive >= 3) return state;
    await sleep(100);
  }
  return null;
};

const travelToBenchmarkDistrict = async (window, sleep) => {
  let preferred = null;
  try {
    preferred = await loadBenchmarkDistrict(window, sleep, 2);
  } catch {
    // District 1 remains the bounded fallback if District 2 cannot load.
  }
  if (preferred && await waitForBenchmarkPopulation(window, sleep)) return preferred;

  const fallback = await loadBenchmarkDistrict(window, sleep, 1);
  assert(
    await waitForBenchmarkPopulation(window, sleep),
    `neither Kamadan America-English district has ${BENCHMARK_MIN_AGENT_COUNT} agents`,
  );
  return fallback;
};

const recertifyBenchmarkPosition = async (window, sleep, anchorId) => {
  const state = await waitForBenchmarkPopulation(window, sleep);
  const anchor = state && xunlaiMatches(state)
    .find((agent) => agent.agentId === anchorId);
  if (!state || !anchor) return null;
  const distance = Math.hypot(anchor.x - state.playerX, anchor.y - state.playerY);
  return distance <= XUNLAI_DISTANCE ? { state, anchor, distance } : null;
};

const xunlaiMatches = (state) =>
  (state.agents?.agents ?? []).filter((agent) => (
    agent.isLiving
    && agent.playerNumber === XUNLAI_AGENT_PLAYER_NUMBER
    && agent.level === XUNLAI_AGENT_LEVEL
    && agent.allegiance === XUNLAI_AGENT_ALLEGIANCE
    && Number.isFinite(agent.x)
    && Number.isFinite(agent.y)
  ));

// Kamadan has more than one otherwise-identical Xunlai storage pair. Select
// the one surrounded by the most nearby living agents: that both avoids a
// coordinate/build identifier and defines the crowded workload this benchmark
// is intended to reproduce. Once selected, the agent id is held only for this
// district instance so moving players cannot switch the destination mid-path.
const xunlaiAnchor = (state) => {
  const matches = xunlaiMatches(state);
  const living = (state.agents?.agents ?? []).filter((agent) => (
    agent.isLiving && Number.isFinite(agent.x) && Number.isFinite(agent.y)
  ));
  const pairs = [];
  for (let left = 0; left < matches.length; left += 1) {
    for (let right = left + 1; right < matches.length; right += 1) {
      const midpointX = (matches[left].x + matches[right].x) / 2;
      const midpointY = (matches[left].y + matches[right].y) / 2;
      const distance = Math.hypot(
        matches[left].x - matches[right].x,
        matches[left].y - matches[right].y,
      );
      if (distance > XUNLAI_PAIR_MAX_DISTANCE) continue;
      pairs.push({
        agents: [matches[left], matches[right]],
        distance,
        crowd: living.reduce((score, agent) => {
          const proximity = 1 - Math.hypot(agent.x - midpointX, agent.y - midpointY)
            / XUNLAI_CROWD_RADIUS;
          return score + Math.max(0, proximity);
        }, 0),
      });
    }
  }
  pairs.sort((a, b) => b.crowd - a.crowd
    || a.distance - b.distance
    || a.agents[0].agentId - b.agents[0].agentId
    || a.agents[1].agentId - b.agents[1].agentId);
  const selected = pairs[0];
  const runnerUp = pairs[1];
  if (
    !selected
    || (runnerUp && runnerUp.crowd === selected.crowd)
  ) return null;
  return [...selected.agents].sort((a, b) => a.x - b.x || a.agentId - b.agentId)[0];
};

const xunlaiDiagnostic = (state) => {
  const captured = state?.agents?.agents ?? [];
  const numbered = captured.filter(
    (agent) => agent.playerNumber === XUNLAI_AGENT_PLAYER_NUMBER,
  );
  const living = numbered.filter((agent) => agent.isLiving);
  const leveled = living.filter((agent) => agent.level === XUNLAI_AGENT_LEVEL);
  const allied = leveled.filter((agent) => agent.allegiance === XUNLAI_AGENT_ALLEGIANCE);
  const points = allied
    .map((agent) => `${agent.agentId}@${Math.round(agent.x)},${Math.round(agent.y)}`)
    .join(';');
  return `captured=${captured.length}/${state?.agents?.total ?? 0},`
    + ` number=${numbered.length}, living=${living.length},`
    + ` level=${leveled.length}, matches=${points || 'none'}`;
};

const positionAtXunlai = async (window, sleep) => {
  let state;
  try {
    state = await waitFor(
      () => {
        const candidate = gameState(window);
        return candidate && xunlaiAnchor(candidate) ? candidate : null;
      },
      sleep,
      10_000,
      'the certified Kamadan Xunlai anchor is unavailable',
    );
  } catch {
    throw new Error(
      `the certified Kamadan Xunlai anchor is unavailable (${xunlaiDiagnostic(gameState(window))})`,
    );
  }
  const anchor = xunlaiAnchor(state);
  assert(anchor, 'the certified Kamadan Xunlai anchor disappeared');
  const initialDistance = Math.hypot(anchor.x - state.playerX, anchor.y - state.playerY);
  if (initialDistance <= XUNLAI_DISTANCE) return { state, anchor, distance: initialDistance };
  let lastDistance = initialDistance;
  for (let attempt = 0; attempt < XUNLAI_INTERACTION_ATTEMPTS; attempt += 1) {
    await window.gwCompanionRuntime.benchmarkSceneCommand(
      'interact-xunlai',
      anchor.agentId,
    );
    for (let poll = 0; poll < XUNLAI_POLLS_PER_ATTEMPT; poll += 1) {
      const next = gameState(window);
      const nextAnchor = next && xunlaiMatches(next)
        .find((agent) => agent.agentId === anchor.agentId);
      if (nextAnchor) {
        lastDistance = Math.hypot(nextAnchor.x - next.playerX, nextAnchor.y - next.playerY);
        if (lastDistance <= XUNLAI_DISTANCE) {
          return { state: next, anchor: nextAnchor, distance: lastDistance };
        }
      }
      await sleep(100);
    }
  }
  throw new Error(
    `the in-client InteractNPC command did not reach the Xunlai anchor (distance=${lastDistance.toFixed(1)})`,
  );
};

const prepareBenchmarkScene = async (window, sleep) => {
  assert(
    typeof window.gwCompanionRuntime?.benchmarkSceneState === 'function'
      && typeof window.gwCompanionRuntime?.benchmarkSceneCommand === 'function',
    'certified benchmark scene API is unavailable',
  );
  await window.gwCompanionRuntime.benchmarkSceneCommand('high-graphics', 0);
  let placement = await travelToBenchmarkDistrict(window, sleep);
  let path = await positionAtXunlai(window, sleep);
  let positioned = await recertifyBenchmarkPosition(window, sleep, path.anchor.agentId);
  if (!positioned && placement.district === 2) {
    placement = await loadBenchmarkDistrict(window, sleep, 1);
    assert(
      await waitForBenchmarkPopulation(window, sleep),
      `Kamadan America-English District 1 has fewer than ${BENCHMARK_MIN_AGENT_COUNT} agents`,
    );
    path = await positionAtXunlai(window, sleep);
    positioned = await recertifyBenchmarkPosition(window, sleep, path.anchor.agentId);
  }
  assert(
    positioned,
    `no populated Kamadan district retained ${BENCHMARK_MIN_AGENT_COUNT} agents at Xunlai`,
  );
  return Object.freeze({
    mapId: placement.mapId,
    district: placement.district,
    language: placement.language,
    playerX: positioned.state.playerX,
    playerY: positioned.state.playerY,
    anchorX: positioned.anchor.x,
    anchorY: positioned.anchor.y,
    anchorDistance: positioned.distance,
    agentCount: positioned.state.agents?.total ?? 0,
    graphicsPreset: 'high',
  });
};

const targetName = (window, canvas, candidate) => {
  if (!candidate || candidate === canvas) return 'canvas';
  const kind = Object.entries(window.Module?.oskInput ?? {})
    .find(([, input]) => input === candidate)?.[0];
  return kind ? `${kind}-proxy` : 'text-proxy';
};

/**
 * Focus the only target a forthcoming native action is allowed to reach.
 *
 * This does not dispatch input. Its acknowledgement is what lets the host send
 * the trusted AppKit event without racing the page's action long poll.
 */
export function prepareNativeE2EAction(action, { window, canvas, secureInput = null }) {
  assert(Number.isSafeInteger(action?.sequence) && action.sequence > 0,
    'E2E action has no valid sequence');
  assert(
    Object.hasOwn(NATIVE_ACTION_CODES, action.action),
    'E2E native action is not in the allowed vocabulary',
  );
  const activeInput = window.Module?.oskActiveInput;
  const target = action.action === 'probe-secure-input'
    ? secureInput
    : action.action === 'activate' && activeInput ? activeInput : canvas;
  assert(target, 'secure-input probe is not installed');
  if (action.action === 'probe-secure-input') target.value = '';
  target.focus?.();
  return action.action === 'probe-secure-input'
    ? 'password-proxy'
    : targetName(window, canvas, target);
}

/**
 * Deliver one page-owned action from the E2E vocabulary.
 *
 * Gameplay input is delivered as an AppKit event by the native host. Keeping
 * it out of this function means a test cannot accidentally regress to an
 * untrusted constructed `KeyboardEvent`, which WebKit and the client are both
 * allowed to treat differently from real input.
 *
 * @param {{ sequence: number, action: string, durationMs: number }} action
 * @param {{
 *   window: Window,
 *   canvas: HTMLCanvasElement,
 *   sleep?: (milliseconds: number) => Promise<void>,
 * }} options
 */
export async function executeE2EAction(action, {
  window,
  canvas,
  sleep = wait,
}) {
  assert(Number.isSafeInteger(action?.sequence) && action.sequence > 0,
    'E2E action has no valid sequence');
  assert(PAGE_ACTIONS.has(action.action), 'gameplay actions are native-only');
  if (action.action === 'sample-performance') {
    assert(
      Number.isSafeInteger(action.durationMs)
        && action.durationMs >= 1_000
        && action.durationMs <= 60_000,
      'performance sample duration is outside its bound',
    );
    assert(
      typeof window.gwFrameAudit?.beginPerformanceSample === 'function',
      'frame performance sampler is not installed',
    );
    const finish = window.gwFrameAudit.beginPerformanceSample();
    await sleep(action.durationMs);
    const sampled = finish();
    return {
      target: 'app-ui',
      activeTarget: targetName(window, canvas, window.Module?.oskActiveInput),
      performanceSample: {
        actionSequence: action.sequence,
        requestedDurationMs: action.durationMs,
        runtime: sampled.runtime,
        durationMs: sampled.durationMs,
        frames: sampled.frames,
        framesPerSecond: sampled.framesPerSecond,
        intervalMs: sampled.intervalMs,
        callbackToSwapMs: sampled.callbackToSwapMs,
        canvas: {
          width: sampled.canvas?.width ?? null,
          height: sampled.canvas?.height ?? null,
          cssWidth: sampled.canvas?.css?.width ?? null,
          cssHeight: sampled.canvas?.css?.height ?? null,
        },
        webgl: {
          type: sampled.webgl?.type ?? null,
          lost: sampled.webgl?.lost ?? null,
          drawingBufferWidth: sampled.webgl?.drawingBufferWidth ?? null,
          drawingBufferHeight: sampled.webgl?.drawingBufferHeight ?? null,
        },
        audit: sampled.audit,
        gpuTiming: 'not-sampled',
      },
    };
  }
  assert(action.durationMs === 0, 'page-owned E2E action duration is outside its bound');
  let layoutProbe;
  let benchmarkUi;
  let benchmarkScene;
  if (action.action === 'test-ui') {
    assert(typeof window.gwRunAppE2E === 'function', 'app UI test is not installed');
    await window.gwRunAppE2E();
  } else if (action.action === 'probe-layout') {
    assert(
      typeof window.gwCompanionRuntime?.probeLayout === 'function',
      'companion layout probe is not installed',
    );
    layoutProbe = window.gwCompanionRuntime.probeLayout();
  } else if (action.action === 'probe-benchmark-ui') {
    assert(
      typeof window.gwCompanionRuntime?.benchmarkUiState === 'function',
      'benchmark UI probe is not installed',
    );
    benchmarkUi = {
      actionSequence: action.sequence,
      ...window.gwCompanionRuntime.benchmarkUiState(),
    };
  } else if (action.action === 'prepare-benchmark-scene') {
    benchmarkScene = {
      actionSequence: action.sequence,
      ...await prepareBenchmarkScene(window, sleep),
    };
  }
  return {
    target: 'app-ui',
    activeTarget: targetName(window, canvas, window.Module?.oskActiveInput),
    ...(layoutProbe ? { layoutProbe } : {}),
    ...(benchmarkUi ? { benchmarkUi } : {}),
    ...(benchmarkScene ? { benchmarkScene } : {}),
  };
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
  const socketActions = new Map();
  const preparedNativeActions = [];
  let activeNativeAction = null;

  // A real password field is deliberately used here. macOS treats it
  // differently from ordinary text input, so a DOM-only test cannot catch the
  // secure-input regression this probe exists for. The host can send only the
  // fixed digit below; no API accepts text or credentials, and only the
  // resulting length crosses the test boundary.
  const secureContainer = window.document.createElement('div');
  secureContainer.className = 'osk-container';
  const secureInput = window.document.createElement('input');
  secureInput.type = 'password';
  secureInput.tabIndex = -1;
  secureInput.autocomplete = 'off';
  secureInput.className = 'osk-input';
  secureInput.setAttribute('aria-hidden', 'true');
  secureContainer.append(secureInput);
  window.document.body.append(secureContainer);
  const nativeKey = (kind, event) => {
    if (!event.isTrusted) return;
    const code = EVENT_CODES[event.code] ?? 'other';
    if (kind === 'native-key-observed') {
      const prepared = preparedNativeActions[0];
      if (!prepared || prepared.code !== code || activeNativeAction) return;
      activeNativeAction = preparedNativeActions.shift();
    } else if (!activeNativeAction || activeNativeAction.code !== code) {
      return;
    }
    const keyCode = Number.isInteger(event.keyCode) && event.keyCode >= 0
      ? event.keyCode
      : 0;
    void report(kind, {
      actionSequence: activeNativeAction.sequence,
      code,
      keyCode,
    }).catch(() => {});
    if (kind === 'native-key-released') activeNativeAction = null;
  };
  const nativeKeyDown = (event) => nativeKey('native-key-observed', event);
  const nativeKeyUp = (event) => nativeKey('native-key-released', event);
  window.addEventListener('keydown', nativeKeyDown, true);
  window.addEventListener('keyup', nativeKeyUp, true);

  const report = async (kind, detail = {}) => {
    const response = await fetch('__e2e/v1/events', {
      method: 'POST',
      headers,
      body: JSON.stringify({ kind, detail }),
    });
    if (!response.ok) throw new Error(`E2E event ${kind} was refused (${response.status})`);
    return response.json();
  };

  secureInput.addEventListener('input', () => {
    const action = activeNativeAction;
    if (action?.action !== 'probe-secure-input') return;
    const length = secureInput.value.length;
    secureInput.value = '';
    canvas.focus?.();
    void report('secure-input-observed', {
      actionSequence: action.sequence,
      length,
    }).catch(() => {});
  });

  let characterSelectionState = 'unavailable';
  const characterSelection = createCharacterSelectionMilestone({
    afterFrame: window.requestAnimationFrame.bind(window),
    selectorReady: () => {
      const state = window.gwCompanionRuntime?.characterSelectionState?.()
        ?? 'unavailable';
      if (state !== characterSelectionState) {
        characterSelectionState = state;
        log(`[e2e] character selection: ${state}`);
      }
      return state === 'ready';
    },
    report: () => report('character-selection-ready'),
  });

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
          // Native gameplay delivery has its own copy of this command. The
          // page focuses the client's finite input target and acknowledges it
          // before AppKit is woken, then only observes resulting socket traffic.
          if (!PAGE_ACTIONS.has(action.action)) {
            let target = 'canvas';
            try {
              target = prepareNativeE2EAction(action, {
                window,
                canvas,
                secureInput,
              });
              const prepared = {
                sequence: action.sequence,
                code: NATIVE_ACTION_CODES[action.action],
                action: action.action,
              };
              // focus-window deliberately emits no key event. Queuing it here
              // would make the next real native key fail correlation.
              const observesNativeKey = action.action !== 'focus-window';
              if (observesNativeKey) preparedNativeActions.push(prepared);
              await report('action-prepared', {
                actionSequence: action.sequence,
                action: action.action,
                target,
              });
              if (action.action === 'focus-window') {
                // The native host brings the NSWindow forward after the
                // preparation acknowledgement. A native animation frame is
                // the semantic proof that WebKit resumed; a timer would let a
                // hidden or sleeping view produce a meaningless FPS sample.
                window.requestAnimationFrame(() => {
                  void report('window-frame-ready', {
                    actionSequence: action.sequence,
                  }).catch(() => {});
                });
              }
            } catch (error) {
              const index = preparedNativeActions.findIndex(
                (candidate) => candidate.sequence === action.sequence,
              );
              if (index >= 0) preparedNativeActions.splice(index, 1);
              await report('action-fail', {
                actionSequence: action.sequence,
                action: action.action,
                target,
                activeTarget: targetName(window, canvas, window.Module?.oskActiveInput),
                message: cleanMessage(error instanceof Error ? error.message : error),
              });
            }
            continue;
          }
          let target = 'canvas';
          let activeTarget = 'canvas';
          try {
            const result = await executeE2EAction(action, { window, canvas });
            ({ target, activeTarget } = result);
            if (result.layoutProbe) await report('layout-probe', result.layoutProbe);
            if (result.benchmarkUi) await report('benchmark-ui', result.benchmarkUi);
            if (result.benchmarkScene) await report('benchmark-scene', result.benchmarkScene);
            if (result.performanceSample) {
              await report('performance-sample', result.performanceSample);
            }
            await report('action-complete', {
              actionSequence: action.sequence,
              action: action.action,
              target,
              activeTarget,
            });
          } catch (error) {
            await report('action-fail', {
              actionSequence: action.sequence,
              action: action.action,
              target,
              activeTarget,
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
      characterSelection.gameReady();
      void report('game-ready').catch(() => {});
    }
  };
  window.addEventListener('gwnative:state', gameState);

  const stop = () => {
    stopped = true;
    controller?.abort();
    window.removeEventListener('gwnative:state', gameState);
    window.removeEventListener('keydown', nativeKeyDown, true);
    window.removeEventListener('keyup', nativeKeyUp, true);
    secureContainer.remove();
  };
  window.addEventListener('pagehide', stop, { once: true });
  void report('bridge-ready').catch((error) => {
    log(`[e2e] bridge: ${cleanMessage(error instanceof Error ? error.message : error)}`);
  });

  const traffic = (direction, socketId, bytes, role = 'other') => {
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

  const socketCreated = (socketId) => {
    socketActions.set(socketId, trafficAction);
  };

  const connection = (socketId) => {
    const actionSequence = socketActions.get(socketId) ?? 0;
    if (actionSequence <= 0) return;
    void report('socket-open', {
      actionSequence,
      socketId,
    }).catch(() => {});
  };

  void pump();

  return Object.freeze({
    report,
    traffic,
    socketCreated,
    connection,
    authenticationCommitted: characterSelection.authenticationCommitted,
    stop,
  });
}

/**
 * Turn the authenticated transition into a semantic selector-ready milestone.
 *
 * The WebGate token response only starts polling. Two consecutive client
 * frames must independently prove that the certified Guild Wars `Selector`
 * is visible and its `Play` frame and parent are created. A ready game
 * cancels the poll for clients that enter a character without a selector.
 */
export function createCharacterSelectionMilestone({
  afterFrame,
  selectorReady,
  report,
  settleFrames = 2,
}) {
  let authenticated = false;
  let finished = false;
  let polling = false;
  let readyFrames = 0;

  const poll = () => {
    if (!authenticated || finished || polling) return;
    polling = true;
    afterFrame(() => {
      polling = false;
      if (finished) return;
      let ready = false;
      try {
        ready = selectorReady() === true;
      } catch {
        ready = false;
      }
      readyFrames = ready ? readyFrames + 1 : 0;
      if (readyFrames >= settleFrames) {
        finished = true;
        void Promise.resolve().then(report).catch(() => {});
      } else {
        poll();
      }
    });
  };

  return Object.freeze({
    authenticationCommitted() {
      authenticated = true;
      poll();
    },
    gameReady() {
      finished = true;
    },
  });
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
      ['party-roster', 'Party roster'],
      ['player-skillbar', 'Player skillbar'],
      ['player-effects', 'Player effects'],
      ['map-agents', 'Map agents'],
      ['quest-log', 'Quest log'],
      ['inventory', 'Inventory'],
      ['completion', 'Mission and map completion'],
      ['social', 'Friends and guild'],
      ['camera', 'Camera and render state'],
      ['trade', 'Trade offer'],
      ['ui-frames', 'UI frame inventory'],
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
          ['party-roster', 'Party roster'],
          ['player-skillbar', 'Player skillbar'],
          ['player-effects', 'Player effects'],
          ['map-agents', 'Map agents'],
          ['quest-log', 'Quest log'],
          ['inventory', 'Inventory'],
          ['completion', 'Mission and map completion'],
          ['social', 'Friends and guild'],
          ['camera', 'Camera and render state'],
          ['trade', 'Trade offer'],
          ['ui-frames', 'UI frame inventory'],
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
