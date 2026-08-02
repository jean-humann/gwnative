import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { executeBenchmarkCommand } from './benchmark-command.js';

describe('finite benchmark command', () => {
  it('maps only the fixed vocabulary through the game-frame queue', async () => {
    const calls = [];
    await executeBenchmarkCommand('travel-america', 2, {
      enabled: true,
      benchmarkCommand: (...args) => {
        calls.push(args);
        return 1;
      },
      queueCommand: async (callback) => callback(),
      runtimeIdle: () => true,
    });
    assert.deepEqual(calls, [[0, 2]]);
    await assert.rejects(
      executeBenchmarkCommand('travel-america', 3, {
        enabled: true,
        benchmarkCommand: () => 1,
        queueCommand: async (callback) => callback(),
        runtimeIdle: () => true,
      }),
      /outside the finite API/,
    );
  });

  it('rejects Asyncify state transitions across a command', async () => {
    let idle = true;
    await assert.rejects(
      executeBenchmarkCommand('high-graphics', 0, {
        enabled: true,
        benchmarkCommand: () => {
          idle = false;
          return 1;
        },
        queueCommand: async (callback) => callback(),
        runtimeIdle: () => idle,
      }),
      /did not return to normal/,
    );
  });
});
