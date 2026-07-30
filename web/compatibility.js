// What this app can and cannot do against the client build it was handed.
//
// The client is ArenaNet's and it changes without warning. One feature depends
// on recognising the inside of it — saving a build template writes a file, and
// the five routines that do the writing are the ones `src/wasm.rs` patches. On a
// build this release has never been checked against, the patch is not applied
// and the Save button in the template window does nothing at all.
//
// Silence there reads as a broken game. It is not: everything else the client
// does is unaffected, which is the part worth saying in the same breath as the
// part that is missing.
//
// The settings panel says it whenever it is open, beside the affected controls.
// This module also records the transition in diagnostics once per artifact.
// Neither path is allowed to delay the original client.

/**
 * What to tell the player about build templates, or null when there is nothing
 * to tell.
 *
 * @param {unknown} state `window.__gwnativeTemplateSave` — 'ready',
 *   'uncertified' or 'failed'
 * @returns {string | null}
 */
export function templateSaveNotice(state) {
  if (state === 'uncertified') {
    return (
      'Build templates cannot be saved: this release has not been checked against ' +
      'the client build ArenaNet is currently shipping. Everything else works, ' +
      'including the characters and settings already on this Mac. Saving comes ' +
      'back after a verified compatibility update, without reinstalling the app.'
    );
  }
  if (state === 'failed') {
    return (
      'Build templates cannot be saved: preparing the client for it did not ' +
      'finish. Everything else works. The Diagnostics window says what failed.'
    );
  }
  return null;
}

/**
 * What to tell a player who enabled an optional observer tool.
 *
 * Template certification and read-only layout certification are deliberately
 * independent: a new pair can safely regain template saving before both live
 * runtime fixtures have proved its memory layout.
 *
 * @param {unknown} state `window.__gwnativeEnhancements`
 * @returns {string | null}
 */
export function enhancementNotice(state) {
  if (state === 'uncertified') {
    return (
      'The native cursor and target-distance tools are disabled for this client ' +
      'build because its read-only layout has not passed both runtime checks yet. ' +
      'The game and build templates still work.'
    );
  }
  if (state === 'failed') {
    return (
      'The native cursor and target-distance tools are disabled because preparing ' +
      'their certified observer did not finish. The Diagnostics window says what failed.'
    );
  }
  return null;
}

/**
 * Whether this launch should record a new compatibility state, and its text.
 *
 * The uncertified case qualifies. `failed` is a fault on this Mac rather than
 * news about the client, it is already in the log and in the settings panel,
 * and — because a build that failed to prepare has no hash — there would be
 * nothing to remember it by, so it would be logged at every launch forever.
 *
 * @param {{ state: unknown, enhancements?: unknown, build: unknown,
 *           seenFor: unknown }} where `state`, `enhancements` and `build` are
 *   what the host injected; `seenFor` is the build already recorded in
 *   settings.
 * @returns {{ sentence: string, build: string } | null}
 */
export function announcement({
  state,
  enhancements = 'off',
  build,
  seenFor,
}) {
  const sentence = state === 'uncertified'
    ? templateSaveNotice(state)
    : enhancements === 'uncertified'
      ? enhancementNotice(enhancements)
      : null;
  if (sentence === null) return null;
  // No hash means nothing to key the record to.
  if (typeof build !== 'string' || build === '') return null;
  if (seenFor === build) return null;
  return { sentence, build };
}

/**
 * Record the compatibility change without putting it in the boot path.
 *
 * The durable, player-facing explanation lives in the settings panel beside
 * the affected switches. An ArenaNet patch must never wait behind a modal merely
 * because optional compatibility has not been certified yet.
 *
 * @param {{ log: (...args: unknown[]) => void,
 *           save: (patch: object) => Promise<object>,
 *           state?: unknown, enhancements?: unknown,
 *           build?: unknown, seenFor?: unknown }} deps
 */
export async function announceCompatibility({ log, save, ...where }) {
  const say = announcement({
    state: where.state ?? window.__gwnativeTemplateSave,
    enhancements: where.enhancements ?? window.__gwnativeEnhancements,
    build: where.build ?? window.__gwnativeClientBuild,
    seenFor: where.seenFor ?? null,
  });
  if (!say) return;

  log(
    `compatibility: client build ${say.build.slice(0, 12)} has optional features disabled —`,
    say.sentence,
  );
  try {
    await save({ compatibilityNoticeSeenFor: say.build });
  } catch (error) {
    log(`[warn] compatibility: the artifact record was not saved: ${error}`);
  }
}
