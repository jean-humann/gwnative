# Packaging and release

This guide is for maintainers. Packaging is safe to run locally; publishing
changes public GitHub state and the update feed seen by installed applications.

## Pipeline

| Stage | Command | Output |
| --- | --- | --- |
| Bundle | `scripts/bundle` | `dist/Guild Wars.app` |
| Release package | `scripts/release` | notarized `.dmg`, updater `.zip`, and dSYM |
| Publish | `scripts/publish vX.Y.Z` | GitHub release and signed `appcast.xml` |

CI uses the same `scripts/release` and `scripts/publish` files. The workflow
creates their Keychain environment; it does not reimplement the release.

## Build a local bundle

```sh
scripts/bundle
```

The script:

1. builds the release profile;
2. creates `dist/Guild Wars.app`;
3. substitutes package and monotonic build versions into `Info.plist`;
4. copies the web shell, excluding live client artifacts and tests;
5. installs GPL, provenance, proprietary-material, and Sparkle notices;
6. rejects any ArenaNet client or game artifact in the finished payload;
7. installs and thins Sparkle when a public update key is configured;
8. copies the dSYM beside the app;
9. signs nested code from the inside out when an identity is available; and
10. verifies signed bundles.

Without an identity, the script emits a warning and leaves a runnable unsigned
local bundle. Release packaging does not permit that fallback.

The bundle exists primarily to declare the game category required for macOS
Game Mode. It also supplies a stable bundle identifier, Retina metadata, icon,
application menu identity, and the Sparkle framework.

The packaged app never patches `Contents/Resources/web` in place. That would
invalidate its signature. The bundle contains a shell seed that is copied to
`~/Library/Application Support/gwnative/web`; live client artifacts are fetched
there.

`scripts/check-distribution` verifies the repository declarations and scans the
finished bundle before signing. It rejects the official JSPI and Asyncify glue
and WebAssembly modules, `Gw.snapshot`, `version.json`, and `*.dat` game data.
The tagged GitHub release supplies the Corresponding Source through its source
archives; `scripts/publish` always creates the release from that exact public
tag.

Signed local bundles use the hardened runtime plus
`packaging/debug.entitlements` (`get-task-allow`) and no trusted timestamp.
Published bundles use the hardened runtime, a trusted timestamp, and no debug
entitlements.

## Configure Sparkle signing

Sparkle verifies every update archive against a public EdDSA key embedded in
the application.

Generate the key once:

```sh
packaging/sparkle/generate_keys
```

Put the printed public key value in:

```text
packaging/sparkle/public-key.txt
```

The public half is committed. The private half stays in the login Keychain.
Export a CI copy without putting it in shell history:

```sh
packaging/sparkle/generate_keys -x private-key.txt
gh secret set SPARKLE_PRIVATE_KEY < private-key.txt
rm private-key.txt
```

Back up the private key securely. It cannot be rotated transparently: every
installed copy contains the existing public half and will reject archives
signed by a replacement. `scripts/appcast` verifies a newly produced signature
against the committed public key before writing the feed.

That verification requires OpenSSL 3 or newer. Install `openssl@3` with
Homebrew, or set `OPENSSL` to a compatible executable. Feed generation fails
closed when no compatible verifier is available.

The framework itself is vendored and pinned. Follow
[`packaging/sparkle/README.md`](../packaging/sparkle/README.md) to update it,
including the upstream checksum and licence checks.

## Configure Apple signing and notarization

The release identity must be a **Developer ID Application** certificate.
`scripts/sign-identity` selects one automatically or accepts
`GWNATIVE_SIGN_IDENTITY`.

Store notarization credentials once:

```sh
xcrun notarytool store-credentials gwnative \
    --key <AuthKey_XXXX.p8> \
    --key-id <key-id> \
    --issuer <issuer-uuid>
```

Use an App Store Connect API key with the Developer ID role. Set
`GWNATIVE_NOTARY_PROFILE` when the profile is not named `gwnative`.

No certificate, private key, Apple ID, password, or notarization credential
belongs in this repository. `packaging/certs` contains only public Developer ID
intermediate certificates used to complete the trust chain in a fresh CI
Keychain.

## Build release artifacts

Preconditions:

- `Cargo.toml` contains the intended version;
- the working tree is clean;
- a Developer ID Application identity is available;
- the notarytool profile works; and
- the committed Sparkle public key matches the private signing key.

Run:

```sh
scripts/release
```

The script performs cheap preflight checks before the LTO build, then:

