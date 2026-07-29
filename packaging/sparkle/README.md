# Sparkle 2.9.4

The updater framework, committed rather than fetched, for the same reason the
certificates next door are: a release build should not depend on somebody
else's web server being up, and a binary downloaded during the build is one
nobody has looked at. What is here is what ships, and it changes only in a
commit.

| File | What it is | Size |
| --- | --- | --- |
| `Sparkle.framework` | The framework, exactly as published | 3.0 MB |
| `sign_update` | Signs a release with the EdDSA key. Build machine only, never shipped | 1.3 MB |
| `generate_keys` | Makes that key, once ever. Build machine only, never shipped | 1.3 MB |
| `LICENSE` | Sparkle's MIT licence | 8 KB |

`generate_keys` is here rather than left to whoever needs it because of what it
is used for. The alternative is that the one irreversible step in this project —
creating the key every future update will be verified against — begins with
"download a binary from the internet and run it". Committed, it is the same
provenance as everything else in this directory and the ceremony needs no
network at all.

Version `2.9.4`, build `2059`, from
<https://github.com/sparkle-project/Sparkle/releases/tag/2.9.4>. To check this
directory against upstream, or to replace it:

```sh
curl -fL -O https://github.com/sparkle-project/Sparkle/releases/download/2.9.4/Sparkle-2.9.4.tar.xz
shasum -a 256 Sparkle-2.9.4.tar.xz
# ce89daf967db1e1893ed3ebd67575ed82d3902563e3191ca92aaec9164fbdef9

mkdir upstream && tar -xJf Sparkle-2.9.4.tar.xz -C upstream
diff -r --no-dereference upstream/Sparkle.framework packaging/sparkle/Sparkle.framework
diff upstream/bin/sign_update packaging/sparkle/sign_update
diff upstream/bin/generate_keys packaging/sparkle/generate_keys
diff upstream/LICENSE packaging/sparkle/LICENSE
```

All four are silent today. The tarball has no top-level directory, hence the
`-C`.

After replacing this directory, run `git status` and count. The framework is 101
files, and one of them is `Versions/B/Updater.app` — a bundle nested inside a
framework, which the repository's `*.app` rule would otherwise swallow whole.
`.gitignore` carries a negation for exactly this path. Without it everything
still builds, still signs and still notarizes; the updater simply has nothing to
draw its window with, and only on a fresh clone.

## What `scripts/bundle` does to it

The copy here is untouched so that the checksum above means something. The
copy that ships is not the copy that is here:

- **Thinned to `arm64`.** Upstream is universal; the app is not, and half of
  those bytes could never run.
- **`XPCServices` removed.** Sparkle uses them to reach the network and the
  filesystem from inside an App Sandbox. This app is not sandboxed, so they are
  never loaded — and each one is a nested bundle that would otherwise have to
  be signed and notarised for nothing.
- **Re-signed** with the Developer ID, innermost first. Upstream ships it
  ad-hoc signed (`TeamIdentifier=not set`), which is fine for a framework you
  are about to sign yourself and fatal if you forget to: the app's own
  signature would not cover it and Gatekeeper would reject the bundle.

Together those take it from 3.0 MB to about 1.6 MB.

## The licence has to travel

`Versions/B/Resources/` holds thirty-six `.lproj` directories and an
`Info.plist`, and no licence text — upstream keeps `LICENSE` beside the
framework rather than inside it. MIT asks that the notice go with the
binary, so `scripts/bundle` copies this file into the app as
`Contents/Resources/Sparkle-LICENSE.txt`. Deleting that line would ship a
licence violation, quietly.

## Upgrading

Sparkle's public API is stable across 2.x, so an upgrade is usually the four
commands at the top with a new version number, plus this file. Two things to
look at before believing it:

- `CHANGELOG` in the tarball, for anything about the appcast format or
  `sign_update` — the release workflow generates one and runs the other.
- `codesign -dv Sparkle.framework` on the new copy, in case upstream starts
  signing releases with a team of their own. That would be good news, and it
  would still be re-signed here.
