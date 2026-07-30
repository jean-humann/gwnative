# Interoperability surface map

This document separates research coverage from gwnative's supported API.
Finding a native function, Python binding, host callback, or resolver name does
not make it safe or available in gwnative. The supported runtime contract
remains the build-certified, read-only surface in
[Game API and overlays](game-api.md).

## Reproducible inventory

The dependency-free mapper records public interface names and source revisions:

```sh
scripts/api-surface \
  --gwtoolbox /path/to/GWToolboxpp \
  --py4gw /path/to/Py4GW_Reforged \
  --py4gw-native /path/to/Py4GW_Reforged_Native \
  --jspi-wasm /path/to/Gw.jspi.wasm \
  --jspi-js /path/to/Gw.jspi.js \
  --asyncify-wasm /path/to/Gw.wasm \
  --json
```

The JSON report contains:

- GWCA manager function names grouped by packaged header;
- Py4GW module functions, classes, methods, and attributes from type stubs;
- Py4GW Reforged Native module binding names;
- native offset namespace, pattern, and resolver names;
- gwnative's public state fields and certified layout field names;
- JSPI host paths, WASM imports, exports, custom sections, and hashes; and
- a normalised cross-project domain index.

It does not emit offset values, native signatures, assertions, function
bodies, data segments, or implementation source. Reports belong outside the
repository when they identify locally installed client artifacts.

## Inspected sources

This map was refreshed on 30 July 2026 against:

