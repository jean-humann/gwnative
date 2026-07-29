// Profile-local skill and team build library.
//
// Codes remain opaque: interpreting them without a versioned codec is how a
// valid future template gets rejected by an older host. The game remains the
// authority that applies a code; this library safely names, groups, searches,
// imports and exports them.

const STORAGE_KEY = 'gwnative.build-library.v1';
const MAX_BUILDS = 500;
const MAX_MEMBERS = 12;
const MAX_NAME = 80;
const MAX_CODE = 1024;

function safeText(value, maximum, field) {
  if (
    typeof value !== 'string'
    || value.trim().length < 1
    || value.trim().length > maximum
    || [...value].some((character) => /\p{Cc}/u.test(character))
  ) {
    throw new Error(`${field} must contain 1–${maximum} printable characters`);
  }
  return value.trim();
}

function member(value) {
  return Object.freeze({
    name: safeText(value?.name, MAX_NAME, 'member name'),
    code: safeText(value?.code, MAX_CODE, 'template code'),
  });
}

function build(value) {
  const members = Array.isArray(value?.members) ? value.members.map(member) : [];
  if (members.length < 1 || members.length > MAX_MEMBERS) {
    throw new Error(`a build needs 1–${MAX_MEMBERS} members`);
  }
  const id =
    typeof value?.id === 'string' && /^[a-f0-9]{16}$/.test(value.id)
      ? value.id
      : crypto.getRandomValues(new Uint32Array(2))
        .reduce((text, word) => text + word.toString(16).padStart(8, '0'), '');
  return Object.freeze({
    id,
    name: safeText(value?.name, MAX_NAME, 'build name'),
    members: Object.freeze(members),
    createdAt:
      Number.isSafeInteger(value?.createdAt) && value.createdAt > 0 ? value.createdAt : Date.now(),
    updatedAt: Date.now(),
  });
}

export function parseLibrary(value) {
  if (!value || value.format !== 1 || !Array.isArray(value.builds)) {
    return Object.freeze({ format: 1, builds: Object.freeze([]) });
  }
  const builds = [];
  const seen = new Set();
  for (const candidate of value.builds.slice(0, MAX_BUILDS)) {
    try {
      const parsed = build(candidate);
      if (!seen.has(parsed.id)) {
        seen.add(parsed.id);
        builds.push(parsed);
      }
    } catch {
      // One malformed imported entry does not erase the valid library around it.
    }
  }
  return Object.freeze({ format: 1, builds: Object.freeze(builds) });
}

export function createBuildLibrary(storage = globalThis.localStorage) {
  let library;
  try {
    library = parseLibrary(JSON.parse(storage?.getItem(STORAGE_KEY) ?? 'null'));
  } catch {
    library = parseLibrary(null);
  }

  const save = (builds) => {
    library = Object.freeze({ format: 1, builds: Object.freeze(builds) });
    storage?.setItem(STORAGE_KEY, JSON.stringify(library));
    return library;
  };

  return Object.freeze({
    list(query = '') {
      const needle = String(query).trim().toLocaleLowerCase();
      return Object.freeze(
        library.builds.filter(
          (entry) =>
            !needle
            || entry.name.toLocaleLowerCase().includes(needle)
            || entry.members.some(
              (entry) =>
                entry.name.toLocaleLowerCase().includes(needle)
                || entry.code.toLocaleLowerCase().includes(needle),
            ),
        ),
      );
    },
    add(value) {
      if (library.builds.length >= MAX_BUILDS) throw new Error(`build limit is ${MAX_BUILDS}`);
      const entry = build(value);
      save([...library.builds, entry]);
      return entry;
    },
    remove(id) {
      const next = library.builds.filter((entry) => entry.id !== id);
      if (next.length === library.builds.length) return false;
      save(next);
      return true;
    },
    export() {
      return JSON.stringify(library, null, 2);
    },
    import(text) {
      const imported = parseLibrary(JSON.parse(text));
      const byId = new Map(library.builds.map((entry) => [entry.id, entry]));
      for (const entry of imported.builds) byId.set(entry.id, entry);
      if (byId.size > MAX_BUILDS) throw new Error(`build limit is ${MAX_BUILDS}`);
      return save([...byId.values()]);
    },
  });
}
