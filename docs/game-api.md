# Game API and overlays

gwnative exposes a small versioned, read-only game-state API for companion
tools. It is deliberately narrower than a general game API: fields appear only
after the exact client build and pointer layout have been certified.

Unknown builds remain playable but expose no ready state. A certified companion
may publish `waiting` or `unsupported` while the client is between readable
states. No guessed offset is read.

## Trust chain

1. A build-hash-pinned transform installs the embedded companion hook.
2. The companion performs bounds-checked, read-only traversal at a coherent
   point in the game loop.
3. A seqlock snapshot crosses into the page.
4. JavaScript copies only the public schema and publishes at most four times a
   second.
5. Rust validates the schema, finite ranges, and target consistency again.

The resulting state covers only the numeric player agent and position,
map/instance identity, and current target position/range.

## Loopback endpoints

Every endpoint below is bound to the selected profile's loopback origin and
requires the `X-Gwnative-Token` header. A windowed app injects the token directly
into its page. `serve` prints the address and token for diagnostics:

```text
127.0.0.1:38112 <token>
```

| Method and path | Result |
| --- | --- |
| `GET /__game/v1` | Version, long-poll transport, state-domain, and action capabilities |
| `GET /__game/v1/state` | Latest validated envelope, or 404 before publication |
| `GET /__game/v1/state?after=REVISION&waitMs=MILLISECONDS` | Wait up to 15 seconds for a newer revision, or return 404 |
| `PUT /__game/v1/state` | Internal page-to-host publisher; not an external game write |
| Any `/__game/v1/actions` request | 409; no write action is certified |

Example read:

```sh
curl -H "X-Gwnative-Token: $token" \
  "http://127.0.0.1:38112/__game/v1/state"
```

The envelope is revisioned and timestamped:

```json
{
  "apiVersion": 1,
  "revision": 42,
  "publishedAtMs": 1785320000000,
  "state": {
    "status": "ready",
    "tickCount": 100,
    "mapId": 55,
    "instanceType": 1,
    "instanceName": "Explorable",
    "playerId": 4,
    "playerX": 1.5,
    "playerY": 2.5,
    "targetValid": false
  }
}
```

When `targetValid` is true, target ID, coordinates, and distance must all be
present. When false, all are absent. Coordinates and distances must be finite
and bounded. Text fields are length- and control-character-checked.

The token is a session capability, not a long-lived API key. Do not persist or
publish it. The API has no remote listener, WebSocket transport, account data,
inventory, chat, party, quest, or action surface.

Long polling sleeps in the native server until the page publishes a newer
revision. A consumer can therefore follow live state without a timer repeatedly
waking the single-threaded WebAssembly client.

## Overlay registry

Built-in tools use a managed overlay surface. A widget receives one section, a
validated identifier, update/visibility handles, and a draggable header in edit
mode. It cannot replace the game canvas through the registry. The format-1 mod
ABI does not currently export this registry to WebAssembly mods.

Layout format 1 stores bounded `x`, `y`, and visibility values in profile-local
`localStorage`. At most 128 widget records are accepted. Unknown versions,
malformed identifiers, non-finite coordinates, and excessive coordinates fall
back safely.

Open **View → Companion Tools…** or press **⌘⇧T**. The available widgets are:

- clock;
- session timer;
- current target and range;
- measured frame rate; and
- profile-local build and team library.

Press **⌘⇧O** to toggle layout editing. The hotkey engine requires an exact
modifier chord, ignores text controls and editable content, and invokes only
local UI callbacks. It never synthesizes game input.

The build library treats template codes as opaque strings. It supports up to
500 entries and 12 members per team, with validated import/export JSON. It does
not apply builds to the game because no write operation is certified.

The broader GWCA, Py4GW, JSPI, and WebAssembly research inventory is documented
in the [interoperability surface map](interoperability-map.md). A mapped name is
not part of this API until it passes the certification gate described there.