| Source | Revision | Inventory role |
| --- | --- | --- |
| [GWToolbox++](https://github.com/gwdevhub/GWToolboxpp/tree/ca20ae743551fd24f4fd0653e8a64d5e5b55a820) | `ca20ae7` | Packaged GWCA 4.7.2.3 manager headers |
| [Py4GW Reforged](https://github.com/apoguita/Py4GW_Reforged/tree/20bb2747fc0fffe3573baa6c8015e1bc523662b7) | `20bb274` | Python-facing type stubs and research notes |
| [Py4GW Reforged Native](https://github.com/apoguita/Py4GW_Reforged_Native/tree/6b74fdc8a0fc64b2fdf85df99165e25f0dffa067) | `6b74fdc` | Current binding and named resolver inventories |

At those revisions the mapper finds 413 functions in 21 GWCA manager headers;
43 Py4GW stub modules with 1,003 class methods; 42 native binding modules; 28
offset namespaces; 205 named patterns; 219 named resolvers; and 51 normalised
cross-project domains. These counts are observations, not a compatibility
promise.

GWToolbox++ distributes the inspected GWCA headers within its MIT-licensed
repository. No separate authoritative GWCA source checkout was available
for this review, so the GWCA inventory is anchored to those packaged public
headers rather than an implementation repository. Py4GW Reforged Native
declares Apache-2.0. The inspected Py4GW Reforged checkout did not contain a
root licence file; gwnative therefore treats it as research material only and
copies no implementation.

## JSPI and Asyncify clients

The current official manifest offers two WebAssembly variants of the same
client generation:

| Property | `Gw.jspi.wasm` | `Gw.wasm` |
| --- | ---: | ---: |
| SHA-256 | `3039ca5489eb2bddb38844d275320e3ac070baf01b5b888fc2062982e343f3a8` | `9f5adca0de63beda860c7ac28a389939600994fe8dde02e9270c1dfce4da84a0` |
| Size | 8,196,718 bytes | 27,837,527 bytes |
| Defined functions | 17,603 | 17,655 |
| Imports | 219 | 219 |
| Exports | 53 | 105 |

All 219 import names and the 53 JSPI exports are shared. The larger variant
adds 52 `asyncify_*` and `dynCall_*` runtime exports. Its bodies are rewritten
for Asyncify, while the smaller module relies on JavaScript Promise
Integration through `WebAssembly.Suspending` and
`WebAssembly.promising`. Exact body hashes therefore do not map functions
between the variants; interface names and independently verified semantic
anchors do.

Both modules advertise `target_features`, `build_id`, and
`external_debug_info` custom sections. The debug reference names
`Gw.wasm.debug`, but that artifact is not present in the inspected manifest.
The mapper does not infer names that the distributed artifacts do not provide.

The associated JSPI glue references 126 top-level `Module` properties across
177 paths. Named host families include image access, DNS, sockets, secure
credential storage, login tokens, browser links, age signals, on-screen
keyboard state, events, and HTTP requests. These are host callbacks into the
client, not an in-game automation API.

## Domain coverage

“Mapped” means the upstream names appear in the generated inventory.
“Certified” means gwnative can publish bounded state for an exact client build.

| Domain | Upstream evidence | gwnative today | Safe next boundary |
| --- | --- | --- | --- |
| Runtime and host services | JSPI host paths and both WASM contracts mapped | Official artifact, image, DNS, socket, login, and storage bridges implemented | Keep callback contract tests generation-pinned |
| Player and target | GWCA Agent/Player; PyAgent/PyPlayer; native resolvers | Certified IDs, coordinates, target kind, distance, range, and bounded agent summaries | Add names only after encoded-string lifetime validation |
| Character progression | GWCA WorldContext; PyPlayer; matching Native binding | Certified hard-mode availability, level, experience, Kurzick/Luxon/Imperial/Balthazar current-earned-cap counters, and skill points with both duplicate copies bounded | Add title/rank semantics only from independently verified tier data |
| Map and instance | GWCA Map; PyMap; native map resolvers | Certified map ID, instance identity, 128-entry map-agent page, and six completion bitmaps expanded to sorted map IDs | Add encoded names only after lifetime validation |
| Party and heroes | GWCA Party; PyParty; native party resolvers | Certified bounded roster, flags, IDs, summary widget, and agent-derived profession/health records | Join party members to agent summaries by ID |
| Skills and effects | GWCA Skillbar/Effect/WorldContext/AccountContext; PySkill/PySkillbar/PyEffects/PyPlayer; matching Native binding | Certified player slots, adrenaline, recharge, event, disabled mask, cast count, bounded buff/effect snapshots, and distinct trainer-visible, character-learned, and account-unlocked skill sets | Add effect expiry semantics only after live timing validation; keep unlock and activation writes closed |
| Items and inventory | GWCA Item; PyItem/PyInventory; item resolvers | Certified 22-bag/512-item page with location, model/type/value/quantity metadata, gold, storage panes, dye components, modifier counts, and interaction-derived flags | Encoded names and modifier words only after lifetime and size certification |
| Quests | GWCA Quest; PyQuest; quest resolvers | Certified active ID, 64-entry quest page, state-bit derivatives, markers, and 32 mission objectives | Encoded quest/objective text only after decoder certification |
| Chat, friends, and guild | GWCA Chat/FriendList/Guild; matching Python modules | Certified 128-entry numeric friend-presence page and privacy-minimised guild/rank/faction/roster/cape summary; no names, UUIDs, messages, or actions | Add no encoded text until lifetime/privacy validation; keep message actions closed |
| Camera and rendering | GWCA Camera/Render; PyCamera/PyRender/PyWorldRender | Certified bounded camera mode, target, vectors, distance, orientation, raw FOV, and derived render FOV; native cursor and local overlays | Keep controller pointers, writes, and WebGL ownership isolated |
| UI and dialogs | GWCA UI; PyUIManager/PyDialog; UI resolvers | Certified 128-frame read-only identity/state/geometry page plus passive typed-event observation of numeric dialog body, agent, buttons, skill IDs, inferred follow-up context, and last selection; no encoded text or interaction | Certify encoded-text lifetime/decoding independently; keep dialog sends and generic UI writes closed |
| Events and packets | GWCA Event/StoC; listener and packet modules | Diagnostics expose host milestones, not game packets | Typed, bounded events derived from certified state |
| Pathing | PyPathing and native pathing resolvers | Not exposed | Geometry inspection only; no unattended movement |
| Input and actions | PyKeystroke/PyMouse and native action bindings | Only normal user input plus finite E2E actions | Review each named, user-triggered action independently |
| Plugins and scripting | GWToolbox modules and Py4GW scripts | Validated `.gwmod` sessions with a narrow ABI | Extend capabilities explicitly; never inherit native injection powers |

The detailed function, method, binding, and resolver names stay in the
machine-readable report so this guide remains reviewable instead of becoming a
generated API dump.

## Promotion rules

A mapped domain reaches the public API only when it has:

1. an exact artifact hash and reproducible transform;
2. independently verified live memory or host semantics;
3. bounds, consistency checks, and an unknown-build fallback;
4. redacted fixtures and a versioned public schema;
5. an end-to-end observation proving that the client keeps rendering; and
6. a separate policy decision for every state-changing operation.

The next implementation sequence is merchant identity/quote semantics, then
encoded-text lifetime/decoding and the remaining read-only domains with
independently verified layouts.
Native action bindings,
packet injection, virtual input, and pathing automation are reference evidence
only; they are not candidates for bulk exposure.
