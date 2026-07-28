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
| WASM memory growth | WebKit allows 4096 MiB; this client caps itself at 2048 |

Frame path cost at 3840x2160: `transferToImageBitmap` p50 0.20 ms / p95 0.30 ms,
`transferFromImageBitmap` p50 0.02 ms. Both the offscreen path and a
direct-to-visible control were vsync-bound at 60 fps, so those are upper bounds
rather than a measured ceiling.

## Why a loopback origin rather than a custom scheme

Both give a secure context with IndexedDB — a custom `WKURLSchemeHandler` scheme
works fine on macOS 27, and can reach cross-origin isolation if it sends
COOP/COEP. But WebKit clamps `performance.now()` to 1 ms on a custom scheme,
while the loopback origin resolves to 0.02 ms. The harness records per-frame
timings in microseconds, so the server wins. It binds 127.0.0.1 on a **fixed**
port, 38112; nothing is reachable off-host.

The port has to be fixed. WebKit keys IndexedDB by origin and the port is part
of the origin, so an ephemeral port would hand the page an empty store on every
launch — skill templates, settings and chat logs gone each time. 38112 is
unassigned by IANA and below macOS's ephemeral floor of 49152, so the kernel
will not lend it to somebody's outbound socket while we are not listening. If it
is taken anyway the bind falls back to an ephemeral port and says so, because a
launchable app that has forgotten everything beats one that will not start.
`GWNATIVE_PORT` overrides it, which is also how a second instance gets a private
store instead of fighting over this one.

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
everything back, host figures and the chunk store's counters included.

The POST answers `204`. It used to answer with the whole snapshot, which meant
serializing every metric the host holds once a second into a body the page never
reads — and the page never reads it because nothing on that side wants it. The
name table is capped at 512 names of 128 bytes, with anything past that counted
under `gw.metrics.dropped`: the names arrive over the loopback from a page that
could be made to send anything, and a map that only grows is a leak with a
network trigger.

`web/memory.js` reports the one number the host cannot see. `footprintMiB` is
the host process; the WASM heap, the GL resources and the JavaScript objects are
all in the web content process, which is accounted for separately — the host
reads about 27 MiB while the client holds hundreds. WebKit exposes no
`performance.memory` and no `measureUserAgentSpecificMemory`, so the client's
own linear memory is measured by wrapping `emscripten_resize_heap`: every
increase is an explicit call, so the wrapper sees each one as it happens rather
than sampling a number that has mostly not moved. It also sees the refusal,
which is otherwise silent — the client aborts or fails a load and nothing says
why.

`web/audio.js` reports the other blind spot. It has to construct the client's
`AudioContext` to reach it at all (see below), and having done so it can say
what the output device actually is: sample rate, state, base and output latency,
context creation and close, and every mute transition.

## Measuring both, from nothing

`scripts/benchmark` runs this build and the Electron one from a blank install,
one after the other, on the same machine within the same few minutes, and prints
what each cost. The comparison this repository exists to make had until now been
argued from readings taken on different days against caches in different states,
which is not a comparison.

A blank install is simulated, never performed — nothing in that script deletes
anything. Everything this build writes hangs off `$HOME`, so `HOME=<empty dir>`
is a machine it has never seen. The same trick does nothing to the Electron
build: Chromium takes the home directory from the password database and ignores
the environment, so it goes on quietly using the real profile. Its equivalent is
`--user-data-dir`, which every path it writes derives from — and which carries
the single-instance lock with it, without which a copy already open on the real
profile turns the launch into an immediate, silent, successful exit. That one
cost an hour.

Physical footprint is what the table leads with, per the reasoning above, and
it is summed over the whole process tree. Both builds keep most of their memory
outside the process that was launched, and they hide it in different places: the
Electron build's helpers are its own children and a walk down from it finds
them, while WebKit's `WebContent`, `GPU` and `Networking` services are started
by launchd, so their parent is pid 1 and no walk will ever reach them. They are
attributed by appearance instead — a WebKit service that was not running before
the launch is a candidate, and candidates still running after the run has been
stopped belonged to some other application and are dropped. On this machine the
difference that makes is between a host holding 33 MiB and a tree holding 554.

