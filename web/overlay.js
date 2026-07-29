// Shared overlay surface for built-in widgets and explicitly loaded mods.
//
// Widgets get one managed element and cannot take over the canvas. Layout is
// profile-specific because each profile has its own WebKit origin/localStorage.

const STORAGE_KEY = 'gwnative.overlay-layout.v1';
const ID = /^[a-z][a-z0-9-]{0,63}$/;

export function validateLayout(value) {
  if (!value || value.format !== 1 || typeof value.widgets !== 'object') {
    return Object.freeze({ format: 1, widgets: {} });
  }
  const widgets = {};
  for (const [id, position] of Object.entries(value.widgets).slice(0, 128)) {
    if (
      ID.test(id)
      && Number.isFinite(position?.x)
      && Number.isFinite(position?.y)
      && Math.abs(position.x) <= 100_000
      && Math.abs(position.y) <= 100_000
    ) {
      widgets[id] = Object.freeze({
        x: Math.round(position.x),
        y: Math.round(position.y),
        visible: position.visible !== false,
      });
    }
  }
  return Object.freeze({ format: 1, widgets: Object.freeze(widgets) });
}

function readLayout(storage) {
  try {
    return validateLayout(JSON.parse(storage?.getItem(STORAGE_KEY) ?? 'null'));
  } catch {
    return validateLayout(null);
  }
}

export function createOverlayManager({
  document,
  storage = globalThis.localStorage,
  log = () => {},
}) {
  const root = document.createElement('div');
  root.id = 'gwnative-overlays';
  root.style.cssText = [
    'position:fixed',
    'inset:0',
    'z-index:2',
    'pointer-events:none',
    'overflow:hidden',
  ].join(';');
  document.body.append(root);

  let editMode = false;
  let layout = readLayout(storage);
  const widgets = new Map();

  const persist = () => {
    const value = {
      format: 1,
      widgets: Object.fromEntries(
        [...widgets].map(([id, widget]) => [
          id,
          {
            x: Number.parseFloat(widget.element.style.left) || 0,
            y: Number.parseFloat(widget.element.style.top) || 0,
            visible: !widget.element.hidden,
          },
        ]),
      ),
    };
    layout = validateLayout(value);
    try {
      storage?.setItem(STORAGE_KEY, JSON.stringify(layout));
    } catch (error) {
      log(`[warn] overlay layout was not saved: ${error}`);
    }
  };

  const edit = (enabled) => {
    editMode = Boolean(enabled);
    root.style.pointerEvents = editMode ? 'auto' : 'none';
    for (const widget of widgets.values()) {
      widget.element.style.outline = editMode ? '1px dashed #d9b25c' : 'none';
      widget.header.hidden = !editMode;
    }
    return editMode;
  };

  const register = ({
    id,
    title,
    position = { x: 16, y: 16 },
    visible = true,
    render,
  }) => {
    if (!ID.test(id)) throw new Error(`invalid overlay widget id ${JSON.stringify(id)}`);
    if (widgets.has(id)) throw new Error(`overlay widget ${id} is already registered`);
    if (typeof render !== 'function') throw new Error(`overlay widget ${id} has no renderer`);

    const saved = layout.widgets[id];
    const element = document.createElement('section');
    element.dataset.widget = id;
    element.style.cssText = [
      'position:absolute',
      `left:${saved?.x ?? position.x}px`,
      `top:${saved?.y ?? position.y}px`,
      'min-width:8em',
      'color:#e8e8e8',
      'background:#080808d9',
      'border:1px solid #444',
      'border-radius:4px',
      'font:12px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace',
      'user-select:none',
    ].join(';');
    element.hidden = saved ? saved.visible === false : !visible;

    const header = document.createElement('header');
    header.textContent = title || id;
    header.hidden = !editMode;
    header.style.cssText =
      'padding:3px 7px;color:#f3dd9d;background:#181825;cursor:move;touch-action:none';
    const body = document.createElement('div');
    body.style.cssText = 'padding:5px 8px;pointer-events:none';
    element.append(header, body);
    root.append(element);

    let drag = null;
    header.addEventListener('pointerdown', (event) => {
      if (!editMode) return;
      drag = {
        x: event.clientX,
        y: event.clientY,
        left: Number.parseFloat(element.style.left) || 0,
        top: Number.parseFloat(element.style.top) || 0,
      };
      header.setPointerCapture?.(event.pointerId);
    });
    header.addEventListener('pointermove', (event) => {
      if (!drag) return;
      element.style.left = `${Math.round(drag.left + event.clientX - drag.x)}px`;
      element.style.top = `${Math.round(drag.top + event.clientY - drag.y)}px`;
    });
    const finish = () => {
      if (!drag) return;
      drag = null;
      persist();
    };
    header.addEventListener('pointerup', finish);
    header.addEventListener('pointercancel', finish);

    const widget = { element, header, body };
    widgets.set(id, widget);
    render(body, null);

    return Object.freeze({
      id,
      update(value) {
        render(body, value);
      },
      visible(visible) {
        element.hidden = !visible;
        persist();
      },
      isVisible: () => !element.hidden,
      dispose() {
        widgets.delete(id);
        element.remove();
        persist();
      },
    });
  };

  return Object.freeze({
    register,
    edit,
    editing: () => editMode,
    list: () => Object.freeze([...widgets.keys()]),
  });
}
