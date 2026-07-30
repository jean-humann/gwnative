# Architecture

gwnative is a native host around ArenaNet's generated Guild Wars WebAssembly
client. It deliberately keeps generated client code in a browser realm and
implements the operating-system boundary in Rust.

## System boundaries

```mermaid
flowchart LR
    subgraph App["gwnative process"]
        Native["Rust host\nAppKit, storage, network, diagnostics"]
        Origin["Loopback origin\n127.0.0.1:38112"]
        Native --> Origin
    end

    subgraph WebKit["WebKit processes"]
        Harness["Web harness\ninput, graphics, audio, filesystem"]
        Client["ArenaNet client\nGw.jspi.js + Gw.jspi.wasm"]
        Companion["Certified companion WASM\nread-only game state"]
        Mods["Explicit selected mods\ntrusted shared memory"]
        Harness <--> Client
        Client --> Companion
        Client <--> Mods
    end

    Origin <--> Harness
    Native --> Keychain["macOS Keychain"]
    Native <--> Patch["ArenaNet patch/CDN services"]
    Native <--> Game["ArenaNet game services"]
    Native --> Disk["Application Support\nchunks, settings, logs, rollback"]
```

ArenaNet regenerates `Gw.jspi.js` with every client patch. That glue expects a
browser implementation of JSPI, WebGL, Web Audio, IndexedDB, DOM events, and the
Emscripten runtime. Replacing it with a native WebAssembly runtime would require
reimplementing those contracts and tracking every generated change. WKWebView
is therefore a compatibility boundary, not a general application framework.

Rust owns patching, chunk storage, outbound network policy, socket bridges,
credentials, windowing, diagnostics, updates, and release integration.
JavaScript is limited to the code that must share the client's realm: graphics
and audio import patches, filesystem compatibility, input translation, the
launcher, settings UI, and metrics.

## Boot sequence

`src/main.rs` orders launch phases by dependency:

1. Parse the command, native options, and Guild Wars compatibility switches
   before opening anything.
2. Select or create the profile and validate any explicit mod session.
3. Acquire the profile lock and, for a primary launch, the global instance
   lock.
4. Check whether the current code-signing identity can use the profile's
   Keychain item.
5. Select a writable profile web root.
6. Load the cached manifest and revalidate it in the background, or fetch a
   current manifest for an explicit sync.
7. Roll back an unproven client, verify installed artifacts, and fetch missing
   or newer artifacts.
8. Open the game-image chunk store, consume a pending clear request, replay the
   boot prefetch list, and start cursor-based readahead.
9. Complete a requested local-image import, full image, or repair operation.
10. Start diagnostics and load profile settings.
11. Prepare certified WebAssembly transforms when available.
12. Start the loopback origin and inject its session token, current keyboard
    layout, settings, update capabilities, and module state at document start.
13. Create the WKWebView, window, menu, native event bridges, renderer recovery,
    and application lifecycle delegate.
14. Mark the client generation proven and seal the boot chunk list when the
    page reports its first frame.

The `sync`, `repair`, and classic `-image` operations exit before creating a
window. The `serve` command stops after the loopback starts and prints
`<address> <session-token>` on stdout.

## Loopback origin and trust model

The page is served from HTTP loopback instead of a custom WebKit scheme:

- loopback is a trustworthy secure context;
- COOP and COEP make it cross-origin isolated;
- IndexedDB works normally; and
- `performance.now()` retains sub-millisecond resolution, which the per-frame
  telemetry needs.

The listener binds only `127.0.0.1`. Port `38112` is fixed because WebKit keys
IndexedDB by origin, including the port. If the port is unavailable, an
ephemeral fallback keeps the app launchable but temporarily presents an empty
page-data origin.

Loopback is machine-local, not user-private. Host capability routes beginning
with `__` therefore require a random session token. The token is injected into
the page through `WKUserScript`; it is never served over the socket. The window
prints it only when `GWNATIVE_PRINT_TOKEN` is set, and `serve` prints it because
external test clients otherwise have no route through the gate.