1. builds the same bundle path used locally;
2. verifies the hardened-runtime signature and trusted timestamp;
3. zips the app for notarization;
4. waits for an accepted notarization result;
5. staples and assesses the application;
6. rebuilds the updater zip from the stapled app;
7. creates a drag-to-Applications disk image;
8. signs, notarizes, staples, and validates the disk image; and
9. leaves `dist/gwnative-<version>.dmg` and `.zip`.

The zip is rebuilt after the application is stapled. The pre-notarization
upload cannot carry a ticket that did not exist when it was created.

## Publish locally

After `scripts/release` succeeds:

```sh
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
git tag -s "v$version" -m "Guild Wars $version"
git push origin "v$version"
scripts/publish "v$version"
```

`scripts/publish` verifies that the tag matches `Cargo.toml`, then:

1. creates or reuses a draft GitHub release;
2. uploads the DMG and ZIP;
3. renders the release notes through GitHub's Markdown endpoint;
4. signs the ZIP and builds a one-item appcast;
5. uploads `appcast.xml`; and
6. makes the release visible and marks it latest.

The draft is a safety boundary. Installed applications resolve
`releases/latest/download/appcast.xml`; publishing before the feed exists would
make the current update URL return 404.

## Release through GitHub Actions

Pushing `v*` triggers `.github/workflows/release.yml`. The job requires approval
through the `release` environment. Repository settings should enforce:

- required reviewers; and
- a deployment branch/tag policy restricted to release tags.

Confirm the protection rules instead of assuming the workflow line provides
them:

```sh
gh api repos/jean-humann/gwnative/environments/release \
  --jq '.protection_rules[].type'
```

The secrets are:

| Secret | Contents |
| --- | --- |
| `APPLE_DEVELOPER_ID_APPLICATION_P12` | Base64-encoded Developer ID Application `.p12` |
| `APPLE_DEVELOPER_ID_PASSWORD` | Export password for the `.p12` |
| `APPLE_NOTARY_KEY_P8` | Base64-encoded App Store Connect API key |
| `APPLE_NOTARY_KEY_ID` | API key ID |
| `APPLE_NOTARY_KEY_ISSUER` | API issuer UUID |
| `SPARKLE_PRIVATE_KEY` | EdDSA private key for update archives |

The separate `certificate-publishing` environment is restricted to `main`,
requires review, and contains only `CERTIFICATE_PRIVATE_KEY`, a base64-encoded
PKCS#8 Ed25519 key. Certificate signing runs in a fresh job that neither checks
out nor executes repository code; the following no-secret job opens the PR.

The workflow creates a temporary Keychain, imports the identity and public
intermediate chain, grants access only to signing tools, stores the notarization
profile, and deletes the Keychain in an `always()` step. It intentionally runs
no third-party action in the secret-bearing job except GitHub's checkout action.

## Dry run

After changing signing, packaging, Sparkle, certificates, or the release
workflow:

```sh
gh workflow run release.yml -f dry_run=true
```

The run still requires approval because it reaches the secrets. It builds,
signs, submits to Apple, staples, validates, and verifies update signing, but
does not publish. Its job summary lists the artifacts a real run would use.

## Update paths

A packaged build with Sparkle reads the signed appcast, presents release notes,
verifies the archive, and can install it on quit.

A bare development build or a bundle without a configured public key uses
`src/release.rs`. It queries the repository's release list anonymously and
opens the release page for a newer compatible tag. It cannot install.

Automatic checks and automatic installation are separate, opt-in settings.
Sparkle's user defaults are authoritative when the framework is available; the
JSON settings profile mirrors them for the web settings panel.

## Failure handling

- A failed `scripts/release` publishes nothing.
- A failed feed/signature step leaves the GitHub release as a draft.
- `scripts/appcast` refuses a key that does not match the committed public key.
- Re-running `scripts/publish` replaces assets on an existing draft.
- `scripts/publish` refuses to replace assets after a release is public.
- Do not rotate `SPARKLE_PRIVATE_KEY` as routine incident recovery. Restore the
  backed-up key or plan a manual reinstall transition.
- Never make a release from a dirty tree; the artifact must correspond to a
  commit.

## CI

`.github/workflows/ci.yml` runs on Apple Silicon `macos-26`:

- formatting;
- Clippy with warnings denied;
- Rust and JavaScript tests;
- release-profile compilation; and
- unsigned bundle assembly checks.

The deployment floor is independently pinned to 15.2 in
`.cargo/config.toml` and `packaging/Info.plist`. Keep both values aligned.
