# Command-line reference

gwnative accepts native macOS options and every switch listed by the official
[Guild Wars command-line reference](https://wiki.guildwars.com/wiki/Command_line_arguments).
Recognition does not mean a Windows-only switch can affect the WebAssembly
client: when no supported equivalent exists, gwnative prints a precise notice
before launch.

Use application arguments after `--` when running through Cargo:

```sh
cargo run -- --profile iron -fps 60
cargo run -- -image --jobs 16
```

For an installed bundle:

```sh
"/Applications/Guild Wars.app/Contents/MacOS/gwnative" --profile iron
```

Options accept a separate value or `=value` where applicable. Duplicate
single-value options, malformed values, conflicting commands, and unknown
arguments fail with status 2 instead of being ignored.

## Commands

| Command | Result |
| --- | --- |
| `run` or no command | Open the game window |
| `sync` | Refresh and verify the small client artifacts, then exit |
| `repair` | Hash cached game data, discard damage, download every missing chunk, then exit |
| `serve` | Start the loopback origin without AppKit and print its address and token |
| `profiles` | List profile ID, display name, colour, and stable loopback port |
| `mods` | Validate and list `.gwmod` bundles in the selected mod directory without executing them |

Only one command may be selected.

## Native options

| Option | Behaviour |
| --- | --- |
| `--profile NAME` | Select or create an isolated profile; see [Profiles](profiles.md) |
| `--new-instance` | Permit a concurrent instance; requires a non-default `--profile` |
| `-p`, `--port`, `--host-port PORT` | Override the profile's loopback port |
| `-d`, `--dir PATH` | Override the writable web shell and client-artifact directory |
| `-c`, `--cache`, `--cache-root PATH` | Override the content-addressed game-data cache |
| `-m`, `--mods`, `--mod-dir PATH` | Select the directory inspected by `mods` |
| `-modfile`, `--modfile PATH` | Explicitly load one `.json`, `.gwmod`, or `.wasm` mod session |
| `-i`, `--image PATH` | Validate a raw local snapshot against the current manifest, seed the cache, then launch |
| `-j`, `--jobs COUNT` | Use 1–32 workers for `-image` or `repair` |
| `--offline` | Forbid launch-time patch network access; a usable cached manifest and artifacts are required |
| `--no-update` | Skip client-manifest and application-update checks |
| `--no-prefetch` | Disable boot-list warming and read-ahead |
| `--no-browser` | Alias for `serve` |
| `--debug`, `--devtools` | Permit Web Inspector for the page |
| `-v`, `--verbose` | Trace loopback requests and game-socket frame sizes |
| `-h`, `--help` | Print built-in help |
| `-V`, `--version` | Print the application version |

`GWNATIVE_WEB_ROOT`, `GWNATIVE_PORT`, and the tracing environment variables
remain available for development. An explicit command-line path or port wins
over its environment equivalent.

## Guild Wars compatibility

### Implemented or translated

| Guild Wars switch | gwnative behaviour |
| --- | --- |
| `-autologin` | Offers invocation credentials or the selected profile's Keychain login to the current client |
| `-email VALUE` and `-password VALUE` | Together supply invocation-only credentials; they are not written to disk |
| `-fps VALUE` | Caps delivered animation frames from 1 through 1000 |
| `-image` | Syncs the current client, downloads and verifies the entire 4.2 GB game image, then exits |
| `-repair` | Alias for `repair` |
| `-update` | Syncs current client artifacts; application installation remains separately consented |
| `-windowed` | Opens in a normal macOS window |
| `-windowedfullscreen` | Opens in macOS full screen |
| `-mute`, `-nosound` | Starts game audio muted |
| `-diag` | Opens the diagnostics overlay |
| `-perf` | Opens the diagnostics overlay with performance counters active |
| `-log` | Enables native HTTP and socket-size tracing |
| `-mock SteamDeck` | Exposes the client's mobile-layout capability flag |
| `-nopatchui` | Hides routine boot progress; fatal errors remain visible |
| `-prefresetlocal` | Backs up and resets the selected profile's host preferences |
| `-uninstall` | Prints safe manual removal instructions and exits without deleting data |

The command line is visible to other processes owned by the same macOS user.
Prefer profile Keychain storage over `-password`. The password value is redacted
from Rust debug output and its owned buffer is overwritten before release, but
those measures cannot hide the original process argument.

Supplying only one of `-email` or `-password` produces a compatibility notice
and injects no partial credential object.

### Recognised but unavailable

| Switches | Reason |
| --- | --- |
| `-character` | The current WebAssembly client has no supported character-selection launch hook |
| `-bmp` | The client has no screenshot-format launch hook |
| `-lodfull` | The client has no supported model-detail launch hook |
| `-fqdn` | Authentication routing belongs to the current client and restricted native network bridge |
| `-noshaders` | The WebGL client cannot render without its shaders |
| `-noui` | The client has no supported UI-suppression launch hook |
| `-oldfov` | The client has no supported field-of-view launch hook |
| `-resetmap` | No separately certified map-state reset exists |
| `-stress COUNT` | The Windows stress harness is not part of the WebAssembly client |
| `-dsound`, `-sndasio`, `-sndwinmm` | Web Audio replaces Windows sound backends |
| `-dx8` | WebGL through WebKit/Metal replaces DirectX |
| `-mce` | Windows Media Center integration has no macOS equivalent |
| `-newauth`, `-oldauth` | Authentication selection belongs to ArenaNet's current client |

`-authsrv`, `-exit`, `-map`, `-port`, and `-sndfastbuf` are accepted
and reported as having no known usable behaviour, matching the official
reference. None silently changes an unrelated gwnative setting.

## Combination rules

- `--offline` cannot be combined with `sync`, `-image`, or `-update`.
- `--new-instance` requires `--profile`.
- `--jobs` requires `-image` or `repair`.
- `-windowed` and `-windowedfullscreen` are mutually exclusive.
- `--image PATH` imports a local raw image; `-image` downloads the official
  image. Their spelling and purpose are deliberately distinct.