One row is a like-for-like comparison of a moment: page load to first frame.
Both builds time it themselves and time it the same way — `performance.now()` at
the first submit here, `frame.firstSubmit` against `renderer.loaded` there — so
both are quoted as they report it. The stage timelines under it are each build's
own account of itself, on each build's own clock, and the two do not have all
the same stages.

What the script cannot make equal, it says out loud rather than papering over:
the saved login is not comparable (this build reads the login keychain, which is
per-user and does not follow `$HOME`; the Electron build passes
`use-mock-keychain` when it is not packaged and keeps its own file under
`userData`), the launcher's one question is pre-answered with `quick` because a
benchmark cannot click, and bytes written measure how far the client got rather
than a fixed cost of installing.

### What it says, alternating runs, same machine, same hour

Single runs of this benchmark are not quotable — first frame on this build has
been measured anywhere from 8.7 to 27.8 seconds depending on conditions neither
build controls — so what follows is samples taken alternately, one build then
the other, never one build's night against the other's.

Both builds have a fast mode and a slow one, they differ by a factor of three,
and nothing in either decides which a launch gets: across eight interleaved
runs the slow ones landed in the same ten minutes of wall clock on *both*
builds, and the fast ones after it. Best and worst are therefore quoted
separately, because an average over the two modes describes no launch anyone
will have.

|                                          |    gwnative |    Electron |
| ---------------------------------------- | ----------: | ----------: |
| first frame, best of four (s)            |         8.7 |        10.8 |
| first frame, worst of four (s)           |        27.8 |        31.1 |
| CPU seconds, whole tree, 60 s window     |     24 – 40 |     29 – 34 |
| footprint peak, summed over tree (MiB)   | 1999 – 2114 | 1370 – 1415 |
| full 4.2 GB download, blank install (s)  |   105 – 106 |    90 – 346 |

The CPU row counts only runs that reached a frame and then rendered out the
window, since a launch that never draws spends nothing on drawing — one such
run flattered the Electron build with 15.5 seconds and is left out. Frame rate
is this build's own counter: 6,258 frames in 53.9 seconds, or 116 fps, on a
120 Hz display. The 60 Hz cap is gone. The Electron build exposes no frame
counter to the harness, so its rate is not quoted here.

Where the columns disagree, each now has a measured reason rather than a
guessed one.

**First frame.** Best case now belongs to this build, 8.7 seconds against
10.8, and the transport stopped being the story a while ago: fetching moved to
the OS HTTP/2 stack, the boot set ships as a built-in chunk list the store
warms before the client asks, and demand reads that once averaged 22 ms now
average 1.4 ms against the warmed cache. What is left is the spread. A launch
either walks its startup at roughly 2,200 files per second or at 215, with
nothing in between, and the slow mode is not waiting on anything this build
owns — range reads average 1–2 ms in *both* modes, retries are zero, and a slow
launch records no frames at all. The two plausible culprits were tested rather
than argued about: `-NSAppSleepDisabled YES` moved the median by 79 ms, and the
slowest of six runs was the one held frontmost, so neither App Nap nor
occlusion is it. Nor is the stall confined to boot — one launch reached its
first frame in 9.0 seconds and then spent 23.4 seconds inside a single frame.
What puts the cause outside both builds is that the Electron one has the same
mode and wears it worse: its slowest of four was 31.1 seconds against this
build's 27.8, in the same ten minutes of wall clock.

