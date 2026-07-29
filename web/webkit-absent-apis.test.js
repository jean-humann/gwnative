// Guards the one mistake this port can make over and over.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// A browser API that macOS WebKit does not implement costs nothing at load: the
// file parses, the module evaluates, and the listener installs. It fails the
// first time a player exercises the path — as a ReferenceError inside a
// listener, which aborts that listener and disturbs nothing else, so the feature
// is simply inert and the app looks fine. `TouchEvent` cost a release that way,
// and the only reason it was found is that someone read a log.
//
// There is no type checker over this directory, and none of these paths can be
// called without a DOM. Reading the source is what is left.

import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

// The constructible half of docs/performance.md's WebKit gaps. The
// rest of that list is absent *properties*, which are a different failure and
// already answered elsewhere: the keyboard layout arrives from the host, and
// the two missing WebGL extensions are ones the client asks about rather than
// assumes. A name here is not banned from the source — the touch path is
// discussed at length in a comment — only from being constructed.
const ABSENT = ['Touch', 'TouchEvent'];

/** Page modules, minus the vendor glue and these tests. */
const pageModules = readdirSync(here)
  .filter((name) => name.endsWith('.js') && !name.endsWith('.test.js'))
  // ArenaNet's build. It names `registerTouchEventCallback` and constructs
  // nothing; it is also not ours to change, so a finding in it would be noise.
  .filter((name) => name !== 'Gw.jspi.js');

describe('interfaces macOS WebKit does not have', () => {
  it('has page modules to check', () => {
    // A rename or a move that emptied this list would otherwise pass silently.
    assert.ok(pageModules.length > 10, `only found ${pageModules.length} modules`);
    assert.ok(pageModules.includes('input.js'));
  });

  for (const name of pageModules) {
    it(`${name} constructs none of them`, () => {
      const source = readFileSync(join(here, name), 'utf8');
      for (const api of ABSENT) {
        const constructed = new RegExp(String.raw`\bnew\s+(?:globalThis\.|window\.)?${api}\s*\(`);
        assert.equal(
          constructed.test(source),
          false,
          `${name} constructs ${api}, which does not exist on macOS WebKit`,
        );
      }
    });
  }
});
