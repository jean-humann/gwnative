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
5. Rust validates the schema, finite ranges, ownership, IDs, and truncation
   consistency again.

The resulting state covers the numeric player agent and position, map/instance
identity, current target position/range, bounded party roster, and the player’s
eight-slot skillbar, buffs, and effects.

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
    "targetValid": false,
    "targetKind": "None",
    "rangeName": "None",
    "party": {
      "id": 3,
      "hardMode": false,
      "defeated": false,
      "leader": true,
      "alliesTruncated": false,
      "players": [
        {
          "loginNumber": 42,
          "calledTargetId": 0,
          "state": 3,
          "connected": true,
          "ticked": true
        }
      ],
      "heroes": [],
      "henchmen": [],
      "allies": []
    },
    "skillbar": {
      "agentId": 4,
      "disabledMask": 0,
      "castCount": 0,
      "casting": false,
      "skills": [
        {
          "slot": 1,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 100,
          "event": 0,
          "disabled": false
        },
        {
          "slot": 2,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 101,
          "event": 0,
          "disabled": false
        },
        {
          "slot": 3,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 102,
          "event": 0,
          "disabled": false
        },
        {
          "slot": 4,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 103,
          "event": 0,
          "disabled": false
        },
        {
          "slot": 5,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 104,
          "event": 0,
          "disabled": false
        },
        {
          "slot": 6,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 105,
          "event": 0,
          "disabled": false
        },
        {
          "slot": 7,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 106,
          "event": 0,
          "disabled": false
        },
        {
          "slot": 8,
          "adrenalineA": 0,
          "adrenalineB": 0,
          "recharge": 0,
          "skillId": 107,
          "event": 0,
          "disabled": false
        }
      ]
    },
    "effects": {
      "agentId": 4,
      "buffsTruncated": false,
      "effectsTruncated": false,
      "buffs": [
        {
          "skillId": 200,
          "buffId": 300,
          "targetAgentId": 4
        }
      ],
      "effects": [
        {
          "skillId": 201,
          "attributeLevel": 12,
          "effectId": 301,
          "agentId": 8,
          "duration": 12.5,
          "timestamp": 400
        }
      ]
    }
  }
}
```

When `targetValid` is true, target ID, coordinates, and distance must all be
present. When false, all are absent. Coordinates and distances must be finite
and bounded. Text fields are length- and control-character-checked.

Party state is optional when its exact layout is unavailable. When present it
contains 1–12 player records and at most 12 combined player, hero, and henchman
records, plus at most 32 other allied agent IDs. Every hero owner must be one
of the published player login numbers. `alliesTruncated` says that the client
held more allied IDs than the snapshot publishes. The numeric login number is
the client’s transient party identifier, not an account name.

Skillbar state is optional under the same certification rule. When present it
belongs to `playerId` and contains exactly eight ordered slots. `recharge` and
`event` retain the client/GWCA numeric fields; they are not wall-clock
timestamps. Empty slots have `skillId: 0`. The API exposes no operation to use
a skill or load a build. `disabledMask` preserves the client’s eight-slot mask,
each slot carries the corresponding derived `disabled` value, and `castCount`
is the bounded size of the client’s current cast queue.

Effects state is also optional and belongs to `playerId`. It publishes at most
32 maintained buffs and 64 active effects. `buffsTruncated` and
`effectsTruncated` distinguish a complete page from a capped one. Buff records
carry the skill, buff, and target-agent identifiers. Effect records retain the
client/GWCA skill, attribute level, effect identifier, source agent, duration,
and timestamp fields. Identifiers are unique within each published array and
durations must be finite and non-negative. Empty arrays are a valid certified
reading: they mean the player currently has no corresponding state.

The token is a session capability, not a long-lived API key. Do not persist or
publish it. The API has no remote listener, WebSocket transport, account data,
inventory, chat, quest, or action surface.

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
- measured frame rate;
- party roster summary;
- player skillbar IDs;
- player buff/effect counts; and
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
