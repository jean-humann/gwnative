# gwnative

A native macOS host for the Guild Wars WebAssembly client. No Electron, no
Chromium, no Node. One Rust binary driving AppKit and WKWebView directly.

Independent reimplementation of the host side of
[gwonmac](https://github.com/Mat4m0/gwonmac); the renderer harness derives from
that project.

## Why a WebView at all

ArenaNet's patch server ships **two** artifacts: `Gw.jspi.wasm` and
`Gw.jspi.js`. The second is Emscripten-generated glue that is regenerated on
every client patch, and it calls `WebAssembly.Suspending`, WebGL, Web Audio,
IndexedDB, and DOM events. That JavaScript is not ours to replace, so the client
must run inside a JS engine with JSPI and WebGL.

Running the module under Wasmtime instead would mean reimplementing that glue —
the whole Emscripten runtime, EGL to GL, audio, filesystem — and redoing it on
every patch. That is not an optimisation; it is writing a browser and then
maintaining a clean-room Emscripten runtime against a moving target.

So JavaScript is confined to one thin layer: the harness that patches ArenaNet's
imports, which must share a JS realm with the client. Everything else —
patching, chunk storage, sockets, credentials, windowing, diagnostics — is Rust.

## Verified on macOS 27.0 (build 26A5368g), Apple M3 Pro

Measured inside this binary's own WKWebView, not Safari:

| Capability | Result |
| --- | --- |
| JSPI functional round-trip | suspend + resume returned 42 |
| WebGL2 on `OffscreenCanvas` | `WebGL2RenderingContext`, GLSL ES 3.00 |
| `transferToImageBitmap` → `bitmaprenderer` | full round-trip |
| IndexedDB | open + write, 20.6 GB quota |
| Secure context | yes, on a loopback origin |
| Cross-origin isolated | yes, `SharedArrayBuffer` available |
| `WEBGL_compressed_texture_s3tc` | present, so DXT stays compressed |
| WASM memory growth | to 4096 MiB |

Frame path cost at 3840x2160: `transferToImageBitmap` p50 0.20 ms / p95 0.30 ms,
`transferFromImageBitmap` p50 0.02 ms. Both the offscreen path and a
direct-to-visible control were vsync-bound at 60 fps, so those are upper bounds
rather than a measured ceiling.

## Why a loopback origin rather than a custom scheme

Both give a secure context with IndexedDB — a custom `WKURLSchemeHandler` scheme
works fine on macOS 27, and can reach cross-origin isolation if it sends
COOP/COEP. But WebKit clamps `performance.now()` to 1 ms on a custom scheme,
while the loopback origin resolves to 0.02 ms. The harness records per-frame
timings in microseconds, so the server wins. It binds 127.0.0.1 on an ephemeral
port; nothing is reachable off-host.

## What WKWebView does not provide

- `navigator.keyboard.getLayoutMap()` is absent. Replacement is `UCKeyTranslate`,
  which is strictly better here: AppKit also posts
  `NSTextInputContextKeyboardSelectionDidChangeNotification`, so layout switches
  can be observed rather than polled on focus.
- `TouchEvent` / `Touch` do not exist on macOS WebKit. These backed trackpad
  tap emulation and resynthesise as `MouseEvent`.
- `EXT_disjoint_timer_query_webgl2` and `OVR_multiview2` are absent; neither
  matters for this client.
- The renderer string is masked to `Apple GPU` rather than Chromium's full ANGLE
  string, so GPU diagnostics lose fidelity.

## Status

Bring-up. The shell opens a window, serves the harness over loopback, and the
capability probe passes end to end. The patch client works against the live
service: it fetches and verifies `Gw.jspi.js`, `Gw.jspi.wasm`, and
`version.json` (currently 1.1.7 build 38735). Still to come: chunk store,
ArenaNet sockets, host-call bridge, Keychain credentials.

## Build

```sh
cargo build
cargo run
```

Missing client artifacts are fetched on first launch; `cargo run -- sync`
refreshes them without opening a window. Neither needs setting up: the patch
service access key identifies the official client rather than a player, so it is
the same value everywhere and ships in `src/patch.rs`.

`cargo run` signs the binary first, through the runner in `.cargo/config.toml`.
That is not packaging: the keychain identifies the application allowed to open a
saved item by its code signature, and the signature cargo links by itself
carries a build hash, so without this every rebuild is a new application to the
keychain and the saved login quietly stops appearing. Signing with a
certificate replaces the hash with a rule naming the identifier and the
certificate's common name, which survives both rebuilds and certificate
renewal. Any codesigning identity in the login keychain will do and the first
one found is used; `GWNATIVE_SIGN_IDENTITY` picks a specific one. With no
identity at all the app still builds and runs — it just goes back to forgetting
the login on every rebuild, and says so.

A login saved by an earlier, differently signed build is not lost. macOS offers
it on first read, and Always Allow adopts it; declining that just means signing
in once more, which replaces the item outright.

`GWNATIVE_ACCESS_KEY` overrides the access key should ArenaNet rotate the value,
`GWNATIVE_WEB_ROOT` overrides the harness directory, and `GWNATIVE_PATCH_ROOT`
the patch endpoint.

## Package

```sh
scripts/bundle
```

Builds `dist/Guild Wars.app`. The bundle exists for one reason that cannot be
had any other way: macOS Game Mode only considers applications whose
`LSApplicationCategoryType` names a game category, and a category can only be
declared in an `Info.plist`. Being eligible is worth it — Game Mode gives the
frontmost full-screen game priority on the performance cores and doubles the
Bluetooth polling rate for controllers and AirPods. The rest follows from the
same file: a real bundle identifier instead of a nil one, `Guild Wars` in the
application menu, and a Retina backing store.

It is signed with the same identity and the same `com.gwnative.app` identifier
as `cargo run` uses, so its designated requirement is byte-for-byte the one the
keychain already knows and the saved login carries over.

A packaged build does not serve out of `Contents/Resources/web`. The patch
client writes `Gw.jspi.wasm` into the web root, and writing into a bundle
invalidates its signature — the same signature the login depends on. The
bundle's copy is a seed for `~/Library/Application Support/gwnative/web`,
refreshed on each launch, and the client artifacts are fetched there. For the
same reason the bundle carries only the shell: a packaged `Gw.jspi.wasm` would
freeze the game at the build it was packaged from, because the sync only runs
for an artifact that is missing.

Launch it from Finder, or run `dist/Guild Wars.app/Contents/MacOS/gwnative` to
keep the diagnostics on the terminal — the executable resolves its own bundle
either way.

## Licence

GPL-2.0-or-later, matching the upstream project.
