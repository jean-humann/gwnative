# Client artifact certification

This is the publishing runbook. The authoritative installation, runtime
fallback, failure-outcome, and audit state model is
[Client compatibility mechanism](client-compatibility.md).

ArenaNet publishes two official WebAssembly runtimes from one client source:
JSPI for engines with JavaScript Promise Integration and Asyncify for older
engines. They share game data, but they do not share control flow or byte
offsets. Asyncify instruments the suspendable call graph and adds functions,
types and globals, so a JSPI transform certificate is never reused as an
Asyncify certificate.

## What ArenaNet publishes

One official patch manifest currently names both complete runtime pairs:

| Runtime | Generated JavaScript | WebAssembly |
| --- | --- | --- |
| JSPI | `Gw.jspi.js` | `Gw.jspi.wasm` |
| Asyncify | `Gw.js` | `Gw.wasm` |

The same manifest also names `version.json` and the chunked `Gw.snapshot`.
gwnative reads one manifest snapshot, downloads and verifies the four runtime
files plus `version.json` in staging, and promotes that five-file set
failure-atomically. The snapshot remains content-addressed chunk storage rather
than a fifth assembled runtime file.

Three internal identities serve different purposes and must not be substituted:

- the **patch generation ID** is derived from manifest sizes and chunk hashes
  for all four runtime files plus `version.json`; it is known before download
  and drives installation rollback;
- the **artifact-family ID** is derived from the assembled SHA-256 of exactly
  the four JavaScript/WebAssembly files; it selects certification and does not
  change for a metadata-only `version.json` update;
- the **runtime-compatibility ID** is a domain-separated SHA-256 of one runtime
  name, Wasm hash, generated-JavaScript hash, transform ABI and selected output
  hash; it keys local fallback and player notices without confusing a glue-only
  change for the same transform or carrying a refusal into a fixed transformer
  or corrected certificate.

The client's reported program/build values are runtime diagnostics only. They
are neither complete file identities nor certification inputs.

Because every manifest chunk is addressed by its digest, a manifest update
during a download cannot silently mix bytes. The later publishing stage fetches
again and requires the same four-file family ID before signing.

## What a certificate can authorize

`certificates/builds.json` is data, not a patch language. Each runtime record
contains:

- exact SHA-256 identities for its official JavaScript and WebAssembly pair;
- the independently expected template-transform output SHA-256;
- five fixed bridge kinds implemented in the application;
- semantic call identities (function, target-call occurrence and total);
- exact hashes of every target and caller function body; and
- when available, an artifact-family proof for shared data, element and
  global-prefix bytes.

It cannot provide WebAssembly instructions, imports, exports, table entries or
native code. The application appends its five compiled-in forwarders and then a
separate structural verifier validates the resulting module with `wasmparser`,
checks the appended types and bodies, and proves that existing code changed only
at the authorized call operands before accepting the pinned output hash.

The read-only companion layout is shared by the two runtime records only after
the section proof matches both exact artifacts. If it does not, candidate
generation keeps `layout` null but still verifies the independent template
transforms. A generated candidate always sets `passiveEnhancements` to `false`;
login, map transition, socket suspension, cursor and target fixtures must pass
on both runtimes before a reviewer enables it. Template save never reads those
memory offsets.

## Fast patch workflow

The normal ArenaNet patch cycle does not require a gwnative release:

1. Fetch all four official files into one directory.
2. Run:

   ```sh
   scripts/client-certify WEB_ROOT
   ```

3. Review `certificates/builds.candidate.json`. Every target body must still
   match its previous exact anchor. If a function moved or changed, the
   generator fails instead of blessing the body at its old index; investigate
   and update the reviewed anchors before continuing.
4. Run the candidate transform and live login, map-transition, socket,
   cursor and target fixtures for both runtimes. Keep `passiveEnhancements`
   off unless both layouts pass.
5. From a machine with the dedicated certificate key in its login Keychain,
   sign the reviewed feed:

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

The publishing path has four deliberately small jobs:

1. an unprivileged job fetches all four official runtime artifacts, generates
   an unsigned candidate with enhancements disabled, verifies both transforms,
   and exports only the derived family identity;
2. a second no-secret job fetches all four files again, reproduces the
   candidate, requires the same derived identity, repeats both transform tests,
   and uploads only the verified JSON;
3. a fresh `certificate-publishing` job, restricted to `main` and held for
   reviewer approval, downloads that JSON and signs it with the dedicated
   certificate key without checking out or executing repository code;
4. a final no-secret job downloads the signed pair and opens a certificate-only
   pull request.

This catches an artifact change between fetches instead of signing whichever
bytes happened to arrive last, and it keeps the private key out of every job
that compiles project code. Its `certify_passive_enhancements` checkbox is an
explicit attestation reviewed at the protected signing gate that both live
runtime fixtures passed. Without it the signed certificate enables template
saving only. The workflow never merges its pull request.

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
4. After the two no-secret fetch/test jobs agree on the family identity,
   approve the protected signing job. It signs only their reproduced JSON; a
   separate no-secret job opens the certificate-only pull request.
5. Review and merge that pull request. Existing applications download the
   signed feed in the background and use it on their next launch; no gwnative
   application release is required.

If candidate generation cannot locate the old semantic anchors, review the
new modules and update the certification data. An application release is
needed only if the compiled transform or its ABI must change, not merely
because function indices moved.

## Trust and rollback

The certificate bundled into an application is covered by the app's code
signature. A downloaded or cached replacement must verify against the dedicated
Ed25519 certificate public key compiled into the app. The corresponding private
key exists only in the protected `certificate-publishing` environment (and an
operator's Keychain); candidate generation, ordinary CI, and app releases never
receive or use it.

Feed `sequence` is monotonic. A validly signed lower sequence is ignored, a bad
signature falls back to the bundled feed, and a refresh is written only after
the JSON/signature pair verifies. Refreshing happens in the background and
takes effect on the next launch, so network availability is not part of boot.
The feed retains the 256 most recently certified artifact families, in
certification order, and the derived cache retains only the active artifact and
transform ABI. Frequent ArenaNet patches therefore do not create unbounded
signed metadata or local Wasm storage.

Certification is never a playability gate. An unknown pair is served directly
from ArenaNet with template saving and passive tools disabled. If an exact
certified transform cannot instantiate or fails before first frame, gwnative
remembers only that runtime-compatibility ID and retries the same official module.
Only a subsequent failed attempt of the unmodified client can reject and roll
back the ArenaNet patch generation. The client files and their active manifest
are stashed and restored together.
