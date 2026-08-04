import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { createSockets } from './sockets.js';

describe('socket callback audit', () => {
  it('bounds every game callback with its external source', () => {
    const previousLocation = globalThis.location;
    const previousWebSocket = globalThis.WebSocket;
    const previousWindow = globalThis.window;
    const calls = [];
    let transport;
    class FakeWebSocket {
      static OPEN = 1;

      constructor(url, protocols) {
        this.url = url;
        this.protocols = protocols;
        this.readyState = 0;
        transport = this;
      }

      send() {}
      close() {}
    }
    globalThis.location = new URL('http://127.0.0.1:38112/');
    globalThis.WebSocket = FakeWebSocket;
    globalThis.window = globalThis;
    globalThis.__gwnativeToken = 'socket-token-canary';
    try {
      const sockets = createSockets({
        log() {},
        launchIdentity: () => ({ nonce: 'exact-launch' }),
        audit: {
          beginExternalCallback(kind) {
            calls.push(['begin', kind]);
            return kind;
          },
          endExternalCallback(kind) {
            calls.push(['end', kind]);
          },
        },
      });
      const socket = sockets.connect('1.2.3.4:6112');
      const gameTransport = transport;
      assert.equal(new URL(gameTransport.url).searchParams.has('launch'), false);
      assert.equal(new URL(gameTransport.url).searchParams.has('token'), false);
      assert.deepEqual(gameTransport.protocols, [
        'gwnative',
        'gwnative-token.socket-token-canary',
        'gwnative-launch.eyJub25jZSI6ImV4YWN0LWxhdW5jaCJ9',
      ]);
      sockets.connect('www.guildwars.com:443');
      const webTransport = transport;
      assert.deepEqual(webTransport.protocols, [
        'gwnative',
        'gwnative-token.socket-token-canary',
      ]);
      socket.onopen = () => calls.push(['game', 'open']);
      socket.onmessage = () => calls.push(['game', 'message']);
      socket.onclose = () => calls.push(['game', 'close']);

      gameTransport.onmessage({ data: JSON.stringify({ type: 'open' }) });
      gameTransport.onmessage({ data: new Uint8Array([1, 2, 3]).buffer });
      gameTransport.onclose({ code: 1000 });

      assert.deepEqual(calls, [
        ['begin', 'socket-open'],
        ['game', 'open'],
        ['end', 'socket-open'],
        ['begin', 'socket-message'],
        ['game', 'message'],
        ['end', 'socket-message'],
        ['begin', 'socket-close'],
        ['game', 'close'],
        ['end', 'socket-close'],
      ]);
    } finally {
      globalThis.location = previousLocation;
      globalThis.WebSocket = previousWebSocket;
      globalThis.window = previousWindow;
      delete globalThis.__gwnativeToken;
    }
  });
});
