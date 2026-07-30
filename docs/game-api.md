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
identity, current target position/range, bounded party roster, the player’s
eight-slot skillbar and effects, a bounded map-agent page, and the quest log
with mission objectives, plus bounded inventory and account-storage summaries.
It also includes a privacy-minimised friend-presence page and numeric guild
summary. Client-owned names, UUIDs, messages, and announcements are not read.

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
    },
    "agents": {
      "truncated": false,
      "total": 1,
      "agents": [
        {
          "agentId": 4,
          "typeBits": 219,
          "kind": "Living",
          "playerNumber": 42,
          "primary": 7,
          "secondary": 0,
          "level": 20,
          "health": 0.75,
          "rotation": 1.25,
          "x": 1.5,
          "y": 2.5,
          "z": 3,
          "modelState": 65,
          "effects": 0,
          "allegiance": 1,
          "isLiving": true,
          "isItem": false,
          "isGadget": false,
          "isDead": false,
          "isMoving": false,
          "isAttacking": false,
          "isKnockedDown": false,
          "isCasting": true
        }
      ]
    },
    "quests": {
      "activeQuestId": 44,
      "questsTruncated": false,
      "objectivesTruncated": false,
      "quests": [
        {
          "questId": 44,
          "logState": 34,
          "mapFrom": 55,
          "markerX": 10,
          "markerY": 20,
          "markerPlane": 3,
          "mapTo": 56,
          "completed": true,
          "currentMission": false,
          "primary": true,
          "areaPrimary": false
        }
      ],
      "missionObjectives": [
        {
          "objectiveId": 7,
          "type": 2
        }
      ]
    },
    "inventory": {
      "itemsTruncated": false,
      "total": 1,
      "goldCharacter": 1234,
      "goldStorage": 50000,
      "storagePanesUnlocked": 4,
      "bags": [
        {
          "bagId": 1,
          "bagType": 1,
          "kind": "Inventory",
          "containerItem": 0,
          "capacity": 20,
          "itemCount": 1,
          "isInventory": true,
          "isEquipped": false,
          "isNotCollected": false,
          "isStorage": false,
          "isMaterialStorage": false
        }
      ],
      "items": [
        {
          "itemId": 500,
          "agentId": 0,
          "bagId": 1,
          "slot": 0,
          "modelFileId": 123,
          "type": 9,
          "typeName": "Usable",
          "value": 100,
          "interaction": 17432577,
          "modelId": 456,
          "itemFormula": 0,
          "quantity": 5,
          "equipped": false,
          "profession": 255,
          "customized": true,
          "materialSalvageable": false,
          "modifierCount": 2,
          "dyeTint": 7,
          "dye1": 2,
          "dye2": 3,
          "dye3": 4,
          "dye4": 5,
          "isStackable": true,
          "isInscribable": false,
          "isIdentified": true,
          "isTradable": true,
          "isUsable": true,
          "isPrefixUpgradable": true,
          "isSuffixUpgradable": true,
          "isInscription": false,
          "isPurple": false,
          "isGreen": false,
          "isGold": true,
          "isInventoryItem": true,
          "isStorageItem": false
        }
      ]
    },
    "social": {
      "playerStatus": 1,
      "playerStatusName": "Online",
      "friends": {
        "truncated": false,
        "total": 1,
        "friends": 1,
        "ignores": 0,
        "partners": 0,
        "traders": 0,
        "entries": [
          {
            "slot": 0,
            "type": 1,
            "typeName": "Friend",
            "status": 1,
            "statusName": "Online",
            "friendId": 77,
            "zoneId": 55,
            "isOnline": true
          }
        ]
      },
      "guild": {
        "index": 2,
        "playerRank": 3,
        "rank": 1,
        "features": 9,
        "rating": 1200,
        "faction": 0,
        "factionName": "Kurzick",
        "factionPoints": 1000,
        "qualifierPoints": 10,
        "rosterTotal": 50,
        "cape": {
          "backgroundColor": 1,
          "detailColor": 2,
          "emblemColor": 3,
          "shape": 4,
          "detail": 5,
          "emblem": 6,
          "trim": 7
        }
      }
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

The agent page contains at most 128 live entries from the client’s agent array,
in increasing `agentId` order. `total` counts readable entries and `truncated`
distinguishes a complete page from the 128-record cap. Each record corresponds
to the read side of GWCA `Agent`/`AgentLiving` and PyAgent: numeric type bits,
position, rotation, model identity, profession, level, health, allegiance, and
model/effect state. The boolean kind and movement/combat fields are derived
from those same numeric words and are rechecked by Rust. Non-living records
must carry zeroes in every living-only field. Names and encoded strings are not
read because their lifetime and decoding contract have not been certified.

The quest domain publishes at most 64 `Quest` records and 32 mission
objectives. It preserves `questId`, `logState`, source/destination map IDs, and
the numeric marker. `completed`, `currentMission`, `primary`, and
`areaPrimary` must exactly match their documented `logState` bits.
`activeQuestId: 0` means no selected quest; on an untruncated page a non-zero
active ID must appear in the quest list. Mission objectives expose only their
numeric ID and type flags, not client-owned encoded text. Independent
truncation flags cover both arrays.

The inventory domain follows the player `ItemContext → Inventory → Bag → Item`
graph used by GWCA `Item` and PyItem/PyInventory. It publishes every present
bag among the 22 defined bag IDs and at most 512 occupied slots, ordered by
`bagId` and zero-based `slot`. `total` is the number of occupied slots across
all readable bags; `itemsTruncated` distinguishes a complete page from the
512-record cap. Bag type, index, capacity, occupied count, and every item
back-reference are rechecked before publication. The sum of bag `itemCount`
values must equal `total`.

Item records retain numeric identity, agent, model/file, type, value,
interaction, formula, quantity, profession, customization, material, dye, and
modifier-count fields. The boolean stackable, inscribable, identified,
tradable, usable, upgrade, inscription, rarity, inventory, and storage fields
are exact derivatives of the same interaction and bag words and are checked
again by Rust. Encoded item names, customization text, modifier words, and
merchant/trade state are not read in this ABI. Inventory is sensitive account
state: the loopback token is mandatory, the page publishes no faster than four
times per second, and the domain has no move/use/equip/salvage/gold action.

The social domain follows the GWCA/Py4GW `FriendList`, `Friend`, `GuildContext`,
`Guild`, and `GuildPlayer` read layouts. It publishes the player's numeric
presence, at most 128 of 256 bounded contacts, exact category totals, contact
type/status, an opaque numeric friend ID, and last zone ID. `isOnline` is true
only for Online, Do Not Disturb, and Away. `truncated` distinguishes a complete
list from the 128-record page.

`guild` is `null` when the client reports player guild index zero. Otherwise it
contains the numeric guild index, player/guild ranks, features, rating,
faction/points, qualifier points, bounded non-null roster count, and seven
numeric cape fields. The guild record must match both the context key and
index before publication. The key itself is used only for validation and is
never exposed. Friend aliases, character names, UUIDs, guild names/tags,
member names, announcements, history, chat, and every social write action are
excluded. This is sensitive account-derived state and remains token-gated.

The token is a session capability, not a long-lived API key. Do not persist or
publish it. The API has no remote listener, WebSocket transport, account
identity, chat, encoded game text, or action surface.

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
- player buff/effect counts;
- map-agent totals;
- quest and mission-objective counts;
- inventory, storage, and gold totals; and
- friend presence and numeric guild summary; and
- profile-local build and team library.

Press **⌘⇧O** to toggle layout editing. The hotkey engine requires an exact
modifier chord, ignores text controls and editable content, and invokes only
local UI callbacks. It never synthesizes game input.

The build library treats template codes as opaque strings. It supports up to
500 entries and 12 members per team, with validated import/export JSON. It does
not apply builds to the game because no write operation is certified.
