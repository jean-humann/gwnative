//! Just enough of the WebAssembly binary format to take a module apart and put
//! it back together byte-for-byte.
//!
//! Nothing here knows what a Guild Wars build is. It reads sections, LEB128
//! integers, index vectors and function bodies, and every encoder is the exact
//! inverse of its decoder — which is what lets [`super::rewrite`] pin an output
//! hash and have that mean something.

use std::collections::HashSet;

use super::Outcome;

pub(super) const WASM_HEADER: [u8; 8] = [0, 97, 115, 109, 1, 0, 0, 0];

/// Width LLVM uses for relocatable call targets, so a repoint fits in place.
const PADDED_INDEX_BYTES: usize = 5;

pub(super) struct Section {
    pub id: u8,
    pub body: Vec<u8>,
}

/// One entry of the type section, as the bytes that spell it.
///
/// The value types are kept raw rather than decoded into an enum because
/// nothing here needs to know what an `f64` is — the only question ever asked
/// is whether a signature is the one that was certified, and comparing the
/// bytes answers it without a decode that could disagree with the encoder.
pub(super) struct FunctionType {
    pub params: Vec<u8>,
    pub results: Vec<u8>,
}

/// `i32`, or `0x7b` for something this codec has no name for.
///
/// Only ever reached on the failure path, where the point is to say what was
/// found rather than to recognise it: a build whose main loop grew a parameter
/// should report the parameter, not "unsupported type form".
pub(super) fn value_type_name(value: u8) -> String {
    match value {
        0x7f => "i32".to_owned(),
        0x7e => "i64".to_owned(),
        0x7d => "f32".to_owned(),
        0x7c => "f64".to_owned(),
        other => format!("0x{other:x}"),
    }
}