Content requests are intentionally not token-gated because the generated client
cannot attach a custom header. Their authority is narrow instead:

- static paths are resolved beneath the configured web root;
- the derived module is mapped to the original module's filename;
- snapshot requests require a bounded byte range;
- HTTP forwarding uses a closed route-to-host table; and
- game sockets permit only ArenaNet/Guild Wars domain suffixes, ports 6112, 80,
  and 443, and public-unicast destination addresses.

This blocks loopback, private networks, link-local services, multicast, IPv4
addresses disguised as IPv6, and unrecognised names.

Every host response carries a content-security policy, COOP/COEP/CORP, and
`nosniff`. The policy allows the Emscripten requirements (`unsafe-eval`,
`wasm-unsafe-eval`, and blob workers) but denies objects, frames, base URL
changes, forms, and off-origin resource loads. A navigation delegate rejects
top-level navigation away from the exact loopback origin.

## Profile boundaries

The default profile preserves the original support directory, Keychain account,
and port. A named profile moves mutable support files below `profiles/<id>`,
uses `login:<id>` in Keychain, and receives a deterministic loopback port.
Because WebKit keys IndexedDB and local storage by origin, the stable port also
isolates page data, overlays, and the build library.

Content-addressed snapshot chunks and the mod discovery directory remain
shared. Chunk pruning retains the union of valid cached manifests so an older
installed profile generation is not evicted by a newer one. A second instance
bypasses only the global primary lock and requires an explicit non-default
profile. Its profile lock is still mandatory, preventing an accidental pair of
writers to the same settings, window, and page origin. See
[Profiles](profiles.md) for the storage map.

## Client artifacts and generation rollback

The patch manifest describes the official client files and the chunked game
image. gwnative stores manifest validators and can start from the disk copy,
moving revalidation off the window's critical path.

Artifact presence is not treated as integrity:

- each installed client artifact is recorded by length and SHA-256;
- launch checks the record and downloads only unsound artifacts;
- the offered build ID is derived from manifest data before download; and
- a newly installed generation is unproven until `POST /__booted`.

Before replacing a proven set, gwnative saves it. If the new set fails to report
a first frame before the next launch, the prior set is restored and the failed
build ID is refused. Refusals are bounded, a damaged installed copy may retry a
refused build when no alternative exists, and first install never deletes its
only client merely because an unrelated boot failure occurred.

## Game-image storage

The 4.2 GB snapshot is represented by roughly 16,000 content-addressed 256 KiB
chunks under `Application Support/gwnative/chunks`. The snapshot file is
virtual: the loopback server maps a requested byte range to chunk windows,
fetching missing chunks and streaming the response without assembling the full
range in memory.

Core invariants:

- a hash identifies immutable content;
- a fetched chunk is verified before an atomic rename makes it visible;
- cached bytes are hashed on first use in a session;
- a corrupt file is unlinked and fetched again;
- concurrent readers of the same missing hash share one fetch;
- at most 48 network fetches run concurrently;
- speculative boot, readahead, and full-download work shares a 32-permit subset
  so demand reads always have capacity;
- up to 2,048 verified chunk descriptors are held open for cheap `pread`; and
- manifest activation prunes chunks the current image cannot reference.

Quick Start records the chunks touched before first frame and replays that list
on the next launch. A built-in list covers the first launch. Readahead follows
the current read cursor. Full Game walks the entire manifest, while its progress
bar reports cache residency rather than how far the current walk has advanced.

A complete Full Game launch runs a separate integrity pass. It hashes distinct
resident chunks, discards failures, and lets the ordinary download path repair
them. Verification is separate from resume so every interrupted download does
not pay for a full reread.

## WebAssembly compatibility layers

ArenaNet's current module has broken or missing file routines for build
templates. `src/wasm` applies a transform only when the input SHA-256 matches a
certified build:

