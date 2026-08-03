// Finite command gate for the E2E-only in-client benchmark controller.

const COMMANDS = Object.freeze({
  'travel-america': 0,
  'interact-xunlai': 1,
  'high-graphics': 2,
  'travel-guild-hall': 3,
  'leave-guild-hall': 4,
  'travel-international': 5,
});

/** Execute one command only while the client is in a synchronous normal state. */
export async function executeBenchmarkCommand(command, argument, {
  enabled,
  benchmarkCommand,
  queueCommand,
  runtimeIdle,
}) {
  if (!enabled) {
    throw new Error('benchmark commands are available only during E2E certification');
  }
  if (typeof benchmarkCommand !== 'function') {
    throw new Error('the finite in-client benchmark command is unavailable');
  }
  if (typeof queueCommand !== 'function') {
    throw new Error('the game-frame benchmark command queue is unavailable');
  }
  const commandId = Object.hasOwn(COMMANDS, command) ? COMMANDS[command] : -1;
  if (
    !Number.isSafeInteger(argument)
    || (commandId === 0 && ![1, 2].includes(argument))
    || (commandId === 1 && (argument <= 0 || argument >= 4096))
    || ([2, 3, 4, 5].includes(commandId) && argument !== 0)
    || commandId < 0
  ) {
    throw new Error('benchmark command arguments are outside the finite API');
  }
  if (!runtimeIdle()) {
    throw new Error('the client is unwinding or rewinding before a benchmark command');
  }
  const result = await queueCommand(() => benchmarkCommand(commandId, argument));
  if (result !== 1) {
    throw new Error('the finite benchmark command was rejected by the client');
  }
  if (!runtimeIdle()) {
    throw new Error('the client did not return to normal after a benchmark command');
  }
}
