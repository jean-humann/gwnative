//! The template-save compatibility transform, and the WebAssembly section
//! codec it needs.
//!
//! ArenaNet's Emscripten build ships four `Base/Os` file routines unimplemented.
//! Creating a directory always fails with error 2, so a build cannot be saved.
//! Enumerating a directory does nothing and deriving a name from an entry writes
//! nothing, so "Load from Skills Template" stays empty. Deleting a file is
//! `assert("not implemented")` followed by `unreachable`, so removing or
//! renaming a build **aborts the client**. A fifth routine is implemented but
//! wrong: the probe that asks whether a rename's destination is taken opens the
//! file `O_RDWR | O_CREAT`, so it creates the file it is testing for and every
//! rename is refused.
//!
//! None of that is repairable from JavaScript. The client never reaches a
//! syscall, and the module imports no `mkdir`, `getdents` or `unlink`. So one
//! derived module — accepted only for an exact official hash — appends
//! forwarders and repoints the template, chat-log and screenshot call sites at
//! them. Each forwarder hands the stub's own arguments to
//! `__syscall_newfstatat` behind a dirfd that no real call can produce, and
//! `web/template-save.js` answers it against the mounted IDBFS.
//!
//! Rewriting appends rather than inserts, so every existing function index keeps
//! its meaning and only the chosen call sites change. Call sites are repointed
//! in place using a five-byte padded index — the width LLVM emits for
//! relocatable call targets — so no body changes length and no offset downstream
//! of a patch moves.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::Digest;

/// A structural or policy fault. These are all "this is not the build we
/// certified", which is never worth failing a launch over: the caller falls
/// back to the untransformed module and the player loses template save, not the
/// game.
type Fault = String;

type Outcome<T> = std::result::Result<T, Fault>;

pub const WASM_HEADER: [u8; 8] = [0, 97, 115, 109, 1, 0, 0, 0];

/// Width LLVM uses for relocatable call targets, so a repoint fits in place.
const PADDED_INDEX_BYTES: usize = 5;

// ---------------------------------------------------------------- codec

pub struct Section {
    pub id: u8,
    pub body: Vec<u8>,
}

