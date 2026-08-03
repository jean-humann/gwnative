# Profiles

A gwnative profile is an isolation boundary, not only a launcher label.
Settings, window state, writable client artifacts, derived modules, diagnostics,
Keychain credentials, WebKit origin, overlays, and the build library all follow
the selected profile. Immutable content-addressed game chunks remain shared so
multiple profiles do not each consume another full game image.

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

- support directory: `~/Library/Application Support/gwnative/profiles/<id>`
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

In a source checkout, the default profile runs directly from the repository's
`web/` directory for live development. Named profiles are automatically seeded
into their own support directory, matching packaged builds. `--dir` deliberately
overrides that boundary; pointing two profiles at the same directory makes
their writable client artifacts shared.

## Isolation map

| Resource | Default profile | Named profile | Shared |
| --- | --- | --- | --- |
| Settings, window, diagnostics | Base support directory | `profiles/<id>/` | No |
| Writable web root and client artifacts | Base support directory | `profiles/<id>/` | No |
| Derived certified modules | Base support directory | `profiles/<id>/` | No |
| Saved login | Keychain account `login` | Keychain account `login:<id>` | No |
| Cookies, IndexedDB and local storage | Default WebKit store, origin on port `38112` | Persistent identified WebKit store and assigned origin | No |
| Game-image chunks | `chunks/` | `chunks/` | Yes |

`--host-port` deliberately overrides the persisted assignment. It is a
diagnostic escape hatch, not a profile-isolation mechanism: selecting a port
belonging to another profile makes that session use the other origin's
IndexedDB and local-storage data.

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
same settings, IndexedDB origin, window state, and credential identity.

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