**The full download.** Both builds sweep the same 16,167 chunks — every hash
distinct, so neither gets a deduplication discount — from the same CloudFront
HTTP/2 origin. What used to stand here, that CFNetwork throttled itself to
~4 MiB/s partway through every transfer, was wrong, and the measurement that
retired it is cheap to repeat: `curl --parallel` over the same URL list, doing
no hashing and writing to `/dev/null`, reaches 47.0 and 39.4 MiB/s at 8
requests in flight, 48.3 and 47.7 at 24, and falls back to 36.4 and 41.0 at 48.
The link underneath reports 529 Mbit/s and 18 ms idle latency, so the ceiling
belongs to the path and not to either client — and this build already sits on
it, at 41–46 MiB/s with hashing and disk writes on top. Alternating
blank-install downloads say the same: 105 and 106 seconds here against 90 and
346 there. The 346 is not a typo, it is the slow mode above landing on a
transfer instead of a boot, and it is why that column is a range and not a
verdict.

**The summed peak** is the row this build loses on its own merits, and it is
now measured rather than inferred. The client's linear memory is 256 MiB and
never grows — the module declares 256 initial against a 2,048 maximum, and the
growth import is called zero times in a full session — so the 1.5 GiB
`WebContent` high-water mark is seven times the heap it serves, and none of the
rest is the game's. Both builds render the same module at the same scale, each
defaulting to 2, which leaves WebKit's texture and object accounting against
Chromium's. A summed peak is a ceiling rather than a reading — the kernel keeps
one high-water mark per process and they need not have coincided — and steady
state is kinder: one run finished holding 549 MiB against its own 1,881 MiB
peak, where the Electron build finishes within 40 MiB of its peak every time.
The ceiling is still 700 MiB above the other build's, and it is the one number
in this table that has not moved.

A blank install used to be the launch nothing could help, since the boot list
that lets the store fetch ahead of demand was recorded by the launch before.
It is now the launch the built-in list exists for; recording still happens,
and a session's own list replaces the shipped one the moment it seals.

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

Until now none of the four could be changed without a text editor. ⌘, opens a
panel over the running game — the page's own, not AppKit's, because every one of
these settings is one whose effect the page owns and a native panel would need
a second copy of the same four values kept in step with the first. The controls
are a table in `web/settings-panel.js`; the markup is built from it, and so are
the tests, so a list that disagrees with itself is not a shape this can take.

Two of the four cannot take effect until the next launch, and the panel says so
instead of pretending. The render scale reaches the client through an import it
reads when it recomputes the canvas, and the gesture translation is a set of
listeners installed once at boot around a mode captured by value. Both are
fixable, and both are a change to the boot path to fix — which is not a change
worth making from inside a settings panel. The overlay and the download
strategy do apply immediately, and the overlay is switched from what the host
answered with rather than from what was asked for, so a patch the host clamped
cannot leave the screen disagreeing with the file.

The game image is the one that gained a behaviour rather than a control. The
launcher asks its question before the client exists and then never again, which
left "download the rest of it" as something a player could only decide at a
launch they had already got past. Changing it in the panel starts or stops the
same host-side sweep the launcher drives — the same `POST /__prefetch` and the
same refusal when the volume cannot take it, which the panel shows rather than
swallowing. `null` remains neither answer: it is the request to be asked again,
and asking is the launcher's job at the next boot.

Only what moved is written. A panel opened and closed sends no patch, which
matters because the host persists on every patch it accepts.

## The one question asked before the client exists

The snapshot is 4.2 GB and only a fraction of it is touched in a session, so
streaming it on demand is the default worth having. It is not the only one worth
offering: on a slow or metered link, paying the whole cost once and never
touching the network for game data again is a real preference. So the launcher
asks, once, and records the answer in `dataStrategy` — where `null` is a third
state distinct from either answer, meaning nobody has been asked yet.

It is asked after the snapshot's size is known and before `appendGlue()`, which
is the last moment there is anywhere to ask: once the client is built it owns
the canvas and the keyboard. The overlay is the `#failure` overlay's twin for
the same reason that one covers everything — there is no client behind it yet.

What the progress bar shows is **residency**, not the sweep's own counter, and
the difference is not cosmetic. `GET /__prefetch` reports both, and six seconds
into a sweep over a half-full cache they read:

```
{"cached":8480,"total":16023,"fetched":5376,"running":true,"chunkSize":262144,
 "outstanding":24,"needed":1976367104,"free":81003741184}
```

`fetched` had passed five thousand because the sweep counts the chunks it walks
past and already has; `cached` had moved by 245, which is what was actually
downloaded. A bar driven by the first would leap to a third full and then crawl.
`cached` also survives a restart, which `fetched` cannot: it comes from the same
directory scan that builds the residency bitmap — 256 listings, not 16023
`stat`s — and a test asserts the count and the bitmap agree, because two answers
to "what is on disk" that could disagree eventually will.

`outstanding` is how many fetches are in flight, which is what separates a sweep
that is slow from one that is wedged. `needed` and `free` are bytes, and the
POST refuses to start a sweep that would leave less than 2 GB behind — a volume
with nothing left does not merely stop downloading, it breaks swap, the WebKit
caches and every atomic rename in this process, and the first casualty would be
the game rather than the download that caused it.

**Play now** does not cancel the download. The sweep is host-side, runs at
Utility QoS, and holds at most three of the eight fetch permits, so it yields to
the reads the game is blocked on rather than queueing in front of them; letting
it continue under a session that has started playing is the whole point of
having built it that way. **Stop downloading** is the one that ends it, and it
rewrites the setting so the question is not asked again.

Nothing here can prevent a boot. A missing progress route, three failed polls in
a row, a sweep that ends early, a setting that will not save — every path ends
in the client starting, because streaming works whether or not any of it does.

Measured on a half-populated cache: `launcher: downloading, 8485/16023 chunks
already cached`, and thirty seconds later the cache held 9458 chunk files rather
than 8486 — 243 MB, with the client correctly still unbuilt behind the overlay.
`./target/release/gwnative serve` prints the loopback address and the session
token on one line so these routes can be exercised from curl, which is the only
way past the gate from outside the page.

## A client is not the same thing as a client that runs

Until now the question asked of the three downloaded artifacts was whether they
existed. That misses both ways a client goes wrong.

The first is that the bytes are not the bytes: a sync interrupted at 90%, a disk
that filled, an editor that saved over a file. Every artifact is now recorded
with its length and the SHA-256 of the whole assembled file, and checked against
that record at launch — length first, because it is a `stat` rather than a read
of eight megabytes and truncation is both the likeliest corruption and the one
this catches for free. Anything that fails is re-downloaded, alone. Live, with
one byte of `version.json` changed and the length left intact — the corruption
an existence check cannot see:

```
[generation] version.json: content is 0130d01e3212cbad…, 845168e34f763c9b… recorded
[gwnative] fetching client artifacts: version.json
[generation] client build 77e195b2c164ffad installed, not yet proven
```

The second is that a perfectly downloaded build can still fail to run here. So a
freshly written set is recorded **unproven**, and the set it replaced is copied
aside first — 8.7 MB, which is cheap next to being unable to play. `__booted`,
which the harness already hits at the first frame, is what settles it. If the
app is launched again while the record still says unproven, the previous set is
restored and the build that never drew a frame is refused by name, so the next
launch does not walk into it again:

```
[gwnative] client build bad00000000000000 never reached a first frame; restored the one before it
```

Two identities do two different jobs. A build id is derived from the manifest's
chunk hashes, so it is known *before* anything is downloaded — that is what
makes refusing a build possible at all. An artifact hash is of the file as
written, and says what is on this disk. Neither substitutes for the other.

Three refusals, all of them deliberate. A build is only refused when there is
something to go back to: on a first install the one client on the disk is not
taken away, because a boot that failed for an unrelated reason would leave an
app with nothing to run. A refused build is retried anyway, loudly, if the disk
copy is incomplete — a broken client and a refused replacement is not a choice.
And only eight refusals are remembered; one that falls off the end is old enough
that nobody is being offered it.

