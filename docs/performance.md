# Performance and measurements

This document separates reproducible observations from architecture contracts.
Numbers describe one environment and are not minimum requirements or promises
for other hardware, WebKit builds, networks, or ArenaNet CDN state.

## Measurement environment

The baseline below was last consolidated on 2026-07-29:

- Apple M3 Pro
- macOS 27.0, build `26A5368g`
- the WKWebView created by gwnative, not Safari

Record the macOS build, hardware, cache state, render scale, display refresh
rate, and measurement window when updating a result.

## WebKit capability probe

| Capability | Observed result |
| --- | --- |
| JSPI functional round trip | Suspend and resume returned `42` |
| WebGL2 on `OffscreenCanvas` | `WebGL2RenderingContext`, GLSL ES 3.00 |
| `transferToImageBitmap` to `bitmaprenderer` | Full round trip |
| IndexedDB | Open and write; 20.6 GB reported quota |
| Secure context | Yes on loopback |
| Cross-origin isolation | Yes; `SharedArrayBuffer` available |
| `WEBGL_compressed_texture_s3tc` | Present |
| WebAssembly memory | WebKit allowed 4 GiB; the client declares a 2 GiB maximum |

At 3840×2160, the measured frame transfer cost was:

| Operation | p50 | p95 |
| --- | ---: | ---: |
| `transferToImageBitmap` | 0.20 ms | 0.30 ms |
| `transferFromImageBitmap` | 0.02 ms | — |

Both the offscreen path and a direct-to-visible control were vsync-bound at
60 fps in that probe, so these are observed costs, not a rendering ceiling.

Known WebKit gaps include `navigator.keyboard.getLayoutMap`, constructible
`Touch`/`TouchEvent` on macOS, `EXT_disjoint_timer_query_webgl2`,
`OVR_multiview2`, and an unmasked GPU renderer string. The architecture guide
describes the implemented input fallbacks.

## Benchmark harness

`scripts/benchmark` compares this project with a configurable Electron reference
checkout.

Prerequisites:

```sh
export GWNATIVE_REFERENCE_ROOT=/path/to/reference-checkout
```

Build `target/release/gwnative`, run `pnpm install` and `pnpm build` in
the reference checkout, then:

```sh
scripts/benchmark --json readings.json
scripts/benchmark --seconds 180 --rounds 7 --json readings.json
scripts/benchmark --only gwnative --json readings.json
```

Blank mode is the default. Every round creates a fresh named gwnative profile,
a unique `login:benchmark-…` Keychain account, a non-persistent WebKit store,
and a disposable Chromium `--user-data-dir`. It never infers an installed
profile from `HOME` or the password database.

Warm mode accepts only an explicitly prepared fixture containing
`benchmark-source.json`; there is no default-profile path. Pass it with
`--warm-gwnative` or `--warm-reference`. The runner hashes bytes and mtimes,
uses an APFS clone/reflink when available, runs only against the disposable
clone, hashes the source again, and refuses the result if the source changed.

Every cell requires at least five clean rounds. Two-build protocols alternate
execution order. The required JSON retains raw samples, manifests, profile and
source hashes, rejection reasons, and statistics derived from those samples.
The manifest records app/runtime/artifact hashes, certificate sequence and
families, selected JSPI/Asyncify and direct/isolated paths, render and display
conditions, window mode, cache/image state, macOS/WebKit build, power/thermal
state, and the exact scene/readiness limitation.

### What is compared

External sampling measures:

- wall-clock milestones;
- peak and final resident memory across attributed processes;
- physical footprint across attributed processes;
- CPU seconds across attributed processes;
- process count; and
- bytes written to the disposable profile.

Each application's own first-frame timing is reported, but the runner does not
compare the values unless both manifests name the same explicitly controlled
scene/readiness state. Train A performs no login, character selection,
movement, targeting, skill use, or other gameplay automation.

WebKit services are launchd children rather than children of the host. The
harness accepts only an exact fresh WebContent/GPU/Networking service set that
ends with the host. Missing, duplicate, unknown, or surviving candidates make
the sample invalid rather than data to average.

