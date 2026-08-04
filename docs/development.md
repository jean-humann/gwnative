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

The binary accepts one command:

```text
gwnative [command]

Commands:
  (none)         open the window
  sync           download the client and exit
  serve          run the origin without a window
  certify        print an unsigned artifact-family certificate candidate
```

It also supports `-h`/`--help` and `-V`/`--version`. Unknown arguments fail
instead of being ignored. With Cargo, pass application arguments after `--`:

```sh
cargo run -- sync
cargo run -- serve
```

Use `scripts/client-certify WEB_ROOT` for the guarded certification
workflow rather than invoking `certify` directly. See
[Client artifact certification](certification.md).

`serve` prints only the bound address, then parks:

```text
127.0.0.1:38112
```

Use it to exercise public content without a window. A tool that needs a gated
route must create an anonymous pipe, inherit the write descriptor into the
child, and set `GWNATIVE_CONTROL_FD` to that descriptor number. The child writes
newline-delimited `browser`, `game-reader`, and `launch-nonce` records to the
pipe and closes it; stdout/stderr and the environment never contain the values.
The reader capability cannot reach credentials, settings, process controls, or
state publication. See `scripts/benchmark` for the descriptor lifecycle and
`pass_fds` setup. There is deliberately no print-token escape.

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

## Environment variables

All runtime overrides are optional.

| Variable | Purpose |
| --- | --- |
| `GWNATIVE_ACCESS_KEY` | Override the public patch-service client key |
| `GWNATIVE_PATCH_ROOT` | Override the patch service base URL |
| `GWNATIVE_WEB_ROOT` | Override the official client-artifact directory; the reviewed shell stays separate |
| `GWNATIVE_PORT` | Override loopback port `38112` |
| `GWNATIVE_CONTROL_FD` | Inherited anonymous-pipe descriptor for explicitly requested capabilities; never a token value |
| `GWNATIVE_TRACE_HTTP` | Log each loopback HTTP request |
| `GWNATIVE_TRACE_SOCKETS` | Log socket direction, destination class, and frame sizes only |
| `GWNATIVE_FRAME_AUDIT` | Value `1` enables animation-callback, draw, suspension and logical-swap correlation |
| `GWNATIVE_PREFER_60_FPS` | Value `1` leaves WebKit's near-60-FPS preference at its default for a diagnostic comparison |
| `GWNATIVE_PRESERVE_DRAWING_BUFFER` | Value `1` preserves WebGL pixels after presentation for an activation-flash comparison; diagnostic only |
| `GWNATIVE_FRAME_ISOLATION` | Complete-frame presentation is enabled by default; value `0` keeps ArenaNet's official direct default-framebuffer path for recovery or comparison |
| `GWNATIVE_ACTIVATION_COVER` | The retained native frame is enabled by default across reactivation; value `0` disables it for recovery or comparison |
| `GWNATIVE_SIGN_IDENTITY` | Select a development or release signing identity |
| `GWNATIVE_NOTARY_PROFILE` | Select the notarytool Keychain profile for `scripts/release` |
| `SPARKLE_PRIVATE_KEY` | Pass the update-signing key to release scripts |
| `CERTIFICATE_PRIVATE_KEY` | Base64-encoded PKCS#8 Ed25519 key for protected certificate-feed signing |

`GWNATIVE_RELEASE` is an internal switch set by `scripts/release`; it selects a
timestamped hardened-runtime signature without debug entitlements. Do not use it
as a normal development mode.

Socket payload tracing is absent because early game packets can carry
credentials, chat, and other private data. Only direction, destination class,
and byte counts are available.
Detailed frame auditing wraps every WebGL framebuffer-write import (draw,
clear, or blit) and is for controlled reproduction only. See
[Rendering diagnostics](rendering-diagnostics.md).

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

Set `GWNATIVE_TRACE_HTTP=1` for loopback routing and
`GWNATIVE_TRACE_SOCKETS=1` for bridge frame sizes. Both are intentionally off
because synchronous terminal output can affect boot timing.

### Host routes

`src/server/api.rs` owns token-gated capabilities such as settings,
credentials, diagnostics, download control, residency, sockets, boot proof,
clear, quit, and relaunch. `src/server/content.rs` owns static content, the
derived module, snapshot byte ranges, the embedded companion module, and the
closed HTTP proxy.

When testing `serve`, read the capability explicitly requested through the
anonymous control pipe described above, then send it as:

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
| `web/` | Harness, player UI, tests, and live client artifacts |
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
