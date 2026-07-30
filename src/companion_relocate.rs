//! Make the freestanding companion safe to instantiate over an existing heap.
//!
//! `wasm-ld --import-memory` imports a memory, but its ordinary executable
//! output still assumes that memory is new: the stack starts at zero, static
//! data starts at 1 MiB, and active data segments initialise that address.
//! Guild Wars owns those bytes already. Instantiating that output over the
//! client's memory therefore overwrites the beginning of the game's data
//! segment before `companion_init` is even called.
//!
//! The kernel is linked as position-independent code and this exact,
//! build-time transform turns its two linker globals into imports:
//!
//! - `__stack_pointer`, set to the top of a block allocated by the game;
//! - `__memory_base`, set to the beginning of that block; and
//! - `__data_base`, used only by the active data segment.
//!
//! JavaScript zeroes the whole allocated block before instantiation. Code and
//! BSS addresses are then relative to `__memory_base`, the stack grows inside
//! the block, and the merged data segment is initialised at `__data_base`.
//! Nothing in the companion writes a fixed client address anymore.

const WASM_HEADER: [u8; 8] = [0, 97, 115, 109, 1, 0, 0, 0];
const STACK_BYTES: u32 = 1_048_576;
const MANIFEST_SECTION: &str = "companion_manifest";
const RELOCATION_ABI: u32 = 1;

#[derive(Clone)]
struct Section {
    id: u8,
    body: Vec<u8>,
}

fn fault(message: impl std::fmt::Display) -> String {
    format!("companion relocation: {message}")
}

fn uleb(mut value: u64) -> Vec<u8> {
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

fn read_uleb(bytes: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let mut value = 0u32;
    let mut shift = 0;
    for _ in 0..5 {
        let byte = *bytes
            .get(*cursor)
            .ok_or_else(|| fault("truncated LEB128"))?;
        *cursor += 1;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
    Err(fault("oversized LEB128"))
}

fn read_sleb(bytes: &[u8], cursor: &mut usize) -> Result<i32, String> {
    let mut value = 0i32;
    let mut shift = 0;
    for _ in 0..5 {
        let byte = *bytes
            .get(*cursor)
            .ok_or_else(|| fault("truncated signed LEB128"))?;
        *cursor += 1;
        value |= i32::from(byte & 0x7f).wrapping_shl(shift);
        if byte & 0x80 == 0 {
            if shift + 7 < 32 && byte & 0x40 != 0 {
                value |= (!0i32) << (shift + 7);
            }
            return Ok(value);
        }
        shift += 7;
    }
    Err(fault("oversized signed LEB128"))
}

fn read_name<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a str, String> {
    let size = read_uleb(bytes, cursor)? as usize;
    let end = cursor
        .checked_add(size)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| fault("truncated name"))?;
    let name = std::str::from_utf8(&bytes[*cursor..end]).map_err(|_| fault("non-UTF-8 name"))?;
    *cursor = end;
    Ok(name)
}

fn encode_name(name: &str) -> Vec<u8> {
    let mut out = uleb(name.len() as u64);
    out.extend_from_slice(name.as_bytes());
    out
}

fn split_sections(bytes: &[u8]) -> Result<Vec<Section>, String> {
    if bytes.get(..WASM_HEADER.len()) != Some(WASM_HEADER.as_slice()) {
        return Err(fault("invalid WebAssembly header"));
    }
    let mut cursor = WASM_HEADER.len();
    let mut sections = Vec::new();
    while cursor < bytes.len() {
        let id = bytes[cursor];
        cursor += 1;
        let size = read_uleb(bytes, &mut cursor)? as usize;
        let end = cursor
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| fault("truncated section"))?;
        sections.push(Section {
            id,
            body: bytes[cursor..end].to_vec(),
        });
        cursor = end;
    }
    Ok(sections)
}

fn encode_section(section: &Section) -> Vec<u8> {
    let mut out = vec![section.id];
    out.extend_from_slice(&uleb(section.body.len() as u64));
    out.extend_from_slice(&section.body);
    out
}

fn section_mut(sections: &mut [Section], id: u8) -> Result<&mut Section, String> {
    sections
        .iter_mut()
        .find(|section| section.id == id)
        .ok_or_else(|| fault(format!("missing section {id}")))
}

fn skip_limits(bytes: &[u8], cursor: &mut usize) -> Result<(), String> {
    let flags = read_uleb(bytes, cursor)?;
    read_uleb(bytes, cursor)?;
    if flags & 1 != 0 {
        read_uleb(bytes, cursor)?;
    }
    if flags & !0x07 != 0 {
        return Err(fault(format!("unsupported limits flags {flags}")));
    }
    Ok(())
}