Physical footprint is the primary memory comparison because it approximates
the process's physical cost. RSS includes shared clean pages and can invert a
comparison. A summed high-water mark is a ceiling: individual process peaks may
not have occurred at the same instant.

### Train A result status

No Train A comparison is published by this repository. Real-machine runs,
reference-build preparation, live scene readiness, and any warm fixtures remain
external evidence gates. Historical figures gathered under the earlier
single-run/real-profile protocol are intentionally not carried forward as if
they satisfied this protocol.

## Measured design decisions

### Fixed loopback origin

A custom `WKURLSchemeHandler` produced a secure context and IndexedDB, but
WebKit clamped `performance.now()` to 1 ms. Loopback measured roughly 0.02 ms
resolution. A fixed port also preserves the IndexedDB origin across launches.

### Connection-per-thread server

The original close-after-response server created hundreds of short-lived
threads during boot. HTTP/1.1 keep-alive removed that churn. WebKit bounds
persistent connections per origin, and the game WebSocket remains long-lived,
so a worker pool would replace a small bounded set of long-lived connection
threads with an equivalent number of workers plus a queue.

Revisit this decision if the protocol, connection lifetime, or browser
connection cap changes; do not generalise it to an internet-facing server.

### Open chunk descriptors

Real-cache microbenchmark, 2,000 chunks, 20 iterations, 32 KiB windows:

| Path | Time |
| --- | ---: |
| Open + `fstat` + `pread` + close | 23.56 µs/op |
| One file per chunk, descriptor held open | 2.82 µs/op |
| One sparse file, descriptor held open | 2.39 µs/op |

Holding the descriptor provided most of the gain. The store therefore keeps up
to 2,048 immutable chunk files open. A single sparse snapshot was rejected
because scattered APFS writes materialised the 4.2 GB file in testing, defeating
on-demand storage.

### Chunk durability

On the measured APFS volume, writing 256 KiB chunks cost:

| Durability action | Time per chunk |
| --- | ---: |
| `F_FULLFSYNC` plus directory sync | 6.74 ms |
| POSIX `fsync` | 0.41 ms |
| No flush | 0.36 ms |

Rust's standard sync methods use the stronger macOS barrier. The cache now calls
POSIX `fsync` and leaves the rename unflushed. Chunks are immutable,
content-addressed, re-hashed on first session use, and re-downloadable, so a
power-loss casualty costs one chunk fetch rather than permanent user data.

### Fetch concurrency and QoS

The store allows 48 HTTP/2 chunk fetches, while speculative work can consume at
most 32. Demand reads retain capacity. Before first frame, fetching is
user-initiated work because the player is waiting for it; after first frame,
background filling moves to Utility QoS.

### Hidden and occluded windows

Current WebKit can stop animation frames, throttle timers, and suppress the web
content process when hidden. That stalled first-install boot work in testing.
gwnative disables the five relevant WebKit feature keys when they are exposed
and races each pre-first-frame animation request with a 16 ms timer.

The timer rescue ends at first frame. A running game then uses native
`requestAnimationFrame` timing so animation and audio are not driven from a
substitute clock. If WebKit removes a feature key, the host logs it and falls
back to that WebKit version's default.

### Render scale

At the login screen, 30 one-second samples across the host and WebKit services:

| Render scale | CPU | RSS |
| --- | ---: | ---: |
| `1×` | 24.2% | 559.9 MiB |
| `2×` | 26.0% | 608.5 MiB |

The 48.6 MiB memory difference is consistent with the larger drawing buffer and
GPU allocation. The CPU gap depends on scene complexity. `2×` remains the
Retina-native default; the setting exposes the trade-off.

## Updating measurements

1. Use alternating runs, not one application's morning against another's
   afternoon.
2. Record every run and report min, median, and max when the distribution is
   multimodal.
3. Keep cache state and render scale comparable.
4. Measure the whole application tree, including WebKit or Electron services.
5. Exclude samples with demonstrably wrong process attribution and say why.
6. Preserve raw JSON with `--json`.
7. Update the date and environment at the top of this document.

Do not promote a single run to a project claim.
