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

The descriptor is `profile.json`, format version 1. Its origin port is allocated
under a catalog lock and probed past ports already assigned to other profiles;
duplicate or out-of-range assignments are refused. Its generated colour and
display name are metadata; changing the `id` or `originPort` by hand does not
migrate the associated Keychain item or WebKit data.

In a source checkout, the default writable web root is the repository's `web/`
directory for every profile. Use `--dir` when development sessions also need
isolated client artifacts. Packaged builds use the per-profile support
directory automatically.

## Isolation map

| Resource | Default profile | Named profile | Shared |
| --- | --- | --- | --- |
| Settings, window, diagnostics | Base support directory | `profiles/<id>/` | No |
| Writable web root and client artifacts | Base support directory | `profiles/<id>/` | No |
| Derived certified modules | Base support directory | `profiles/<id>/` | No |
| Saved login | Keychain account `login` | Keychain account `login:<id>` | No |
| IndexedDB and local storage | Origin on port `38112` | Origin on assigned persisted port | No |
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

Shared chunks are unaffected.
