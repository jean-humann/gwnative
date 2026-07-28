//! Appending the forwarders and repointing the certified call sites.
//!
//! Rewriting appends rather than inserts, so every existing function index keeps
//! its meaning and only the chosen call sites change. Call sites are repointed
//! in place using a five-byte padded index — the width LLVM emits for
//! relocatable call targets — so no body changes length and no offset downstream
//! of a patch moves.

use super::builds::{BridgeKind, KnownBuild};
use super::codec::{
    Section, WASM_HEADER, encode_code, encode_index_vector, encode_section, padded_call,
    parse_code, parse_index_vector, section_by_id, sleb, split_sections, uleb,
};
use super::{Outcome, digest};

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
pub(super) fn rewrite(input: &[u8], build: &KnownBuild) -> Outcome<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::super::builds::{ALL_BRIDGE_KINDS, BUILDS, find_build};
    use super::*;

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
            note!("skipping: {} has not been fetched", base.display());
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
}
