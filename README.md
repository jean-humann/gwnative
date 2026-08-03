<p align="center">
  <img src="docs/assets/app-icon.png" width="128" alt="gwnative application icon">
</p>

<h1 align="center">gwnative</h1>

<p align="center">
  A lightweight native macOS host for Guild Wars Reforged on Apple Silicon.
</p>

<p align="center">
  <a href="https://github.com/jean-humann/gwnative/releases/latest"><img src="https://img.shields.io/badge/Download_for_macOS-Apple_Silicon-000000?style=for-the-badge&amp;logo=apple&amp;logoColor=white" alt="Download gwnative for macOS"></a>
</p>

<p align="center">
  <a href="https://github.com/jean-humann/gwnative/actions/workflows/ci.yml"><img src="https://github.com/jean-humann/gwnative/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <a href="https://github.com/jean-humann/gwnative/releases/latest"><img src="https://img.shields.io/github/v/release/jean-humann/gwnative" alt="Latest release"></a>
</p>

<p align="center"><strong>Apple Silicon · macOS 15.2 or newer</strong></p>

gwnative runs the official Guild Wars Reforged WebAssembly client in one native
Rust application. AppKit and WKWebView provide the platform directly, without
an embedded browser distribution, Windows, Wine, or a virtual machine.

<p align="center">
  <img src="docs/assets/game-login.jpg" width="1000" alt="Guild Wars Reforged running in a native gwnative window">
</p>

gwnative is an independent interoperability project. It is not affiliated with,
endorsed, sponsored, or approved by ArenaNet or NCSOFT. See the
[legal notice](NOTICE.md) for ownership, trademark, and game-material details.

gwnative was made possible by
[gw_in_browser](https://github.com/gwdevhub/gw_in_browser), created by
[Marc Henderkes](https://github.com/henderkes). The renderer harness traces
back to that work. The native macOS direction was later inspired by
[gwonmac](https://github.com/Mat4m0/gwonmac); the host itself is an independent
Rust implementation.

## Requirements

- Apple Silicon
- macOS 15.2 or newer

Both are hard requirements. The application ships only an `arm64` binary. The
app tests JSPI inside its own WKWebView and falls back to ArenaNet's official
Asyncify client when the system WebKit does not support it. Installing Safari
Technology Preview does not replace the system WebKit used by WKWebView.

The Asyncify path is verified end to end on macOS 26.6, and the JSPI path on
macOS 27 beta. macOS 15.2–15.x meet the known WebKit requirements but have not
yet had the same end-to-end run; 15.2 is the deployment floor, not a claim of
verified gameplay on every later point release.

## What works

gwnative is playable. It:

- fetches and verifies the current official client artifacts;
- streams the 4.2 GB game image on demand or downloads it in full;
- bridges the client's HTTP and game sockets through a restricted native
  network layer;
- stores the saved login in the macOS Keychain;
- supports Retina rendering, high-refresh displays, macOS keyboard layouts,
  double-click translation, pointer lock, and native window state;
- repairs build-template operations on client builds that have been explicitly
  certified;
- keeps unknown or failed compatibility transforms out of the gameplay path by
  running ArenaNet's original JSPI or Asyncify module;
- offers an optional native game cursor and target-distance readout;
- records structured diagnostics and can produce a redacted problem report;
- rolls back a newly downloaded client that never reaches a first frame; and
- checks for updates, with signed automatic installation in packaged builds.

## Install

1. [Download the latest disk image](https://github.com/jean-humann/gwnative/releases/latest).
2. Open it and drag **Guild Wars** to **Applications**.
3. Open **Guild Wars** and choose how to store the game image.

The first launch downloads the small client artifacts and asks how to handle the
game image:

- **Quick Start** streams areas when first needed and keeps them.
- **Full Game** downloads and verifies missing chunks in the background. You can
  wait for it to finish or start playing while it continues.

<details>
<summary>See the first-launch storage choice</summary>

<p align="center">
  <img src="docs/assets/first-run.png" width="800" alt="gwnative offering Quick Start and Full Game on first launch">
</p>

</details>

See the [user guide](docs/user-guide.md) for settings, storage, updates, and
troubleshooting.

## Build from source

The pinned toolchain declares both required Rust targets. On an Apple Silicon
Mac with Xcode Command Line Tools and `rustup` installed:

```sh
cargo run
```

The first run downloads missing client artifacts. A stable signing identity is
recommended because macOS associates Keychain access with the executable's code
signature; development runs still work without one.

Useful commands:

| Command | Purpose |
| --- | --- |
| `cargo run` | Open the game window |
| `cargo run -- sync` | Refresh the client artifacts and exit |
| `cargo run -- serve` | Run the loopback origin without a window |
| `cargo test` | Run the Rust and dependency-free JavaScript tests |
| `scripts/bundle` | Build `dist/Guild Wars.app` |

The complete setup, environment variables, quality checks, and repository map
are in the [development guide](docs/development.md).

## Documentation

| Document | Audience and scope |
| --- | --- |
| [User guide](docs/user-guide.md) | Installation, settings, data, updates, and recovery |
| [Architecture](docs/architecture.md) | Trust boundaries, boot flow, storage, networking, WebAssembly transforms, and module map |
| [Game API capabilities](docs/game-api-capabilities.md) | Per-domain certification, demand scheduling, API boundaries, and merge train |
| [Development](docs/development.md) | Toolchain, commands, tests, debugging, and environment variables |
| [Client compatibility mechanism](docs/client-compatibility.md) | Update transaction, runtime fallback, certification trust, graphs, and audit checklist |
| [Client certification](docs/certification.md) | Dual-runtime certificates, automation, signing, and rollback |
| [Performance](docs/performance.md) | Reproducible measurements, baselines, and measured design decisions |
| [Release guide](docs/releasing.md) | Bundling, signing, notarization, Sparkle, CI, and publication |
| [Contributing](CONTRIBUTING.md) | Change workflow and documentation expectations |
| [Legal notice](NOTICE.md) | Unofficial-project disclosure, game-material ownership, and trademarks |

The application also ships an offline user guide under **Help → Guild Wars
Guide**. It describes the exact build currently running; this repository's user
guide adds developer-facing paths and recovery details.

## Licence

The gwnative source code is distributed under GPL-2.0-or-later. That licence
does not cover Guild Wars names, client code, game data, artwork, audio, logos,
or other proprietary material. See [LICENSE](LICENSE) and the
[legal notice](NOTICE.md).