1. append small forwarding functions;
2. redirect specific call sites without shifting later bytecode;
3. use impossible negative directory descriptors as bridge markers; and
4. handle those markers in `web/template-save.js` against IDBFS.

The transform asserts the exact output hash. Unknown or failed transforms fall
back to the unmodified module, preserving playability at the cost of the
compatibility feature.

When either optional enhancement is enabled, a second certified transform
clones the client's main-loop function, adds a hook slot and manifest, and
dispatches through an optional table entry. `build.rs` compiles
`src/companion-kernel/lib.rs` directly as dependency-free `no_std`
`wasm32-unknown-unknown` code and embeds it in the host.

The companion imports the client's memory, performs bounds-checked read-only
pointer traversal at a coherent point in the game loop, and publishes fixed
state and cursor blocks through a seqlock. The page validates the snapshot again
before rendering it. Installation allocates through the client's own allocator,
instantiates the companion, fills the table slot, and only then enables the
hook.

The validated companion state is narrowed into the versioned v1 map, player,
target, party, skillbar, effects, agent, quest, inventory, social, completion,
camera, trade, UI-frame, merchant item-ID, and character-progression schema.
Large agent, quest,
effect, bag, item, friend, UI-frame, and merchant pages remain fixed and
bounded in the seqlock block rather than being copied onto the companion’s
imported-memory stack. Inventory traversal retains only the owning
pointer across the original tick, then rechecks bag arrays and every item
back-reference while publishing. Social traversal likewise retains only the
friend-list and guild-context owners, rechecks every numeric friend and
guild/roster pointer, and never follows client-owned names, UUIDs, messages, or
announcements. Completion traversal retains six bounded array descriptors and
publishes at most 32 bitmap words per category; the page derives sorted map IDs.
Camera collection reads one exact-build singleton, validates the stable GWCA
field offsets and finite geometry, and publishes no controller pointer or
transition target. The page derives current yaw and render FOV from the copied
values, then Rust recomputes both before accepting the public state.
Trade collection follows the certified `GameContext + 0x58` pointer, validates
the fixed context flags plus two bounded item arrays, and drops stale offer
contents whenever the window is closed. Neither the companion nor the public
API exposes a trade write.
UI collection reads the exact-build global frame-array descriptor used by the
compiled frame lookup routine. Publication rechecks each slot's embedded ID,
parent back-reference, scalar state, and finite local rectangle while excluding
labels, callbacks, tooltips, relation lists, dialog payloads, and every UI
write. The 128-record page retains full-array totals and explicit truncation.
Merchant collection reads only the independently verified numeric
`WorldContext::merch_items` array. It validates at most 512 IDs and publishes
128 with explicit totals and truncation, without inferring a merchant window,
catalog, price, quote, identity, or action from that transient array.
Progression collection reads only fixed scalar `WorldContext` fields verified
in both GWCA and Py4GW Reforged Native. It bounds both client copies of every
duplicated counter before selecting the higher one, then enforces level,
faction-cap, total-earned, and skill-point relationships. The append-only
snapshot exposes no title derivation, encoded text, or progression write.
The page publishes no faster than four times per second and Rust validates it
again before making it available on token-gated loopback routes. There is no
certified action endpoint. The overlay registry and Companion Tools consume the
same read-only state; see
[Game API and overlays](game-api.md).

Explicit mods follow a separate trust path. `src/mods.rs` parses the selected
format-1 manifest and ZIP structure, resolves nested dependencies, enforces
resource limits, and hashes every module before WebKit starts. The page checks
the catalog and SHA-256 again, instantiates modules in dependency order against
game memory and earlier exports, and calls `mod_init`. Shared memory means the
module is trusted even after package validation; see [Mods](mods.md).

## WebKit and native integration

WKWebView lacks several Chromium APIs the client assumes:

- keyboard layout comes from Carbon's `UCKeyTranslate`;
- macOS double-clicks can be translated into the client's touch path;
- mouse focus and application focus are pushed from AppKit;
- graphics imports bridge OffscreenCanvas output to the visible canvas;
- the host constructs and monitors the client's AudioContext; and
- filesystem sync is pulled during application termination.

WebKit preferences that cap high-refresh rendering or suppress hidden content
are disabled by guarded feature-key lookup. Until first frame, the harness also
races `requestAnimationFrame` with a 16 ms timer so an occluded boot continues
to drive client work. After first frame the native frame callback is restored;
game animation and audio retain WebKit's normal timing.

The renderer guard reloads once if WebKit terminates the content process. The
window layer validates persisted geometry against current display work areas,
restores mode separately from the normal frame, and reports the active display's
refresh-rate ceiling.

## Diagnostics

Three producers share one JSONL clock:

- a host sampler records physical footprint, resident memory, CPU time, chunk
  statistics, and registered metrics;
- the page posts count, gauge, and peak buckets to `__diag`; and
- console and unhandled-error lines are batched through `__report`.

Records have `session`, `sample`, `page`, or `mark` kinds. Metric names, page
line counts, line lengths, and file rotation are bounded. The problem-report
layer adds environment and settings, selects the tail of the log, and redacts
email-shaped text.

See the [performance guide](performance.md) for measurement semantics.

## Module map

| Area | Primary paths |
| --- | --- |
| Launch and lifecycle | `src/main.rs`, `src/cli.rs`, `src/app.rs`, `src/instance.rs`, `src/relaunch.rs` |
| AppKit and WebKit | `src/window/`, `src/webview.rs`, `src/renderer.rs`, `src/menu/`, `src/commands.rs`, `src/layout.rs` |
| HTTP origin | `src/server/`, `src/http/`, `src/proxy.rs`, `src/ws.rs` |
| Network bridges | `src/net.rs`, `src/sockets.rs`, `src/transport.rs` |
| Patching and generations | `src/patch.rs`, `src/manifest.rs`, `src/generation.rs` |
| Snapshot cache | `src/chunks/`, `src/cache.rs`, `src/disk.rs`, `src/qos.rs` |
| Profiles, credentials, settings | `src/profile.rs`, `src/keychain.rs`, `src/settings.rs`, `src/paths.rs` |
| WebAssembly transforms | `src/wasm/`, `src/companion-kernel/lib.rs`, `build.rs` |
| Mods and game API | `src/mods.rs`, `src/game_api.rs`, `web/mod-runtime.js`, `web/game-api.js` |
| Overlays and tools | `web/overlay.js`, `web/tools-panel.js`, `web/build-library.js`, `web/hotkeys.js` |
| Diagnostics | `src/diagnostics.rs`, `src/report.rs`, `web/diagnostics.js`, `web/memory.js` |
| Web harness | `web/harness.js`, `web/graphics.js`, `web/audio.js`, `web/filesystem.js`, `web/input.js` |
| Player UI | `web/launcher.js`, `web/settings-panel.js`, `web/guide.js`, `web/loading.js` |
| Packaging and release | `packaging/`, `scripts/bundle`, `scripts/release`, `scripts/publish`, `scripts/appcast` |

## Platform risks

- The deployment floor is macOS 15.2, but WebKit behaviour is verified on newer
  releases and can change independently of this binary.
- Hidden-page and high-refresh behaviour relies on guarded WebKit feature keys.
  Missing keys degrade to WebKit defaults and are logged.
- Client transforms are intentionally pinned to exact module hashes. A new
  ArenaNet build can temporarily disable templates and tools without preventing
  launch.
- Development and packaged builds use separate WebKit storage roots.
- The loopback port fallback changes the page origin for that session.
- Named-profile port hashes can collide; an explicit port resolves the launch
  but selects another page origin.
- A validated mod still shares writable game memory and must be trusted by the
  player.
