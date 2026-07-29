# gwnative

[![CI](https://github.com/jean-humann/gwnative/actions/workflows/ci.yml/badge.svg)](https://github.com/jean-humann/gwnative/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/jean-humann/gwnative)](https://github.com/jean-humann/gwnative/releases/latest)

An unofficial native macOS host for the Guild Wars Reforged WebAssembly client.
One Rust binary drives AppKit and WKWebView directly, without an embedded
browser distribution.

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

Both are hard requirements. The application ships only an `arm64` binary, and
the client depends on WebKit's JavaScript Promise Integration support introduced
with Safari 18.2 on macOS 15.2.

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
- offers an optional native game cursor and target-distance readout;
- records structured diagnostics and can produce a redacted problem report;
- rolls back a newly downloaded client that never reaches a first frame; and
- checks for updates, with signed automatic installation in packaged builds.

## Install

Download the disk image from the
[latest release](https://github.com/jean-humann/gwnative/releases/latest), open
it, and drag **Guild Wars** to **Applications**.

The first launch downloads the small client artifacts and asks how to handle the
game image:

- **Quick Start** streams areas when first needed and keeps them.
- **Full Game** downloads and verifies missing chunks in the background. You can
  wait for it to finish or start playing while it continues.

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
| [Development](docs/development.md) | Toolchain, commands, tests, debugging, and environment variables |
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
