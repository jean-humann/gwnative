// Context-aware hotkeys for companion UI and explicitly user-triggered tools.
//
// This engine does not generate game input. A handler is an ordinary local UI
// callback, which keeps hotkeys separate from unattended gameplay automation.

const MODIFIERS = Object.freeze({
  command: 'metaKey',
  cmd: 'metaKey',
  meta: 'metaKey',
  control: 'ctrlKey',
  ctrl: 'ctrlKey',
  option: 'altKey',
  alt: 'altKey',
  shift: 'shiftKey',
});

export function parseChord(chord) {
  const parts = String(chord)
    .split('+')
    .map((part) => part.trim().toLocaleLowerCase())
    .filter(Boolean);
  const result = {
    key: '',
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
  };
  for (const part of parts) {
    const modifier = MODIFIERS[part];
    if (modifier) {
      result[modifier] = true;
    } else if (!result.key) {
      result.key = part === 'space' ? ' ' : part;
    } else {
      throw new Error(`hotkey ${JSON.stringify(chord)} has more than one key`);
    }
  }
  if (!result.key) throw new Error(`hotkey ${JSON.stringify(chord)} has no key`);
  return Object.freeze(result);
}

export function matchesChord(event, chord) {
  return (
    String(event.key).toLocaleLowerCase() === chord.key
    && Boolean(event.metaKey) === chord.metaKey
    && Boolean(event.ctrlKey) === chord.ctrlKey
    && Boolean(event.altKey) === chord.altKey
    && Boolean(event.shiftKey) === chord.shiftKey
  );
}

export function createHotkeyEngine(window) {
  const registrations = new Map();
  let state = Object.freeze({ status: 'waiting' });

  window.addEventListener('gwnative:state', (event) => {
    state = event.detail ?? state;
  });
  window.addEventListener(
    'keydown',
    (event) => {
      const target = event.target;
      if (
        target?.isContentEditable
        || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target?.nodeName)
      ) {
        return;
      }
      for (const registration of registrations.values()) {
        if (
          matchesChord(event, registration.chord)
          && (!registration.when || registration.when(state))
        ) {
          event.preventDefault();
          registration.run({ state, event });
          return;
        }
      }
    },
    true,
  );

  return Object.freeze({
    register({ id, chord, run, when }) {
      if (!/^[a-z][a-z0-9-]{0,63}$/.test(id)) throw new Error('invalid hotkey id');
      if (registrations.has(id)) throw new Error(`hotkey ${id} is already registered`);
      if (typeof run !== 'function') throw new Error(`hotkey ${id} has no handler`);
      registrations.set(id, {
        chord: parseChord(chord),
        run,
        when: typeof when === 'function' ? when : null,
      });
      return () => registrations.delete(id);
    },
    list: () => Object.freeze([...registrations.keys()]),
    state: () => state,
  });
}
