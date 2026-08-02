//! The fixed template-save transform and its structural verifier.
//!
//! Certificates identify calls semantically — the Nth call to the certified
//! target in a certified function — rather than by a byte offset. Asyncify
//! rewrites every suspendable body and therefore moves offsets even when the
//! source-level call graph is unchanged. The transform preserves whatever LEB
//! width ArenaNet used at the selected call, appends five fixed forwarders, and
//! then verifies the complete output from fresh parses before returning it.

use std::collections::HashMap;
use std::ops::Range;

use wasmparser::{
    BinaryReader, FunctionBody, GlobalSectionReader, ImportSectionReader, Operator, TypeRef,
    TypeSectionReader,
};

use super::certificate::{
    BridgeCertificate, BridgeKind, CallSiteCertificate, LayoutCertificate, RuntimeCertificate,
    TemplateCertificate,
};
use super::codec::{
    Section, WASM_HEADER, encode_code, encode_index_vector, encode_section, parse_code,
    parse_index_vector, read_uleb, section_by_id, sleb, split_sections, uleb,
};
use super::{Outcome, digest};

const CARRIER_MODULE: &str = "env";
const CARRIER_NAME: &str = "__syscall_newfstatat";
const BENCHMARK_EXPORT: &str = "__gwnative_e2e_benchmark_command";
const TRAVEL_UI_MESSAGE: i32 = 0x1000_0183;
const PREFERENCE_ENUM_UI_MESSAGE: i32 = 0x1000_0140;
const PREFERENCE_FLAG_UI_MESSAGE: i32 = 0x1000_0141;
const KAMADAN_MAP_ID: i32 = 449;
const AMERICA_REGION_ID: i32 = 0;
const ENGLISH_LANGUAGE_ID: i32 = 0;
const INTERACT_NPC_ACTION: i32 = 2;
const MAX_AGENT_ID_EXCLUSIVE: i32 = 4096;
const HIGH_ENUM_PREFERENCES: &[(u32, i32)] = &[(1, 4), (2, 3), (3, 3), (4, 4), (5, 4), (7, 0)];
const HIGH_NUMBER_PREFERENCES: &[(u32, i32)] = &[(21, 2), (22, 1)];
const HIGH_FLAG_PREFERENCES: &[u32] = &[82, 84, 97, 98, 100];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BenchmarkTargets {
    dispatcher: u32,
    interaction_dispatcher: u32,
    interaction_agent_validator: u32,
    enum_setter: u32,
    flag_setter: u32,
    number_setter: u32,
    enum_values: u32,
    flag_values: u32,
    number_values: u32,
    flag_bound: i32,
}

#[derive(Clone, Copy)]
pub(super) enum BenchmarkRuntime {
    Jspi,
    Asyncify,
}

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

#[derive(Clone, Debug)]
struct AuthorizedCall {
    input: Range<usize>,
    forwarder: u32,
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

fn certify_function_imports(input: &[u8], template: &TemplateCertificate) -> Outcome<()> {
    let sections = split_sections(input)?;
    let imports = ImportSectionReader::new(BinaryReader::new(section_by_id(&sections, 2)?, 0))
        .map_err(|e| format!("template-save: cannot read imports: {e}"))?;
    let mut function_count = 0u32;
    let mut carrier = None;
    for import in imports.into_imports() {
        let import = import.map_err(|e| format!("template-save: cannot read import: {e}"))?;
        if matches!(import.ty, TypeRef::Func(_) | TypeRef::FuncExact(_)) {
            if function_count == template.carrier_import {
                carrier = Some((import.module.to_owned(), import.name.to_owned()));
            }
            function_count = function_count
                .checked_add(1)
                .ok_or_else(|| "template-save: too many function imports".to_owned())?;
        }
    }
    if function_count != template.import_count {
        return Err(format!(
            "template-save: module has {function_count} function imports, certificate has {}",
            template.import_count
        ));
    }
    let carrier =
        carrier.ok_or_else(|| "template-save: carrier import is out of range".to_owned())?;
    if carrier.0 != CARRIER_MODULE || carrier.1 != CARRIER_NAME {
        return Err(format!(
            "template-save: carrier import is {}.{}, expected {CARRIER_MODULE}.{CARRIER_NAME}",
            carrier.0, carrier.1
        ));
    }
    Ok(())
}

fn build_expected(input: &[u8], certificate: &RuntimeCertificate) -> Outcome<Vec<u8>> {
    certify_function_imports(input, &certificate.template)?;
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
            let body = certify_caller(bodies, bridge, site)?;
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
    if digest(body) != bridge.stub_body_sha256 {
        return Err(format!(
            "template-save: {} has an unexpected body",
            bridge.kind.key()
        ));
    }
    Ok(())
}

fn certify_caller<'a>(
    bodies: &'a [Vec<u8>],
    bridge: &BridgeCertificate,
    site: &CallSiteCertificate,
) -> Outcome<&'a [u8]> {
    let body = bodies.get(site.local_function).ok_or_else(|| {
        format!(
            "template-save: {} call site is out of range",
            bridge.kind.key()
        )
    })?;
    if digest(body) != site.caller_body_sha256 {
        return Err(format!(
            "template-save: {} caller {} has an unexpected body",
            bridge.kind.key(),
            site.local_function
        ));
    }
    Ok(body)
}

fn authorized_calls(
    bodies: &[Vec<u8>],
    template: &TemplateCertificate,
) -> Outcome<HashMap<usize, Vec<AuthorizedCall>>> {
    let mut by_function: HashMap<usize, Vec<AuthorizedCall>> = HashMap::new();
    for (bridge_index, bridge) in template.bridges.iter().enumerate() {
        certify_stub(bodies, bridge)?;
        let target = template.import_count + bridge.stub_function as u32;
        let forwarder = template.import_count
            + bodies.len() as u32
            + u32::try_from(bridge_index)
                .map_err(|_| "template-save: too many forwarders".to_owned())?;
        for site in &bridge.call_sites {
            let body = certify_caller(bodies, bridge, site)?;
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
            let input = calls.get(site.occurrence).cloned().ok_or_else(|| {
                format!(
                    "template-save: {} call occurrence is out of range",
                    bridge.kind.key()
                )
            })?;
            by_function
                .entry(site.local_function)
                .or_default()
                .push(AuthorizedCall { input, forwarder });
        }
    }
    for calls in by_function.values_mut() {
        calls.sort_by_key(|call| call.input.start);
        for pair in calls.windows(2) {
            if pair[0].input.end > pair[1].input.start {
                return Err("template-save: overlapping certified call sites".to_owned());
            }
        }
    }
    Ok(by_function)
}