fn relocate_imports(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut cursor = 0;
    let count = read_uleb(body, &mut cursor)?;
    let entries = cursor;
    let mut global_imports = 0;
    for _ in 0..count {
        read_name(body, &mut cursor)?;
        read_name(body, &mut cursor)?;
        let kind = *body
            .get(cursor)
            .ok_or_else(|| fault("truncated import kind"))?;
        cursor += 1;
        match kind {
            0x00 => {
                read_uleb(body, &mut cursor)?;
            }
            0x01 => {
                cursor = cursor
                    .checked_add(1)
                    .filter(|cursor| *cursor <= body.len())
                    .ok_or_else(|| fault("truncated table import"))?;
                skip_limits(body, &mut cursor)?;
            }
            0x02 => skip_limits(body, &mut cursor)?,
            0x03 => {
                global_imports += 1;
                cursor = cursor
                    .checked_add(2)
                    .filter(|cursor| *cursor <= body.len())
                    .ok_or_else(|| fault("truncated global import"))?;
            }
            other => return Err(fault(format!("unsupported import kind {other}"))),
        }
    }
    if cursor != body.len() || global_imports != 0 {
        return Err(fault(
            "the raw kernel import section no longer has the certified shape",
        ));
    }

    let mut out = uleb(u64::from(count) + 3);
    out.extend_from_slice(&body[entries..]);
    for (name, mutable) in [
        ("__stack_pointer", true),
        ("__memory_base", false),
        ("__data_base", false),
    ] {
        out.extend_from_slice(&encode_name("env"));
        out.extend_from_slice(&encode_name(name));
        out.push(0x03); // global import
        out.push(0x7f); // i32
        out.push(u8::from(mutable));
    }
    Ok(out)
}

fn read_i32_global(bytes: &[u8], cursor: &mut usize, mutable: bool) -> Result<u32, String> {
    if bytes.get(*cursor..*cursor + 3) != Some(&[0x7f, u8::from(mutable), 0x41]) {
        return Err(fault("unexpected linker global type or initializer"));
    }
    *cursor += 3;
    let value = read_sleb(bytes, cursor)?;
    if bytes.get(*cursor) != Some(&0x0b) {
        return Err(fault("unterminated linker global"));
    }
    *cursor += 1;
    u32::try_from(value).map_err(|_| fault("negative linker global"))
}

struct LinkerLayout {
    data_end: u32,
    workspace_bytes: u32,
}

fn relocate_globals(body: &[u8]) -> Result<LinkerLayout, String> {
    let mut cursor = 0;
    if read_uleb(body, &mut cursor)? != 4 {
        return Err(fault("expected four certified linker globals"));
    }
    let stack = read_i32_global(body, &mut cursor, true)?;
    let memory_base = read_i32_global(body, &mut cursor, false)?;
    let data_end = read_i32_global(body, &mut cursor, false)?;
    let heap_base = read_i32_global(body, &mut cursor, false)?;
    if cursor != body.len()
        || stack != STACK_BYTES
        || memory_base != 0
        || data_end < STACK_BYTES
        || heap_base < data_end
    {
        return Err(fault(format!(
            "unexpected linker layout stack={stack} memory={memory_base} \
             data_end={data_end} heap={heap_base}",
        )));
    }
    Ok(LinkerLayout {
        data_end,
        workspace_bytes: heap_base,
    })
}

fn relocate_exports(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut cursor = 0;
    let count = read_uleb(body, &mut cursor)?;
    let mut kept = Vec::new();
    let mut removed = [false; 2];
    for _ in 0..count {
        let start = cursor;
        let name = read_name(body, &mut cursor)?;
        let kind = *body
            .get(cursor)
            .ok_or_else(|| fault("truncated export kind"))?;
        cursor += 1;
        let index = read_uleb(body, &mut cursor)?;
        match name {
            "__data_end" if kind == 0x03 && index == 2 => removed[0] = true,
            "__heap_base" if kind == 0x03 && index == 3 => removed[1] = true,
            "__data_end" | "__heap_base" => {
                return Err(fault("linker-bound export changed kind or index"));
            }
            _ if kind == 0x03 => {
                return Err(fault(format!("unexpected global export {name}")));
            }
            _ => kept.push(body[start..cursor].to_vec()),
        }
    }
    if cursor != body.len() || removed != [true, true] {
        return Err(fault("missing certified linker-bound exports"));
    }
    let mut out = uleb(kept.len() as u64);
    for entry in kept {
        out.extend_from_slice(&entry);
    }
    Ok(out)
}