An installation that predates all of this is adopted on first launch — hashed
once, then checked like any other. Without that step the install most likely to
have rotted would be the only one nothing was watching, until some future patch
happened to replace it.

## The window, and the page inside it

Two things WKWebView will not do on its own, and one thing the disk will not.

**A window that comes back where it was.** The last *normal* frame is stored,
never the zoomed or full-screen one — a full-screen frame restored as a normal
window is a window the size of the display with no obvious way back to a usable
size. The mode is stored beside it and applied after the frame, so a maximized
window comes back maximized over the frame it would return to.

A stored frame is a request rather than an instruction. Displays get unplugged,
rearranged and rescaled, and a frame that made sense last week can put the
titlebar off-screen — at which point the window cannot be dragged back, because
the part you drag it by is what is missing. So the frame is fitted to the
displays that exist now: it goes to whichever work area it overlaps most, and if
it overlaps none it is centred on the primary one at its remembered size. Tried
against a real machine: `x: 99000` came back at `x: 450`, centred; a `width` of
`1e9` was refused by name, the file removed, and a default window written in its
place.

**A page confined to its origin.** Nothing in `web/` links off-origin, so a
navigation that leaves the loopback address is either the client following
something it parsed out of server content or a script that got somewhere it
should not have. Allowing it would turn the game's window into a browser
pointed at somebody else's page with the session token still in the realm it
came from, so a `WKNavigationDelegate` cancels anything that is not the origin
the window was opened at — compared as a prefix that must be followed by `/`,
which is what stops `:381120` from passing for `:38112`.

Underneath that, every response carries a CSP. Two `unsafe` grants are
unavoidable — Emscripten calls `new Function` for its dynamic call thunks and
`'wasm-unsafe-eval'` is what permits `WebAssembly.instantiate` at all — so what
the policy buys is everything it does *not* list: `object-src 'none'`,
`frame-src 'none'`, `base-uri 'none'` so nothing can retarget every relative URL
on the page, and `form-action 'none'`. Those are the routes by which injected
content in an 8.2 MB third-party module could reach off the machine, and the
module itself has to be run as it was shipped.

**A renderer that restarts.** The web content process is a separate process and
it can be killed — by the jetsam pressure a 4.2 GB streaming game invites, or by
a driver fault. WKWebView does not reload itself when that happens; it leaves a
blank white view and no error, which reads exactly like the app hanging. One
automatic reload turns that into a re-boot the player watches happen. Only one:
a client that crashed its renderer every boot would otherwise reload forever,
and a loop is worse than a message. Verified by killing the content process
outright mid-session — the reload ran and the client reached a first frame
again.

**A cache that forgets old builds.** The chunk cache is content-addressed, which
is what makes deduplication free and what makes this necessary: when ArenaNet
patches, the chunks whose contents changed get new hashes, and nothing ever
writes to the old names again. The cache was a union of every snapshot the
machine had ever seen — a second 4.2 GB after the first patch and another after
the next. Activating a manifest now drops every cached chunk it cannot name. The
set to keep comes from the manifest rather than from a listing, so a chunk being
written this instant is one the manifest named and survives; a manifest that
names nothing at all is disbelieved rather than obeyed.

**A menu bar that reaches the things AppKit hides.** A ⌘-key is delivered as a
key equivalent, and what turns ⌘V into `paste:` is an Edit menu item claiming
it: with no main menu, pasting an account name into the login field does
nothing, and neither does ⌘Q. Full screen is the same story — without an item
there is no ⌃⌘F, and the green button is a poor only way in for a game.

The rest of the menu is things with no other route. **Reset Window Size and
Position** is the escape hatch for a window the fitting above cannot rescue,
because nothing about it is wrong: dragged mostly off an edge, or left full
screen on a machine whose second display went away. Leaving full screen is
animated and ends by restoring the frame the window had before it started —
which is the frame being replaced — so the new frame is queued and applied when
`NSWindowDidExitFullScreen` says the animation is over. **Toggle Diagnostics**
writes the setting and tells the live page, because the page reads that setting
once at boot and will not read it again. **Reload Game** is deliberately
unguarded, unlike the Electron build, which asks first when a game socket is
open: there ⌘R is a browser reflex Chromium honours everywhere, here the key
equivalent exists only because the item does, and the reload is the escape
hatch for a client that has already stopped answering — which is exactly when a
modal about sockets is in the way.

