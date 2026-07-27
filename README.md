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

## Occluding the window is already WebKit's job

Do not add host-side throttling for a covered window. WKWebView observes
`NSWindowDidChangeOcclusionStateNotification` itself and applies the strongest
throttle there is.

Measured with a probe that raises an opaque cover window from the same process
on the same `NSScreen` — the only way to be sure what is covering what on a
multi-display machine, and the reason an earlier measurement claiming the
opposite was wrong:

```
HOST: cover on
HOST: occlusionState=0x2000 visible=NO
PAGE: t=14s rAF=9.5 fps hidden=true  vis=hidden
PAGE: t=16s rAF=0.0 fps hidden=true  vis=hidden
HOST: cover off
HOST: occlusionState=0x2002 visible=YES
PAGE: t=26s rAF=60.0 fps hidden=false vis=visible
```

`requestAnimationFrame` goes to zero within two seconds and `document.hidden`
follows, which is what the client's own main loop already tests. A host
override that rescheduled those callbacks onto a timer would not save power; it
would restart a loop WebKit had stopped, and drive the client's animation and
audio off `performance.now()` instead of the frame clock — which is audible.
`web/harness.js` says the same thing at the one place that is tempted to do it.

## A thread per connection, deliberately

`src/server.rs` still spawns one thread per accepted connection, which sounds
like the thing every server guide tells you not to do. It was, once: before the
loopback kept connections alive, one profile counted **689 distinct threads in
four seconds**, each with a 2 MiB stack, and the `mmap`/`munmap`/`mprotect`
churn of creating and tearing them down was a large fraction of host CPU. An
elastic worker pool was the obvious fix.

Keep-alive removed the premise instead. Re-measured on the same machine, a
six-second profile covering the whole boot — the heaviest load the app ever
sees — finds **33 threads in the process, 19 of them running any gwnative code
at all, and 6 of those serving connections**. `lsof` at steady state shows
**8 established connections**, held open for the session.

That is not a number a pool improves. WebKit caps persistent connections per
origin at six, plus the `__socket` WebSocket, so the connection count is bounded
by the browser and every one of those threads now lives for the whole session —
a pool would replace 8 long-lived threads with 8 long-lived workers and add a
queue in front of them. It would also have to be elastic to be correct at all,
because a WebSocket occupies its worker until the player quits and a cold chunk
read blocks on the network, so a fixed pool deadlocks. More machinery, a new
failure mode, no measurable gain.

## One file per chunk, held open

The other queued change was to replace the 16,167-file content-addressed cache
with a single sparse `snapshot.bin`, on a microbenchmark showing 44.42 µs/op for
the cache path against 0.63 µs for a pread on one open sparse file. Re-measured
against the pread path that has since landed, over 2000 real cached chunks, 20
iterations, 32 KiB windows:

```
A  landed path (open + fstat + pread + close)   23.56 us/op
C  file per chunk, descriptor held open          2.82 us/op
B  one sparse file, descriptor held open         2.39 us/op
```

The layout was never what mattered. Of the 9.9x available, **8.3x is holding
the descriptor open** and the single-file layout adds 1.18x on top — so
`src/chunks.rs` now keeps up to 2048 cached chunk files open and preads into
them. A real boot ends with 38 held, 130 file descriptors in the process, and
the limit here is 1048576. It is sound because the cache is content-addressed:
the bytes behind a hash never change, so a descriptor cannot come to name the
wrong content.

The sparse file was refused on the other half of its premise. Sparseness does
not survive scattered writes on this filesystem — measured by writing 256 KiB
chunks into a truncate-extended 4.2 GB file and reading back `st_blocks`:

```
   10 chunks:    2.6 MB payload ->     2.6 MB allocated  ( 1.00x)
  100 chunks:   26.2 MB payload ->    90.4 MB allocated  ( 3.45x)
  500 chunks:  131.1 MB payload ->  4200.0 MB allocated  (32.04x)
 2000 chunks:  524.3 MB payload ->  4200.0 MB allocated  ( 8.01x)
```