fn call_at(body: &[u8], offset: usize) -> Outcome<(u32, Range<usize>)> {
    let function = FunctionBody::new(BinaryReader::new(body, 0));
    let operators = function
        .get_operators_reader()
        .map_err(|e| format!("template-save: cannot read output function: {e}"))?
        .into_iter_with_offsets()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("template-save: cannot read output operator: {e}"))?;
    let Some((index, (operator, start))) = operators
        .iter()
        .enumerate()
        .find(|(_, (_, start))| *start == offset)
    else {
        return Err("template-save: authorized output call moved".to_owned());
    };
    let Operator::Call { function_index } = operator else {
        return Err("template-save: authorized output instruction is not a call".to_owned());
    };
    let end = operators
        .get(index + 1)
        .map_or(body.len(), |(_, next)| *next);
    Ok((*function_index, *start..end))
}

fn verify_body_mutations(input: &[u8], output: &[u8], calls: &[AuthorizedCall]) -> Outcome<()> {
    let mut input_cursor = 0usize;
    let mut output_cursor = 0usize;
    for call in calls {
        let unchanged = call
            .input
            .start
            .checked_sub(input_cursor)
            .ok_or_else(|| "template-save: authorized calls are out of order".to_owned())?;
        let output_unchanged_end = output_cursor
            .checked_add(unchanged)
            .filter(|end| *end <= output.len())
            .ok_or_else(|| "template-save: output body is truncated".to_owned())?;
        if input[input_cursor..call.input.start] != output[output_cursor..output_unchanged_end] {
            return Err(
                "template-save: existing body changed outside an authorized call".to_owned(),
            );
        }
        output_cursor = output_unchanged_end;
        let (target, output_call) = call_at(output, output_cursor)?;
        if target != call.forwarder {
            return Err(format!(
                "template-save: authorized call targets {target}, expected {}",
                call.forwarder
            ));
        }
        if output_call.len() < call.input.len() {
            return Err("template-save: call operand width was not preserved".to_owned());
        }
        input_cursor = call.input.end;
        output_cursor = output_call.end;
    }
    if input[input_cursor..] != output[output_cursor..] {
        return Err("template-save: existing body changed outside an authorized call".to_owned());
    }
    Ok(())
}

pub(super) fn verify_layout(input: &[u8], layout: &LayoutCertificate) -> Outcome<()> {
    let proof = layout_proof(input, layout.shared_global_count)?;
    if proof.data_sha256 != layout.data_sha256 {
        return Err("certificate: data section does not match the artifact family".to_owned());
    }
    if proof.element_sha256 != layout.element_sha256 {
        return Err("certificate: element section does not match the artifact family".to_owned());
    }
    if proof.shared_global_prefix_sha256 != layout.shared_global_prefix_sha256 {
        return Err("certificate: shared global prefix does not match".to_owned());
    }
    Ok(())
}

