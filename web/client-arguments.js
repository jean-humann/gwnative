// Official Guild Wars arguments handed to Emscripten's main().
//
// Keep this translation separate from the generated glue: Emscripten reads
// `Module.arguments` once before calling the unchanged client entry point.
// Only credentials deliberately supplied on the native command line are
// represented here. A profile Keychain login stays behind secureStorage and
// never enters argv. The JavaScript vector is scrubbed as soon as Emscripten
// has synchronously copied it into the unchanged client.

const nonempty = (value) => typeof value === 'string' && value.length > 0;
const completeCredentials = (value) =>
  nonempty(value?.username) && nonempty(value?.password);

export function guildWarsClientArguments(options) {
  const result = [];
  const credentials = completeCredentials(options?.credentials);
  // `-autologin` asks the client to start its saved-login operation. Complete
  // explicit arguments start a different portal operation; combining both
  // violates build 38795's single-active-operation invariant.
  if (options?.autologin === true && !credentials) result.push('-autologin');
  if (credentials) {
    result.push(
      '-email',
      options.credentials.username,
      '-password',
      options.credentials.password,
    );
  }
  if (nonempty(options?.character)) result.push('-character', options.character);
  return result;
}

export function scrubGuildWarsClientArguments(arguments_) {
  if (!Array.isArray(arguments_)) return;
  arguments_.fill('');
  arguments_.length = 0;
}

export function scrubGuildWarsLaunchCredentials(options) {
  if (!options || typeof options !== 'object') return;
  const credentials = options.credentials;
  if (credentials && typeof credentials === 'object') {
    credentials.username = '';
    credentials.password = '';
  }
  options.credentials = null;
}