`set_len` alone occupies nothing, and a handful of chunks stays honest, but by
five hundred APFS has materialised the entire file. A `snapshot.bin` would
therefore cost 4.2 GB on disk however little of the game was cached — which is
most of the point of caching on demand, and the whole point of offering to start
without downloading everything first.

## Diagnostics

Every run appends a JSON record per second to
`~/Library/Application Support/gwnative/diagnostics/gwnative.jsonl`, rotating at
5 MiB and keeping five files. One record has what the kernel says about the
process and what the page has counted, on the same clock:

```json
{"t":1753627543.6,"uptime":25.2,"footprintMiB":34.7,"residentMiB":109.7,
 "cpuPercent":3.6,"cpuSeconds":1.93,
 "host":{"fromCache":391,"fetched":0,"coalesced":0},
 "metrics":{"gw.boot.first-frame.ms":889.18,"gw.boot.wasm.ms":47.3,
            "gw.frame.ms":16.62,"gw.frame.ms.max":31.72,"gw.frames":1391,
            "gw.range.ms":0.26,"gw.range.ms.max":44.54,"gw.range.requests":226}}
```

`footprintMiB` is the figure to compare between applications — it is what
Activity Monitor's Memory column and `footprint` report. `residentMiB` is three
times larger here and always will be, because it counts shared clean pages this
process did not cause to exist. Reasoning from resident size inverted the
ranking against Electron twice before this file existed, which is why both are
written and only one is recommended.

CPU comes from `proc_pid_rusage`, whose `ri_user_time` is in **mach absolute
time**, not the nanoseconds the field name suggests. On Apple silicon a tick is
41.67 ns, so reading it directly understates CPU by that factor and looks
entirely plausible while doing it. `src/diagnostics.rs` converts through
`mach_timebase_info`; the conversion was checked against `getrusage(2)` over the
same burn, both giving 0.3279 s.

Metrics carry three behaviours, matching on both sides of the loopback origin: a
count accumulates, a gauge keeps the last value, a peak keeps the largest. The
page posts increments to `__diag` once a second and `GET /__diag` reads
everything back, host figures included.

## When booting fails, and what the host pushes

A failed boot used to end at one line of red text, leaving the player nothing to
try but the relaunch they had already tried. Most of these failures are
transient — a range request that lost its connection, a module fetch that raced
the server coming up — so `web/loading.js` puts an overlay over the canvas
offering **Try again**, **Reset game data…** and **Show log**. Its markup lives
in `index.html` rather than being built in JavaScript, because one of the
failures it has to survive is a module failing to load.

Reset is two steps with the warning on screen rather than a `confirm()` dialog:
a native modal over the canvas blocks the whole WebView until it is answered. It
deletes this origin's IndexedDB databases, enumerated rather than named, because
emscripten derives the name from the mount point and hardcoding it would strand
data the day either side changes. Downloaded game files are kept — those live in
the chunk store, not in IndexedDB.

Separately, `src/commands.rs` pushes the few things only AppKit knows about into
the page, by evaluating a one-line dispatch into its realm; `web/commands.js`
holds the vocabulary. The one that matters is the window resigning key. The
page's own `blur` handler covers most of it, but not ⌘Tab away mid-stride: the
keyup goes to whatever took focus, so the client walks into a wall until the key
is pressed again. AppKit sees `NSWindowDidResignKeyNotification` every time,
including the times `blur` does not fire. It is observed through
`CFNotificationCenterGetLocalCenter()`, which is the same centre as
`NSNotificationCenter`'s default — bridged — so no Objective-C class has to be
declared just to own a selector.

## Saving a skill template, which the shipped client cannot do

Four `Base/Os` file routines are unimplemented in ArenaNet's build — creating a
directory always fails, enumerating one does nothing, deriving a name from an
entry writes nothing, and deleting a file is `assert("not implemented")`
followed by `unreachable`, so it takes the client down with it. A fifth is
implemented but wrong: `File::Open` mode 1 is meant to open an existing file and
instead opens `O_RDWR | O_CREAT`, so the client's own "is this name taken?"
probe creates the file it is testing for, and every rename is refused.

