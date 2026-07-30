//! The fixed template-save transform and its independent verifier.
//!
//! Certificates identify calls semantically — the Nth call to the certified
//! target in a certified function — rather than by a byte offset. Asyncify
//! rewrites every suspendable body and therefore moves offsets even when the
//! source-level call graph is unchanged. The transform preserves whatever LEB
//! width ArenaNet used at the selected call, appends five fixed forwarders, and
//! then verifies the complete output from fresh parses before returning it.

use std::collections::HashMap;
use std::ops::Range;

use wasmparser::{BinaryReader, FunctionBody, GlobalSectionReader, Operator};

use super::certificate::{
    BridgeCertificate, BridgeKind, LayoutCertificate, RuntimeCertificate, TemplateCertificate,
};
use super::codec::{
    Section, WASM_HEADER, encode_code, encode_index_vector, encode_section, parse_code,
    parse_index_vector, section_by_id, sleb, split_sections, uleb,
};
use super::{Outcome, digest};

#[derive(Clone, Debug)]
struct Patch {
    local_function: usize,
    range: Range<usize>,
    replacement: Vec<u8>,
}

struct Plan {
    patches: Vec<Patch>,
    forwarders: Vec<Vec<u8>>,
}

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
        BridgeKind::EnsureDirectory => {
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&[0x41, 0x00]);
            body.extend_from_slice(&local(1));
            call(&mut body);
        }
        BridgeKind::FileExists => {
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&[0x41, 0x00]);
            body.extend_from_slice(&[0x41, 0x00]);
            call(&mut body);
            body.extend_from_slice(&[0x04, 0x7f]);
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&local(1));
            body.extend_from_slice(&local(2));
            body.push(0x10);
            body.extend_from_slice(&uleb(u64::from(target)));
            body.push(0x05);
            body.extend_from_slice(&[0x41, 0x00]);
            body.push(0x0b);
        }
        BridgeKind::DeleteFile => {
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&[0x41, 0x00]);
            body.extend_from_slice(&[0x41, 0x00]);
            call(&mut body);
        }
        BridgeKind::FindFiles => {
            body.extend_from_slice(&local(1));
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&local(2));
            call(&mut body);
            body.push(0x1a);
        }
        BridgeKind::FileBaseName => {
            body.extend_from_slice(&local(4));
            body.extend_from_slice(&local(0));
            body.extend_from_slice(&local(5));
            call(&mut body);
        }
    }
    body.push(0x0b);
    body
}

/// Encode an unsigned LEB using at least the width of the certified operand.
///
/// LLVM commonly leaves five-byte relocation slots, but neither the
/// certificate nor this transform assumes that. A canonical one-byte call and
/// a padded five-byte call are both preserved at their original width. If the
/// appended target crosses a LEB boundary, as it does in the larger Asyncify
/// module, the operand grows canonically and the enclosing body is re-sized.
fn fixed_uleb(mut value: u32, width: usize) -> Outcome<Vec<u8>> {
    if width == 0 || width > 5 {
        return Err(format!(
            "template-save: unsupported call operand width {width}"
        ));
    }
    let needed = ((32 - value.leading_zeros()).max(1) as usize).div_ceil(7);
    let width = width.max(needed);
    if width > 5 {
        return Err("template-save: replacement index does not fit".to_owned());
    }
    let mut out = Vec::with_capacity(width);
    for index in 0..width {
        let last = index + 1 == width;
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if last {
            out.push(byte);
        } else {
            out.push(byte | 0x80);
        }
    }
    Ok(out)
}

fn call_ranges(body: &[u8], target: u32) -> Outcome<Vec<Range<usize>>> {
    let function = FunctionBody::new(BinaryReader::new(body, 0));
    let operators = function
        .get_operators_reader()
        .map_err(|e| format!("template-save: cannot read function: {e}"))?
        .into_iter_with_offsets()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("template-save: cannot read operator: {e}"))?;
    let mut ranges = Vec::new();
    for (index, (operator, start)) in operators.iter().enumerate() {
        if matches!(operator, Operator::Call { function_index } if *function_index == target) {
            let end = operators
                .get(index + 1)
                .map_or(body.len(), |(_, offset)| *offset);
            ranges.push(*start..end);
        }
    }
    Ok(ranges)
}

