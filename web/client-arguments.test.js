import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  guildWarsClientArguments,
  scrubGuildWarsClientArguments,
  scrubGuildWarsLaunchCredentials,
} from './client-arguments.js';

describe('Guild Wars client arguments', () => {
  it('hands official startup switches to the unchanged client entry point', () => {
    assert.deepEqual(
      guildWarsClientArguments({
        autologin: true,
        credentials: { username: 'player@example.test', password: 'secret' },
        character: 'Devona',
      }),
      [
        '-email',
        'player@example.test',
        '-password',
        'secret',
        '-character',
        'Devona',
      ],
    );
  });

  it('never combines saved-login and complete-credential portal operations', () => {
    const arguments_ = guildWarsClientArguments({
      autologin: true,
      credentials: { username: 'player@example.test', password: 'secret' },
    });
    assert.equal(arguments_.includes('-autologin'), false);
    assert.equal(arguments_.includes('-password'), true);
  });

  it('requires a complete login before adding credential switches', () => {
    assert.deepEqual(guildWarsClientArguments({ autologin: true }), ['-autologin']);
    assert.deepEqual(
      guildWarsClientArguments({
        credentials: { username: 'only-one-field' },
        character: '',
      }),
      [],
    );
  });

  it('overwrites the JavaScript argument copy after startup parsing', () => {
    const arguments_ = ['-password', 'secret'];
    scrubGuildWarsClientArguments(arguments_);
    assert.deepEqual(arguments_, []);
  });

  it('drops the injected launch credential copy after constructing argv', () => {
    const options = {
      credentials: { username: 'player@example.test', password: 'secret' },
    };
    scrubGuildWarsLaunchCredentials(options);
    assert.deepEqual(options, { credentials: null });
  });
});
