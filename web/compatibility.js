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
// Two places say it, for two different reasons. The settings panel says it
// whenever it is open, because it is a state and a player who half-watched a
// boot needs somewhere to go and check. This module says it once at the launch
// where it becomes true, because a client build the app has not caught up with
// is also an event — it happened between this launch and the last one, and it is
// the only way a player learns that a Save button that worked yesterday will not
// work today. Once acknowledged for that build it stays quiet, and the next
// build ArenaNet ships asks again.

/**
 * What to tell the player about build templates, or null when there is nothing
 * to tell.
 *
 * @param {unknown} state `window.__gwnativeTemplateSave` — 'ready',
 *   'uncertified', 'asyncify' or 'failed'
 * @returns {string | null}
 */
export function templateSaveNotice(state) {
  if (state === 'uncertified') {
    return (
      'Build templates cannot be saved: this release has not been checked against ' +
      'the client build ArenaNet is currently shipping. Everything else works, ' +
      'including the characters and settings already on this Mac. Saving comes ' +
      'back in a later release of this app.'
    );
  }
  if (state === 'asyncify') {
    return (
      'This Mac uses ArenaNet\'s Asyncify compatibility client, so the game is ' +
      'playable but build-template saving and optional enhancements are ' +
      'unavailable. The system WKWebView does not implement JSPI; installing ' +
      'Safari Technology Preview does not change WKWebView.'
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
 * Whether this launch should interrupt to say it, and what it would say.
 *
 * The uncertified and Asyncify cases qualify. `failed` is a fault on this Mac
 * rather than news about the client, it is already in the log and in the
 * settings panel, and — because a build that failed to prepare has no hash —
 * there would be nothing to remember an acknowledgement by, so it would ask at
 * every launch forever. A notice that cannot be silenced is one that stops
 * being read.
 *
 * @param {{ state: unknown, build: unknown, seenFor: unknown }} where
 *   `state` and `build` are what the host injected; `seenFor` is the build the
 *   player has already acknowledged, from settings.
 * @returns {{ sentence: string, build: string } | null}
 */
export function announcement({ state, build, seenFor }) {
  if (state !== 'uncertified' && state !== 'asyncify') return null;
  const sentence = templateSaveNotice(state);
  if (sentence === null) return null;
  // No hash means nothing to key the acknowledgement to. Saying it anyway would
  // be a sentence the player can never turn off.
  if (typeof build !== 'string' || build === '') return null;
  if (seenFor === build) return null;
  return { sentence, build };
}

/**
 * Say it, if there is anything to say, and resolve once the player has read it.
 *
 * Uses the launcher's overlay, which by this point in the boot has finished with
 * it: this is the same surface, at the same moment, for the same reason — the
 * last place there is to say anything before the client takes the canvas and the
 * keyboard. It has its own action row rather than the launcher's because the two
 * never run at once and sharing the builder would be a dependency between a
 * question about the network and a statement about the client.
 *
 * Nothing here can stop a boot. A settings write that fails costs one repeated
 * notice at the next launch, which is a great deal better than not starting.
 *
 * @param {{ log: (...args: unknown[]) => void,
 *           save: (patch: object) => Promise<object>,
 *           state?: unknown, build?: unknown, seenFor?: unknown }} deps
 */
export async function announceCompatibility({ log, save, ...where }) {
  const say = announcement({
    state: where.state ?? window.__gwnativeTemplateSave,
    build: where.build ?? window.__gwnativeClientBuild,
    seenFor: where.seenFor ?? null,
  });
  if (!say) return;

  const el = (id) => document.getElementById(id);
  const overlay = el('launcher');
  const actions = el('launcher-actions');
  if (!overlay || !actions) {
    log(`[warn] compatibility: nowhere to say it — ${say.sentence}`);
    return;
  }

  log(`compatibility: client build ${say.build.slice(0, 12)} is not one this release patches`);
  el('launcher-title').textContent = 'One thing is missing';
  el('launcher-text').textContent = say.sentence;
  el('launcher-detail').textContent = '';
  el('launcher-rail').hidden = true;
  overlay.hidden = false;

  const dismissed = await new Promise((resolve) => {
    actions.replaceChildren();
    for (const [index, [label, value, primary]] of [
      ['Continue', false, true],
      ["Don't tell me again for this build", true, false],
    ].entries()) {
      const element = document.createElement('button');
      element.textContent = label;
      if (primary) element.classList.add('primary');
      element.addEventListener('click', () => resolve(value));
      actions.append(element);
      if (index === 0) element.focus();
    }
  });

  overlay.hidden = true;
  if (!dismissed) return;
  try {
    await save({ compatibilityNoticeSeenFor: say.build });
  } catch (error) {
    log(`[warn] compatibility: the acknowledgement was not saved: ${error}`);
  }
}