fn build_expected(input: &[u8], certificate: &RuntimeCertificate) -> Outcome<Vec<u8>> {
    let sections = split_sections(input)?;
    let function_types = parse_index_vector(section_by_id(&sections, 3)?)?;
    let bodies = parse_code(section_by_id(&sections, 10)?)?;
    if function_types.len() != bodies.len() {
        return Err("template-save: function and code sections disagree".to_owned());
    }

    // `patches` needs the actual stub types. Keep its semantic scan separate
    // from construction, then derive the five types here.
    let Plan {
        patches: mut certified,
        forwarders,
    } = patches_without_types(&bodies, &certificate.template)?;
    certified.sort_by_key(|patch| (patch.local_function, patch.range.start));
    for pair in certified.windows(2) {
        if pair[0].local_function == pair[1].local_function
            && pair[0].range.end > pair[1].range.start
        {
            return Err("template-save: overlapping certified call sites".to_owned());
        }
    }

    let mut next_types = function_types.clone();
    for bridge in &certificate.template.bridges {
        next_types.push(
            *function_types
                .get(bridge.stub_function)
                .ok_or_else(|| format!("template-save: {} stub has no type", bridge.kind.key()))?,
        );
    }

    let mut by_function: HashMap<usize, Vec<&Patch>> = HashMap::new();
    for patch in &certified {
        by_function
            .entry(patch.local_function)
            .or_default()
            .push(patch);
    }
    let mut next_bodies = Vec::with_capacity(bodies.len() + forwarders.len());
    for (index, body) in bodies.iter().enumerate() {
        let Some(function_patches) = by_function.get(&index) else {
            next_bodies.push(body.clone());
            continue;
        };
        let mut rewritten = Vec::with_capacity(body.len());
        let mut cursor = 0;
        for patch in function_patches {
            rewritten.extend_from_slice(&body[cursor..patch.range.start]);
            rewritten.extend_from_slice(&patch.replacement);
            cursor = patch.range.end;
        }
        rewritten.extend_from_slice(&body[cursor..]);
        next_bodies.push(rewritten);
    }
    next_bodies.extend(forwarders.iter().cloned());

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
    Ok(output)
}

fn patches_without_types(bodies: &[Vec<u8>], template: &TemplateCertificate) -> Outcome<Plan> {
    let mut patches = Vec::new();
    let mut forwarders = Vec::with_capacity(template.bridges.len());
    for bridge in &template.bridges {
        certify_stub(bodies, bridge)?;
        let target = template.import_count + bridge.stub_function as u32;
        let forwarder_index = template.import_count
            + bodies.len() as u32
            + u32::try_from(forwarders.len())
                .map_err(|_| "template-save: too many forwarders".to_owned())?;
        forwarders.push(forwarder(bridge.kind, template.carrier_import, target));
        for site in &bridge.call_sites {
            let body = bodies.get(site.local_function).ok_or_else(|| {
                format!(
                    "template-save: {} call site is out of range",
                    bridge.kind.key()
                )
            })?;
            let calls = call_ranges(body, target)?;
            if calls.len() != site.expected_target_calls {
                return Err(format!(
                    "template-save: {} caller {} has {} target calls, expected {}",
                    bridge.kind.key(),
                    site.local_function,
                    calls.len(),
                    site.expected_target_calls
                ));
            }
            let range = calls[site.occurrence].clone();
            let mut replacement = vec![0x10];
            replacement.extend_from_slice(&fixed_uleb(forwarder_index, range.len() - 1)?);
            patches.push(Patch {
                local_function: site.local_function,
                range,
                replacement,
            });
        }
    }
    Ok(Plan {
        patches,
        forwarders,
    })
}

fn certify_stub(bodies: &[Vec<u8>], bridge: &BridgeCertificate) -> Outcome<()> {
    let body = bodies
        .get(bridge.stub_function)
        .ok_or_else(|| format!("template-save: missing stub for {}", bridge.kind.key()))?;
    if let Some(expected) = &bridge.stub_body_sha256
        && digest(body) != *expected
    {
        return Err(format!(
            "template-save: {} has an unexpected body",
            bridge.kind.key()
        ));
    }
    Ok(())
}

pub(super) fn verify_layout(input: &[u8], layout: &LayoutCertificate) -> Outcome<()> {
    let Some(data_hash) = layout.data_sha256.as_deref() else {
        return Ok(());
    };
    let proof = layout_proof(input, layout.shared_global_count.unwrap_or_default())?;
    if proof.data_sha256 != data_hash {
        return Err("certificate: data section does not match the build family".to_owned());
    }
    if proof.element_sha256 != layout.element_sha256.as_deref().unwrap_or_default() {
        return Err("certificate: element section does not match the build family".to_owned());
    }
    if proof.shared_global_prefix_sha256
        != layout
            .shared_global_prefix_sha256
            .as_deref()
            .unwrap_or_default()
    {
        return Err("certificate: shared global prefix does not match".to_owned());
    }
    Ok(())
}

pub(super) struct LayoutProof {
    pub data_sha256: String,
    pub element_sha256: String,
    pub shared_global_prefix_sha256: String,
}

