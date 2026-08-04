# Profiles

A gwnative profile is an isolation boundary, not only a launcher label.
Game settings, window state, writable client artifacts, derived modules,
diagnostics, Keychain credentials, WebKit state, overlays, and the build library
all follow the selected profile. Application update preferences and immutable
content-addressed game chunks remain shared so multiple profiles do not each
consume another 4.2 GB. Cache cleanup retains the union named by every valid
cached profile active and rollback manifest, even when patch-service roots
differ, allowing profiles on different installed client generations to keep
launching independently.

## Create and use a profile

A safe profile ID contains 1–64 ASCII letters, numbers, dots, underscores, or
hyphens. Selecting an unknown ID creates its descriptor atomically:

```sh
gwnative --profile iron
gwnative profiles
```

The default profile remains backward-compatible with releases that predate
profiles:

- support directory: `~/Library/Application Support/gwnative`
- Keychain account: `login`
- loopback port: `38112`

A named profile uses:

- support directory:
  `~/Library/Application Support/gwnative/profiles/<id>`
- Keychain account: `login:<id>`
- an assigned port in `38113` through `39112`, persisted in the descriptor
- a persistent WebKit data-store UUID, also persisted in the descriptor

The descriptor is `profile.json`, format version 1. Its origin port is allocated
under a catalog lock and probed past ports already assigned to other profiles;
duplicate or out-of-range assignments are refused. Its generated colour and
display name are metadata; changing the `id` or `originPort` by hand does not
migrate the associated Keychain item or WebKit data.

The separate WebKit store is required even with separate ports: IndexedDB and
local storage are origin-keyed, but HTTP cookies ignore ports. The default
profile keeps WebKit's historical default store so existing browser state
continues to load. Named profiles use the public persistent identified-store API
available below this app's deployment floor. Removing a descriptor and later
creating a different profile cannot inherit the deleted profile's browser state,
because a newly created profile receives a new random store identifier.

Every profile installs the reviewed shell allowlist as an immutable,
inventory-verified revision after its lock is held. Official artifacts, tests,
chunks, and player data never enter it. `--dir` shares only a custom
official-client root, never the selected shell.

## Isolation map

| Resource | Default profile | Named profile | Shared |
| --- | --- | --- | --- |
| Game settings, window, diagnostics | Base support directory | `profiles/<id>/` | No |
| Application update preferences and cadence | Base `updates.json` | Base `updates.json` | Yes |
| Official client artifacts and reviewed shell | Base support directory | `profiles/<id>/` | No |
| Derived certified modules | Base support directory | `profiles/<id>/` | No |
| Saved login | Keychain account `login` | Keychain account `login:<id>` | No |
| Cookies, IndexedDB and local storage | Default WebKit store, origin on port `38112` | Persistent identified WebKit store and assigned origin | No |
| Game-image chunks | `chunks/` | `chunks/` | Yes |

`--host-port` deliberately overrides the persisted assignment. It is a
diagnostic escape hatch, not a stable profile origin: changing it makes that
session see a different origin inside the selected profile's WebKit store. It
does not cross into another profile's identified data store, even when the port
matches that profile's assignment.

## Concurrent instances

Ordinary launches acquire the global primary-instance lock and their
profile-specific lock. To run two accounts at once, give the second process a
different non-default profile and explicitly permit it to bypass only the
global lock:

```sh
gwnative --profile main
gwnative --profile second --new-instance
```

`--new-instance` without a non-default `--profile` is refused. The
profile-specific lock is never bypassed, so two live processes cannot write the
same settings, IndexedDB origin, window state, and credential identity. Each
process also keeps a shared lease on the common game-chunk cache and
active-manifest catalog. Migration, clear, sweep, and union-aware pruning run
only while the first process owns the exclusive maintenance window. It then
downgrades before network or client installation work so a second profile does
not wait for a patch. A pending clear is deferred until every profile releases
the lease; it is never performed under another process's open files or chunk
writes.

## Removal

There is intentionally no broad destructive profile command. To remove one:

1. Quit every process using it.
2. Move the exact `profiles/<id>` directory to Trash.
3. Remove the corresponding `login:<id>` item from Keychain Access if the saved
   login should also be forgotten.

Shared chunks are unaffected. WebKit's identified data store is intentionally
not deleted by this filesystem operation: WebKit owns it outside the profile
directory, and removing browser data is a separate destructive action for which
this release exposes no broad command. The UUID in a restored descriptor finds
the same store again. Creating a profile after deleting its descriptor always
generates a new UUID—even when the same profile ID is reused—so the orphaned
store is never attached to the replacement profile.

Application update preferences are also intentionally unaffected. Sparkle and
the installed application bundle are global to the app, so automatic-check and
automatic-install intent plus the last-check time live in the small global
`updates.json` file and appear consistently in every profile. On upgrade it is
seeded once from the default profile's existing preferences. Rendering, input,
diagnostics, game-data strategy, compatibility acknowledgements, and
enhancements remain profile-local.
