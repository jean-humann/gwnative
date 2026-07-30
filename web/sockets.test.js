import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { createNetworkRegistry } from './sockets.js';

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