pub(super) fn layout_proof(input: &[u8], global_count: u32) -> Outcome<LayoutProof> {
    let sections = split_sections(input)?;
    let globals = section_by_id(&sections, 6)?;
    let count = global_count as usize;
    let reader = GlobalSectionReader::new(BinaryReader::new(globals, 0))
        .map_err(|e| format!("certificate: cannot read globals: {e}"))?;
    if reader.count() < count as u32 {
        return Err("certificate: shared global prefix is truncated".to_owned());
    }
    let start = reader.original_position();
    let offsets = reader
        .into_iter_with_offsets()
        .map(|entry| {
            entry
                .map(|(offset, _)| offset)
                .map_err(|e| format!("certificate: cannot read global: {e}"))
        })
        .collect::<Outcome<Vec<_>>>()?;
    let end = offsets.get(count).copied().unwrap_or(globals.len());
    Ok(LayoutProof {
        data_sha256: digest(section_by_id(&sections, 11)?),
        element_sha256: digest(section_by_id(&sections, 9)?),
        shared_global_prefix_sha256: digest(&globals[start..end]),
    })
}

fn verify_output(input: &[u8], output: &[u8], certificate: &RuntimeCertificate) -> Outcome<()> {
    wasmparser::validate(output)
        .map_err(|e| format!("template-save: output is invalid WebAssembly: {e}"))?;
    let expected = build_expected(input, certificate)?;
    if output != expected {
        return Err("template-save: independent output comparison failed".to_owned());
    }

    let before = split_sections(input)?;
    let after = split_sections(output)?;
    if before.len() != after.len()
        || before
            .iter()
            .zip(&after)
            .any(|(left, right)| left.id != right.id)
    {
        return Err("template-save: section order changed".to_owned());
    }
    for (left, right) in before.iter().zip(&after) {
        if left.id != 3 && left.id != 10 && left.body != right.body {
            return Err(format!(
                "template-save: unauthorized mutation of section {}",
                left.id
            ));
        }
    }
    Ok(())
}

pub(super) fn rewrite(input: &[u8], certificate: &RuntimeCertificate) -> Outcome<Vec<u8>> {
    let input_hash = digest(input);
    if input_hash != certificate.wasm_sha256 {
        return Err(format!("template-save: unsupported input {input_hash}"));
    }
    let output = build_expected(input, certificate)?;
    verify_output(input, &output, certificate)?;
    let output_hash = digest(&output);
    if output_hash != certificate.template.output_sha256 {
        return Err(format!("template-save: unexpected output {output_hash}"));
    }
    Ok(output)
}

pub(super) fn recertify(
    input: &[u8],
    glue: &[u8],
    prototype: &RuntimeCertificate,
) -> Outcome<(RuntimeCertificate, Vec<u8>)> {
    wasmparser::validate(input)
        .map_err(|e| format!("certificate candidate: invalid input WebAssembly: {e}"))?;
    let sections = split_sections(input)?;
    let bodies = parse_code(section_by_id(&sections, 10)?)?;
    let mut certificate = prototype.clone();
    certificate.wasm_sha256 = digest(input);
    certificate.glue_sha256 = digest(glue);
    certificate.passive_enhancements = false;
    for bridge in &mut certificate.template.bridges {
        if bridge.stub_body_sha256.is_some() {
            let body = bodies.get(bridge.stub_function).ok_or_else(|| {
                format!("certificate candidate: missing {} stub", bridge.kind.key())
            })?;
            bridge.stub_body_sha256 = Some(digest(body));
        }
    }
    certificate.template.output_sha256 = "0".repeat(64);
    let output = build_expected(input, &certificate)?;
    verify_output(input, &output, &certificate)?;
    certificate.template.output_sha256 = digest(&output);
    Ok((certificate, output))
}

#[cfg(test)]
pub(super) fn candidate(input: &[u8], certificate: &RuntimeCertificate) -> Outcome<Vec<u8>> {
    let output = build_expected(input, certificate)?;
    verify_output(input, &output, certificate)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_leb_preserves_the_certified_width() {
        assert_eq!(fixed_uleb(3, 1).unwrap(), [3]);
        assert_eq!(
            fixed_uleb(17_608, 5).unwrap(),
            [0xc8, 0x89, 0x81, 0x80, 0x00]
        );
        assert_eq!(fixed_uleb(128, 1).unwrap(), [0x80, 0x01]);
        assert!(fixed_uleb(0, 0).is_err());
    }

    #[test]
    fn every_forwarder_is_closed_and_calls_the_carrier_once() {
        for kind in BridgeKind::ALL {
            let body = forwarder(kind, 207, 771);
            assert_eq!(body[0], 0);
            assert_eq!(*body.last().unwrap(), 0x0b);
            let mut call = vec![0x10];
            call.extend_from_slice(&uleb(207));
            assert_eq!(
                body.windows(call.len())
                    .filter(|window| **window == call)
                    .count(),
                1
            );
        }
    }
}
