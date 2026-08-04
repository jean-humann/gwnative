// The page and the modules that wire themselves into it.
//
// Every overlay in this app is markup in index.html filled in by a module, and
// the two agree only by convention: a module that cannot find its elements logs
// a line and returns a no-op opener, so a renamed or missing id is a menu item
// that silently does nothing. Nothing else here would catch that — the other
// tests deliberately test decisions rather than markup, and there is no DOM in
// the runner to render against.
//
// So this reads both sides as text. Crude, and it has found the only class of
// bug it is aimed at: an id that exists on one side of the pair and not the
// other.

import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { describe, it } from 'node:test';
import { TextDecoder, TextEncoder } from 'node:util';
import { runInNewContext } from 'node:vm';

const web = dirname(fileURLToPath(import.meta.url));
const read = (name) => readFileSync(join(web, name), 'utf8');

const modules = readdirSync(web).filter((f) => f.endsWith('.js') && !f.endsWith('.test.js'));

describe('the page', () => {
  const html = read('index.html');
  const ids = new Set([...html.matchAll(/id="([a-z0-9-]+)"/g)].map((m) => m[1]));

  it('has an element for every id a module looks up', () => {
    for (const name of modules) {
      const source = read(name);
      // `el(...)` is the one-character helper launcher.js and compatibility.js
      // both define for the same call.
      const lookups = [
        ...source.matchAll(/getElementById\(\s*'([a-z0-9-]+)'\s*\)/g),
        ...source.matchAll(/\bel\(\s*'([a-z0-9-]+)'\s*\)/g),
      ];
      for (const [, id] of lookups) {
        assert.ok(ids.has(id), `${name} looks up #${id}, which is not in index.html`);
      }
    }
  });

  // The other direction is not an error — the client creates elements of its own
  // and the OSK inputs are looked up by the game rather than by us — so this
  // checks only the overlays, whose contents exist for a module to fill in.
  it('has a module filling in every overlay it declares', () => {
    const sources = modules.map(read).join('\n');
    for (const id of ['launcher', 'settings', 'guide', 'failure']) {
      assert.ok(ids.has(id), `#${id} is not in index.html`);
      assert.ok(
        sources.includes(`'${id}'`),
        `#${id} is in the page and no module ever opens it`,
      );
    }
  });

  // Each overlay hides itself with the `hidden` attribute, which does nothing
  // against a `display: flex` rule unless the stylesheet says so. Getting this
  // wrong leaves a permanent black sheet over the game.
  it('gives every overlay a rule that makes hidden mean hidden', () => {
    for (const id of ['launcher', 'settings', 'guide', 'failure']) {
      assert.match(
        html,
        new RegExp(`#${id}\\[hidden\\]\\s*{\\s*display:\\s*none`),
        `#${id} can be hidden in the markup and still drawn`,
      );
    }
  });

  it('labels the launcher as an unofficial project without waiting for JavaScript', () => {
    assert.match(html, /id="launcher-legal"/);
    assert.match(html, /<title>Guild Wars — Unofficial macOS host<\/title>/);
    assert.match(html, /Independent, unofficial project/);
    assert.match(html, /ArenaNet or NCSOFT/);
    assert.match(html, /Guild Wars Reforged/);
  });

  it('scrubs active credentials before console and host diagnostics', () => {
    const harness = read('harness.js');
    assert.ok(
      harness.indexOf('const protectedDiagnostics') < harness.indexOf('const CONSOLE_METHODS'),
      'tokens must be protected before console forwarding is installed',
    );
    assert.match(harness, /pending\.push\(scrubDiagnostic\(line\)\)/);
    assert.match(harness, /original\(\.\.\.scrubbed\)/);
    assert.match(harness, /logBuf\.push\(scrubbed\.join\(' '\)\)/);
    assert.match(harness, /containsDiagnosticSpelling\(variant, protectedValue, budget\)/);
    assert.match(harness, /decodeJsonDiagnosticLayer\(variant\)/);
    assert.match(harness, /decodePercentDiagnosticLayer\(variant, true\)/);
    assert.match(harness, /window\.__gwnativeLaunchNonce/);
    assert.match(harness, /'dirxml'.*'table'.*'trace'/s);
    assert.match(harness, /response\.json\(\)\.then\(protectCredentials\)/);
    assert.match(
      harness,
      /protectCredentials\(\{ username, password \}\);\s*const response = await credentials\('PUT'/,
    );
    assert.match(harness, /const MAX_CREDENTIAL_DIAGNOSTICS = 18/);
    assert.match(harness, /if \(suppressPageDiagnostics\) return ''/);
    assert.doesNotMatch(harness, /credential-realm/);
    assert.match(harness, /if \(launch\.transformed === true\)/);
    assert.match(harness, /host\.deliverRuntimeProof\('__runtime', launch\)/);
    assert.match(harness, /host\.deliverRuntimeProof\('__transform-failed'/);
  });

  it('cancels raw WebKit exception output after queuing only scrubbed text', () => {
    const harness = read('harness.js');
    const prefix = harness.slice(
      harness.indexOf('const LOG_LINES'),
      harness.indexOf('// Keep the main loop'),
    );
    const listeners = new Map();
    const printed = [];
    const consoleMethods = [
      'log', 'info', 'debug', 'warn', 'error', 'dir', 'dirxml', 'table', 'trace',
      'group', 'groupCollapsed', 'groupEnd', 'clear', 'count', 'countReset',
      'assert', 'time', 'timeLog', 'timeEnd', 'timeStamp', 'profile', 'profileEnd',
    ];
    const context = {
      window: {
        __gwnativeToken: 'browser-token',
        __gwnativeGamePublisherToken: 'publisher-token',
        __gwnativeLaunchNonce: 'launch-nonce-canary',
        addEventListener: (name, callback) => listeners.set(name, callback),
      },
      console: Object.fromEntries(
        consoleMethods.map((level) => [level, (...values) => printed.push([level, ...values])]),
      ),
      TextDecoder,
      TextEncoder,
      setTimeout: () => 1,
      fetch: () => Promise.resolve(),
    };
    runInNewContext(
      `${prefix}\nthis.audit = { protectCredentials, scrubDiagnostic, pending };`,
      context,
    );
    const secret = 'page exception-canary';
    context.audit.protectCredentials({ username: 'player', password: secret });
    for (const spelling of [
      'page+exception-canary',
      '%70age%20exception%2Dcanary',
      'page \\u0065xception-canary',
      'page \\\\u0065xception-canary',
      '%6c%61%75%6e%63%68%2d%6e%6f%6e%63%65%2d%63%61%6e%61%72%79',
    ]) {
      assert.equal(context.audit.scrubDiagnostic(spelling), '', spelling);
    }
    assert.equal(context.audit.scrubDiagnostic('x'.repeat(1024 * 1024 + 1)), '');
    for (const [name, event] of [
      ['error', { message: secret, filename: 'client.js', lineno: 1 }],
      ['unhandledrejection', { reason: new Error(secret) }],
    ]) {
      let prevented = false;
      listeners.get(name)({ ...event, preventDefault: () => { prevented = true; } });
      assert.equal(prevented, true, `${name} default output was not cancelled`);
    }
    assert.deepEqual([...context.audit.pending].slice(-2), ['', '']);
    for (const level of consoleMethods.filter((name) => name !== 'assert')) {
      context.console[level](secret, { password: secret });
    }
    context.console.assert(false, secret);
    context.console.assert(true, secret);
    assert.ok(printed.flat(2).every((value) => !String(value).includes(secret)));
    assert.ok([...context.audit.pending].every((value) => !String(value).includes(secret)));
  });
});