fn relocate_data(body: &[u8]) -> Result<(Vec<u8>, u32), String> {
    let mut cursor = 0;
    let count = read_uleb(body, &mut cursor)?;
    if count != 2 {
        return Err(fault(format!(
            "expected two contiguous data segments, found {count}",
        )));
    }
    let mut merged = Vec::new();
    let mut expected_offset = STACK_BYTES;
    for _ in 0..count {
        if read_uleb(body, &mut cursor)? != 0 || body.get(cursor) != Some(&0x41) {
            return Err(fault("expected an active i32.const data segment"));
        }
        cursor += 1;
        let offset = u32::try_from(read_sleb(body, &mut cursor)?)
            .map_err(|_| fault("negative data offset"))?;
        if body.get(cursor) != Some(&0x0b) || offset != expected_offset {
            return Err(fault(format!(
                "non-contiguous data segment at {offset}, expected {expected_offset}",
            )));
        }
        cursor += 1;
        let size = read_uleb(body, &mut cursor)? as usize;
        let end = cursor
            .checked_add(size)
            .filter(|end| *end <= body.len())
            .ok_or_else(|| fault("truncated data segment"))?;
        merged.extend_from_slice(&body[cursor..end]);
        cursor = end;
        expected_offset = expected_offset
            .checked_add(size as u32)
            .ok_or_else(|| fault("data segment overflow"))?;
    }
    if cursor != body.len() {
        return Err(fault("trailing data-section bytes"));
    }

    let mut out = uleb(1);
    out.push(0); // active, implicit memory 0
    out.extend_from_slice(&[0x23, 0x02, 0x0b]); // global.get $__data_base; end
    out.extend_from_slice(&uleb(merged.len() as u64));
    out.extend_from_slice(&merged);
    Ok((out, merged.len() as u32))
}

fn remove_fixed_bss_initializer(
    code: &[u8],
    start: &[u8],
    layout: &LinkerLayout,
    data_bytes: u32,
) -> Result<Vec<u8>, String> {
    let mut start_cursor = 0;
    if read_uleb(start, &mut start_cursor)? != 1 || start_cursor != start.len() {
        return Err(fault(
            "the certified BSS initializer is no longer function 1",
        ));
    }

    let mut cursor = 0;
    let count = read_uleb(code, &mut cursor)?;
    if count == 0 {
        return Err(fault("the kernel has no BSS initializer body"));
    }
    let first_size = read_uleb(code, &mut cursor)? as usize;
    let first_end = cursor
        .checked_add(first_size)
        .filter(|end| *end <= code.len())
        .ok_or_else(|| fault("truncated BSS initializer body"))?;
    let first = &code[cursor..first_end];
    let mut body_cursor = 0;
    if read_uleb(first, &mut body_cursor)? != 0 || first.get(body_cursor) != Some(&0x41) {
        return Err(fault("unexpected BSS initializer locals or first opcode"));
    }
    body_cursor += 1;
    let bss_start = u32::try_from(read_sleb(first, &mut body_cursor)?)
        .map_err(|_| fault("negative BSS start"))?;
    if first.get(body_cursor..body_cursor + 2) != Some(&[0x41, 0x00]) {
        return Err(fault("the BSS initializer no longer fills with zero"));
    }
    body_cursor += 2;
    if first.get(body_cursor) != Some(&0x41) {
        return Err(fault("the BSS initializer has no bounded length"));
    }
    body_cursor += 1;
    let bss_bytes = u32::try_from(read_sleb(first, &mut body_cursor)?)
        .map_err(|_| fault("negative BSS length"))?;
    if first.get(body_cursor..) != Some(&[0xfc, 0x0b, 0x00, 0x0b]) {
        return Err(fault("the BSS initializer contains unexpected work"));
    }
    let static_end = STACK_BYTES
        .checked_add(data_bytes)
        .ok_or_else(|| fault("static data end overflow"))?;
    let aligned_static_end = static_end
        .checked_add(15)
        .map(|end| end & !15)
        .ok_or_else(|| fault("BSS start overflow"))?;
    let aligned_data_end = layout
        .data_end
        .checked_add(15)
        .map(|end| end & !15)
        .ok_or_else(|| fault("workspace alignment overflow"))?;
    // wasm-ld versions disagree about whether up-to-16-byte alignment belongs
    // before the BSS, after it, or neither. Certify the meaningful boundaries
    // instead: initialised data, the exact zero-fill span, and the allocation
    // containing both with alignment padding only.
    if bss_start < static_end
        || bss_start > aligned_static_end
        || bss_start.checked_add(bss_bytes) != Some(layout.data_end)
        || layout.workspace_bytes < layout.data_end
        || layout.workspace_bytes > aligned_data_end
    {
        return Err(fault(format!(
            "unexpected data/BSS layout static_end={static_end} \
             bss={bss_start}+{bss_bytes} data_end={} workspace={}",
            layout.data_end, layout.workspace_bytes,
        )));
    }

    // The page zeroes the entire allocated workspace before instantiation.
    // Keeping wasm-ld's fixed memory.fill would zero the client at 0x100800.
    let replacement = [0x00, 0x0b]; // no locals; end
    let mut out = uleb(u64::from(count));
    out.extend_from_slice(&uleb(replacement.len() as u64));
    out.extend_from_slice(&replacement);
    out.extend_from_slice(&code[first_end..]);
    Ok(out)
}