#[derive(Clone)]
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

    let before_types = parse_index_vector(section_by_id(&before, 3)?)?;
    let after_types = parse_index_vector(section_by_id(&after, 3)?)?;
    let expected_type_count = before_types
        .len()
        .checked_add(certificate.template.bridges.len())
        .ok_or_else(|| "template-save: function section is too large".to_owned())?;
    if after_types.len() != expected_type_count
        || after_types.get(..before_types.len()) != Some(before_types.as_slice())
    {
        return Err("template-save: existing function types changed".to_owned());
    }
    for (offset, bridge) in certificate.template.bridges.iter().enumerate() {
        let expected = before_types
            .get(bridge.stub_function)
            .ok_or_else(|| format!("template-save: {} stub has no type", bridge.kind.key()))?;
        if after_types.get(before_types.len() + offset) != Some(expected) {
            return Err(format!(
                "template-save: {} forwarder has the wrong type",
                bridge.kind.key()
            ));
        }
    }

    let before_bodies = parse_code(section_by_id(&before, 10)?)?;
    let after_bodies = parse_code(section_by_id(&after, 10)?)?;
    if before_types.len() != before_bodies.len() {
        return Err("template-save: function and code sections disagree".to_owned());
    }
    let expected_body_count = before_bodies
        .len()
        .checked_add(certificate.template.bridges.len())
        .ok_or_else(|| "template-save: code section is too large".to_owned())?;
    if after_bodies.len() != expected_body_count {
        return Err("template-save: unexpected appended function count".to_owned());
    }
    let authorized = authorized_calls(&before_bodies, &certificate.template)?;
    for (index, (original, rewritten)) in before_bodies.iter().zip(after_bodies.iter()).enumerate()
    {
        verify_body_mutations(
            original,
            rewritten,
            authorized.get(&index).map_or(&[], Vec::as_slice),
        )?;
    }
    for (offset, bridge) in certificate.template.bridges.iter().enumerate() {
        let target = certificate.template.import_count + bridge.stub_function as u32;
        let expected = forwarder(bridge.kind, certificate.template.carrier_import, target);
        if after_bodies.get(before_bodies.len() + offset) != Some(&expected) {
            return Err(format!(
                "template-save: {} forwarder body changed",
                bridge.kind.key()
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

/// Add the E2E runner's finite in-client scene command.
///
/// This is deliberately a wrapper rather than an export of the discovered
/// dispatcher. JavaScript can request only command 0 (travel to map 449,
/// America/English, district 1 or 2), command 1 (interact with one bounded
/// agent ID), or command 2 (apply the fixed high-quality benchmark preset).
/// The normal player module never receives the extra export.
pub(super) fn add_benchmark_api(
    input: &[u8],
    import_count: u32,
    targets: BenchmarkTargets,
    runtime: BenchmarkRuntime,
) -> Outcome<Vec<u8>> {
    match runtime {
        BenchmarkRuntime::Jspi if benchmark_targets(input, import_count)? != targets => {
            return Err("benchmark API: JSPI targets changed after paired proof".to_owned());
        }
        BenchmarkRuntime::Asyncify => {
            certify_asyncify_benchmark_targets(input, import_count, targets)?;
        }
        BenchmarkRuntime::Jspi => {}
    }
    let sections = split_sections(input)?;
    let function_types = parse_index_vector(section_by_id(&sections, 3)?)?;
    let bodies = parse_code(section_by_id(&sections, 10)?)?;
    if function_types.len() != bodies.len() {
        return Err("benchmark API: function and code sections disagree".to_owned());
    }
    let type_index = append_benchmark_type_index(section_by_id(&sections, 1)?)?;
    let function_index = import_count
        .checked_add(
            u32::try_from(bodies.len())
                .map_err(|_| "benchmark API: too many functions".to_owned())?,
        )
        .ok_or_else(|| "benchmark API: function index overflow".to_owned())?;
    let wrapper = benchmark_wrapper(targets);

    let mut output = WASM_HEADER.to_vec();
    for section in &sections {
        let body = match section.id {
            1 => append_benchmark_type(&section.body)?,
            3 => {
                let mut next = function_types.clone();
                next.push(type_index);
                encode_index_vector(&next)
            }
            7 => append_function_export(&section.body, BENCHMARK_EXPORT, function_index)?,
            10 => {
                let mut next = bodies.clone();
                next.push(wrapper.clone());
                encode_code(&next)
            }
            _ => section.body.clone(),
        };
        output.extend_from_slice(&encode_section(&Section {
            id: section.id,
            body,
        }));
    }
    verify_benchmark_api(input, &output, function_index, &wrapper)?;
    Ok(output)
}

fn benchmark_targets(input: &[u8], import_count: u32) -> Outcome<BenchmarkTargets> {
    let sections = split_sections(input)?;
    let bodies = parse_code(section_by_id(&sections, 10)?)?;
    let parameter_counts = function_parameter_counts(&sections)?;
    if parameter_counts.len() != bodies.len() {
        return Err("benchmark API: function and code sections disagree".to_owned());
    }
    let dispatcher = benchmark_ui_dispatcher(&bodies)?;
    let (interaction_dispatcher, interaction_agent_validator) =
        interaction_dispatcher(&bodies, &parameter_counts, import_count)?;
    let (enum_setter, enum_values) = preference_setter(
        &bodies,
        &parameter_counts,
        import_count,
        dispatcher,
        PREFERENCE_ENUM_UI_MESSAGE,
        enum_preference_guard,
        "enum",
    )?;
    let (flag_setter, flag_values) = preference_setter(
        &bodies,
        &parameter_counts,
        import_count,
        dispatcher,
        PREFERENCE_FLAG_UI_MESSAGE,
        |operators| {
            local_zero_bound(operators).is_some_and(|bound| {
                bound > HIGH_FLAG_PREFERENCES.last().copied().unwrap_or_default() as i32
                    && bound < 512
            })
        },
        "flag",
    )?;
    let (number_setter, number_values) =
        number_preference_setter(&bodies, &parameter_counts, import_count, dispatcher)?;
    let flag_body = bodies
        .get((flag_setter - import_count) as usize)
        .ok_or_else(|| "benchmark API: flag setter index is outside the code section".to_owned())?;
    let flag_bound = local_zero_bound(&function_operators(flag_body)?)
        .ok_or_else(|| "benchmark API: flag setter has no certified bound".to_owned())?;
    for (name, base, last) in [
        (
            "enum",
            enum_values,
            HIGH_ENUM_PREFERENCES
                .last()
                .map(|preference| preference.0)
                .unwrap_or_default(),
        ),
        (
            "flag",
            flag_values,
            HIGH_FLAG_PREFERENCES.last().copied().unwrap_or_default(),
        ),
        (
            "number",
            number_values,
            HIGH_NUMBER_PREFERENCES
                .last()
                .map(|preference| preference.0)
                .unwrap_or_default(),
        ),
    ] {
        if base
            .checked_add(last * 4)
            .is_none_or(|address| address > i32::MAX as u32)
        {
            return Err(format!(
                "benchmark API: {name} preference values are outside i32 memory"
            ));
        }
    }
    Ok(BenchmarkTargets {
        dispatcher,
        interaction_dispatcher,
        interaction_agent_validator,
        enum_setter,
        flag_setter,
        number_setter,
        enum_values,
        flag_values,
        number_values,
        flag_bound,
    })
}

/// Locate the compact JSPI implementation, then prove that Asyncify retained
/// the same functions, signatures, UI messages, and preference storage at the
/// same source-level indices. This avoids guessing through Asyncify's expanded
/// state-machine bodies while still adapting to a new paired ArenaNet build.
pub(super) fn benchmark_target_pair(
    jspi: &[u8],
    jspi_import_count: u32,
    asyncify: &[u8],
    asyncify_import_count: u32,
) -> Outcome<BenchmarkTargets> {
    if jspi_import_count != asyncify_import_count {
        return Err("benchmark API: paired runtimes have different function imports".to_owned());
    }
    let targets = benchmark_targets(jspi, jspi_import_count)?;
    certify_asyncify_benchmark_targets(asyncify, asyncify_import_count, targets)?;
    Ok(targets)
}

fn function_parameter_counts(sections: &[Section]) -> Outcome<Vec<usize>> {
    let function_types = parse_index_vector(section_by_id(sections, 3)?)?;
    let types = TypeSectionReader::new(BinaryReader::new(section_by_id(sections, 1)?, 0))
        .map_err(|error| format!("benchmark API: cannot read types: {error}"))?
        .into_iter_err_on_gc_types()
        .map(|function| {
            function
                .map(|function| function.params().len())
                .map_err(|error| format!("benchmark API: cannot read function type: {error}"))
        })
        .collect::<Outcome<Vec<_>>>()?;
    function_types
        .iter()
        .map(|type_index| {
            types
                .get(*type_index as usize)
                .copied()
                .ok_or_else(|| "benchmark API: function references a missing type".to_owned())
        })
        .collect()
}

fn asyncify_state_guard(operators: &[Operator<'_>]) -> bool {
    operators.windows(4).any(|window| {
        matches!(
            window,
            [
                Operator::GlobalGet { global_index },
                Operator::I32Const { value: 0 },
                Operator::I32Eq,
                Operator::If { .. },
            ] if *global_index != 0
        )
    })
}

fn asyncify_storage_proof(operators: &[Operator<'_>], base: u32) -> bool {
    let base_value = i32::try_from(base).ok();
    base_value.is_some_and(|base_value| {
        asyncify_state_guard(operators)
            && operators.iter().any(
                |operator| matches!(operator, Operator::I32Load { memarg } if memarg.offset == u64::from(base)),
            )
            && operators
                .iter()
                .any(|operator| matches!(operator, Operator::I32Const { value } if *value == base_value))
            && operators.iter().any(
                |operator| matches!(operator, Operator::I32Store { memarg } if memarg.offset == 0),
            )
    })
}

fn interaction_switch(operators: &[Operator<'_>]) -> bool {
    operators.iter().any(
        |operator| matches!(operator, Operator::BrTable { targets } if targets.len() == 6),
    ) && operators
        .iter()
        .any(|operator| matches!(operator, Operator::I32Const { value: 6 }))
        && operators
            .iter()
            .any(|operator| matches!(operator, Operator::I32LtS))
        && [0, 1, 2].iter().all(|wanted| {
            operators.iter().any(
                |operator| matches!(operator, Operator::LocalGet { local_index } if local_index == wanted),
            )
        })
}

fn interaction_dispatcher(
    bodies: &[Vec<u8>],
    parameter_counts: &[usize],
    import_count: u32,
) -> Outcome<(u32, u32)> {
    let mut matches = Vec::new();
    for (local, body) in bodies.iter().enumerate() {
        if parameter_counts.get(local) != Some(&3) {
            continue;
        }
        let operators = function_operators(body)?;
        let switches = operators
            .iter()
            .filter(
                |operator| matches!(operator, Operator::BrTable { targets } if targets.len() == 6),
            )
            .count();
        let direct_switch = operators.windows(2).any(|window| {
            matches!(
                window,
                [
                    Operator::LocalGet { local_index: 0 },
                    Operator::BrTable { targets },
                ] if targets.len() == 6
            )
        });
        let bounded_type = operators.windows(3).any(|window| {
            matches!(
                window,
                [
                    Operator::LocalGet { local_index: 0 },
                    Operator::I32Const { value: 6 },
                    Operator::I32LtS,
                ]
            )
        });
        let validator = operators.windows(3).find_map(|window| match window {
            [
                Operator::LocalGet { local_index: 1 },
                Operator::Call { function_index },
                Operator::BrIf { relative_depth: 0 },
            ] => Some(*function_index),
            _ => None,
        });
        let Some(validator) = validator else {
            continue;
        };
        let distinct_calls = operators
            .iter()
            .filter_map(|operator| match operator {
                Operator::Call { function_index } => Some(*function_index),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>()
            .len();
        if switches == 1
            && direct_switch
            && bounded_type
            && distinct_calls >= 10
            && restores_stack_pointer(&operators)
            && operators.windows(2).any(|window| {
                matches!(
                    window,
                    [
                        Operator::LocalGet { local_index: 2 },
                        Operator::BrIf { relative_depth: 0 }
                    ]
                )
            })
        {
            let function = import_count
                .checked_add(u32::try_from(local).map_err(|_| {
                    "benchmark API: interaction dispatcher index overflow".to_owned()
                })?)
                .ok_or_else(|| "benchmark API: interaction dispatcher index overflow".to_owned())?;
            matches.push((function, validator));
        }
    }
    match matches.as_slice() {
        [target] => Ok(*target),
        [] => Err("benchmark API: no interaction dispatcher was found".to_owned()),
        _ => Err(format!(
            "benchmark API: interaction dispatcher is ambiguous: {matches:?}"
        )),
    }
}

fn certify_asyncify_benchmark_targets(
    input: &[u8],
    import_count: u32,
    targets: BenchmarkTargets,
) -> Outcome<()> {
    let sections = split_sections(input)?;
    let bodies = parse_code(section_by_id(&sections, 10)?)?;
    let parameter_counts = function_parameter_counts(&sections)?;
    if parameter_counts.len() != bodies.len() {
        return Err("benchmark API: Asyncify function and code sections disagree".to_owned());
    }
    if benchmark_ui_dispatcher(&bodies)? != targets.dispatcher {
        return Err("benchmark API: paired runtimes disagree on the UI dispatcher".to_owned());
    }
    let interaction_local = targets
        .interaction_dispatcher
        .checked_sub(import_count)
        .and_then(|local| usize::try_from(local).ok())
        .ok_or_else(|| {
            "benchmark API: paired interaction dispatcher index is invalid".to_owned()
        })?;
    let interaction = bodies
        .get(interaction_local)
        .ok_or_else(|| "benchmark API: paired interaction dispatcher is missing".to_owned())?;
    let interaction_operators = function_operators(interaction)?;
    if parameter_counts.get(interaction_local) != Some(&3)
        || !asyncify_state_guard(&interaction_operators)
        || !interaction_switch(&interaction_operators)
        || !restores_stack_pointer(&interaction_operators)
        || !interaction_operators.iter().any(|operator| {
            matches!(operator, Operator::Call { function_index } if *function_index == targets.interaction_agent_validator)
        })
    {
        return Err(
            "benchmark API: paired Asyncify interaction dispatcher failed correspondence"
                .to_owned(),
        );
    }
    for (name, function, message, base) in [
        (
            "enum",
            targets.enum_setter,
            PREFERENCE_ENUM_UI_MESSAGE,
            targets.enum_values,
        ),
        (
            "flag",
            targets.flag_setter,
            PREFERENCE_FLAG_UI_MESSAGE,
            targets.flag_values,
        ),
    ] {
        let local = function
            .checked_sub(import_count)
            .and_then(|local| usize::try_from(local).ok())
            .ok_or_else(|| format!("benchmark API: paired {name} setter index is invalid"))?;
        let body = bodies
            .get(local)
            .ok_or_else(|| format!("benchmark API: paired {name} setter is missing"))?;
        let operators = function_operators(body)?;
        if parameter_counts.get(local) != Some(&3)
            || ui_message_calls(&operators, message, targets.dispatcher) != 1
            || !restores_stack_pointer(&operators)
            || !asyncify_storage_proof(&operators, base)
        {
            return Err(format!(
                "benchmark API: paired Asyncify {name} setter failed correspondence"
            ));
        }
        if name == "enum"
            && !operators
                .windows(2)
                .any(|window| matches!(window, [Operator::I32Const { value: 2 }, Operator::I32Shl]))
        {
            return Err("benchmark API: paired Asyncify enum indexing changed".to_owned());
        }
        if name == "flag"
            && (!operators.iter().any(
                |operator| matches!(operator, Operator::I32Const { value } if *value == targets.flag_bound),
            ) || !operators
                .iter()
                .any(|operator| matches!(operator, Operator::I32LtU)))
        {
            return Err("benchmark API: paired Asyncify flag bound changed".to_owned());
        }
    }
    let number_local = targets
        .number_setter
        .checked_sub(import_count)
        .and_then(|local| usize::try_from(local).ok())
        .ok_or_else(|| "benchmark API: paired number setter index is invalid".to_owned())?;
    let number = bodies
        .get(number_local)
        .ok_or_else(|| "benchmark API: paired number setter is missing".to_owned())?;
    let number_operators = function_operators(number)?;
    if parameter_counts.get(number_local) != Some(&3)
        || !restores_stack_pointer(&number_operators)
        || !asyncify_storage_proof(&number_operators, targets.number_values)
        || !number_operators
            .iter()
            .any(|operator| matches!(operator, Operator::CallIndirect { .. }))
    {
        return Err(
            "benchmark API: paired Asyncify number setter failed correspondence".to_owned(),
        );
    }
    Ok(())
}

fn function_operators(body: &[u8]) -> Outcome<Vec<Operator<'_>>> {
    FunctionBody::new(BinaryReader::new(body, 0))
        .get_operators_reader()
        .map_err(|error| format!("benchmark API: cannot read function: {error}"))?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("benchmark API: cannot read operator: {error}"))
}

fn benchmark_ui_dispatcher(bodies: &[Vec<u8>]) -> Outcome<u32> {
    let mut targets = HashMap::<u32, usize>::new();
    for body in bodies {
        let operators = function_operators(body)?;
        for (index, operator) in operators.iter().enumerate() {
            if !matches!(
                operator,
                Operator::I32Const {
                    value: TRAVEL_UI_MESSAGE
                }
            ) {
                continue;
            }
            let direct = operators.get(index + 1..index + 4).and_then(|tail| {
                if matches!(tail[0], Operator::LocalGet { .. })
                    && matches!(tail[1], Operator::I32Const { value: 0 })
                    && let Operator::Call { function_index } = tail[2]
                {
                    return Some(function_index);
                }
                None
            });
            let adjusted = operators.get(index + 1..index + 6).and_then(|tail| {
                if matches!(tail[0], Operator::LocalGet { .. })
                    && matches!(tail[1], Operator::I32Const { .. })
                    && matches!(tail[2], Operator::I32Add)
                    && matches!(tail[3], Operator::I32Const { value: 0 })
                    && let Operator::Call { function_index } = tail[4]
                {
                    return Some(function_index);
                }
                None
            });
            if let Some(target) = direct.or(adjusted) {
                *targets.entry(target).or_default() += 1;
            }
        }
    }
    let matches = targets
        .iter()
        .filter(|(_, occurrences)| **occurrences >= 3)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [(target, _)] => Ok(**target),
        [] => Err("benchmark API: no repeated kTravel dispatcher was found".to_owned()),
        _ => Err("benchmark API: kTravel resolves to multiple dispatchers".to_owned()),
    }
}

fn ui_message_calls(operators: &[Operator<'_>], message: i32, target: u32) -> usize {
    operators
        .iter()
        .enumerate()
        .filter(|(index, operator)| {
            matches!(operator, Operator::I32Const { value } if *value == message)
                && operators[index + 1..operators.len().min(index + 9)]
                    .iter()
                    .any(|tail| matches!(tail, Operator::Call { function_index } if *function_index == target))
        })
        .count()
}

fn local_zero_bound(operators: &[Operator<'_>]) -> Option<i32> {
    operators.windows(3).find_map(|window| match window {
        [
            Operator::LocalGet { local_index: 0 },
            Operator::I32Const { value },
            Operator::I32LtU,
        ] => Some(*value),
        _ => None,
    })
}

fn enum_preference_guard(operators: &[Operator<'_>]) -> bool {
    operators.windows(8).any(|window| {
        matches!(
            window,
            [
                Operator::LocalGet { local_index: 0 },
                Operator::I32Const { value: 2 },
                Operator::I32Shl,
                Operator::LocalTee { .. },
                Operator::I32Load { .. },
                Operator::LocalGet { local_index: 1 },
                Operator::I32Eq,
                Operator::BrIf { relative_depth: 0 },
            ]
        )
    })
}

fn indexed_value_base(operators: &[Operator<'_>]) -> Option<u32> {
    operators.windows(5).find_map(|window| match window {
        [
            Operator::LocalGet { local_index: 0 },
            Operator::I32Const { value: 2 },
            Operator::I32Shl,
            Operator::LocalTee { .. },
            Operator::I32Load { memarg },
        ] if memarg.offset > 1_000_000 && memarg.offset <= u64::from(u32::MAX) => {
            Some(memarg.offset as u32)
        }
        _ => None,
    })
}

fn restores_stack_pointer(operators: &[Operator<'_>]) -> bool {
    operators
        .iter()
        .any(|operator| matches!(operator, Operator::GlobalGet { global_index: 0 }))
        && operators
            .iter()
            .any(|operator| matches!(operator, Operator::GlobalSet { global_index: 0 }))
}

fn preference_setter(
    bodies: &[Vec<u8>],
    parameter_counts: &[usize],
    import_count: u32,
    dispatcher: u32,
    message: i32,
    valid_guard: impl Fn(&[Operator<'_>]) -> bool,
    kind: &str,
) -> Outcome<(u32, u32)> {
    let mut matches = Vec::new();
    for (local_index, body) in bodies.iter().enumerate() {
        let operators = function_operators(body)?;
        if parameter_counts.get(local_index) != Some(&3)
            || ui_message_calls(&operators, message, dispatcher) != 1
            || !valid_guard(&operators)
            || !restores_stack_pointer(&operators)
            || !operators
                .iter()
                .any(|operator| matches!(operator, Operator::LocalGet { local_index: 2 }))
        {
            continue;
        }
        if let Some(values) = indexed_value_base(&operators) {
            let function = import_count
                .checked_add(u32::try_from(local_index).map_err(|_| {
                    format!("benchmark API: {kind} preference function index overflow")
                })?)
                .ok_or_else(|| {
                    format!("benchmark API: {kind} preference function index overflow")
                })?;
            matches.push((function, values));
        }
    }
    match matches.as_slice() {
        [target] => Ok(*target),
        [] => Err(format!(
            "benchmark API: no {kind} preference setter was found"
        )),
        _ => Err(format!(
            "benchmark API: {kind} preference setter is ambiguous: {matches:?}"
        )),
    }
}

fn number_preference_setter(
    bodies: &[Vec<u8>],
    parameter_counts: &[usize],
    import_count: u32,
    dispatcher: u32,
) -> Outcome<(u32, u32)> {
    let mut initializers = Vec::new();
    for body in bodies {
        let operators = function_operators(body)?;
        if ui_message_calls(&operators, PREFERENCE_ENUM_UI_MESSAGE, dispatcher) >= 3
            && ui_message_calls(&operators, PREFERENCE_FLAG_UI_MESSAGE, dispatcher) >= 10
        {
            initializers.push(operators);
        }
    }
    let [initializer] = initializers.as_slice() else {
        return Err("benchmark API: preference initializer is not unique".to_owned());
    };
    let mut calls = HashMap::<u32, usize>::new();
    for operator in initializer {
        if let Operator::Call { function_index } = operator {
            *calls.entry(*function_index).or_default() += 1;
        }
    }
    let mut matches = Vec::new();
    for (target, count) in calls {
        if count < 20 || target < import_count {
            continue;
        }
        let Some(body) = bodies.get((target - import_count) as usize) else {
            continue;
        };
        if parameter_counts.get((target - import_count) as usize) != Some(&3) {
            continue;
        }
        let operators = function_operators(body)?;
        if operators.len() < 100
            || !restores_stack_pointer(&operators)
            || !operators
                .iter()
                .any(|operator| matches!(operator, Operator::CallIndirect { .. }))
            || !operators
                .iter()
                .any(|operator| matches!(operator, Operator::LocalGet { local_index: 2 }))
        {
            continue;
        }
        if let Some(values) = indexed_value_base(&operators) {
            matches.push((target, values));
        }
    }
    match matches.as_slice() {
        [target] => Ok(*target),
        [] => Err("benchmark API: no number preference setter was found".to_owned()),
        _ => Err("benchmark API: number preference setter is ambiguous".to_owned()),
    }
}

fn append_benchmark_type_index(body: &[u8]) -> Outcome<u32> {
    let mut cursor = 0;
    let count = read_uleb(body, &mut cursor)?;
    if cursor > body.len() {
        return Err("benchmark API: malformed type section".to_owned());
    }
    Ok(count)
}

fn append_benchmark_type(body: &[u8]) -> Outcome<Vec<u8>> {
    let mut cursor = 0;
    let count = read_uleb(body, &mut cursor)?;
    let mut output = uleb(u64::from(
        count
            .checked_add(1)
            .ok_or("benchmark API: too many types")?,
    ));
    output.extend_from_slice(&body[cursor..]);
    // One implicit recursion group containing (func (param i32 i32) (result i32)).
    output.extend_from_slice(&[0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f]);
    Ok(output)
}

fn append_function_export(body: &[u8], name: &str, target: u32) -> Outcome<Vec<u8>> {
    let reader = wasmparser::ExportSectionReader::new(BinaryReader::new(body, 0))
        .map_err(|error| format!("benchmark API: cannot read exports: {error}"))?;
    for export in reader {
        let export =
            export.map_err(|error| format!("benchmark API: cannot read export: {error}"))?;
        if export.name == name {
            return Err("benchmark API: export already exists".to_owned());
        }
    }
    let mut cursor = 0;
    let count = read_uleb(body, &mut cursor)?;
    let mut output = uleb(u64::from(
        count
            .checked_add(1)
            .ok_or("benchmark API: too many exports")?,
    ));
    output.extend_from_slice(&body[cursor..]);
    output.extend_from_slice(&uleb(name.len() as u64));
    output.extend_from_slice(name.as_bytes());
    output.push(0); // function export
    output.extend_from_slice(&uleb(u64::from(target)));
    Ok(output)
}

fn instruction_i32_const(body: &mut Vec<u8>, value: i32) {
    body.push(0x41);
    body.extend_from_slice(&sleb(i64::from(value)));
}

fn instruction_local_get(body: &mut Vec<u8>, local: u32) {
    body.push(0x20);
    body.extend_from_slice(&uleb(u64::from(local)));
}

fn store_i32(body: &mut Vec<u8>, local: u32, offset: u32, value: Option<i32>) {
    instruction_local_get(body, local);
    match value {
        Some(value) => instruction_i32_const(body, value),
        None => instruction_local_get(body, 1),
    }
    body.push(0x36);
    body.extend_from_slice(&uleb(2));
    body.extend_from_slice(&uleb(u64::from(offset)));
}

fn allocate_stack(body: &mut Vec<u8>, bytes: i32) {
    body.extend_from_slice(&[0x23, 0x00]); // global.get 0
    instruction_i32_const(body, bytes);
    body.push(0x6b); // i32.sub
    body.extend_from_slice(&[0x22, 0x02]); // local.tee 2
    body.extend_from_slice(&[0x24, 0x00]); // global.set 0
}

fn restore_stack(body: &mut Vec<u8>, bytes: i32) {
    instruction_local_get(body, 2);
    instruction_i32_const(body, bytes);
    body.push(0x6a); // i32.add
    body.extend_from_slice(&[0x24, 0x00]); // global.set 0
}

fn call_ui(body: &mut Vec<u8>, message: i32, target: u32) {
    instruction_i32_const(body, message);
    instruction_local_get(body, 2);
    instruction_i32_const(body, 0);
    body.push(0x10);
    body.extend_from_slice(&uleb(u64::from(target)));
}

fn call_preference_setter(body: &mut Vec<u8>, target: u32, preference: i32, value: i32) {
    instruction_i32_const(body, preference);
    instruction_i32_const(body, value);
    instruction_i32_const(body, 0); // do not persist the disposable benchmark preset
    body.push(0x10);
    body.extend_from_slice(&uleb(u64::from(target)));
}

fn check_preference(body: &mut Vec<u8>, base: u32, preference: u32, expected: i32) {
    instruction_i32_const(body, (base + preference * 4) as i32);
    body.push(0x28); // i32.load
    body.extend_from_slice(&uleb(2));
    body.extend_from_slice(&uleb(0));
    instruction_i32_const(body, expected);
    body.push(0x46); // i32.eq
    body.push(0x71); // i32.and with the accumulated result
}

fn benchmark_wrapper(targets: BenchmarkTargets) -> Vec<u8> {
    // Two i32 parameters (command, argument), then private i32 locals for the
    // temporary packet pointer and the fail-closed command result.
    let mut body = vec![0x01, 0x02, 0x7f];
    instruction_i32_const(&mut body, 0);
    body.extend_from_slice(&[0x21, 0x03]); // local.set result

    // command 0: fixed Kamadan/America/English travel, district argument 1/2.
    instruction_local_get(&mut body, 0);
    body.push(0x45); // i32.eqz
    body.extend_from_slice(&[0x04, 0x40]); // if
    instruction_local_get(&mut body, 1);
    instruction_i32_const(&mut body, 1);
    body.push(0x46); // i32.eq
    instruction_local_get(&mut body, 1);
    instruction_i32_const(&mut body, 2);
    body.push(0x46); // i32.eq
    body.push(0x72); // i32.or
    body.extend_from_slice(&[0x04, 0x40]); // if
    allocate_stack(&mut body, 16);
    store_i32(&mut body, 2, 0, Some(KAMADAN_MAP_ID));
    store_i32(&mut body, 2, 4, Some(AMERICA_REGION_ID));
    store_i32(&mut body, 2, 8, Some(ENGLISH_LANGUAGE_ID));
    store_i32(&mut body, 2, 12, None); // district argument
    call_ui(&mut body, TRAVEL_UI_MESSAGE, targets.dispatcher);
    restore_stack(&mut body, 16);
    instruction_i32_const(&mut body, 1);
    body.extend_from_slice(&[0x21, 0x03]); // result = accepted
    body.push(0x0b); // end district guard
    body.push(0x0b); // end travel command

    // command 1: use the game's own interaction dispatcher so an out-of-range
    // NPC is approached before interaction. A packet-only world action cannot
    // provide that pathing behavior.
    instruction_local_get(&mut body, 0);
    instruction_i32_const(&mut body, 1);
    body.push(0x46); // i32.eq
    body.extend_from_slice(&[0x04, 0x40]); // if
    instruction_local_get(&mut body, 1);
    body.push(0x45); // i32.eqz
    body.push(0x45); // i32.eqz (argument != 0)
    instruction_local_get(&mut body, 1);
    instruction_i32_const(&mut body, MAX_AGENT_ID_EXCLUSIVE);
    body.push(0x49); // i32.lt_u
    body.push(0x71); // i32.and
    body.extend_from_slice(&[0x04, 0x40]); // if
    instruction_i32_const(&mut body, INTERACT_NPC_ACTION);
    instruction_local_get(&mut body, 1); // certified agent ID
    instruction_i32_const(&mut body, 0); // do not emit a party call target
    body.push(0x10);
    body.extend_from_slice(&uleb(u64::from(targets.interaction_dispatcher)));
    instruction_i32_const(&mut body, 1);
    body.extend_from_slice(&[0x21, 0x03]); // result = accepted
    body.push(0x0b); // end agent guard
    body.push(0x0b); // end interact command

    // command 2: fixed, non-persistent high-quality benchmark preset. These
    // are Guild Wars' own preference setters, located from their bounds,
    // storage arrays and UI notifications rather than build-specific indices.
    instruction_local_get(&mut body, 0);
    instruction_i32_const(&mut body, 2);
    body.push(0x46); // i32.eq
    body.extend_from_slice(&[0x04, 0x40]); // if
    instruction_local_get(&mut body, 1);
    body.push(0x45); // argument == 0
    body.extend_from_slice(&[0x04, 0x40]); // if
    for &(preference, value) in HIGH_ENUM_PREFERENCES {
        call_preference_setter(&mut body, targets.enum_setter, preference as i32, value);
    }
    for &(preference, value) in HIGH_NUMBER_PREFERENCES {
        call_preference_setter(&mut body, targets.number_setter, preference as i32, value);
    }
    for &preference in HIGH_FLAG_PREFERENCES {
        call_preference_setter(&mut body, targets.flag_setter, preference as i32, 1);
    }

    // Read the setters' independently discovered value arrays. Returning one
    // is proof that the complete preset landed; unsupported future layouts
    // fail transformation before this wrapper can exist.
    instruction_i32_const(&mut body, 1);
    for &(preference, value) in HIGH_ENUM_PREFERENCES {
        check_preference(&mut body, targets.enum_values, preference, value);
    }
    for &(preference, value) in HIGH_NUMBER_PREFERENCES {
        check_preference(&mut body, targets.number_values, preference, value);
    }
    for &preference in HIGH_FLAG_PREFERENCES {
        check_preference(&mut body, targets.flag_values, preference, 1);
    }
    body.extend_from_slice(&[0x21, 0x03]); // local.set result
    body.push(0x0b); // end argument guard
    body.push(0x0b); // end preset command
    instruction_local_get(&mut body, 3);
    body.push(0x0b); // function end
    body
}

fn verify_benchmark_api(
    input: &[u8],
    output: &[u8],
    function_index: u32,
    wrapper: &[u8],
) -> Outcome<()> {
    wasmparser::validate(output)
        .map_err(|error| format!("benchmark API: invalid output: {error}"))?;
    let before = split_sections(input)?;
    let after = split_sections(output)?;
    if before.len() != after.len()
        || before
            .iter()
            .zip(&after)
            .any(|(left, right)| left.id != right.id)
    {
        return Err("benchmark API: section order changed".to_owned());
    }
    for (left, right) in before.iter().zip(&after) {
        if !matches!(left.id, 1 | 3 | 7 | 10) && left.body != right.body {
            return Err(format!(
                "benchmark API: unauthorized mutation of section {}",
                left.id
            ));
        }
    }
    let after_bodies = parse_code(section_by_id(&after, 10)?)?;
    if after_bodies.len() != parse_code(section_by_id(&before, 10)?)?.len() + 1
        || after_bodies.last().map(Vec::as_slice) != Some(wrapper)
    {
        return Err("benchmark API: wrapper body changed".to_owned());
    }
    let exports =
        wasmparser::ExportSectionReader::new(BinaryReader::new(section_by_id(&after, 7)?, 0))
            .map_err(|error| format!("benchmark API: cannot verify exports: {error}"))?;
    let matches = exports
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("benchmark API: cannot verify export: {error}"))?
        .into_iter()
        .filter(|export| export.name == BENCHMARK_EXPORT)
        .collect::<Vec<_>>();
    if matches.len() != 1
        || matches[0].kind != wasmparser::ExternalKind::Func
        || matches[0].index != function_index
    {
        return Err("benchmark API: exported wrapper does not match its proof".to_owned());
    }
    Ok(())
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
    for bridge in &certificate.template.bridges {
        certify_stub(&bodies, bridge)?;
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

    fn function_import_count(input: &[u8]) -> u32 {
        let sections = split_sections(input).unwrap();
        ImportSectionReader::new(BinaryReader::new(section_by_id(&sections, 2).unwrap(), 0))
            .unwrap()
            .into_imports()
            .map(|import| import.unwrap())
            .filter(|import| matches!(import.ty, TypeRef::Func(_) | TypeRef::FuncExact(_)))
            .count()
            .try_into()
            .unwrap()
    }

    fn module_with_function_imports(imports: &[(&str, &str)]) -> Vec<u8> {
        let mut body = uleb(imports.len() as u64);
        for (module, name) in imports {
            body.extend_from_slice(&uleb(module.len() as u64));
            body.extend_from_slice(module.as_bytes());
            body.extend_from_slice(&uleb(name.len() as u64));
            body.extend_from_slice(name.as_bytes());
            body.push(0x00); // function import
            body.push(0x00); // type index
        }
        let mut module = WASM_HEADER.to_vec();
        module.extend_from_slice(&encode_section(&Section { id: 2, body }));
        module
    }

    fn import_template() -> TemplateCertificate {
        TemplateCertificate {
            output_sha256: "0".repeat(64),
            import_count: 2,
            carrier_import: 1,
            bridges: Vec::new(),
        }
    }

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

    #[test]
    fn candidate_generation_rejects_a_reordered_carrier_import() {
        let template = import_template();
        let certified = module_with_function_imports(&[
            ("env", "same_typed_import"),
            (CARRIER_MODULE, CARRIER_NAME),
        ]);
        certify_function_imports(&certified, &template).unwrap();

        let reordered = module_with_function_imports(&[
            (CARRIER_MODULE, CARRIER_NAME),
            ("env", "same_typed_import"),
        ]);
        let reason = certify_function_imports(&reordered, &template).unwrap_err();
        assert!(
            reason.contains("expected env.__syscall_newfstatat"),
            "{reason}"
        );
    }

    #[test]
    fn structural_verifier_accepts_only_the_authorized_call_operand() {
        let input = [0x00, 0x41, 0x00, 0x10, 0x01, 0x1a, 0x0b];
        let output = [0x00, 0x41, 0x00, 0x10, 0xac, 0x02, 0x1a, 0x0b];
        let calls = [AuthorizedCall {
            input: 3..5,
            forwarder: 300,
        }];
        verify_body_mutations(&input, &output, &calls).unwrap();

        let mut wrong_target = output;
        wrong_target[4] = 0xad;
        assert!(verify_body_mutations(&input, &wrong_target, &calls).is_err());

        let mut unauthorized = output;
        unauthorized[6] = 0x01;
        assert!(verify_body_mutations(&input, &unauthorized, &calls).is_err());
    }

    #[test]
    fn candidate_generation_rejects_a_reused_index_with_a_changed_caller() {
        let original = vec![0x00, 0x10, 0x01, 0x0b];
        let bridge = BridgeCertificate {
            kind: BridgeKind::FileExists,
            stub_function: 0,
            stub_body_sha256: "0".repeat(64),
            call_sites: Vec::new(),
        };
        let site = CallSiteCertificate {
            local_function: 0,
            caller_body_sha256: digest(&original),
            occurrence: 0,
            expected_target_calls: 1,
        };

        certify_caller(std::slice::from_ref(&original), &bridge, &site).unwrap();
        let changed = vec![0x00, 0x01, 0x10, 0x01, 0x0b];
        assert!(
            certify_caller(&[changed], &bridge, &site).is_err(),
            "the same function index and target-call count do not certify new semantics"
        );
    }

    /// Exercise the semantic benchmark locator against locally downloaded
    /// ArenaNet artifacts without making the ordinary unit suite depend on
    /// proprietary files. CI/review runs opt in with explicit paths.
    #[test]
    #[ignore = "requires official ArenaNet artifacts"]
    fn external_benchmark_artifacts_accept_finite_api() {
        let load = |variable| {
            let path = std::env::var(variable)
                .unwrap_or_else(|_| panic!("set {variable} to an official Wasm artifact"));
            std::fs::read(&path)
                .unwrap_or_else(|error| panic!("cannot read {variable}={path}: {error}"))
        };
        let jspi = load("GWNATIVE_TEST_JSPI_WASM");
        let asyncify = load("GWNATIVE_TEST_ASYNCIFY_WASM");
        let jspi_imports = function_import_count(&jspi);
        let asyncify_imports = function_import_count(&asyncify);
        let targets =
            benchmark_target_pair(&jspi, jspi_imports, &asyncify, asyncify_imports).unwrap();
        for (name, input, imports, runtime) in [
            (
                "JSPI",
                jspi.as_slice(),
                jspi_imports,
                BenchmarkRuntime::Jspi,
            ),
            (
                "Asyncify",
                asyncify.as_slice(),
                asyncify_imports,
                BenchmarkRuntime::Asyncify,
            ),
        ] {
            let output = add_benchmark_api(input, imports, targets, runtime)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            wasmparser::validate(&output)
                .unwrap_or_else(|error| panic!("{name}: invalid output: {error}"));
        }
    }
}
