// Module.socket and Module.dns — the client's network.
//
// Each game socket is one WebSocket to the loopback origin, which the host
// bridges to a real TCP connection. Binary frames map one-to-one onto TCP
// payload in both directions, so packets cross the boundary as bytes with no
// base64 or JSON in the way. The only text frames are the host's `open` and
// `error` control messages, which report the outcome of a dial that necessarily
// happens after the WebSocket handshake has already succeeded.

/** Resolve a hostname to an IPv4 dotted quad, which is the shape the client
 *  expects back — it hands the result straight to connect(). */
export function createDns({ log }) {
  return {
    async resolve(name) {
      const response = await fetch(`__dns?name=${encodeURIComponent(name)}`, {
        headers: { 'X-Gwnative-Token': window.__gwnativeToken ?? '' },
      });
      const body = (await response.text()).trim();
      if (!response.ok) throw new Error(`dns ${name}: ${body}`);
      log('dns', name, '->', body);
      return body;
    },
  };
}

/**
 * @param {{ log(...values: unknown[]): void }} options
 */
export function createSockets({ log }) {
  let nextId = 1;

  function connect(destination) {
    const id = nextId++;
    const trace = (...values) => log(`[socket ${id}] ${destination}`, ...values);
    trace('connect');

    const url = new URL('__socket', location.href);
    url.protocol = 'ws:';
    url.searchParams.set('to', destination);
    // A WebSocket handshake carries no headers this side can set, so this one
    // route takes its token in the query string. The host accepts it there for
    // the same reason.
    url.searchParams.set('token', window.__gwnativeToken ?? '');

    const ws = new WebSocket(url);
    ws.binaryType = 'arraybuffer';

    // The client may send before the TCP connection is up: it treats connect()
    // as returning an already-usable socket. Queue until the host says open.
    const queued = [];
    let open = false;
    let closed = false;

    const socket = {
      onopen: null,
      onclose: null,
      onmessage: null,
      onerror: null,

      send(data) {
        if (closed) return;
        // The client hands over a view straight onto the WASM heap, which it
        // reuses the moment this returns — and growing the heap would detach it
        // outright. Compact it before it can be queued or sent.
        const bytes = data.slice();
        window.gwE2E?.traffic('send', id, bytes.byteLength);
        if (open) ws.send(bytes);
        else queued.push(bytes);
      },

      // The client removes the socket from its own table *before* calling
      // close(), and its onclose handler reads that same table to decide
      // whether the connection ever came up. Reporting a close it asked for
      // would therefore be logged as a failed connect.
      close() {
        closed = true;
        queued.length = 0;
        if (ws.readyState <= WebSocket.OPEN) ws.close();
      },
    };

    ws.onmessage = (event) => {
      if (typeof event.data === 'string') {
        let message;
        try {
          message = JSON.parse(event.data);
        } catch {
          return log('[warn] socket: unparseable control frame');
        }
        if (message.type === 'open') {
          open = true;
          trace('open');
          void window.gwE2E?.report('socket-open', { socketId: id }).catch(() => {});
          for (const bytes of queued.splice(0)) ws.send(bytes);
          socket.onopen?.();
        } else {
          trace('host refused:', message.message);
          socket.onerror?.(new Error(message.message));
        }
        return;
      }
      window.gwE2E?.traffic('receive', id, event.data.byteLength);
      socket.onmessage?.(new Uint8Array(event.data));
    };

    ws.onclose = (event) => {
      // Only an unrequested close is worth a line, and only with the code: a
      // 1006 here means the transport died without a close frame, which is the
      // signature of a rejected upgrade rather than a peer hanging up.
      if (!closed) trace(`closed by transport (code=${event.code})`);
      if (closed) return;
      closed = true;
      socket.onclose?.();
    };

    // A transport-level error is always followed by close, which is where the
    // client is told; reporting it twice would close the socket twice.
    ws.onerror = () => log('[warn] socket transport error for', destination);

    return socket;
  }

  return { connect };
}