pub(super) fn uleb(mut value: u64) -> Vec<u8> {
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

pub(super) fn sleb(mut value: i64) -> Vec<u8> {
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
pub(super) fn read_uleb(bytes: &[u8], cursor: &mut usize) -> Outcome<u32> {
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

/// The signed twin, capped the same way and for the same reason.
///
/// Only element-segment offsets are read through this, and those are `i32`
/// constants — so the sign extension at the end is what makes a negative offset
/// arrive as a negative number rather than as two billion.
fn read_sleb(bytes: &[u8], cursor: &mut usize) -> Outcome<i32> {
    let mut result: i32 = 0;
    let mut shift = 0;
    for _ in 0..PADDED_INDEX_BYTES {
        let byte = *bytes.get(*cursor).ok_or("wasm: truncated signed LEB128")?;
        *cursor += 1;
        result |= i32::from(byte & 0x7f).wrapping_shl(shift);
        if byte & 0x80 == 0 {
            // The sign bit of the last group is bit 6, and everything above it
            // is whatever that bit says. Skipped once the group already reaches
            // the top of the word, where there is nothing left to extend into.
            if shift + 7 < 32 && byte & 0x40 != 0 {
                result |= (!0i32) << (shift + 7);
            }
            return Ok(result);
        }
        shift += 7;
    }
    Err("wasm: oversized signed LEB128".to_owned())
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
pub(super) fn padded_call(function: u32) -> Vec<u8> {
    let mut out = vec![0x10];
    out.extend_from_slice(&padded_index(function));
    out
}

pub(super) fn split_sections(bytes: &[u8]) -> Outcome<Vec<Section>> {
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

pub(super) fn section_by_id(sections: &[Section], id: u8) -> Outcome<&[u8]> {
    sections
        .iter()
        .find(|section| section.id == id)
        .map(|section| section.body.as_slice())
        .ok_or_else(|| format!("wasm: missing section {id}"))
}

pub(super) fn encode_section(section: &Section) -> Vec<u8> {
    let mut out = vec![section.id];
    out.extend_from_slice(&uleb(section.body.len() as u64));
    out.extend_from_slice(&section.body);
    out
}

/// Room for `count` elements, refused if `remaining` bytes cannot hold that many.
///
/// Every element of both vectors below costs at least one byte on the wire — an
/// index is one LEB128 byte at minimum, a function body one size byte — so a
/// count larger than what is left is malformed however the rest of it parses.
/// Worth catching before the `read_uleb` that would eventually fail on its own,
/// because the reservation happens first: `read_uleb` yields up to `u32::MAX`,
/// and asking the allocator for four billion elements is tens of gigabytes.
/// A refused allocation in Rust is not an error a parser gets to report — it
/// aborts, which for an 8 MB module that merely arrived damaged is the wrong
/// end of the trade.
fn with_room<T>(count: u32, remaining: usize) -> Outcome<Vec<T>> {
    if count as usize > remaining {
        return Err(format!(
            "wasm: {count} elements declared in {remaining} bytes"
        ));
    }
    Ok(Vec::with_capacity(count as usize))
}

pub(super) fn parse_types(bytes: &[u8]) -> Outcome<Vec<FunctionType>> {
    let mut cursor = 0;
    let count = read_uleb(bytes, &mut cursor)?;
    // Three bytes minimum per entry: the `0x60` form and two empty counts.
    let mut types = with_room(count, bytes.len() - cursor)?;
    for _ in 0..count {
        if bytes.get(cursor) != Some(&0x60) {
            return Err("wasm: unsupported type form".to_owned());
        }
        cursor += 1;
        let take = |cursor: &mut usize| -> Outcome<Vec<u8>> {
            let count = read_uleb(bytes, cursor)? as usize;
            let end = cursor
                .checked_add(count)
                .filter(|end| *end <= bytes.len())
                .ok_or("wasm: truncated function type")?;
            let taken = bytes[*cursor..end].to_vec();
            *cursor = end;
            Ok(taken)
        };
        let params = take(&mut cursor)?;
        let results = take(&mut cursor)?;
        types.push(FunctionType { params, results });
    }
    if cursor != bytes.len() {
        return Err("wasm: malformed type section".to_owned());
    }
    Ok(types)
}

/// How many elements a vector declares, and the bytes holding them.
///
/// Used where a section is only being *appended to*: the globals and exports
/// below gain one entry and two entries respectively, and every existing entry
/// is copied through untouched. Re-encoding them would mean parsing shapes this
/// codec has no other reason to know — an export is four kinds and a global is
/// an arbitrary constant expression — and every one of those parsers would be a
/// new way for a byte to change on its way through.
pub(super) fn vector_payload(bytes: &[u8]) -> Outcome<(u32, &[u8])> {
    let mut cursor = 0;
    let count = read_uleb(bytes, &mut cursor)?;
    Ok((count, &bytes[cursor..]))
}

/// The limits of the module's one function table.
pub(super) struct TableLimits {
    pub min: u32,
    pub max: Option<u32>,
}

pub(super) fn parse_table(bytes: &[u8]) -> Outcome<TableLimits> {
    let mut cursor = 0;
    if read_uleb(bytes, &mut cursor)? != 1 {
        return Err("wasm: expected exactly one table".to_owned());
    }
    if bytes.get(cursor) != Some(&0x70) {
        return Err("wasm: expected a funcref table".to_owned());
    }
    cursor += 1;
    let flags = read_uleb(bytes, &mut cursor)?;
    let min = read_uleb(bytes, &mut cursor)?;
    let max = if flags & 1 != 0 {
        Some(read_uleb(bytes, &mut cursor)?)
    } else {
        None
    };
    Ok(TableLimits { min, max })
}

/// Every table slot some element segment already fills.
///
/// The one question this answers is whether the slot a hook wants to borrow is
/// free. Slot 0 usually is — Emscripten reserves it for the null function
/// pointer — but "usually" is not something to rewrite an 8 MB module on, so the
/// segments are walked and asked.
pub(super) fn occupied_table_slots(bytes: &[u8]) -> Outcome<HashSet<u32>> {
    let mut cursor = 0;
    let count = read_uleb(bytes, &mut cursor)?;
    let mut occupied = HashSet::new();
    for _ in 0..count {
        let flags = read_uleb(bytes, &mut cursor)?;
        if flags != 0 {
            return Err(format!("wasm: unsupported element segment flags {flags}"));
        }
        if bytes.get(cursor) != Some(&0x41) {
            return Err("wasm: expected an i32.const element offset".to_owned());
        }
        cursor += 1;
        let base = read_sleb(bytes, &mut cursor)?;
        if bytes.get(cursor) != Some(&0x0b) {
            return Err("wasm: malformed element offset".to_owned());
        }
        cursor += 1;
        let entries = read_uleb(bytes, &mut cursor)?;
        for index in 0..entries {
            read_uleb(bytes, &mut cursor)?;
            // A segment based below zero, or one running past the top of the
            // index space, cannot describe a slot — and silently dropping it
            // would report the slot it covers as free.
            let slot = i64::from(base) + i64::from(index);
            let slot = u32::try_from(slot)
                .map_err(|_| format!("wasm: element segment covers slot {slot}"))?;
            occupied.insert(slot);
        }
    }
    if cursor != bytes.len() {
        return Err("wasm: malformed element section".to_owned());
    }
    Ok(occupied)
}

/// A length-prefixed name, the shape an export and a custom section both start
/// with.
pub(super) fn encode_name(name: &str) -> Vec<u8> {
    let mut out = uleb(name.len() as u64);
    out.extend_from_slice(name.as_bytes());
    out
}

pub(super) fn parse_index_vector(bytes: &[u8]) -> Outcome<Vec<u32>> {
    let mut cursor = 0;
    let count = read_uleb(bytes, &mut cursor)?;
    let mut values = with_room(count, bytes.len() - cursor)?;
    for _ in 0..count {
        values.push(read_uleb(bytes, &mut cursor)?);
    }
    if cursor != bytes.len() {
        return Err("wasm: malformed index vector".to_owned());
    }
    Ok(values)
}

pub(super) fn encode_index_vector(values: &[u32]) -> Vec<u8> {
    let mut out = uleb(values.len() as u64);
    for value in values {
        out.extend_from_slice(&uleb(u64::from(*value)));
    }
    out
}

pub(super) fn parse_code(bytes: &[u8]) -> Outcome<Vec<Vec<u8>>> {
    let mut cursor = 0;
    let count = read_uleb(bytes, &mut cursor)?;
    let mut bodies = with_room(count, bytes.len() - cursor)?;
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

pub(super) fn encode_code(bodies: &[Vec<u8>]) -> Vec<u8> {
    let mut out = uleb(bodies.len() as u64);
    for body in bodies {
        out.extend_from_slice(&uleb(body.len() as u64));
        out.extend_from_slice(body);
    }
    out
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

    /// A count is refused for being impossible, not for being large: four
    /// billion indices cannot live in four bytes, and finding that out by
    /// reserving room for them is an allocator abort rather than a parse error.
    /// Both vectors take their count the same way, so both are checked.
    #[test]
    fn a_count_larger_than_the_bytes_holding_it_is_refused_not_reserved() {
        // `uleb(u32::MAX)` then nothing at all to put in the vector.
        let mut wild = uleb(u64::from(u32::MAX));
        assert!(
            parse_index_vector(&wild).is_err(),
            "no room for any of them"
        );
        assert!(parse_code(&wild).is_err());

        // Still refused with a plausible amount of data behind it, since the
        // count is what is impossible rather than the shortfall.
        wild.extend_from_slice(&[1u8; 64]);
        assert!(parse_index_vector(&wild).is_err());
        assert!(parse_code(&wild).is_err());

        // And an honest vector, exactly as long as its count allows, still
        // parses — the check must not cost the boundary case.
        let mut tight = uleb(3);
        tight.extend_from_slice(&[1, 2, 3]);
        assert_eq!(parse_index_vector(&tight).unwrap(), vec![1, 2, 3]);
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
    fn a_signed_leb_reads_back_what_it_wrote() {
        for value in [
            0i64,
            1,
            -1,
            63,
            -64,
            64,
            -65,
            8191,
            -70_001,
            i64::from(i32::MAX),
        ] {
            let encoded = sleb(value);
            let mut cursor = 0;
            assert_eq!(i64::from(read_sleb(&encoded, &mut cursor).unwrap()), value);
            assert_eq!(cursor, encoded.len(), "{value} left bytes behind");
        }
        // The five-byte group: the sign has nowhere left to extend into, so the
        // guard that would have written over the top of the word must not fire.
        let mut cursor = 0;
        assert_eq!(
            read_sleb(&sleb(i64::from(i32::MIN)), &mut cursor).unwrap(),
            i32::MIN
        );
    }

    #[test]
    fn a_function_type_is_read_as_the_bytes_that_spell_it() {
        // (i32, f64) -> i32, then () -> ().
        let bytes = [0x02, 0x60, 0x02, 0x7f, 0x7c, 0x01, 0x7f, 0x60, 0x00, 0x00];
        let types = parse_types(&bytes).unwrap();
        assert_eq!(types.len(), 2);
        assert_eq!(types[0].params, vec![0x7f, 0x7c]);
        assert_eq!(types[0].results, vec![0x7f]);
        assert!(types[1].params.is_empty() && types[1].results.is_empty());
        assert_eq!(value_type_name(0x7f), "i32");
        assert_eq!(value_type_name(0x70), "0x70");
        // A form this codec does not know is refused rather than skipped.
        assert!(parse_types(&[0x01, 0x5e, 0x7f, 0x00]).is_err());
        // And trailing bytes are a malformed section, not a shorter one.
        assert!(parse_types(&[0x01, 0x60, 0x00, 0x00, 0x00]).is_err());
    }

    #[test]
    fn a_table_and_the_slots_its_segments_fill() {
        // One funcref table, min 8, no maximum.
        assert_eq!(parse_table(&[0x01, 0x70, 0x00, 0x08]).unwrap().min, 8);
        let bounded = parse_table(&[0x01, 0x70, 0x01, 0x08, 0x10]).unwrap();
        assert_eq!((bounded.min, bounded.max), (8, Some(16)));
        assert!(
            parse_table(&[0x02, 0x70, 0x00, 0x08]).is_err(),
            "two tables"
        );
        assert!(parse_table(&[0x01, 0x6f, 0x00, 0x08]).is_err(), "externref");

        // One segment at offset 1 holding three functions: 1, 2 and 3 are taken
        // and 0 — the slot the hook wants — is not.
        let occupied =
            occupied_table_slots(&[0x01, 0x00, 0x41, 0x01, 0x0b, 0x03, 0x0a, 0x0b, 0x0c]).unwrap();
        assert_eq!(occupied, HashSet::from([1, 2, 3]));
        // A passive or declarative segment is a shape this cannot reason about,
        // so it says so instead of reporting every slot free.
        assert!(occupied_table_slots(&[0x01, 0x01, 0x00, 0x00]).is_err());
    }

    #[test]
    fn a_vector_payload_hands_back_the_entries_untouched() {
        let (count, entries) = vector_payload(&[0x03, 0xaa, 0xbb]).unwrap();
        assert_eq!(count, 3);
        assert_eq!(entries, &[0xaa, 0xbb]);
        assert_eq!(encode_name("gw"), vec![0x02, b'g', b'w']);
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
}