pub fn uleb(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

pub fn sleb(mut value: i64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let sign = byte & 0x40 != 0;
        if (value == 0 && !sign) || (value == -1 && sign) {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// Deliberately capped at five bytes. This reads an 8 MB module we did not
/// build; a wider read would silently accept a malformed index rather than
/// reporting it.
fn read_uleb(bytes: &[u8], cursor: &mut usize) -> Outcome<u32> {
    let mut result = 0u32;
    let mut shift = 0;
    for _ in 0..PADDED_INDEX_BYTES {
        let byte = *bytes.get(*cursor).ok_or("wasm: truncated LEB128")?;
        *cursor += 1;
        result |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
    Err("wasm: oversized LEB128".to_owned())
}

/// Fixed-width index, so a rewritten `call` stays byte-for-byte as long.
fn padded_index(mut value: u32) -> [u8; PADDED_INDEX_BYTES] {
    let mut out = [0u8; PADDED_INDEX_BYTES];
    for (index, slot) in out.iter_mut().enumerate() {
        let last = index == PADDED_INDEX_BYTES - 1;
        *slot = (value as u8 & 0x7f) | if last { 0 } else { 0x80 };
        value >>= 7;
    }
    out
}

/// `call` with a padded target, the six bytes a certified call site holds.
fn padded_call(function: u32) -> Vec<u8> {
    let mut out = vec![0x10];
    out.extend_from_slice(&padded_index(function));
    out
}

pub fn split_sections(bytes: &[u8]) -> Outcome<Vec<Section>> {
    if bytes.len() < WASM_HEADER.len() || bytes[..WASM_HEADER.len()] != WASM_HEADER {
        return Err("wasm: invalid WebAssembly header".to_owned());
    }
    let mut sections = Vec::new();
    let mut cursor = WASM_HEADER.len();
    while cursor < bytes.len() {
        let id = bytes[cursor];
        cursor += 1;
        let size = read_uleb(bytes, &mut cursor)? as usize;
        let end = cursor
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or("wasm: truncated section")?;
        sections.push(Section {
            id,
            body: bytes[cursor..end].to_vec(),
        });
        cursor = end;
    }
    Ok(sections)
}

pub fn section_by_id(sections: &[Section], id: u8) -> Outcome<&[u8]> {
    sections
        .iter()
        .find(|section| section.id == id)
        .map(|section| section.body.as_slice())
        .ok_or_else(|| format!("wasm: missing section {id}"))
}

pub fn encode_section(section: &Section) -> Vec<u8> {
    let mut out = vec![section.id];
    out.extend_from_slice(&uleb(section.body.len() as u64));
    out.extend_from_slice(&section.body);
    out
}

pub fn parse_index_vector(bytes: &[u8]) -> Outcome<Vec<u32>> {
    let mut cursor = 0;
    let count = read_uleb(bytes, &mut cursor)?;
    let mut values = Vec::with_capacity(count as usize);
    for _ in 0..count {
        values.push(read_uleb(bytes, &mut cursor)?);
    }
    if cursor != bytes.len() {
        return Err("wasm: malformed index vector".to_owned());
    }
    Ok(values)
}

pub fn encode_index_vector(values: &[u32]) -> Vec<u8> {
    let mut out = uleb(values.len() as u64);
    for value in values {
        out.extend_from_slice(&uleb(u64::from(*value)));
    }
    out
}

pub fn parse_code(bytes: &[u8]) -> Outcome<Vec<Vec<u8>>> {
    let mut cursor = 0;
    let count = read_uleb(bytes, &mut cursor)?;
    let mut bodies = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let size = read_uleb(bytes, &mut cursor)? as usize;
        let end = cursor
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or("wasm: truncated function body")?;
        bodies.push(bytes[cursor..end].to_vec());
        cursor = end;
    }
    if cursor != bytes.len() {
        return Err("wasm: malformed code section".to_owned());
    }
    Ok(bodies)
}

pub fn encode_code(bodies: &[Vec<u8>]) -> Vec<u8> {
    let mut out = uleb(bodies.len() as u64);
    for body in bodies {
        out.extend_from_slice(&uleb(body.len() as u64));
        out.extend_from_slice(body);
    }
    out
}

// ------------------------------------------------------------- the build

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BridgeKind {
    EnsureDirectory,
    FindFiles,
    FileBaseName,
    DeleteFile,
    FileExists,
}

impl BridgeKind {
    /// The dirfd this bridge travels behind. Far outside any fd a real call can
    /// produce, and negative, so the carrier's own callers cannot collide with
    /// it. `web/template-save.js` receives these from the host rather than
    /// restating them, so the side that writes them into the module and the side
    /// that answers them cannot drift apart.
    pub const fn marker(self) -> i64 {
        match self {
            Self::EnsureDirectory => -70_001,
            Self::FindFiles => -70_002,
            Self::FileBaseName => -70_003,
            Self::DeleteFile => -70_004,
            Self::FileExists => -70_005,
        }
    }

    /// The name the page knows this by.
    pub const fn key(self) -> &'static str {
        match self {
            Self::EnsureDirectory => "ensureDirectory",
            Self::FindFiles => "findFiles",
            Self::FileBaseName => "fileBaseName",
            Self::DeleteFile => "deleteFile",
            Self::FileExists => "fileExists",
        }
    }
}

pub const ALL_BRIDGE_KINDS: [BridgeKind; 5] = [
    BridgeKind::EnsureDirectory,
    BridgeKind::FindFiles,
    BridgeKind::FileBaseName,
    BridgeKind::DeleteFile,
    BridgeKind::FileExists,
];

pub struct CallSite {
    /// Index into the code section, i.e. function index minus import count.
    pub local_function: usize,
    /// Byte offset of the `call` opcode inside that function body.
    pub body_offset: usize,
}

pub struct StubBridge {
    pub kind: BridgeKind,
    /// The function the certified call sites currently target. Its type is
    /// reused for the forwarder, and `FileExists` also calls it.
    pub stub_function: usize,
    /// Whole body of the stub, so an unexpected build fails closed. `None` when
    /// the target is a real implementation rather than a stub — there the call
    /// site's own target index is the certification.
    pub stub_body: Option<&'static [u8]>,
    pub call_sites: &'static [CallSite],
}

pub struct KnownBuild {
    pub sha256: &'static str,
    pub output_sha256: &'static str,
    pub import_count: u32,
    /// Import index of `__syscall_newfstatat`, the bridge's carrier.
    pub carrier_import: u32,
    pub bridges: &'static [StubBridge],
}

pub const BUILDS: &[KnownBuild] = &[KnownBuild {
    sha256: "b0319704f3072d6948a66026a35af5eb0af12b48d70986783c293e7c77e98483",
    output_sha256: "68c6e09cec0f6992058a44a5617ca9eac7fab4697be1421943bbf664e6d444f6",
    import_count: 219,
    carrier_import: 207,
    bridges: &[
        // PathCreateDirectory(path, recursive) -> error. `i32.const 2` is
        // ERROR_FILE_NOT_FOUND, returned unconditionally.
        StubBridge {
            kind: BridgeKind::EnsureDirectory,
            stub_function: 185,
            stub_body: Some(&[0x00, 0x41, 0x02, 0x0b]),
            call_sites: &[
                CallSite {
                    local_function: 9538,
                    body_offset: 171,
                }, // template save
                CallSite {
                    local_function: 11525,
                    body_offset: 142,
                }, // chat log
                CallSite {
                    local_function: 12214,
                    body_offset: 127,
                }, // screenshot
            ],
        },
        // FindFiles(out, pattern, flags) -> void. An empty body leaves the
        // caller's list zeroed, so every directory reads as empty.
        StubBridge {
            kind: BridgeKind::FindFiles,
            stub_function: 186,
            stub_body: Some(&[0x00, 0x0b]),
            call_sites: &[
                CallSite {
                    local_function: 9527,
                    body_offset: 157,
                }, // skills list
                CallSite {
                    local_function: 9528,
                    body_offset: 157,
                }, // equipment list
                CallSite {
                    local_function: 11525,
                    body_offset: 210,
                }, // chat log
                CallSite {
                    local_function: 12214,
                    body_offset: 419,
                }, // screenshot
            ],
        },
        // FileBaseName(dst, _, baseDir, _, path, dstChars) -> written.
        // `i32.const 0` leaves the caller reading uninitialised stack. Only the
        // two template lists are repointed; the model paths that also call it
        // keep the fallback branch they take today.
        StubBridge {
            kind: BridgeKind::FileBaseName,
            stub_function: 197,
            stub_body: Some(&[0x00, 0x41, 0x00, 0x0b]),
            call_sites: &[
                CallSite {
                    local_function: 9527,
                    body_offset: 276,
                },
                CallSite {
                    local_function: 9528,
                    body_offset: 278,
                },
            ],
        },
        // DeleteFile(path) -> deleted. Not a silent stub like the others: the
        // body is `assert("not implemented")` followed by `unreachable`, so
        // deleting a build aborted the client outright.
        StubBridge {
            kind: BridgeKind::DeleteFile,
            stub_function: 333,
            stub_body: Some(&[
                0x00, //
                0x41, 0x9e, 0x87, 0xc5, 0x80, 0x00, //
                0x41, 0x80, 0xbb, 0xc3, 0x80, 0x00, //
                0x41, 0xc8, 0x06, //
                0x10, 0xc2, 0x82, 0x80, 0x80, 0x00, //
                0x00, //
                0x0b,
            ]),
            call_sites: &[CallSite {
                local_function: 459,
                body_offset: 201,
            }],
        },
        // File::Open(path, mode, err). Mode 1 is meant to open an existing
        // file, and 9757 uses it to ask whether a rename's destination is
        // already taken — but in this build mode 1 opens O_RDWR|O_CREAT, the
        // same as the write mode. The probe creates the file it is testing for,
        // reports it present, and refuses every rename.
        //
        // Only the probe is repointed. The write call five instructions later
        // keeps the real function, and so does the load path.
        StubBridge {
            kind: BridgeKind::FileExists,
            stub_function: 552,
            stub_body: None,
            call_sites: &[CallSite {
                local_function: 9538,
                body_offset: 201,
            }],
        },
    ],
}];

pub fn find_build(sha256: &str) -> Option<&'static KnownBuild> {
    BUILDS.iter().find(|build| build.sha256 == sha256)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Body of a forwarder that hands the stub's arguments to
/// `__syscall_newfstatat(dirfd, path, buffer, flags)` behind a dirfd marker.
fn forwarder(kind: BridgeKind, carrier: u32, target: u32) -> Vec<u8> {
    let local = |index: u8| [0x20, index];
    let mut body = vec![0x00]; // no locals
    body.push(0x41); // i32.const
    body.extend_from_slice(&sleb(kind.marker()));

    let call = |body: &mut Vec<u8>| {
        body.push(0x10);
        body.extend_from_slice(&uleb(u64::from(carrier)));
    };

    match kind {
        // (path, recursive) -> error
        BridgeKind::EnsureDirectory => {
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&[0x41, 0x00]);
            body.extend_from_slice(&local(1));
            call(&mut body);
        }
        // (path, mode, err) -> handle. Ask the host first; only open when the
        // file is really there, so the probe cannot create its own answer.
        BridgeKind::FileExists => {
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&[0x41, 0x00]);
            body.extend_from_slice(&[0x41, 0x00]);
            call(&mut body);
            body.extend_from_slice(&[0x04, 0x7f]); // if (result i32)
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&local(1));
            body.extend_from_slice(&local(2));
            body.push(0x10);
            body.extend_from_slice(&uleb(u64::from(target)));
            body.push(0x05); // else
            body.extend_from_slice(&[0x41, 0x00]);
            body.push(0x0b); // end if
        }
        // (path) -> deleted
        BridgeKind::DeleteFile => {
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&[0x41, 0x00]);
            body.extend_from_slice(&[0x41, 0x00]);
            call(&mut body);
        }
        // (out, pattern, flags) -> void
        BridgeKind::FindFiles => {
            body.extend_from_slice(&local(1));
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&local(2));
            call(&mut body);
            body.push(0x1a); // drop
        }
        // (dst, _, baseDir, _, path, dstChars) -> written
        BridgeKind::FileBaseName => {
            body.extend_from_slice(&local(4));
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&local(5));
            call(&mut body);
        }
    }
    body.push(0x0b); // end
    body
}

