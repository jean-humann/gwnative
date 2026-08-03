# User guide

This guide covers installation, settings, local data, updates, and recovery.
The application includes a shorter offline version under **Help → Guild Wars
Guide**.

gwnative is an independent, unofficial interoperability project for Guild Wars
Reforged. It is not affiliated with or endorsed by ArenaNet or NCSOFT, and it
does not bundle the official client or game data. See the [legal
notice](../NOTICE.md).

## Install and start

gwnative requires Apple Silicon and macOS 15.2 or newer.

1. Download the disk image from the
   [latest release](https://github.com/jean-humann/gwnative/releases/latest).
2. Open it and drag **Guild Wars** to **Applications**.
3. Open **Guild Wars**.
4. Choose **Quick Start** or **Full Game** when asked.
5. Sign in with the ArenaNet account used by the game.

The application fetches the official JavaScript and WebAssembly client from
ArenaNet. Characters, storage, account settings, and other server-side game
state remain on ArenaNet's servers.

### Quick Start and Full Game

Guild Wars reads from a 4.2 GB game image.

**Quick Start** fetches chunks as the client asks for them, prefetches near the
current read position, and keeps every verified chunk. It starts fastest and
gradually fills the local image as areas are visited.

**Full Game** downloads the missing chunks in the background. Before a launch
that believes the image is complete, gwnative reads and hashes the cached
chunks. A damaged chunk is discarded and fetched again. The full download is
refused when it would leave less than 2 GiB of usable disk space.

The choice can be changed at any time under **Settings → Game data**. Pausing a
full download keeps the Full Game choice and resumes later; **Switch to Quick
Start** changes the saved mode.

## Settings

Open Settings with **⌘,**. The host owns the default profile's
`~/Library/Application Support/gwnative/settings.json` and each named profile's
`profiles/<id>/settings.json`; the page never writes those files directly.
Application-update intent and cadence are shared by every profile in the base
`updates.json` file.

| Setting | Default | Takes effect | Notes |
| --- | --- | --- | --- |
| Render scale | `2×` | After relaunch | `2×` is Retina-native; lower values trade sharpness for performance |
| Double-click | On | After relaunch | Recommended; the client recognises double taps rather than macOS double-click events |
| Diagnostics overlay | Hidden | Immediately | Shows the live host and page log |
| Game data mode | Ask on first launch | Immediately | Quick Start, Full Game, or ask again next launch |
| Update check | Only when requested | Immediately | Optional daily check at launch |
| Install updates | Ask first | Immediately | Available only when the packaged Sparkle updater is present |
| Game cursor | Follow the mouse | After relaunch | Draws the game's cursor art as the native pointer |
| Target distance | Hidden | After relaunch | Shows the target distance and range band |

Each profile settings file includes a format version and the
runtime-compatibility hash for a recorded state. The global update file includes
its own format version and the last successful update-check time. An unreadable
file is preserved with a `.corrupt-<timestamp>` suffix before defaults are
restored; the three most recent backups for that file are kept.

## Input and window behaviour

- Click once inside the game before using right-drag camera control. WebKit
  grants pointer lock only after a user gesture.
- Leave **Double-click** on for inventory actions such as equipping or picking
  up an item. **Off** removes the tap translation; **Touch only** withholds
  normal mouse clicks.
- Keyboard layout changes are detected while the application is running.
  Option-modified movement keys are mapped back to their unmodified physical
  key so a key release cannot become stranded.
- Moving focus away releases held game input and mutes audio. Returning to the
  application unmutes it.
- Window position, size, maximized state, and full-screen state are restored.
  **View → Reset Window Size and Position** is the recovery path for a window
  left in an unusable position after displays change.
- Closing the last window quits. A normal quit waits briefly for the client's
  persistent filesystem to flush; a force quit cannot.

## Enhancements and build templates

The optional tools are read-only:

- **Game cursor** uses the game-provided cursor bitmap as the macOS pointer, so
  it follows the mouse rather than the rendered frame.
- **Target distance** displays the distance already held by the client and its
  range band.

Both require a relaunch because their signed layout certificate is selected
before the page starts. They require gwnative to recognise the exact official
JavaScript/WebAssembly pair for the runtime this Mac selected.

Build-template save, rename, list, and delete support uses the same exact-pair
rule, with independent JSPI and Asyncify transform outputs. When ArenaNet
publishes an unrecognised pair, gwnative runs the original module safely and
disables only the transform and optional tools. Settings explains the current
compatibility state; startup never waits for that notice. Loading existing
templates remains available. A later signed certificate can restore template
saving on the next launch without an application update.

## Updates

Automatic checks are off until enabled. **Guild Wars → Check for Updates…**
always performs an explicit check when the build has a repository configured.

Packaged builds include Sparkle when the repository contains a configured public
update key. Sparkle verifies the downloaded archive before installing it and can
install on quit. Development builds and bundles without Sparkle fall back to a
GitHub release check; that path opens the releases page and never installs
software.

Stable builds are offered stable releases only. Recognised prerelease tags are
`-alpha.N`, `-beta.N`, and `-rc.N`.

## Local data

The host's ordinary filesystem state lives below
`~/Library/Application Support/gwnative`. The saved login lives separately in
the macOS login Keychain, and WebKit page data uses the roots described below.
Named profiles isolate mutable state, credentials, and WebKit origins while
sharing immutable game chunks; see [Profiles](profiles.md) for the exact map.

| Path | Contents |
| --- | --- |
| `chunks/` | Content-addressed game-image chunks |
| `web/` | Writable copy of the web shell plus downloaded client artifacts |
| `derived/` | Certified transformed WebAssembly modules |
| `certificates/` | Verified signed compatibility-feed updates |
| `diagnostics/` | Rotating JSONL logs and generated problem reports |
| `manifest.cache` | Cached patch manifest, service URL, and validator |
| `manifest.pending.cache` | Verified newer manifest awaiting client promotion |
| `settings.json` | Default-profile game and host settings |
| `updates.json` | Application-wide update preferences and last-check time |
| `profiles/<id>/` | Named-profile settings, client, diagnostics, generation, and window state |
| `window.json` | Window frame and mode |
| `generations/` | Installed-client hashes, proof state, rollback copy, and refusals |
| `gwnative.lock` | Single-instance lock |
| `profiles.lock`, `updates.lock`, `chunks.lock` | Cross-process catalog, updater-state, and shared-cache locks |

`chunks.clear` appears temporarily when clearing the chunk cache has been
scheduled. It remains pending while any profile is using the shared cache and
is consumed by the next launch that can obtain exclusive maintenance access.

The packaged application stores WebKit page data under
`~/Library/WebKit/com.gwnative.app`. A development `cargo run` build uses
`~/Library/WebKit/gwnative`. The roots are intentionally separate because
WebKit derives them from the bundle identifier or process name. IndexedDB in
these roots holds the client's local files, including templates and chat logs.

**Settings → Clear Game Data…** schedules deletion of the chunk cache for the
next launch, before any background reader opens it. The game re-fetches what it
needs. This does not delete characters or other account state.

The failed-boot action **Reset game data…** is different: it deletes the
current origin's IndexedDB data while retaining downloaded chunks. Use it only
when the client cannot boot; it removes local game settings, templates, and chat
logs.

## Diagnostics and problem reports

The host appends structured records to:

```text
~/Library/Application Support/gwnative/diagnostics/gwnative.jsonl
```

The log rotates at 5 MiB and keeps five files. It combines:

- a session record describing the Mac and build;
- one-second host resource and chunk-store samples;
- page console output; and
- marked moments with temporarily higher-frequency samples.

For a crash, failed download, or boot that never finishes, choose **Help →
Report a Problem… → Save Report…**. The generated text report includes the
environment, settings, and the last 400 records. Email-shaped strings are
redacted; the report states the scope of that redaction and can be read before
sharing.

For stutter or slow frames, press **⌘⇧M while the problem is happening**, each
time it happens, then save the report. The mark records the moment and samples
the following ten seconds at 100 ms intervals; it cannot increase detail
retroactively. Normal reports also include complete-frame submission and
activation-cover health, so a rendering fallback or 500 ms cover fail-safe is
visible without enabling invasive tracing. Diagnostic builds can attach the
more detailed structured rendering state described in
[Rendering diagnostics](rendering-diagnostics.md).

## Recovery

### Boot failure overlay

When the client fails to boot, the overlay offers:

- **Try again** — reload the page and retry transient failures.
- **Reset game data…** — delete the current WebKit origin's IndexedDB data,
  keep downloaded chunks, and reload.
- **Show log** — open the diagnostics overlay.

### Newly downloaded client does not run

Client artifacts are checked by length and SHA-256 at launch. A newly installed
set remains *unproven* until it reports a first frame. If a certified transform
fails first, only that exact runtime transform is disabled and the same
ArenaNet module is retried unmodified. If the unmodified client then also fails,
gwnative restores the previous artifact set and its manifest and remembers the
rejected patch generation. On a first install, where there is no previous set,
the only available client is retained. Running `gwnative sync` explicitly
retries a rejected generation.

### Renderer disappears

If WebKit terminates the web content process, gwnative reloads once. A second
termination becomes a visible error instead of an infinite reload loop.

### Saved login is missing

Keychain access is tied to the application's signing identity. Moving the same
signed app does not matter, but changing the bundle identifier or signing
certificate does. gwnative suppresses the macOS account-password prompt and
falls back to the game's normal sign-in form. Signing in once recreates the
item for the current identity.

Development builds without a stable signing certificate can appear as a new
application after every rebuild. See the
[development guide](development.md#code-signing-and-the-keychain).

### Page data appears missing for one session

The loopback origin normally uses fixed port `38112` because IndexedDB is keyed
by scheme, host, and port. If the port is occupied, gwnative falls back to an
ephemeral port and logs that the saved page state will not be visible for that
session. Quit the process using the port and relaunch to return to the normal
origin.