fn manifest(workspace_bytes: u32, data_bytes: u32) -> Section {
    let json = format!(
        concat!(
            r#"{{"relocationAbi":{},"workspaceBytes":{},"stackBytes":{},"#,
            r#""dataOffset":{},"dataBytes":{}}}"#,
        ),
        RELOCATION_ABI, workspace_bytes, STACK_BYTES, STACK_BYTES, data_bytes,
    );
    let mut body = encode_name(MANIFEST_SECTION);
    body.extend_from_slice(json.as_bytes());
    Section { id: 0, body }
}

/// Relocate one exact compiler/linker output.
///
/// Every structural assumption is checked. A Rust or linker update that emits
/// another global, data segment or export fails the build instead of producing
/// a companion that might write into the client.
pub fn relocate(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut sections = split_sections(input)?;
    let layout = {
        let globals = section_mut(&mut sections, 6)?;
        let layout = relocate_globals(&globals.body)?;
        globals.body = vec![0]; // all former globals are now imports
        layout
    };
    {
        let imports = section_mut(&mut sections, 2)?;
        imports.body = relocate_imports(&imports.body)?;
    }
    {
        let exports = section_mut(&mut sections, 7)?;
        exports.body = relocate_exports(&exports.body)?;
    }
    let data_bytes = {
        let data = section_mut(&mut sections, 11)?;
        let (body, bytes) = relocate_data(&data.body)?;
        data.body = body;
        bytes
    };
    let start = section_mut(&mut sections, 8)?.body.clone();
    {
        let code = section_mut(&mut sections, 10)?;
        code.body = remove_fixed_bss_initializer(&code.body, &start, &layout, data_bytes)?;
    }
    if STACK_BYTES
        .checked_add(data_bytes)
        .is_none_or(|end| end > layout.workspace_bytes)
    {
        return Err(fault(
            "the static data does not fit in the linker workspace",
        ));
    }
    if let Some(data_count) = sections.iter_mut().find(|section| section.id == 12) {
        data_count.body = uleb(1);
    }
    sections.push(manifest(layout.workspace_bytes, data_bytes));

    let mut output = WASM_HEADER.to_vec();
    for section in &sections {
        output.extend_from_slice(&encode_section(section));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(bytes: &[u8], id: u8) -> Vec<u8> {
        split_sections(bytes)
            .unwrap()
            .into_iter()
            .find(|section| section.id == id)
            .unwrap()
            .body
    }

    #[test]
    fn generated_kernel_has_no_fixed_globals_or_data_offset() {
        let kernel = include_bytes!(env!("GWNATIVE_COMPANION_KERNEL"));
        let globals = section(kernel, 6);
        assert_eq!(globals, vec![0], "the linker globals survived relocation");

        let data = section(kernel, 11);
        let mut cursor = 0;
        assert_eq!(read_uleb(&data, &mut cursor).unwrap(), 1);
        assert_eq!(
            &data[cursor..cursor + 4],
            &[0x00, 0x23, 0x02, 0x0b],
            "data is not based on the imported allocation",
        );
    }

    #[test]
    fn generated_kernel_carries_a_bounded_workspace_manifest() {
        let kernel = include_bytes!(env!("GWNATIVE_COMPANION_KERNEL"));
        let sections = split_sections(kernel).unwrap();
        let custom = sections
            .iter()
            .filter(|section| {
                if section.id != 0 {
                    return false;
                }
                let mut cursor = 0;
                read_name(&section.body, &mut cursor).ok() == Some(MANIFEST_SECTION)
            })
            .collect::<Vec<_>>();
        assert_eq!(custom.len(), 1);
        let mut cursor = 0;
        assert_eq!(
            read_name(&custom[0].body, &mut cursor).unwrap(),
            MANIFEST_SECTION,
        );
        let text = std::str::from_utf8(&custom[0].body[cursor..]).unwrap();
        assert!(text.contains(r#""relocationAbi":1"#));
        assert!(text.contains(r#""stackBytes":1048576"#));
    }
}