/// Rewrite `input` into the derived module for `build`.
///
/// There is no `WebAssembly.validate` on this side, and it would add nothing:
/// the output hash is pinned in the table above, so a byte-exact match proves
/// the result is the same module the transform's own certification validated.
/// A hash check is the stronger of the two, not a substitute for it.
pub fn rewrite(input: &[u8], build: &KnownBuild) -> Outcome<Vec<u8>> {
    let input_hash = digest(input);
    if input_hash != build.sha256 {
        return Err(format!("template-save: unsupported input {input_hash}"));
    }

    let sections = split_sections(input)?;
    let function_types = parse_index_vector(section_by_id(&sections, 3)?)?;
    let bodies = parse_code(section_by_id(&sections, 10)?)?;
    if function_types.len() != bodies.len() {
        return Err("template-save: function and code sections disagree".to_owned());
    }

    let mut next_types = function_types.clone();
    let mut next_bodies = bodies;

    for bridge in build.bridges {
        let kind = bridge.kind.key();
        let stub = next_bodies
            .get(bridge.stub_function)
            .ok_or_else(|| format!("template-save: missing stub for {kind}"))?;
        if let Some(expected) = bridge.stub_body
            && stub.as_slice() != expected
        {
            return Err(format!("template-save: {kind} is not the expected stub"));
        }

        // Appending keeps every existing function index valid, so only the
        // chosen call sites change meaning. The forwarder reuses the stub type.
        let forwarder_index = build.import_count + next_bodies.len() as u32;
        let stub_type = *function_types
            .get(bridge.stub_function)
            .ok_or_else(|| format!("template-save: {kind} stub has no type"))?;
        next_types.push(stub_type);
        next_bodies.push(forwarder(
            bridge.kind,
            build.carrier_import,
            build.import_count + bridge.stub_function as u32,
        ));

        let expected = padded_call(build.import_count + bridge.stub_function as u32);
        let replacement = padded_call(forwarder_index);
        for site in bridge.call_sites {
            let body = next_bodies
                .get_mut(site.local_function)
                .ok_or_else(|| format!("template-save: {kind} call site is out of range"))?;
            let end = site.body_offset + expected.len();
            if end > body.len() || body[site.body_offset..end] != expected[..] {
                return Err(format!(
                    "template-save: {kind} call site signature mismatch"
                ));
            }
            body[site.body_offset..end].copy_from_slice(&replacement);
        }
    }

    let mut output = WASM_HEADER.to_vec();
    for section in &sections {
        let rewritten = match section.id {
            3 => Section {
                id: 3,
                body: encode_index_vector(&next_types),
            },
            10 => Section {
                id: 10,
                body: encode_code(&next_bodies),
            },
            _ => Section {
                id: section.id,
                body: section.body.clone(),
            },
        };
        output.extend_from_slice(&encode_section(&rewritten));
    }

    let output_hash = digest(&output);
    if output_hash != build.output_sha256 {
        return Err(format!("template-save: unexpected output {output_hash}"));
    }
    Ok(output)
}

