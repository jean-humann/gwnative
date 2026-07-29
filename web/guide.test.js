// Tests for the user guide.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.
//
// What is tested is the text, which is the whole of it — `installGuide` builds
// headings and paragraphs and nothing else. The assertions below are about the
// two ways a guide goes wrong: it stops describing the app, or it never
// mentioned the thing everybody actually gets stuck on.

import assert from 'node:assert/strict';
import { before, describe, it } from 'node:test';

describe('user guide', () => {
  let guide;

  before(async () => {
    // `diagnostics.js` starts reporting on import and would hold the runner
    // open past the last assertion. Same unref the other page tests use.
    const interval = globalThis.setInterval;
    globalThis.setInterval = (...args) => interval(...args).unref();
    globalThis.window = globalThis;
    globalThis.addEventListener ??= () => {};
    guide = await import('./guide.js');
  });

  const text = () => guide.GUIDE.flatMap((section) => section.body).join(' ');

  it('has a heading and something under it in every section', () => {
    assert.ok(guide.GUIDE.length > 0);
    for (const section of guide.GUIDE) {
      assert.ok(section.heading.length > 0, 'a section with no heading');
      assert.ok(section.body.length > 0, `${section.heading} says nothing`);
      for (const paragraph of section.body) assert.ok(paragraph.trim().length > 0);
    }
  });

  // The reason this guide exists. Three of the four touch modes stop
  // double-clicking from working and one of those withholds every mouse click,
  // and a player who lands on one of them has an inventory that does nothing
  // and no way to find out why. If nothing else in here survives a rewrite,
  // this has to.
  it('explains why double-clicking might not work', () => {
    assert.match(text(), /double-click/i);
    assert.match(text(), /Touch only/);
  });

  // The other thing that reads as a broken game and is not: pointer lock is
  // granted after a click into the window, so a first attempt at turning the
  // camera can do nothing.
  it('explains why the camera might not turn', () => {
    assert.match(text(), /pointer lock/i);
  });

  // Every claim below is about something that exists. A guide that describes a
  // setting the panel does not have is worse than one that is short.
  it('describes settings this build actually has', () => {
    const body = text();
    for (const subject of ['Render scale', 'Game image', 'Diagnostics', 'Clear Game Data']) {
      assert.match(body, new RegExp(subject, 'i'), `the guide never mentions ${subject}`);
    }
  });

  // The one thing a player could lose sleep over, said plainly and more than
  // once: nothing on this Mac holds a character, so anything here is safe to
  // delete.
  it('says that nothing on this Mac holds an account', () => {
    assert.match(text(), /ArenaNet’s servers|held by\s+ArenaNet/);
  });

  it('explains which accounts use Steam and where its credential is kept', () => {
    const body = text();
    assert.match(body, /Steam-purchased accounts/i);
    assert.match(body, /Mac Keychain/i);
    assert.match(body, /signing out removes/i);
  });

  it('identifies the project as unofficial and carries the required legal notice', () => {
    const body = text();
    assert.match(body, /independent, unofficial interoperability project/i);
    assert.match(body, /Guild Wars Reforged/);
    assert.match(body, /© ArenaNet LLC\. All rights reserved\./);
    assert.match(body, /trademarks or registered trademarks of NCSOFT Corporation/);
  });

  // ⌘⇧M is the one control in this app with no button, no menu item a player
  // will stumble over mid-stutter, and a hard timing requirement: it raises the
  // sampling rate for the ten seconds *after* it, so pressing it once the game
  // has recovered records nothing. If the guide does not say that, nobody will
  // ever press it at the right moment.
  it('says how to report a problem, and when pressing the key is too late', () => {
    const body = text();
    assert.match(body, /Report a Problem/);
    assert.match(body, /⌘⇧M/);
    assert.match(body, /while it is happening/i);
    assert.match(body, /afterwards does not work/i);
  });
});
