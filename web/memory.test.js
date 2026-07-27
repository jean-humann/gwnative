// Tests for the client heap sensor.
//
// Run by `cargo test` through `tests/web.rs`, or directly with
// `node --test web/*.test.js`.

import assert from 'node:assert/strict';
import { before, describe, it } from 'node:test';

describe('memory sensor', () => {
  let memory;

  before(async () => {
    const interval = globalThis.setInterval;
    globalThis.setInterval = (...args) => interval(...args).unref();
    globalThis.addEventListener ??= () => {};
    globalThis.window = globalThis;

    memory = await import('./memory.js');
  });

  /** A client whose heap grows when asked, up to a ceiling. */
  const client = (ceiling = Infinity) => {
    const state = { bytes: 16 * 1048576, logged: [] };
    const env = {
      emscripten_resize_heap: (requested) => {
        if (requested > ceiling) return 0;
        state.bytes = requested;
        return 1;
      },
    };
    const readHeap = memory.installMemorySensor({
      env,
      heapBytes: () => state.bytes,
      log: (...values) => state.logged.push(values.join(' ')),
    });
    return { env, state, readHeap };
  };

  it('lets the client grow its heap', () => {
    const { env, state } = client();
    assert.equal(env.emscripten_resize_heap(64 * 1048576), 1);
    assert.equal(state.bytes, 64 * 1048576, 'the wrapper must not change the outcome');
  });

  it('reports a refusal, which is otherwise invisible', () => {
    const { env, state } = client(32 * 1048576);
    assert.equal(env.emscripten_resize_heap(64 * 1048576), 0);
    assert.equal(state.bytes, 16 * 1048576);
    assert.match(state.logged.join('\n'), /could not grow to 64 MiB/);
  });

  it('says so when there is nothing to instrument', () => {
    const logged = [];
    const result = memory.installMemorySensor({
      env: {},
      heapBytes: () => 0,
      log: (...values) => logged.push(values.join(' ')),
    });
    assert.equal(result, undefined);
    assert.match(logged.join('\n'), /no emscripten_resize_heap/);
  });

  it('hands back a way to re-read the heap after the fact', () => {
    const { readHeap } = client();
    assert.equal(typeof readHeap, 'function');
    assert.doesNotThrow(() => readHeap());
  });
});