**Show Diagnostics Log…** reveals `gwnative.jsonl` in the Finder rather than
exporting a report. The Electron build has to build one because its diagnostics
live in memory; here they have been a file on disk all along, a line a second
for the whole session, so an export would be a copy of something the player can
attach to an issue directly. **Project Website** is absent from this build on
purpose: the item's URL comes from the package's `repository` field, and a Help
menu that offers to open a website and then opens nothing — or opens someone
else's repository because the URL was guessed — is worse than a Help menu with
one item in it.

And one instance at a time. Two copies share the web root, the generation
record, the settings file and the boot list, each written whole — only the
content-addressed chunk cache survives the collision, which is why it went
unnoticed. An advisory `flock` is the right shape for it: the kernel releases it
when the process dies however it dies, so a crash cannot leave a stale lock the
way a pid file would. A second launch raises the window that already exists
instead of failing silently.

## Status

Playable. The window opens, the harness and the client boot over loopback, the
patch client fetches `Gw.jspi.js`, `Gw.jspi.wasm` and `version.json` from the
live service and checks them by size and content at every launch, the 4.2 GB
snapshot is served on
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
in once more, which replaces the item outright — the update is refused for the
same reason the read was, so `src/keychain.rs` deletes and recreates instead.
Removing an item does not require permission to open it, which is the whole
trick. Once only: the second write is a create against nothing, so a refusal
there is a different condition and the only thing left to say is which item to
delete by hand.

The three ways the keychain says no are kept apart, because they used to share
one message and it was wrong twice out of three times. Only `errSecAuthFailed`
is the signature story. `errSecUserCanceled` means the prompt was dismissed a
second ago, and `errSecInteractionNotAllowed` means the screen was locked and
nobody was asked at all — sending either of those to read about code signing
wastes the reader's time. The retry is behind a small trait so the refusal paths
have tests; against the real keychain they would need a second signing identity
and somebody to press Cancel.

## Two WebKit data roots, and why they stay that way

WebKit keys its storage root by bundle identifier, falling back to the process
name when there is no bundle. So `cargo run` writes to
`~/Library/WebKit/gwnative` and `dist/Guild Wars.app` writes to
`~/Library/WebKit/com.gwnative.app`, and IndexedDB — the account record in
`Gw.dat`, skill templates, chat logs — does not cross between them. Signing the
development binary does not merge them: the identifier the keychain matches on
is not the one WebKit reads.

`WKWebsiteDataStore.dataStoreForIdentifier:` looks like the fix and is not. An
identified store is created *inside* whichever root the process already got, as
`CustomWebsiteData/<uuid>`, so it nests the split rather than closing it — and
adopting one would abandon what both roots already hold. The split is inherent;
what matters is knowing it is there. Wiping game state for a measurement means
naming the right root, and the two are 44 MB and 1.2 MB apart precisely because
they are not the same store.

`GWNATIVE_ACCESS_KEY` overrides the access key should ArenaNet rotate the value,
`GWNATIVE_WEB_ROOT` overrides the harness directory, and `GWNATIVE_PATCH_ROOT`
the patch endpoint. `GWNATIVE_PORT` moves the loopback origin. `GWNATIVE_PRINT_TOKEN`
prints the session token to stderr: the windowed app otherwise keeps it to
itself — it reaches the page over the injection channel and nowhere else — and
every measurement worth taking is behind that gate on `__diag`.
`GWNATIVE_TRACE_HTTP` and `GWNATIVE_TRACE_SOCKETS` log per request and per
socket, both off by default because a boot issues a couple of hundred range
requests and stderr is a synchronous write on the thread serving the read.

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
