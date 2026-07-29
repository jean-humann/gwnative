# Development

## Prerequisites

- Apple Silicon Mac
- macOS 15.2 or newer
- Xcode Command Line Tools
- `rustup`
- Node.js to execute the web test suite locally

Node is not a runtime dependency. When it is absent, `cargo test` runs the Rust
suite and prints that the web tests were skipped.

`rust-toolchain.toml` pins Rust `1.97.1`, `rustfmt`, `clippy`,
`aarch64-apple-darwin`, and `wasm32-unknown-unknown`. Cargo installs the pinned
toolchain and targets through rustup on first use. `Cargo.toml` declares Rust
1.90 as the minimum supported compiler for the crate; the repository pin is the
reproducible development and CI version.

## First run

```sh
cargo run
```

In a source checkout, the writable web root is `web/`. Missing live client
artifacts are downloaded there before the window opens.

`build.rs` compiles `src/companion-kernel/lib.rs` directly for
`wasm32-unknown-unknown` and embeds the result. It is intentionally not a second
Cargo crate: the host transform and companion ABI are built together.

## Commands

The binary accepts one command plus native and Guild Wars-compatible options:

```text
gwnative [command] [options]

Commands:
  run       open the window (default)
  sync      refresh client artifacts and exit
  repair    verify and refill the full game image
  serve     run the origin without a window
  profiles  list launch profiles
  mods      validate discovered mod bundles
```

See the [command-line reference](command-line.md) for native options and the
complete official-switch compatibility table. Unknown arguments fail instead
of being ignored. With Cargo, pass application arguments after `--`:

```sh
cargo run -- sync
cargo run -- serve
cargo run -- --profile test --new-instance
```

`serve` prints the bound address and session token on one line, then parks:

```text
127.0.0.1:38112 <token>
```

Use it to exercise the snapshot and gated host routes without a window. Set
`GWNATIVE_PRINT_TOKEN=1` when a windowed diagnostic tool also needs the token.

## Code signing and the Keychain

`.cargo/config.toml` configures `scripts/signed-run` as the runner for the
native target. Before executing the application, it signs `gwnative` with:

1. the identity named by `GWNATIVE_SIGN_IDENTITY`;
2. the first Developer ID Application identity;
3. the first Apple Development identity; or
4. another available code-signing identity.

With no identity, the binary still runs. Its ad-hoc signature changes across
builds, so the saved login may not survive a rebuild.

The stable signing identifier is `com.gwnative.app`. Keep it aligned between
`scripts/signed-run` and `packaging/Info.plist`; changing it creates a new
Keychain identity. gwnative suppresses the system's macOS-password prompt when
an identity does not match and lets the game ask for its account credentials
instead.

`cargo build` alone does not pass through the runner. Use `cargo run` or invoke
`scripts/signed-run target/debug/gwnative` when testing saved credentials.

## Quality checks

CI runs:

```sh
scripts/check-docs
scripts/check-scripts
cargo deny check
msrv="$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml)"
rustup toolchain install "$msrv" --profile minimal \
    --target aarch64-apple-darwin,wasm32-unknown-unknown
cargo "+$msrv" check --locked --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
scripts/bundle
```

The script check parses every repository-owned shell and Python maintenance
script without executing release or packaging effects. The dependency policy
requires [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) and checks
the shipped target for advisories, duplicate or wildcard dependencies,
licences, and nonstandard sources. The separate `cargo check` uses the
`rust-version` declared in `Cargo.toml`, so changing the compiler floor changes
the check instead of leaving a second version constant behind. The Rust suite
contains unit and socket-level integration tests. One ignored test reaches the
GitHub API and is excluded from the default run. The `tests/web.rs` integration
test invokes:

```sh
node --test "*.test.js"
```

from `web/`. The JavaScript suite uses only Node's built-in runner and has no
package manifest or dependency installation.

Useful focused commands:

```sh
cargo test settings::
cargo test server::
node --test web/settings-panel.test.js
```

Run `scripts/bundle` after changes to `build.rs`, `packaging/`, the web shell
copy rules, release profile, or embedded resources.

## Native end-to-end smoke test

`scripts/e2e` exercises the real signed WKWebView app and the installed Guild
Wars client. The runner:

1. builds and signs gwnative with the normal development identity;
2. lets the app—not the runner—read the default profile's saved login from
   Keychain;
3. exercises Settings, the User Guide, Companion Tools, widgets, layout mode,
   and the build library by semantic DOM name;
4. verifies the token boundary and both versioned API descriptions;
5. waits for launch milestones over a bounded long-poll channel;
6. sends only the finite **activate** and **move-forward** test actions; and
7. when the installed client has a certified state layout, confirms movement
   through a newer game-state revision.

```sh
scripts/e2e
scripts/e2e --no-gameplay
```

The session token is captured and redacted. Credential values never leave the
application: the event channel reports only whether both fields were offered.
The control plane exists only under `GWNATIVE_E2E`, has no arbitrary JavaScript,
coordinates, text-entry or credential action, and sleeps between events. It
does not use screenshots, OCR, Accessibility scripting, or focus polling.

The page restores the two profile-local `localStorage` values it touches
byte-for-byte before the runner stops the process. The test intentionally uses
the installed client root, because a source checkout can hold an older ignored
client module than the one the packaged app last downloaded. A newly patched,
uncertified ArenaNet build can prove the host and input path but cannot honestly
prove character position until its companion layout has been certified.

