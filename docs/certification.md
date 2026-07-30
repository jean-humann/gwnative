# Client artifact certification

ArenaNet publishes two official WebAssembly runtimes from one client source:
JSPI for engines with JavaScript Promise Integration and Asyncify for older
engines. They share game data, but they do not share control flow or byte
offsets. Asyncify instruments the suspendable call graph and adds functions,
types and globals, so a JSPI transform certificate is never reused as an
Asyncify certificate.

## What a certificate can authorize

`certificates/builds.json` is data, not a patch language. Each runtime record
contains:

- exact SHA-256 identities for its official JavaScript and WebAssembly pair;
- the independently expected template-transform output SHA-256;
- five fixed bridge kinds implemented in the application;
- semantic call identities (function, target-call occurrence and total);
- hashes of stub bodies where the target is a stub; and
- an artifact-family proof for the shared data, element and global-prefix bytes.

It cannot provide WebAssembly instructions, imports, exports, table entries or
native code. The application appends its five compiled-in forwarders, validates
the resulting module with `wasmparser`, and independently proves that only the
function and code sections changed before accepting the pinned output hash.

The read-only companion layout is shared by the two runtime records only after
the section proof matches both exact artifacts. A generated candidate always
sets `passiveEnhancements` to `false`; login, map transition, socket suspension,
cursor and target fixtures must pass on both runtimes before a reviewer enables
it. Template save can be certified independently because it does not use those
memory offsets.

## Fast patch workflow

The normal ArenaNet patch cycle does not require a gwnative release:

1. Fetch all four official files into one directory.
2. Run:

   ```sh
   scripts/client-certify WEB_ROOT
   ```

3. Review `certificates/builds.candidate.json`. If semantic anchors moved, the
   generator fails instead of guessing. Investigate and update the fixed
   transform before continuing.
4. Run the candidate transform and live login, map-transition, socket,
   cursor and target fixtures for both runtimes. Keep `passiveEnhancements`
   off unless both layouts pass.
5. From the protected certificate-publishing environment, sign the reviewed
   feed:

   ```sh
   cp certificates/builds.candidate.json certificates/builds.json
   scripts/certificate-sign --publish certificates/builds.json
   ```

6. Review and merge the certificate-only pull request.

The manual `Client certificate` workflow automates fetching, candidate
generation, static validation and creation of that signed review pull request.
No publisher build number is an input. The generator derives a stable family
identity from the exact JSPI and Asyncify Wasm/JavaScript hashes.

The same workflow scans the official files every six hours, because ArenaNet
does not provide a release event this project can subscribe to:

- a known family passes silently;
- a new family whose two template transforms pass opens one tracking issue;
- moved semantic anchors open one transformer-review issue and fail the scan;
- transient download failures fail the Actions run but never alter or sign the
  feed.

The issue, rather than an ArenaNet build-number feed, starts the human part of
certification. There is no need to know when ArenaNet plans a release.

The workflow has two trust stages:

1. an unprivileged job fetches the official pair, generates an unsigned
   candidate with enhancements disabled, verifies both transforms, and exports
   only the derived family identity;
2. the protected publishing job fetches the files again, reproduces the
   candidate, requires the same derived identity, repeats both transform tests,
   and only then receives the signing key.

This catches an artifact change between stages instead of signing whichever
bytes happened to arrive last. Its `certify_passive_enhancements` checkbox is
an explicit attestation, behind the protected publishing environment, that
both live runtime fixtures passed. Without it the signed certificate enables
template saving only. The workflow never merges its pull request.

## Operator runbook for a detected family

When the automatic issue appears:

1. The scheduled scan has already proved both exact template outputs. Generate
   the same candidate in a temporary test checkout and confirm its family ID
   matches the issue.
2. Run the local, unshipped enhancement candidate through JSPI on macOS 27 and
   Asyncify on macOS 26. Cover sign-in, a map transition, socket suspension,
   cursor and target readout. Watch for traps, rewinds and duplicate resumes.
3. If both live fixtures pass, dispatch `Client certificate` with `publish`
   and `certify_passive_enhancements` enabled. If only template saving passes,
   publish with passive enhancements disabled instead.
4. Approve the protected publishing environment. It refetches everything,
   requires the same family identity, repeats both transforms, signs the feed,
   and opens a certificate-only pull request.
5. Review and merge that pull request. Existing applications download the
   signed feed in the background and use it on their next launch; no gwnative
   application release is required.

If candidate generation cannot locate the old semantic anchors, review the
new modules and update the certification data. An application release is
needed only if the compiled transform or its ABI must change, not merely
because function indices moved.

## Trust and rollback

The certificate bundled into an application is covered by the app's code
signature. A downloaded or cached replacement must verify against the Ed25519
public key compiled into the app. The same protected secret currently used for
Sparkle signs the detached feed, but candidate generation and ordinary CI never
receive or use it.

Feed `sequence` is monotonic. A validly signed lower sequence is ignored, a bad
signature falls back to the bundled feed, and a refresh is written only after
the JSON/signature pair verifies. Refreshing happens in the background and
takes effect on the next launch, so network availability is not part of boot.
The feed retains the 32 most recently certified artifact families, in
certification order, and the derived cache retains only the active artifact and
transform ABI. Frequent ArenaNet patches therefore do not create unbounded
signed metadata or local Wasm storage.
