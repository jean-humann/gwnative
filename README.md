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
capability probe passes end to end. The host services — patching, chunk store,
ArenaNet sockets, Keychain credentials — are not written yet.

## Build

```sh
cargo build
cargo run
```

`GWNATIVE_WEB_ROOT` overrides the harness directory.

## Licence

GPL-2.0-or-later, matching the upstream project.
