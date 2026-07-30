// Built-in companion tools.
//
// These are deliberately useful on the narrow certified state available
// today. Features requiring effects, inventory, chat or other layouts are named
// as unavailable instead of reading guessed offsets from a live client.

import { createBuildLibrary } from './build-library.js';

export const FEATURES = Object.freeze([
  { id: 'clock', group: 'Telemetry', name: 'Clock', status: 'available' },
  { id: 'timer', group: 'Telemetry', name: 'Session timer', status: 'available' },
  { id: 'target', group: 'Telemetry', name: 'Target and range', status: 'available' },
  { id: 'performance', group: 'Telemetry', name: 'Frame rate', status: 'available' },
  { id: 'builds', group: 'Builds', name: 'Build and team library', status: 'available' },
  { id: 'party', group: 'Game state', name: 'Party and hero roster', status: 'available' },
  { id: 'skillbar', group: 'Game state', name: 'Player skillbar', status: 'available' },
  { id: 'effects', group: 'Game state', name: 'Effects and buffs', status: 'needs-layout' },
  { id: 'maps', group: 'Game state', name: 'Map agents, quests and completion', status: 'needs-layout' },
  { id: 'inventory', group: 'Game state', name: 'Inventory and account storage', status: 'needs-layout' },
  { id: 'chat', group: 'Game state', name: 'Chat and party search', status: 'needs-layout' },
  { id: 'textures', group: 'Presentation', name: 'Texture and shader packs', status: 'research' },
  { id: 'automation', group: 'Policy', name: 'Unattended gameplay automation', status: 'blocked' },
]);

export function formatDuration(milliseconds) {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = seconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, '0')}:${String(remainder).padStart(2, '0')}`
    : `${minutes}:${String(remainder).padStart(2, '0')}`;
}

const setText = (element, value) => {
  element.textContent = String(value);
};

function installWidgets(overlays) {
  const clock = overlays.register({
    id: 'clock',
    title: 'Clock',
    position: { x: 16, y: 16 },
    visible: false,
    render: (body, value) => setText(body, value ?? '--:--:--'),
  });
  const started = performance.now();
  const timer = overlays.register({
    id: 'session-timer',
    title: 'Session',
    position: { x: 16, y: 72 },
    visible: false,
    render: (body, value) => setText(body, value ?? '0:00'),
  });
  const target = overlays.register({
    id: 'target-details',
    title: 'Target',
    position: { x: 16, y: 128 },
    visible: false,
    render: (body, state) => {
      if (state?.status !== 'ready') return setText(body, state?.reason ?? 'Waiting for game');
      if (!state.targetValid) return setText(body, `Map ${state.mapId} · no target`);
      setText(body, `#${state.targetId} · ${Math.round(state.distance)} · ${state.rangeName}`);
    },
  });
  const performanceWidget = overlays.register({
    id: 'performance',
    title: 'Performance',
    position: { x: 16, y: 184 },
    visible: false,
    render: (body, value) => setText(body, value ?? 'Measuring…'),
  });
  const party = overlays.register({
    id: 'party-roster',
    title: 'Party',
    position: { x: 16, y: 240 },
    visible: false,
    render: (body, state) => {
      if (state?.status !== 'ready') return setText(body, state?.reason ?? 'Waiting for game');
      if (!state.party) return setText(body, 'Unavailable for this client build');
      setText(
        body,
        `${state.party.players.length} players · ${state.party.heroes.length} heroes · `
          + `${state.party.henchmen.length} henchmen`,
      );
    },
  });
  const skillbar = overlays.register({
    id: 'player-skillbar',
    title: 'Skillbar',
    position: { x: 16, y: 296 },
    visible: false,
    render: (body, state) => {
      if (state?.status !== 'ready') return setText(body, state?.reason ?? 'Waiting for game');
      if (!state.skillbar) return setText(body, 'Unavailable for this client build');
      setText(
        body,
        state.skillbar.skills
          .map((skill) => `${skill.slot}:${skill.skillId || '—'}`)
          .join(' · '),
      );
    },
  });

  const updateTime = () => {
    if (clock.isVisible()) clock.update(new Date().toLocaleTimeString());
    if (timer.isVisible()) timer.update(formatDuration(performance.now() - started));
  };
  updateTime();
  const interval = setInterval(updateTime, 1000);

  let frames = 0;
  let frameStarted = performance.now();
  const frame = (now) => {
    frames += 1;
    if (now - frameStarted >= 1000) {
      const fps = (frames * 1000) / (now - frameStarted);
      if (performanceWidget.isVisible()) performanceWidget.update(`${fps.toFixed(1)} FPS`);
      frames = 0;
      frameStarted = now;
    }
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);

  window.addEventListener('gwnative:state', (event) => {
    if (target.isVisible()) target.update(event.detail);
    if (party.isVisible()) party.update(event.detail);
    if (skillbar.isVisible()) skillbar.update(event.detail);
  });

  return Object.freeze({
    clock,
    timer,
    target,
    performance: performanceWidget,
    party,
    skillbar,
    dispose: () => clearInterval(interval),
  });
}

