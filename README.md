# gwnative

A native macOS host for the Guild Wars WebAssembly client. No Electron, no
Chromium, no Node. One Rust binary driving AppKit and WKWebView directly.

Independent reimplementation of the host side of
[gwonmac](https://github.com/Mat4m0/gwonmac); the renderer harness derives from
that project.

**Requires Apple Silicon and macOS 15.2 or newer.** Both are hard floors rather
than defaults. macOS 15.2 is the release where WebKit shipped the JavaScript
Promise Integration API, and `Gw.jspi.wasm` suspends and resumes its stacks
through it — anything older loads the page and then fails to instantiate the
module. Apple Silicon is a decision: there is one `arm64` slice, so an Intel Mac
is told the application will not open on it rather than being started under
Rosetta and finding out slowly.

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
- `TouchEvent` / `Touch` do not exist on macOS WebKit. They cannot be replaced
  with `MouseEvent`: the client has no double-click of its own and assembles one
  from taps, so the tap has to arrive as a touch or not at all. It registers the
  four touch events unconditionally and its handler only reads — three lists,
  four modifier flags, a timestamp — so `web/input.js` assembles the touches as
  plain objects and dispatches a `UIEvent` subclass that answers like a
  `TouchEvent`.
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
{"kind":"sample","t":1753627543.6,"uptime":25.2,"footprintMiB":34.7,
 "residentMiB":109.7,"cpuPercent":3.6,"cpuSeconds":1.93,
 "host":{"fromCache":391,"fetched":0,"coalesced":0},
 "metrics":{"gw.boot.first-frame.ms":889.18,"gw.boot.wasm.ms":47.3,
            "gw.frame.ms":16.62,"gw.frame.ms.max":31.72,"gw.frames":1391,
            "gw.range.ms":0.26,"gw.range.ms.max":44.54,"gw.range.requests":226}}
```

Every record carries a `kind`, so one file can hold four shapes and stay
greppable. `session` is written once at launch and is the only record that says
what machine this is — macOS version and build, hardware model, core count and
memory, read straight out of `sysctl` — which the file went without for a long
time and which is the first thing anyone reading someone else's log asks. `page`
is a line the client or the harness printed: `console.log`, `warn` and `error`
are wrapped in `harness.js` and posted to `__report`, which used to echo them to
stderr and nowhere else — a terminal nobody running the app has. They are capped
at 5,000 lines of 2,000 characters per session, with the overflow counted under
`gw.pagelog.dropped`, because a client stuck in a failing loop would otherwise
push every sample out of the rotation. `mark` is ⌘⇧M: the moment the player
pointed at, plus the next hundred samples taken at 100 ms instead of a second.
The honest limit is that it cannot make the seconds *before* the press finer,
which is why the in-app guide says to press it while the game is misbehaving
rather than afterwards.

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

`--warm` runs the same comparison against the installs already on the machine
instead of two blank ones. That is the launch every day after the first, and
the only one of the two that can be re-measured on an afternoon when the CDN is
slow — see [the warm route](#the-other-launch-scriptsbenchmark---warm).

### What it says, alternating runs, same machine, same hour

Single runs of this benchmark are not quotable — first frame on this build has
been measured anywhere from 8.5 to 15.4 seconds depending on conditions neither
build controls — so what follows is samples taken alternately, one build then
the other, never one build's night against the other's.

Both builds have a fast mode and a slow one, and nothing in either decides
which a launch gets: across four interleaved rounds the slow one landed on
*both* builds in the same minute. Best, median and worst are therefore all
quoted, because an average over the two modes describes no launch anyone will
actually have.

|                                          |         gwnative |           Electron |
| ---------------------------------------- | ---------------: | -----------------: |
| first frame, best · median · worst (s)   | 8.5 · 9.1 · 15.4 | 10.7 · 11.4 · 14.9 |
| CPU seconds, whole tree, 60 s window     |          27 – 39 |            15 – 33 |
| footprint peak, summed over tree (MiB)   |      2107 – 2112 |        1316 – 1412 |
| full 4.2 GB download, blank install (s)  |          87 – 90 |         90 – 346   |

The CPU row is the one that cannot be normalised, and it should be read that
way. This build counts its own frames — 6,045 in 53.7 seconds, or 112 fps, on a
120 Hz display, so the 60 Hz cap is gone — while the Electron build exposes no
frame counter to the harness. Its lower seconds may be cheaper frames or simply
fewer of them, and this benchmark cannot tell which.

Where the columns disagree, each now has a measured reason rather than a
guessed one.

**First frame.** This build now leads three of four interleaved rounds, on the
best run and on the median alike: 8.5 seconds against 10.7 at best, 9.1 against
11.4 in the middle. The transport stopped being the story a while ago —
fetching moved to the OS HTTP/2 stack, the boot set ships as a built-in chunk
list the store warms before the client asks, and demand reads that once
averaged 22 ms now average 1.4 ms against the warmed cache — and the most
recent gain was not in the network at all, but in the drive barrier the next
paragraph is about. A quick launch writes some 1,800 chunks before it draws
anything, and at 6.74 ms of cache flush apiece that was twelve seconds of boot
spent waiting on the drive rather than on the game. Removing it took the worst
of four runs from 27.8 seconds to 15.4.

What survives is a slow mode both builds share, and round three is what it
looks like: 15.4 seconds here against 14.9 there, in the same minute. A launch
caught in it walks its startup at roughly 215 files per second instead of
2,200, with nothing in between, and it is not waiting on anything either build
owns — range reads average 1–2 ms in *both* modes, and retries are zero. The
two plausible culprits were tested rather than argued about: `-NSAppSleepDisabled
YES` moved the median by 79 ms, and the slowest of six runs was the one held
frontmost, so it is neither App Nap nor occlusion. Nor is the stall confined to
boot — one launch reached its first frame in 9.0 seconds and then spent 23.4
seconds inside a single frame.

**The full download.** Both builds sweep the same 16,167 chunks — every hash
distinct, so neither gets a deduplication discount — from the same CloudFront
HTTP/2 origin. What used to stand here, that CFNetwork throttled itself to
~4 MiB/s partway through every transfer, was wrong, and the measurement that
retired it is cheap to repeat: `curl --parallel` over the same URL list, doing
no hashing and writing to `/dev/null`, reaches 47.0 and 39.4 MiB/s at 8
requests in flight, 48.3 and 47.7 at 24, and falls back to 36.4 and 41.0 at 48.
The link underneath reports 529 Mbit/s and 18 ms idle latency, so 47–48 MiB/s
is the path's own limit and there is no more to ask either client for.

This build used to reach 38 of it, and the missing ten were its own doing.
Every chunk was written with `File::sync_all`, which on macOS is `F_FULLFSYNC`
— not "get these bytes to the device" but "device, empty your write cache" —
and then the containing directory got the same treatment, so two full drive
barriers per 256 KiB. Writing chunks back to back that costs 6.74 ms each, or
37 MiB/s, which is exactly where the full download sat; plain `fsync` costs
0.41 ms and no flush at all costs 0.36. The barrier was the ceiling, and the 48
fetch threads were queueing at the drive behind it. It is gone. What it bought
was a chunk surviving a power cut, which this store is the wrong place to pay
for: chunks are content-addressed, every one is hashed the first time a session
reads it, and one that fails is unlinked and refetched — so a chunk lost to a
power cut costs a single 256 KiB request, and that is not worth 2.7× on every
install. Blank-install downloads now finish in 87 and 90 seconds at 44–46
MiB/s, against 90 and 346 for the Electron build. The 346 is not a typo; it is
the slow mode above landing on a transfer instead of a boot, and it is why that
column is a range rather than a verdict.

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
in this table that has not moved. It is also the one that does not survive
leaving this table: measured warm, with nothing downloading, the ordering
reverses and this build peaks 270 MiB *below* the other one at the median. The
gap is a cost of the install, not of the game — see the warm route below.

A blank install used to be the launch nothing could help, since the boot list
that lets the store fetch ahead of demand was recorded by the launch before.
It is now the launch the built-in list exists for; recording still happens,
and a session's own list replaces the shipped one the moment it seals.

### The other launch: `scripts/benchmark --warm`

Everything above is a blank install, which happens once and is mostly the CDN.
That makes it a poor thing to re-measure on demand: the numbers in the table
were taken with the path running at 47 MiB/s, and on the afternoon this section
was written the same probe — `curl --parallel`, 48 in flight, one chunk from
each fan-out bucket — returned 18.6, 25.3 and 16.5 MiB/s on three consecutive
samples. Nothing in either build changed between those readings. Re-running the
blank-install table under them would not have refreshed it; it would have
replaced a measurement of two clients with a measurement of one afternoon.

So `--warm` measures the other launch, the one a player repeats. Both builds run
against the profile actually on this machine, which is read and not written to,
and each keeps its own single-instance lock so a copy the player already has
open refuses the run rather than letting it time a process that exited. Neither
cache moved by a byte across the eight runs below — 2.5 GB for this build, 385
MB for the Electron one, unchanged before and after — so nothing here depends on
the link at all.

|                                        |         gwnative |          Electron |
| -------------------------------------- | ---------------: | ----------------: |
| page load to first frame, min · med · max (ms) | 745 · 821 · 898 | 1118 · 1260 · 1366 |
| wall clock to first frame (ms)         | 1358 · 1496 · 1735 | 1715 · 1831 · 2124 |
| footprint peak, summed over tree (MiB) |  767 · 773 · 783 | 1025 · 1043 · 1057 |
| peak RSS, summed over tree (MiB)       |  571 · 572 · 592 |    847 · 848 · 855 |
| CPU seconds, whole tree, 60 s window   | 18.5 · 19.2 · 19.8 |  10.1 · 22.0 · 26.4 |

Four rounds, alternating, same machine, same hour. The first two rows are all
four; the tree-wide rows below them are the three per build whose process
accounting came out clean, and the discarded pair is the reason the row exists
in this shape. WebKit services are launchd children rather than children of the
process that caused them, so they are identified by having started just after
it — and twice in eight runs that went wrong in a way worth naming: one gwnative
round had eight of its own services outlive it and drop off the tree, which is
why its CPU came out at 5.0 seconds against 18.5–19.8 everywhere else, and one
Electron round was charged a `WebContent` process, which is a WebKit process
that build has no way to spawn. Both are the harness miscounting, not either
client behaving differently, and both runs are excluded rather than averaged in.

**The footprint deficit belongs to installing, not to playing.** This is the row
that inverts. Blank install has this build 700 MiB above the other one and that
figure is real, but it is a cost of the download path: warm, the same tree peaks
at 773 MiB against 1,043 at the median, and holds 572 resident against 848. The
1.5 GiB
`WebContent` high-water mark that the paragraph above attributes to WebKit's
accounting is not steady state — it is what the renderer touches while 4.2 GB
moves through it, and once nothing is moving through it the ordering reverses.

**First frame is the clean win**, and it is the row both builds measure
themselves, the same way. 821 ms against 1,260 at the median, and this build's
worst round is still 220 ms faster than the other's best. Nothing about it is
network: the whole boot set is already on disk for both.

**CPU is still the row that cannot be normalised**, and warm does not fix it —
the Electron column ranges over 10.1 to 26.4 seconds across three clean rounds
of what should be identical work. This build counts its own frames and the other
exposes no counter to the harness, so a lower number there may be cheaper frames
or fewer of them, and a warm launch does not make that distinguishable. Read the
range, not the middle.

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

## GWonMac Tools, and the second module that runs beside the client

Two of them, and both only read. **Game cursor** draws the game's own pointer as
the Mac's pointer — the game paints its cursor into the picture, so it arrives
with the frame and lags the hand by however long the frame took, while the
system's own pointer moves at pointer rate. **Target distance** shows how far
away the current target is and which of the game's range bands that falls in.
Neither writes a byte the client will read, and with both off nothing below
happens at all.

Reading it out is the part with a shape worth explaining. The values live in the
client's heap and are only coherent at one point in its frame, so sampling them
from the page on an animation frame would read a structure the game is halfway
through updating. What is needed is a callback *inside* the client's own loop —
and the client has exactly one place to hang it: `EmscriptenExeThreadMainLoop`,
function 446 on the pinned build, which the browser drives.

So a second transform runs on top of the template-save one, keyed on that
module's output hash and asserting its own — `68c6e09c…` in, `903967df…` out. It
clones function 446 to a fresh index, exports the clone as
`enhancement_tick_original`, and overwrites 446's body with a dispatcher that
reads a new mutable global, `enhancement_hook_slot`: zero calls the original and
nothing else, and *n* calls table slot *n*−1 instead. Slot 0 is Emscripten's
reserved null-function-pointer entry and is never filled, which is what makes it
free to borrow. A manifest describing all of it — ABIs, block sizes, the table
slot, and the 29 struct offsets the readings come from — is appended as a custom
section, so the page learns the layout from the module it is actually running
rather than from a constant compiled into the page months earlier.

The callback itself is a separate wasm module, `src/companion-kernel/lib.rs`:
dependency-free `no_std` Rust with no `Cargo.toml`, compiled to
`wasm32-unknown-unknown` by `build.rs` and embedded in the host binary with
`include_bytes!`, then served from memory at `/companion-kernel.wasm`. It is
linked `--import-memory`, so `env.memory` is the *client's* memory rather than
one of its own — it reads the game's heap directly, at the one moment in the
frame when reading it is meaningful. Nothing in it can trap: every read is
bounds-checked against the heap's length, every pointer chase returns an
`Option`, and there is no allocator to fail. A trap here would abort the client.

It publishes two fixed-size blocks under a seqlock — a 64-byte state snapshot
and a 4160-byte cursor bitmap — and the page reads them on the animation frame
with no lock, no message and no copy of the heap: a reader that sees the same
even sequence before and after its copy knows nothing moved underneath it.
`web/companion-snapshot.js` then re-checks every field the companion already
checked, and answers `waiting` rather than rendering a coordinate it does not
believe. The two halves are compiled separately and share nothing but that
manifest, so the page treats the region as what it is — a span of a heap that
anything in the client could in principle have written.

The install order in `web/enhancements.js` is the other load-bearing part.
Memory for the blocks comes from the *client's own* `malloc`, so it cannot land
somewhere the client will later allocate over; the companion is instantiated
over that memory; the table slot is filled; and only then is the global set. Set
in that order there is no window in which 446 dispatches to a slot that is not
yet a function.

There is no master switch, and that is deliberate: "enhancements enabled" is
derived from "is any tool on", because a stored third state can disagree with
the two it governs. Both need a relaunch, and the panel says so rather than
pretending — the module the page runs is chosen before the renderer exists. Both
also need the same client recognition template save needs; on a build this
release has not been checked against they stay off and say so, and the game
launches exactly as ArenaNet shipped it.

## Settings, and the render scale they revealed

`Application Support/gwnative/settings.json` holds nine fields, and every one of
them is read by something that already exists here. Four are what a player sets
and a launch acts on: `renderScale` is what the client asks for through
`emscripten_get_device_pixel_ratio`, `touchMode` selects the gesture translation
`web/input.js` installs, `showDiagnostics` opens the log pane at boot, and
`dataStrategy` records the answer to the launcher's question. Two are the
GWonMac Tools above, `nativeCursor` and `targetReadout`, which between them
decide whether the enhanced module is built at all. The last three are what the
app has to remember rather than what anyone sits down to choose:
`autoCheckUpdates` and the `lastUpdateCheckAt` that makes an opted-in launch ask
once a day rather than once a launch, and `compatibilityNoticeSeenFor`, which
holds the hash of the client build a warning was acknowledged for — a boolean
there would either nag every launch or stay silent through every future patch.
Nothing is stored for a feature this app does not have.

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

Until now none of them could be changed without a text editor. ⌘, opens a
panel over the running game — the page's own, not AppKit's, because every one of
these settings is one whose effect the page owns and a native panel would need
a second copy of the same four values kept in step with the first. The controls
are a table in `web/settings-panel.js`; the markup is built from it, and so are
the tests, so a list that disagrees with itself is not a shape this can take.

Four of them cannot take effect until the next launch, and the panel says so
instead of pretending. The render scale reaches the client through an import it
reads when it recomputes the canvas, and the gesture translation is a set of
listeners installed once at boot around a mode captured by value; both are
fixable, and both are a change to the boot path to fix — which is not a change
worth making from inside a settings panel. The two tools are not fixable in the
same sense: which module the page is handed is settled before the renderer
exists, and a client that is already running cannot be exchanged for a different
one. The overlay and the download strategy do apply immediately, and the overlay is switched from what the host
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

The two answers are called **Quick Start** and **Full Game**, and they are
called that in all three places they appear: the launcher's prose, its buttons,
and the Settings row that undoes the choice. That is a fix rather than a
description. The screen used to describe the two modes without naming either,
the buttons said one thing and the setting said another, and the row that is the
only way to revisit the decision read as a different subject from the screen it
overrides — which is why the first report of this was that the setting did not
exist. It did; nothing connected it to the question it answers.

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
having built it that way.

Beside it are **Pause** and **Switch to Quick Start**, which used to be one
button answering two questions with the same word. A player who wants their
evening back and a player who wants their bandwidth back for ten minutes both
pressed *Stop*, and both got the first one's answer written to `dataStrategy` —
so the second one's next launch never resumed. Only Switch to Quick Start
rewrites the setting now. Pause stops the sweep and touches nothing else, which
means the next launch picks it up exactly where this one left it; the host
needed no change for that, because `POST /__prefetch?stop` never wrote the
setting in the first place. The launcher did.

Pausing is the one way this screen stays up with nothing moving, so the watcher
that treats `running: false` as "the sweep gave up, let them play" has to know
the difference. The flag is set before the stop and cleared after the resume,
both so a poll landing mid-toggle cannot read a pause as a failure and boot the
game out from under it.

### Residency is a filename; the check is what makes it a promise

A Full Game launch that finds the cache complete used to boot straight through,
on the strength of 16023 files existing with the right names. Nothing had read
them. That is fine for a Quick Start session — it verifies each chunk on the
read that wants it, and pays nothing extra to do so, because it was going to
read the chunk anyway — but Full Game is the one launch that promised the
network was finished with, and a truncated write or a bad block turns that
promise into a hash mismatch several zones later, with no download running to
repair it.

So `POST /__prefetch?verify` reads the cache back and hashes it, and the same
`GET /__prefetch` the download bar already polls carries `verifying`,
`verified`, `verifyTotal` and `discarded` beside the sweep's numbers — one
timer on the page for both, which never run at once anyway. It is a separate
one-shot rather than a phase of the sweep, deliberately: the sweep
trusts a `stat`, which is what makes resuming a 4 GB download cost seconds
instead of minutes, and folding a re-hash into it would undo that decision on
every resume. This runs on exactly one occasion instead — before a launch that
believes it is complete.

It is built on the same `read_cached` every demand read uses, which already
unlinks a chunk that fails. That is the entire repair path: unlinking drops the
chunk out of residency, the image stops being complete, and the ordinary
download screen behind this one refetches precisely what was discarded. The pass
needs no notion of repair of its own, and warms the `verified` set that the
window `pread`s depend on as a side effect.

Two details it would be easy to get wrong. It walks distinct chunks, not
indices, so a repeated chunk is hashed once and `verifyTotal` is the number the
bar must be drawn against rather than `total`. On the manifest as it ships today
those two are the same — all 16,167 hashes are distinct, the same fact that
denies the download a deduplication discount — so the pass costs a full re-hash
of the image and the deduplication buys nothing. It is kept because it is a
`HashSet` over a list already in memory, and because the alternative is code
that quietly does the repetition factor's worth of extra work the first time
ArenaNet ships a snapshot that repeats. And a chunk that was never fetched is
not damage: an interrupted download reported as a corrupt one would be a lie
told at the worst possible moment.

Eight threads rather than the sweep's thirty-two, and no permits, because this
one never touches the network — past the point where the volume is saturated,
more threads buy context switches. Measured over a cold 1.0 GB slice of a real
chunk cache on this machine, hashing is 500 MB/s on one core and 1.33 GB/s
across eight, which puts a full 4.2 GB image at roughly three seconds — a third
of it CPU and the rest waiting on the volume, which is why the thread count
matters more than the digest does. The disk check the POST applies to a download
is skipped here for the same kind of reason: this pass writes nothing and can
only ever free space, so refusing it for want of room would refuse it on exactly
the full volume where an interrupted write is likeliest to have left damage.

Nothing here can prevent a boot. A missing progress route, three failed polls in
a row, a sweep that ends early, a check that will not start or cannot be
watched, a setting that will not save — every path ends in the client starting,
because streaming works whether or not any of it does.

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

**Report a Problem…** writes `problem-report-<stamp>.txt` next to the log and
reveals it in the Finder. This replaced an item that only revealed
`gwnative.jsonl`, on the reasoning that the diagnostics were a file on disk
already so an export would be a copy of something the player could attach
directly. That was true and unhelpful: what it attached was thousands of
unlabelled records about a Mac the file never named. The report is a cover sheet
— machine, settings, and the last 400 records — in the same folder, so the raw
file is still one click away for anyone who wants it. Anything shaped like an
email address is replaced, because the account name is the one identifier the
client is ever handed; the report says so in its own body rather than implying a
general secret scrubber, and the password never reaches the page at all.
**Mark a Slowdown** (⌘⇧M) is in the View menu rather than Help, because it is
pressed repeatedly mid-session and never by opening a menu. **Project Website**
is absent from this build on
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
the login is kept in the Keychain, and a packaged build reads a signed feed to
find, show and install its own updates.

## Build

```sh
cargo build
cargo run
```

The build needs a second Rust target, `wasm32-unknown-unknown`: `build.rs`
invokes `rustc` directly to compile the companion above, and embeds the result.
`rust-toolchain.toml` lists it beside `aarch64-apple-darwin`, so a fresh
checkout installs both with the toolchain rather than failing on the first
build; `rustup target add wasm32-unknown-unknown` is the manual equivalent. It
is not a Cargo dependency and there is no second crate — one `rustc` call on one
`no_std` file, whose output has to be exactly the module the transform in the
same binary wrote a manifest for, which is why the two are compiled together or
not at all.

Missing client artifacts are fetched on first launch; `cargo run -- sync`
refreshes them without opening a window. Neither needs setting up: the patch
service access key identifies the official client rather than a player, so it is
the same value everywhere and ships in `src/patch.rs`.

One executable, three runs — no argument opens the window, `sync` downloads and
exits, `serve` runs the origin without a window — plus `--version` and `--help`,
which answer and exit. `src/cli.rs` decides which before anything is opened,
downloaded or read, and refuses an argument it does not know rather than
ignoring it. Ignoring it is how `--sync`, a plausible typo for a real command,
used to start a 4 GB download and a game window instead of doing nothing
visible. A `-psn_*` from Launch Services is the one exception it skips, since
refusing that would mean an app that opens from a terminal and not from the
Dock.

`cargo run` signs the binary first, through the runner in `.cargo/config.toml`.
That is not packaging: the keychain identifies the application allowed to open a
saved item by its code signature, and the signature cargo links by itself
carries a build hash, so without this every rebuild is a new application to the
keychain and the saved login quietly stops appearing. Signing with a
certificate replaces the hash with a rule naming the identifier and the
certificate's common name, which survives both rebuilds and certificate
renewal. `scripts/sign-identity` chooses it: a Developer ID Application
certificate first, then an Apple Development one, then whatever is installed.
That order matters beyond the release — Developer ID is the only certificate
Apple will notarize, so a published build carries one, and signing development
builds with anything else would give them a different designated requirement
and make everyone moving between the two sign in again.
`GWNATIVE_SIGN_IDENTITY` picks a specific one. With no identity at all the app
still builds and runs — it just goes back to forgetting the login on every
rebuild, and says so.

### The dialog, and why there is not one

A saved item carries a list of the code allowed to open it, and the entry is a
*designated requirement* — the identifier plus the signing certificate. Not a
path: the same signed build reads its item from anywhere on disk, so installing
into `/Applications` costs nothing by itself. Not the binary either: a rebuild
with a different code hash still reads, which is what lets an update ship
without logging everybody out. Only the signature counts, and the list grows by
one entry per signature that ever wrote the item rather than replacing.

When the running code is not on that list, macOS's own answer is a dialog:
*Guild Wars wants to use your confidential information stored in "gwnative
(Guild Wars)" — enter the "login" keychain password.* Over a game. Every answer
to it is bad. Deny loses the login, Allow types the password to the whole
account into a dialog nobody can verify, and Always Allow does that and adds
another entry to the list that grew the problem.

So it is never raised. `src/keychain.rs` suppresses keychain interaction around
every call it makes, which turns the dialog into `errSecInteractionNotAllowed`,
and treats that as "nothing saved" — the client shows its own sign-in form,
asking for the account password it is actually signing in with. Signing in
there replaces the item under the current signature, so a signature change
costs one sign-in, once, instead of a system-password prompt.

Replacing works where overwriting does not: an update asks the same permission
a read does and is refused the same way, but removing an item does not require
permission to open it, so `store_in` deletes and recreates. Once only — the
second write is a create against nothing, so a refusal there is a different
condition and the only thing left to say is which item to delete by hand.

The data-protection keychain would remove the list entirely, since it decides
access by signing team. It is not reachable here: it needs
`keychain-access-groups`, that entitlement needs a provisioning profile
embedded in the bundle to authorise it, and without one `SecItemAdd` returns
`errSecMissingEntitlement` while *with* the entitlement and no profile macOS
kills the process at launch. Such a profile is issued per App ID and expires,
which would give a downloadable Developer ID build an expiry date. For one
saved password that is a bad trade.

`errSecUserCanceled` is kept apart from the other two: it means a prompt was
dismissed, and sending someone to read about code signing over that wastes
their time. The retry is behind a small trait so the refusal paths have tests;
against the real keychain they would need a second signing identity.

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

The one capability a bundle has that no `cargo run` build can is
`Contents/Frameworks/Sparkle.framework`, and with it the ability to install its
own updates — see [Updating](#updating-and-the-two-paths-it-takes). It is
thinned to `arm64`, stripped of its XPC services and re-signed innermost-first,
because a signature seals what is inside it and signing the application before
its framework produces a seal over the old contents. It is installed only when
`packaging/sparkle/public-key.txt` exists; without one the bundle is exactly the
one this project built before Sparkle.

The bundle is signed with the hardened runtime on every build, not only on
released ones — it is a precondition for notarization, and turning it on at the
last moment would mean the first build to run under its rules is the one nobody
tested. Locally it comes with `packaging/debug.entitlements`, which is
`get-task-allow` and nothing else: without it the hardened runtime refuses
`sample`, `leaks`, Instruments and lldb, which is how nearly every number in
this file was arrived at. Notarization rejects that entitlement — it is
permission for any process the user runs to read this one — so `scripts/release`
passes no entitlements at all, and there are none it wants. The only exception
this application might have asked for is JIT, and the JavaScript needing one
runs in WebKit's own `WebContent` process under Apple's signature.

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

The icon is built by `scripts/icon` from `packaging/icon.png` and committed as
`packaging/AppIcon.icns`, because an icon is designed rather than compiled and
`iconutil` is not on every machine that can build this. macOS draws app icons
inside a grid whose body fills 824 of a 1024 canvas, so the artwork is inset to
that ratio at every size; artwork that fills its canvas is the tell of a ported
icon, visibly larger than everything beside it in the Dock. The same `.icns` is
embedded in the binary and installed at launch, so a `cargo run` build — which
has no bundle and therefore no `CFBundleIconFile` — shows the icon too, and the
download progress bar has something to draw over.

## Release

```sh
scripts/release
```

Builds, signs, notarizes, staples and packages two files:
`dist/gwnative-<version>.dmg` for a person to open, and
`dist/gwnative-<version>.zip` for the updater to install unattended. Both hold
the same stapled bundle, and the zip is rebuilt from it *after* stapling — the
copy that went to Apple was made before the ticket existed, and a bundle
unpacked from that one shows the unidentified-developer refusal on a machine
that happens to be offline, in exactly the case where nobody is watching.

The difference from `scripts/bundle` is not the build — it is the same binary —
it is that Gatekeeper has to be willing to open the result on a Mac that has
never seen this project. Four things have to be true for that: a Developer ID
signature, a countersignature from Apple's timestamp server, a notarization
ticket, and that ticket stapled into the artifact. The last one is the one
people skip. Notarization without stapling works only while the machine opening
it is online; stapled, the ticket travels inside the file.

Notarization credentials are a keychain profile, created once by whoever
releases:

```sh
xcrun notarytool store-credentials gwnative \
    --key <AuthKey_XXXX.p8> --key-id <key id> --issuer <issuer uuid>
```

That is an App Store Connect API key, from Users and Access > Integrations,
with the Developer ID role. An Apple ID and an app-specific password work too
(`--apple-id/--team-id/--password`), but they tie every release to one person's
account login and cannot be revoked without disturbing it. `GWNATIVE_NOTARY_PROFILE`
names a different profile. Nothing in this repository holds a key, a password or
an Apple ID, and nothing should: the script reads the profile by name and the
keychain answers.

The preflight refuses a development certificate, proves the credentials still
work with a one-second call, and stops on a dirty working tree — all before a
five-minute LTO build, because finding out afterwards that there are no
credentials is finding out after the build. After signing it re-reads what
`codesign` produced rather than trusting the flags it passed: a missing hardened
runtime and a missing timestamp are both silent until Apple returns `Invalid`.

There are two ways to publish and one implementation.
`.github/workflows/release.yml` runs this same script rather than
reimplementing it: a release path only exercised by CI is one nobody can
reproduce when it breaks, and a local one CI works around is one CI is not
testing. The workflow's job is to build the environment the script already
expects — a keychain holding the identity and the notarization profile — and
then get out of the way. Locally: run the script, then `git tag -s` and
`scripts/publish`, which it prints. From CI: push the tag and approve the run.

`scripts/publish` is the second half, and also one implementation for both
routes. It creates the release as a **draft**, renders the release notes to HTML
through GitHub's own markdown endpoint, signs the zip, writes `appcast.xml` from
all three, uploads it, and only then makes the release current. The draft is
what makes that order possible:
`releases/latest/download/appcast.xml` is the URL every installed copy asks, so
the moment a release stops being a draft it becomes the answer — and a release
that is visible before its feed is uploaded answers with a 404.

Approve is the word. This is a public repository and a Developer ID certificate
signs everything its owner ships, not only this project, so the release job
declares `environment: release`. That environment carries a required reviewer
and a deployment branch policy limiting it to `v*` tags — without both, the line
is decoration and anyone who can land a workflow file can read every secret in
it. Protection rules live in repository settings rather than in this file, so
they are worth checking rather than assuming:

```sh
gh api repos/jean-humann/gwnative/environments/release \
    --jq '.protection_rules[].type'
```

For the same reason the job runs no
third-party actions at all — no cache, no toolchain action, no release action —
since each would be someone else's code in a process that can reach a signing
key, and the cache is worth about two minutes.

Six secrets. The five Apple ones are named as they are wherever they came from,
so that moving one is a copy and never a rename:

| Secret | What it is |
| --- | --- |
| `APPLE_DEVELOPER_ID_APPLICATION_P12` | base64 of a `.p12` export of the Developer ID Application certificate and its key |
| `APPLE_DEVELOPER_ID_PASSWORD` | the export password for that `.p12` |
| `APPLE_NOTARY_KEY_P8` | base64 of the App Store Connect API key file |
| `APPLE_NOTARY_KEY_ID` | that key's ID |
| `APPLE_NOTARY_KEY_ISSUER` | the issuer UUID it belongs to |
| `SPARKLE_PRIVATE_KEY` | the EdDSA key that signs updates — see below |

The Apple five are recoverable: a certificate can be revoked and reissued, and
anyone who reinstalls by hand is unaffected. `SPARKLE_PRIVATE_KEY` is not. Its
public half is compiled into every copy already installed, so replacing it means
every one of those copies silently refuses every future update until its owner
downloads a build by hand. It is the one secret here worth backing up rather
than only protecting.

The keychain the job builds from them is created in `RUNNER_TEMP`, made default
so the scripts find it exactly as they do on a developer's machine, and deleted
in an `if: always()` step so it does not outlive a cancelled run. The private
key is imported without `-A`, so `codesign` can use it and nothing else in the
job can read it back out.

Half of this cannot be tested on the machine it was written on — a keychain that
has never held a certificate behaves differently from one that has — so the
workflow takes a `dry_run` input that does everything including the submission
to Apple and stops before publishing:

```sh
gh workflow run release.yml -f dry_run=true
```

It still needs the approval, because it still reaches the secrets. What it
proves is the part a local run cannot: that the certificate imports and chains,
that Apple accepts the build, and that the ticket staples to the image. Use it
after touching anything in the signing path; the alternative is a first real run
whose notarization log arrives after the release is public.

The two certificates in `packaging/certs` are imported alongside it. They are
Apple's public Developer ID intermediates and they are committed rather than
fetched, because a keychain created from nothing has no path from the leaf to
the Apple Root — the identity is present, `find-identity` marks it
`CSSMERR_TP_NOT_TRUSTED`, and `codesign` declines it. See the README there.

## Updating, and the two paths it takes

There are two, and which one a build takes is decided by whether Sparkle loaded.
They are not alternatives anybody chose between: the second exists because the
first cannot be present in a build that was never packaged.

**In a bundle: Sparkle.** `packaging/sparkle` holds the framework, committed at
a pinned version with the checksum of the tarball it came from, for the reason
the certificates next door are committed — a release build should not depend on
somebody else's web server being up, and a binary fetched during a build is one
nobody has looked at. `scripts/bundle` thins it to `arm64`, drops the XPC
services (they exist to reach the network from inside an App Sandbox, and this
application is not sandboxed), re-signs it innermost-first with the same
Developer ID, and writes the feed URL and public key into `Info.plist`. 3.0 MB
becomes 1.6 MB, and the application is 5.1 MB in total.

The feed is a single release asset,
`releases/latest/download/appcast.xml`, written by `scripts/appcast` at publish
time. One item long, always the current release: Sparkle shows the notes of the
item it selects, so a longer feed would buy nothing anybody sees and would cost
a file that has to be merged on every release. Those notes are the release's
own, rendered to HTML by GitHub, so the update panel shows what the release page
shows.

**Everything else: `src/release.rs`.** `cargo run` builds, benchmarks and the
test harness have no `Contents/Frameworks` to load a framework from. `build.rs`
links Sparkle *weakly* so they still start, `src/updater.rs` asks the
Objective-C runtime whether the classes are actually there rather than assuming,
and when they are not this is what answers the question — the repository's own
release list, `GET /repos/<owner>/<name>/releases`, anonymous, compared against
this build's version. Tags are its whole interface: `vX.Y.Z`, optionally
`-alpha.N`, `-beta.N` or `-rc.N`, and anything else is skipped rather than
guessed at, so a stable install is never offered a prerelease. It cannot install
anything; the one answer with anywhere to go opens the releases page, and the
button says so.

The same fallback covers a bundle built without a signing key, which is what a
fresh clone produces. No key means no `SUPublicEDKey`, which means no feed URL,
which means `updater::available()` is false and nothing about the build changed:
a feed nobody can sign is a feed nothing should trust.

### The two switches

Off by default, both of them, because a launch that asks GitHub about itself is
doing something on the player's behalf that they did not ask for at that moment.

| Setting | What it allows |
| --- | --- |
| `autoCheckUpdates` | looking for a newer build at all |
| `autoInstallUpdates` | downloading it and installing it on quit, without asking |

Sparkle keeps both in the application's user defaults and says, in as many
words, not to keep a second copy — because its own update window carries an
"install automatically in the future" checkbox, and a launch that pushed a
stored profile over the top would quietly undo the box a player had just ticked.
This project keeps one anyway, in `settings.json`, because the settings panel is
a web page and cannot read `NSUserDefaults`. What keeps that honest is the
direction of the copy: **Sparkle's answer is the truth**, every launch reads it
and writes the profile to match, and the profile is pushed the other way in
exactly two cases — the first launch after this shipped, where Sparkle has no
stored answer and an existing opt-in would otherwise be lost, and a player moving
the switch in the panel a moment ago.

Asked, never volunteered, either way. On the fallback path nothing checks at
launch unless `autoCheckUpdates` is on, and even then only once a day, and even
then only a genuinely newer version is allowed to interrupt. "Could not check"
is never reported as "up to date" — being told you are current by a request that
never left the machine is the one answer that stops you looking again.

### The key, generated once

Sparkle verifies every download against the public key compiled into the
application before it unpacks it, which is what makes installing from a URL this
project does not control safe. That needs a keypair, and it is the one step
nobody can do for you: the private half must never be in this repository, in a
config file, or in anybody's shell history.

```sh
packaging/sparkle/generate_keys
```

It writes the private key into your login keychain and prints the public one.
Put that printed string — the contents of the `<string>` it shows, nothing else
— in `packaging/sparkle/public-key.txt` and commit it. It is public; it is
supposed to travel with the source.

```sh
packaging/sparkle/generate_keys -x private-key.txt
gh secret set SPARKLE_PRIVATE_KEY < private-key.txt
rm private-key.txt
```

That is the copy CI signs with. Delete the file afterwards — the keychain still
has it, and `generate_keys -p` prints the public half again whenever it is
needed.

Back it up somewhere you would still have in a year. Losing it is not like
losing the Developer ID certificate, which can be revoked and reissued: the
public half is inside every copy already installed, so a new key means every one
of those copies silently refuses every future update until its owner downloads a
build by hand. `scripts/appcast` guards the near miss — it verifies each
signature against `packaging/sparkle/public-key.txt` before writing it into a
feed, because a release signed with the wrong key is accepted by GitHub, looks
perfect on the page, and is refused by every client without a word.

## Continuous integration

`.github/workflows/ci.yml` runs on every push and pull request: `cargo fmt
--check`, `cargo clippy` with warnings denied, the tests, a release build and
`scripts/bundle`. On `macos-26`, which is Apple Silicon and the newest image
GitHub offers.

Newest, because the runner's SDK is a ceiling on what the code may call and it
should not sit below the SDK on the machine the code is written on — a symbol
that compiles locally and fails in CI is a failure about nobody's change.
Development happens on macOS 27 and there is no macOS 27 runner, so one major
version of drift is the floor of what is achievable here. That is separate from
the *deployment* floor, which is 15.2 and set in two places that have to agree:
`MACOSX_DEPLOYMENT_TARGET` in `.cargo/config.toml`, so `LC_BUILD_VERSION` says
15.2 rather than rustc's default of macOS 11, and `LSMinimumSystemVersion` in
the plist, which is the one Launch Services enforces.

The release build is a separate step from the tests on purpose — fat LTO with
one codegen unit and `panic = "abort"` is a different compilation, and
`scripts/release` should not be the first thing to discover that. The bundle
step runs without any certificate; `scripts/bundle` reports that it found none
and carries on, which is exactly what makes the step meaningful there.

## Licence

GPL-2.0-or-later, matching the upstream project.