## Environment variables

All runtime overrides are optional.

| Variable | Purpose |
| --- | --- |
| `GWNATIVE_ACCESS_KEY` | Override the public patch-service client key |
| `GWNATIVE_PATCH_ROOT` | Override the patch service base URL |
| `GWNATIVE_WEB_ROOT` | Override the web shell and client-artifact directory |
| `GWNATIVE_PORT` | Override loopback port `38112` |
| `GWNATIVE_PRINT_TOKEN` | Print the injected host-route token to stderr |
| `GWNATIVE_E2E` | Enable the tokened, finite native test control plane used by `scripts/e2e` |
| `GWNATIVE_TRACE_HTTP` | Log each loopback HTTP request |
| `GWNATIVE_TRACE_SOCKETS` | Log socket frame sizes; value `hex` also logs at most the first 16 bytes |
| `GWNATIVE_SIGN_IDENTITY` | Select a development or release signing identity |
| `GWNATIVE_NOTARY_PROFILE` | Select the notarytool Keychain profile for `scripts/release` |
| `SPARKLE_PRIVATE_KEY` | Pass the update-signing key to `scripts/appcast` or `scripts/publish` |

`GWNATIVE_RELEASE` is an internal switch set by `scripts/release`; it selects a
timestamped hardened-runtime signature without debug entitlements. Do not use it
as a normal development mode.

Socket hex traces are deliberately capped because early game packets can carry
credentials. Prefer size-only tracing unless the packet header is required.

## Data and clean profiles

Development data is under:

```text
~/Library/Application Support/gwnative
~/Library/WebKit/gwnative
```

A packaged application uses the same Application Support directory but page
data under `~/Library/WebKit/com.gwnative.app`.

Most host paths derive from `HOME`, which makes an isolated profile possible
without deleting real data:

```sh
scratch_home="$(mktemp -d)"
HOME="$scratch_home" cargo run
```

The login Keychain is per macOS user and does not follow `HOME`. A clean host
profile can therefore still see a saved credential if the signing identity
matches. The benchmark reports this limitation explicitly.

Do not delete the real Application Support directory to create a test case.
Use a temporary `HOME`, or move a narrowly identified test directory aside when
the scenario specifically requires the packaged WebKit root.

## Debugging

### Logs

Terminal output combines native notes and page console lines. Persistent JSONL
is written to:

```text
~/Library/Application Support/gwnative/diagnostics/gwnative.jsonl
```

Named profiles use `profiles/<id>/diagnostics/gwnative.jsonl` beneath that
Application Support root.

Set `GWNATIVE_TRACE_HTTP=1` for loopback routing and
`GWNATIVE_TRACE_SOCKETS=1` for bridge frame sizes. Both are intentionally off
because synchronous terminal output can affect boot timing.

### Host routes

`src/server/api.rs` owns token-gated capabilities such as settings,
credentials, diagnostics, download control, residency, sockets, boot proof,
clear, quit, and relaunch. `src/server/content.rs` owns static content, the
derived module, snapshot byte ranges, the embedded companion module, and the
closed HTTP proxy.

When testing `serve`, send the printed token as:

```text
X-Gwnative-Token: <token>
```

Never put a real saved credential in a test transcript.

### WebKit processes

The host process does not contain the client's JavaScript heap, WebAssembly
linear memory, or GPU resources. WebKit runs those in launchd-managed service
processes. Use the in-page metrics and whole-process-group measurement approach
described in [Performance](performance.md) instead of reading only the host's
RSS.

Local bundles carry `packaging/debug.entitlements`, whose only capability is
`get-task-allow`, so Instruments, `sample`, `leaks`, and lldb can attach while
the hardened runtime remains enabled. Published bundles omit it.

## Repository map

| Path | Role |
| --- | --- |
| `src/` | Native host |
| `src/chunks/` | Chunk cache, coalescing, readahead, prefetch, and verification |
| `src/http/`, `src/server/` | Loopback HTTP parsing, policy, routing, and streaming |
| `src/wasm/` | Certified WebAssembly codecs and transforms |
| `src/companion-kernel/` | Embedded read-only companion module |
| `src/mods.rs`, `src/game_api.rs` | Validated mod catalog and versioned game-state boundary |
| `web/` | Harness, player UI, overlays, mod runtime, tools, tests, and live client artifacts |
| `tests/web.rs` | Cargo bridge to Node's test runner |
| `packaging/` | Bundle metadata, icon, Sparkle, certificates, and entitlements |
| `scripts/` | Benchmark, bundle, signing, notarization, feed, and publication tools |
| `.github/workflows/` | Read-only CI and approval-gated release automation |

The fuller component mapping and runtime contracts are in
[Architecture](architecture.md).

## Documentation

Keep the root README as the entry point. Put detailed material in the
audience-specific guide and update `web/guide.js` when player-visible behaviour
changes.

Check local Markdown links with:

```sh
scripts/check-docs
```

The checker is dependency-free, validates local targets and Markdown heading
anchors, and runs in CI. It deliberately does not make network requests; remote
link availability is not a deterministic pull-request check.

Measurements belong in `docs/performance.md` with date, hardware, conditions,
and limitations. Procedures that can change external state belong in
`docs/releasing.md`, not in the README.