function button(document, label, run, className = '') {
  const element = document.createElement('button');
  element.type = 'button';
  element.textContent = label;
  element.className = className;
  element.addEventListener('click', run);
  return element;
}

/**
 * Keep the semantic hidden state and the inline layout in agreement.
 *
 * The panel is built dynamically, so it cannot use index.html's
 * `#overlay[hidden] { display:none }` pattern. A `display:flex` inline style
 * overrides the browser's default rendering for the hidden attribute in
 * WKWebView; setting only `hidden` therefore leaves the sheet over the game.
 *
 * @param {HTMLElement} overlay
 * @param {boolean} visible
 */
export function setPanelVisible(overlay, visible) {
  overlay.hidden = !visible;
  overlay.style.display = visible ? 'flex' : 'none';
  overlay.setAttribute('aria-hidden', visible ? 'false' : 'true');
}

export function installToolsPanel({
  document,
  overlays,
  storage = globalThis.localStorage,
  log = () => {},
}) {
  const widgets = installWidgets(overlays);
  const builds = createBuildLibrary(storage);
  const overlay = document.createElement('div');
  overlay.dataset.surface = 'companion-tools';
  overlay.hidden = true;
  overlay.style.cssText = [
    'position:fixed',
    'inset:0',
    'z-index:4',
    'display:flex',
    'align-items:center',
    'justify-content:center',
    'padding:24px',
    'background:#000c',
  ].join(';');
  // `display:flex` above is the open layout, not the initial state. Make the
  // inline display agree with `hidden` before the element can be painted.
  setPanelVisible(overlay, false);
  const dialog = document.createElement('section');
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-modal', 'true');
  dialog.style.cssText = [
    'width:min(54em,100%)',
    'max-height:100%',
    'overflow:auto',
    'padding:18px',
    'color:#cdd6f4',
    'background:#181825f5',
    'border:1px solid #45475a',
    'border-radius:10px',
    'font:13px/1.5 -apple-system,BlinkMacSystemFont,sans-serif',
  ].join(';');
  const title = document.createElement('h1');
  title.id = 'companion-tools-title';
  title.textContent = 'Companion tools';
  title.style.cssText = 'margin:0 0 14px;font-size:20px;color:#f3dd9d';
  dialog.setAttribute('aria-labelledby', title.id);
  dialog.append(title);

  const widgetTitle = document.createElement('h2');
  widgetTitle.textContent = 'Widgets';
  widgetTitle.style.cssText = 'font-size:15px;color:#d9b25c';
  const widgetButtons = document.createElement('div');
  widgetButtons.style.cssText = 'display:flex;flex-wrap:wrap;gap:8px';
  for (const [label, widget] of [
    ['Clock', widgets.clock],
    ['Session timer', widgets.timer],
    ['Target details', widgets.target],
    ['Performance', widgets.performance],
    ['Party roster', widgets.party],
    ['Player skillbar', widgets.skillbar],
  ]) {
    widgetButtons.append(
      button(document, label, () => widget.visible(!widget.isVisible())),
    );
  }
  widgetButtons.append(
    button(document, 'Edit layout', () => overlays.edit(!overlays.editing())),
  );
  dialog.append(widgetTitle, widgetButtons);

  const buildTitle = document.createElement('h2');
  buildTitle.textContent = 'Build library';
  buildTitle.style.cssText = 'margin-top:20px;font-size:15px;color:#d9b25c';
  const form = document.createElement('form');
  form.style.cssText = 'display:grid;grid-template-columns:1fr 1fr 2fr auto;gap:8px';
  const buildName = document.createElement('input');
  buildName.placeholder = 'Build name';
  const memberName = document.createElement('input');
  memberName.placeholder = 'Character or role';
  const code = document.createElement('input');
  code.placeholder = 'Template code';
  const add = button(document, 'Save', () => {}, 'primary');
  add.type = 'submit';
  form.append(buildName, memberName, code, add);
  const buildList = document.createElement('div');
  buildList.style.cssText = 'display:flex;flex-direction:column;gap:6px;margin-top:10px';
  const transfer = document.createElement('textarea');
  transfer.placeholder = 'Exported build-library JSON';
  transfer.rows = 4;
  transfer.style.cssText = 'box-sizing:border-box;width:100%;margin-top:10px';
  const buildActions = document.createElement('div');
  buildActions.style.cssText = 'display:flex;gap:8px;margin-top:8px';

  const drawBuilds = () => {
    buildList.replaceChildren();
    for (const entry of builds.list()) {
      const row = document.createElement('div');
      row.style.cssText =
        'display:flex;align-items:center;justify-content:space-between;gap:10px;padding:6px 8px;background:#11111b';
      const line = document.createElement('span');
      line.textContent = `${entry.name} · ${entry.members
        .map((member) => `${member.name}: ${member.code}`)
        .join(' · ')}`;
      row.append(
        line,
        button(document, 'Delete', () => {
          builds.remove(entry.id);
          drawBuilds();
        }),
      );
      buildList.append(row);
    }
    if (buildList.childElementCount === 0) {
      const empty = document.createElement('div');
      empty.textContent = 'No builds saved in this profile.';
      empty.style.color = '#9399b2';
      buildList.append(empty);
    }
  };
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    try {
      builds.add({
        name: buildName.value,
        members: [{ name: memberName.value, code: code.value }],
      });
      form.reset();
      drawBuilds();
    } catch (error) {
      log(`[warn] build library: ${error}`);
    }
  });
  buildActions.append(
    button(document, 'Export', () => {
      transfer.value = builds.export();
    }),
    button(document, 'Import', () => {
      try {
        builds.import(transfer.value);
        drawBuilds();
      } catch (error) {
        log(`[warn] build import: ${error}`);
      }
    }),
  );
  drawBuilds();
  dialog.append(buildTitle, form, buildList, transfer, buildActions);

  const statusTitle = document.createElement('h2');
  statusTitle.textContent = 'Compatibility status';
  statusTitle.style.cssText = 'margin-top:20px;font-size:15px;color:#d9b25c';
  const statusList = document.createElement('ul');
  for (const feature of FEATURES) {
    const item = document.createElement('li');
    item.textContent = `${feature.name} — ${feature.status}`;
    statusList.append(item);
  }
  let restoreFocus = null;
  const dismiss = () => {
    if (overlay.hidden) return;
    setPanelVisible(overlay, false);
    const target = restoreFocus;
    restoreFocus = null;
    target?.focus?.();
  };
  const close = button(document, 'Done', dismiss, 'primary');
  close.style.marginTop = '12px';
  dialog.append(statusTitle, statusList, close);
  overlay.append(dialog);
  document.body.append(overlay);

  const open = () => {
    restoreFocus = document.activeElement;
    setPanelVisible(overlay, true);
    buildName.focus();
  };
  overlay.addEventListener('click', (event) => {
    if (event.target === overlay) dismiss();
  });
  window.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && !overlay.hidden) dismiss();
  });
  return open;
}
