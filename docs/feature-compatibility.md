# Feature compatibility and roadmap

This review compares gwnative with community projects at the revisions
inspected for this work:

- [`gw_in_browser` at `a1487a0`](https://github.com/gwdevhub/gw_in_browser/tree/a1487a0683628ce186748510205d16be5c89caaa);
- [`GWToolbox++` at `ca20ae7`](https://github.com/gwdevhub/GWToolboxpp/tree/ca20ae743551fd24f4fd0653e8a64d5e5b55a820);
- [`Py4GW Reforged` at `20bb274`](https://github.com/apoguita/Py4GW_Reforged/tree/20bb2747fc0fffe3573baa6c8015e1bc523662b7);
- [`Py4GW Reforged Native` at `6b74fdc`](https://github.com/apoguita/Py4GW_Reforged_Native/tree/6b74fdc8a0fc64b2fdf85df99165e25f0dffa067);
- [`Daybreak` at `d800a63`](https://github.com/gwdevhub/Daybreak/tree/d800a630b5f0599c825bd4ddf9461bc702614fcd);
- [`gwonmac` at `3295a95`](https://github.com/Mat4m0/gwonmac/tree/3295a95a804ef49e8db95b7d839436bfc826152b).

It is a capability review, not a promise of source compatibility. GWToolbox++
and parts of Daybreak integrate with the native Windows client through
injection and GWCA. gwnative hosts ArenaNet's WebAssembly client in WebKit.
Offsets, hooks, rendering, and policy boundaries therefore differ.
The names-only, reproducible comparison is in the
[interoperability surface map](interoperability-map.md).

## Current result

| Capability family | Status | gwnative result |
| --- | --- | --- |
| Official client/artifact sync | Available | Verified patch manifest, generation rollback, `sync`, `-update` |
| Streamed or full game image | Available | On-demand content-addressed chunks, resumable full download, `-image`, `repair`, local image import |
| Guild Wars CLI recognition | Available | Every documented switch parsed; native translations or explicit notices |
| Isolated launch profiles | Available | Per-profile mutable state, Keychain identity, origin, overlays, and build library |
| Explicit `.gwmod` sessions | Available | Compatible format, host-side ZIP/graph validation, double hash validation, ordered runtime |
| Versioned game API | Foundation available | Token-gated v1 read-only map/player/target/party/skillbar/effects/agents/quests/inventory/social/completion/camera/trade/UI state |
| Overlay framework | Available | Profile-local movable widgets and exact-context hotkeys |
| Clock, session timer, FPS | Available | Built-in Companion Tools widgets |
| Target distance/range | Available on certified builds | Bounds-checked companion snapshot |
| Build-template client repair | Available on certified builds | Save/list/rename/delete transform; unknown builds fall back |
| Build/team library | Available | Profile-local opaque-code library with validated import/export |
| Game cursor | Available on certified builds | Game bitmap rendered as native pointer |
| Party and heroes | Available on certified builds | Bounded roster, flags, identifiers, and summary widget |
| Player skillbar | Available on certified builds | Eight ordered read-only slots, disabled mask, cast count, recharge, adrenaline, and event fields |
| Effects and buffs | Available on certified builds | Bounded player buff/effect records, explicit truncation, and summary widget |
| Map agents | Available on certified builds | 128-entry bounded page with PyAgent-compatible position, profession, health, allegiance, and derived state |
| Quests and mission objectives | Available on certified builds | Active ID, 64 bounded quest records, log-state derivatives, markers, and 32 objective records |
| Inventory and account storage | Available on certified builds | 22 bounded bag summaries, 512 ordered item records, explicit truncation, gold, storage panes, PyItem-compatible numeric metadata and derived flags |
| Friends and guild | Available on certified builds | 128-entry bounded presence page, exact category totals, numeric status/zone/ID, and privacy-minimised guild/rank/faction/roster/cape summary |
| Mission and map completion | Available on certified builds | Six bounded WorldContext bitmaps expanded into sorted normal/HM mission, bonus, unlocked-map, and vanquish IDs |
| Camera and render state | Available on certified builds | Bounded camera mode, agent target, position/look-at vectors, distance, yaw/pitch, raw FOV, and derived render FOV |
| Trade offer | Available on certified builds | Bounded local status flags, both gold offers, and two 16-item read-only pages with explicit truncation and stale-close normalisation |
| UI frame inventory | Available on certified builds | 128-frame read-only page with exact array identity, parent back-references, state-bit derivatives, local geometry, full totals, and explicit truncation |
| Chat and party search | Needs certified layout and policy | No chat write or packet injection surface |
| Skill activation or build application | Read-only only | Codes can be stored; no game action is exposed |
| Texture/shader packs | Research | WebGL/WASM pipeline differs from native DirectX replacement |
| Unattended gameplay automation | Blocked | Not exposed through API, hotkeys, overlays, or mods by default |

The same status list is visible in **View → Companion Tools…**. A feature is
not labelled available merely because an upstream native client can implement
it.

## Certification gate

A new game-facing domain requires all of the following:

1. exact current-module hash;
2. documented pointer or function layout for that hash;
3. bounded read/write semantics and invariants;
4. fixtures that reject malformed, stale, and partial state;
5. an API-version decision;
6. player-visible unavailable state on unknown builds; and
7. policy review for any action that changes game state.

Read-only state is the default. A user gesture in gwnative is not by itself
proof that an arbitrary game write is safe. The v1 actions endpoint therefore
returns 409 until a specific operation passes certification.

## Next implementation slices

The dependency order for further parity is:

1. certify bounded dialog identity from read-only state or observed typed events;
2. research WebGL-native texture replacement without patching unknown modules;
3. map bounded read-only merchant inventory; and
4. consider narrowly named, user-triggered actions one at a time.

Large upstream windows should not be ported as one monolith. Each slice needs
its own schema, fixtures, compatibility fallback, and Conventional Commit.

## Explicit non-goals

- DLL injection, Windows process scanning, and GWCA ABI compatibility;
- remote control of a running account;
- exposing credentials, chat logs, or inventory through an unauthenticated
  endpoint;
- guessing offsets after an ArenaNet update;
- promising that arbitrary `.gwmod` code is sandboxed; and
- unattended farming, combat, movement, or trade automation.

See [Game API and overlays](game-api.md), [Mods](mods.md), and
[Acknowledgements](../ACKNOWLEDGEMENTS.md) for the implemented boundaries and
project lineage.