`src/wasm.rs` rewrites the module at launch. It appends one forwarder per broken
routine, each calling `__syscall_newfstatat` behind a negative dirfd marker
(-70001…-70005) that no real call can produce, and repoints the call sites at
them. Call indices are overwritten in place using LLVM's 5-byte padded encoding,
so nothing after them shifts. `web/template-save.js` recognises the markers and
answers against the mounted IDBFS; every other dirfd falls through to the real
syscall untouched.

The transform is gated on the exact input hash and asserts the exact output
hash, both pinned in the build table. Reproducing 8.2 MB byte for byte proves
the LEB128 encoders, the section split and re-encode, all five forwarder bodies
and every call-site offset in a single assertion — that is what
`the_real_client_transforms_to_the_certified_output` checks, and it skips rather
than fails when the client is not downloaded yet. An unrecognised input is
simply not transformed: the day ArenaNet patches the client, template save stops
working and the game still launches.

The derived module is cached under `Application Support/gwnative/derived` and
served under the base module's own name, so the glue's `locateFile` needs no
special case. Two log lines say it took: `template save: serving the derived
client` from the host, `template save bridge installed` from the page.

## Settings, and the render scale they revealed

`Application Support/gwnative/settings.json` holds four fields, and every one of
them is read by something that already exists here: `renderScale` is what the
client asks for through `emscripten_get_device_pixel_ratio`, `touchMode` selects
the gesture translation `web/input.js` installs, `showDiagnostics` opens the log
pane at boot, and `dataStrategy` records the answer to the launcher's question.
Nothing is stored for a feature this app does not have — the Electron build
carries five more fields, and each of them belongs to something that is not
here.

The host owns the file; `GET /__settings` reads it back and `PUT /__settings`
takes a patch and answers with the merged whole, so the page never has to guess
what its change turned into. Both are behind the session token with every other
`__` route. But the page does **not** fetch them at boot: the current values are
injected as `window.__gwnativeSettings` at document start, beside the token and
the keyboard layout, because the render scale is read by the client's first call
into the graphics host and the touch mode decides which listeners are installed
before anything the page could await. A boot that fetched its settings would
either add a round trip in front of every launch or draw one frame at the wrong
scale and correct it in front of the player.

The reader is deliberately lopsided: an unknown **field** is ignored, an unknown
**value** is refused. A file written by a later build should still yield
everything this build understands, but a `touchMode` of `"maybe"` means the file
cannot be trusted about touch, and quietly substituting a default there would
leave input behaving in a way no setting of theirs explains. A patch is stricter
still — a misspelled name is a 400, because answering 200 would hide a page bug
behind a control that silently never works. A `formatVersion` this build cannot
read is refused rather than reinterpreted, and the file is then moved aside
intact as `settings.json.corrupt-<epoch>` (three kept) rather than overwritten.

Wiring this up turned up something worth stating plainly. `renderScale` began
life here as a multiplier the harness applied while it sized the canvas itself;
when that sizing was removed the variable became the device pixel ratio handed
straight to the client, and its value was left at `1`. So this app had been
rendering at 1x on a Retina panel while the Electron build renders at 2x — the
same import, the same semantics, a different default — which means every
resource comparison made between the two until now had this one drawing a
quarter of the pixels. The default is now `2`, matching. Measured at the login
screen, 30 samples a second apart, host plus its three WebKit helpers:

| render scale | CPU | RSS |
| --- | --- | --- |
| 1 | 24.2 % | 559.9 MiB |
| 2 | 26.0 % | 608.5 MiB |

That is a smaller gap than four times the pixels suggests, and the reason is
that the login screen is not drawing much: the 48.6 MiB is where the difference
is honest — a 2560×1600 RGBA drawing buffer against a 1280×800 one, plus the GPU
process's share. Under a loaded zone the CPU column would separate further. A
player who wants the cheaper picture can have it; that is what the setting is
for. Which one a session paid is in its log: `settings: render scale 2, touch
mode off`.

## Status

Playable. The window opens, the harness and the client boot over loopback, the
patch client fetches and verifies `Gw.jspi.js`, `Gw.jspi.wasm` and
`version.json` against the live service, the 4.2 GB snapshot is served on
demand out of the chunk store, the ArenaNet sockets bridge through to the game,
and the login is kept in the Keychain.

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
