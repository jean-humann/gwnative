import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { createNetworkRegistry, createSockets } from './sockets.js';

describe('socket callback audit', () => {
  it('bounds every game callback with its external source', () => {
    const previousLocation = globalThis.location;
    const previousWebSocket = globalThis.WebSocket;
    const previousWindow = globalThis.window;
    const calls = [];
    let transport;
    class FakeWebSocket {
      static OPEN = 1;

      constructor(url) {
        this.url = url;
        this.readyState = 0;
        transport = this;
      }

      send() {}
      close() {}
    }
    globalThis.location = new URL('http://127.0.0.1:38112/');
    globalThis.WebSocket = FakeWebSocket;
    globalThis.window = globalThis;
    try {
      const sockets = createSockets({
        log() {},
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
      socket.onopen = () => calls.push(['game', 'open']);
      socket.onmessage = () => calls.push(['game', 'message']);
      socket.onclose = () => calls.push(['game', 'close']);

      transport.onmessage({ data: JSON.stringify({ type: 'open' }) });
      transport.onmessage({ data: new Uint8Array([1, 2, 3]).buffer });
      transport.onclose({ code: 1000 });

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
    }
  });
});

describe('client socket roles', () => {
  it('identifies only resolved Auth service destinations as authentication', () => {
    const registry = createNetworkRegistry();
    registry.resolved('File1.ArenaNetworks.com', '192.0.2.10');
    registry.resolved('Auth1.ArenaNetworks.com.', '192.0.2.20');

    assert.equal(registry.role('192.0.2.10:6112'), 'other');
    assert.equal(registry.role('192.0.2.20:6112'), 'authentication');
    assert.equal(registry.role('192.0.2.30:6112'), 'other');
  });
});