// -------------------------------------------------------- derived cache

/// Bumped whenever a derived module stops being interchangeable with one an
/// older build published.
const TRANSFORM_ABI: u32 = 1;

const DERIVED: &str = "Gw.jspi.wasm";
const STAMP: &str = "derived.json";

/// The derived module for `base`, transforming only when the cache cannot prove
/// it already holds exactly this output.
///
/// Returns `Ok(None)` when the input is a build we have not certified, which is
/// the ordinary state of affairs the day after ArenaNet ships a patch. The
/// caller serves the untransformed module and template save goes back to being
/// broken — which is where it started, and much better than refusing to launch.
pub fn prepare(base: &Path, cache_root: &Path) -> Outcome<Option<PathBuf>> {
    let input = fs::read(base).map_err(|e| format!("template-save: {base:?}: {e}"))?;
    let input_hash = digest(&input);
    let Some(build) = find_build(&input_hash) else {
        // Nothing here can serve this input, and the entries are ~8 MB each.
        let _ = fs::remove_dir_all(cache_root);
        return Ok(None);
    };

    let dir = cache_root.join(&input_hash).join(TRANSFORM_ABI.to_string());
    let derived = dir.join(DERIVED);
    if usable(&dir, build) {
        return Ok(Some(derived));
    }

    let output = rewrite(&input, build)?;

    // Only after a successful transform: a failing one must leave whatever the
    // last good build published exactly where it is.
    let _ = fs::remove_dir_all(cache_root);
    fs::create_dir_all(&dir).map_err(|e| format!("template-save: {dir:?}: {e}"))?;
    write_atomic(&derived, &output)?;
    write_atomic(
        &dir.join(STAMP),
        serde_json::json!({
            "inputSha256": input_hash,
            "transformAbi": TRANSFORM_ABI,
            "outputSha256": build.output_sha256,
        })
        .to_string()
        .as_bytes(),
    )?;
    Ok(Some(derived))
}

