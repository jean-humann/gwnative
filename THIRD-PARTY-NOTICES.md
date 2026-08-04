# Third-party notices

This file records source incorporated into or adapted by gwnative and
third-party material distributed with the application. Unless a section says
otherwise, modifications made for gwnative are distributed under
GPL-3.0-only. The complete machine-readable path annotations are in
[`REUSE.toml`](REUSE.toml).

## GWoNmac

- Project: [Mat4m0/gwonmac](https://github.com/Mat4m0/gwonmac)
- Copyright: © 2026 Matthias Amon and GWoNmac contributors
- Licence: GPL-3.0-only
- Reviewed baselines:
  [`459c269e`](https://github.com/Mat4m0/gwonmac/commit/459c269e41f52b9aa995c56999f0a61a33e5def6),
  [`34cff799`](https://github.com/Mat4m0/gwonmac/commit/34cff799a02b8f2717eca73531cb5eb3bc737985), and
  [`5219446`](https://github.com/Mat4m0/gwonmac/commit/5219446f4f475551606ebacdae8da54df16507d7)

gwnative modifies and reorganises GWoNmac-derived companion, input,
enhancement, filesystem, platform, template, and WebAssembly transformation
code. The affected current paths are:

- `src/companion-kernel/lib.rs`
- `src/wasm/rewrite.rs`
- `web/companion-snapshot.js`
- `web/enhancement-cursor.js`
- `web/enhancement-readout.js`
- `web/enhancements.js`
- `web/filesystem.js`
- `web/input.js`
- `web/platform-capabilities.js`
- `web/template-save.js`

The implementations in this repository are modified versions, not verbatim
upstream releases.

## gw_in_browser

- Project: [shiburito/gw_in_browser](https://github.com/shiburito/gw_in_browser)
- Copyright: © 2026 Marc and gw_in_browser contributors
- Licence: GPL-3.0-only
- Reviewed baseline:
  [`e6b49991`](https://github.com/shiburito/gw_in_browser/commit/e6b499915d743556c34f601eba005f6f912ac1ac)

gwnative modifies and reorganises browser-host, graphics, image, and
WebAssembly codec code descended from this project. The affected current paths
are:

- `src/wasm/codec.rs`
- `web/graphics.js`
- `web/harness.js`
- `web/image.js`

The implementations in this repository are modified versions, not verbatim
upstream releases.

## Sparkle 2.9.4

The macOS application bundle includes a modified-for-packaging copy of
[Sparkle 2.9.4](https://github.com/sparkle-project/Sparkle/releases/tag/2.9.4).
The framework is thinned to arm64, unused sandbox XPC services are removed,
and the remaining code is re-signed. Build-machine copies of Sparkle's
`generate_keys` and `sign_update` utilities are also vendored but are not
included in the application.

Sparkle and the third-party components it incorporates are distributed under
the terms reproduced in [`packaging/sparkle/LICENSE`](packaging/sparkle/LICENSE).
That complete notice is copied into every application bundle as
`Contents/Resources/Sparkle-LICENSE.txt`.

## ArenaNet and Guild Wars material

ArenaNet's Guild Wars client, WebAssembly modules, JavaScript glue, game data,
and filesystem image are fetched from ArenaNet at runtime. They are not part of
the repository or release packages and are not licensed by gwnative.

The application icon in `packaging/icon.png`, its generated
`packaging/AppIcon.icns` copy, `docs/assets/app-icon.png`, and the Guild Wars
login screenshot in `docs/assets/game-login.jpg` contain ArenaNet-owned
artwork. They remain proprietary and are identified by
`LicenseRef-ArenaNet-Proprietary` in the REUSE metadata. See
[`NOTICE.md`](NOTICE.md) for the official-project disclaimer, ownership notice,
and links to ArenaNet's governing terms.