/// Whether the entry in `dir` is provably the module this build certifies.
///
/// The stamp is not evidence on its own — anything that can write the cache can
/// write a stamp next to it. What settles it is hashing the file and comparing
/// against the constant compiled into this binary.
fn usable(dir: &Path, build: &KnownBuild) -> bool {
    let Ok(stamp) = fs::read(dir.join(STAMP)) else {
        return false;
    };
    let Ok(stamp) = serde_json::from_slice::<serde_json::Value>(&stamp) else {
        return false;
    };
    if stamp["transformAbi"].as_u64() != Some(u64::from(TRANSFORM_ABI))
        || stamp["inputSha256"].as_str() != Some(build.sha256)
    {
        return false;
    }
    fs::read(dir.join(DERIVED)).is_ok_and(|bytes| digest(&bytes) == build.output_sha256)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Outcome<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|e| format!("template-save: {temporary:?}: {e}"))?;
    fs::rename(&temporary, path).map_err(|e| format!("template-save: {path:?}: {e}"))
}

/// `{ ensureDirectory: -70001, … }`, for injection into the page.
pub fn markers_json() -> String {
    let pairs: Vec<String> = ALL_BRIDGE_KINDS
        .iter()
        .map(|kind| format!("\"{}\":{}", kind.key(), kind.marker()))
        .collect();
    format!("{{{}}}", pairs.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_round_trips_the_edges() {
        for value in [0u64, 1, 127, 128, 624_485, u64::from(u32::MAX)] {
            let encoded = uleb(value);
            let mut cursor = 0;
            assert_eq!(u64::from(read_uleb(&encoded, &mut cursor).unwrap()), value);
            assert_eq!(cursor, encoded.len());
        }
        assert_eq!(uleb(624_485), vec![0xe5, 0x8e, 0x26]);
        assert_eq!(sleb(0), vec![0x00]);
        assert_eq!(sleb(-1), vec![0x7f]);
        // The marker encoding the forwarders depend on, group by group:
        // -70001 = -547·128 + 15, -547 = -5·128 + 93, -5 terminates at 123.
        assert_eq!(sleb(-70_001), vec![0x8f, 0xdd, 0x7b]);
    }

    #[test]
    fn a_padded_index_is_always_five_bytes() {
        // The whole repoint depends on this: a call site is overwritten in
        // place, so a shorter encoding for a smaller index would shift every
        // instruction after it.
        for value in [0u32, 1, 219, 12_345, u32::MAX] {
            let padded = padded_index(value);
            assert_eq!(padded.len(), PADDED_INDEX_BYTES);
            let mut cursor = 0;
            assert_eq!(read_uleb(&padded, &mut cursor).unwrap(), value);
        }
    }

    #[test]
    fn an_oversized_leb_is_refused_rather_than_wrapped() {
        let mut cursor = 0;
        assert!(read_uleb(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00], &mut cursor).is_err());
    }

    #[test]
    fn sections_survive_a_round_trip() {
        let mut module = WASM_HEADER.to_vec();
        module.extend_from_slice(&encode_section(&Section {
            id: 3,
            body: vec![1, 2, 3],
        }));
        module.extend_from_slice(&encode_section(&Section {
            id: 10,
            body: vec![4],
        }));
        let sections = split_sections(&module).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(section_by_id(&sections, 3).unwrap(), &[1, 2, 3]);
        assert_eq!(section_by_id(&sections, 10).unwrap(), &[4]);
        assert!(section_by_id(&sections, 7).is_err());
    }

    #[test]
    fn a_truncated_section_is_refused() {
        let mut module = WASM_HEADER.to_vec();
        module.extend_from_slice(&[3, 40, 1, 2]); // declares 40 bytes, holds 2
        assert!(split_sections(&module).is_err());
    }

    #[test]
    fn something_that_is_not_wasm_is_refused() {
        assert!(split_sections(b"not a module at all").is_err());
        assert!(split_sections(&[]).is_err());
    }

    #[test]
    fn code_and_index_vectors_survive_a_round_trip() {
        let bodies = vec![vec![0x00, 0x0b], vec![0x00, 0x41, 0x02, 0x0b]];
        assert_eq!(parse_code(&encode_code(&bodies)).unwrap(), bodies);
        let values = vec![0u32, 7, 200, 40_000];
        assert_eq!(
            parse_index_vector(&encode_index_vector(&values)).unwrap(),
            values
        );
    }

    #[test]
    fn every_marker_is_distinct_and_out_of_reach_of_a_real_fd() {
        let mut seen = std::collections::HashSet::new();
        for kind in ALL_BRIDGE_KINDS {
            assert!(
                seen.insert(kind.marker()),
                "duplicate marker for {}",
                kind.key()
            );
            // A real dirfd is a small non-negative integer or AT_FDCWD (-100).
            assert!(kind.marker() < -1000);
        }
    }

    #[test]
    fn the_markers_reach_the_page_as_an_object() {
        let json: serde_json::Value = serde_json::from_str(&markers_json()).unwrap();
        assert_eq!(json["ensureDirectory"], -70_001);
        assert_eq!(json["fileExists"], -70_005);
        assert_eq!(json.as_object().unwrap().len(), ALL_BRIDGE_KINDS.len());
    }

    #[test]
    fn a_forwarder_ends_where_a_function_body_must() {
        for kind in ALL_BRIDGE_KINDS {
            let body = forwarder(kind, 207, 771);
            assert_eq!(body[0], 0x00, "{} declares locals", kind.key());
            assert_eq!(*body.last().unwrap(), 0x0b, "{} does not end", kind.key());
            // Every forwarder reaches the carrier exactly once.
            let call = {
                let mut call = vec![0x10];
                call.extend_from_slice(&uleb(207));
                call
            };
            assert_eq!(
                body.windows(call.len())
                    .filter(|window| **window == call[..])
                    .count(),
                1,
                "{} does not call the carrier once",
                kind.key()
            );
        }
    }

    #[test]
    fn an_uncertified_input_is_refused_rather_than_guessed_at() {
        let build = &BUILDS[0];
        assert!(rewrite(b"\0asm\x01\0\0\0", build).is_err());
        assert!(find_build("not a hash").is_none());
        assert!(find_build(build.sha256).is_some());
    }

    /// The whole port in one assertion.
    ///
    /// The output hash is pinned from a transform that was certified against
    /// this exact input, so producing it byte-for-byte proves every part of the
    /// rewrite: the LEB128 encoders, the section split and re-encode, each
    /// forwarder body, and every call-site offset. Nothing short of an exact
    /// match can pass, and no smaller test covers as much.
    ///
    /// Skipped when the client has not been fetched yet, which is the state of
    /// a clean checkout — this is the one test that needs an 8.2 MB artifact.
    #[test]
    fn the_real_client_transforms_to_the_certified_output() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("web/Gw.jspi.wasm");
        let Ok(input) = fs::read(&base) else {
            note!("skipping: {base:?} has not been fetched");
            return;
        };
        let Some(build) = find_build(&digest(&input)) else {
            note!("skipping: the fetched client is not a certified build");
            return;
        };
        let output = rewrite(&input, build).expect("the certified build rewrites");
        assert_eq!(digest(&output), build.output_sha256);
        // Appending five forwarders is the only growth; nothing is removed.
        assert!(output.len() > input.len());
    }

    #[test]
    fn the_table_is_self_consistent() {
        for build in BUILDS {
            assert_eq!(build.sha256.len(), 64);
            assert_eq!(build.output_sha256.len(), 64);
            assert!(build.carrier_import < build.import_count);
            let kinds: Vec<_> = build.bridges.iter().map(|b| b.kind).collect();
            for kind in ALL_BRIDGE_KINDS {
                assert!(kinds.contains(&kind), "{} has no bridge", kind.key());
            }
            for bridge in build.bridges {
                assert!(
                    !bridge.call_sites.is_empty(),
                    "{} repoints nothing",
                    bridge.kind.key()
                );
            }
        }
    }
}
