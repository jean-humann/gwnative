//! Just enough of the WebAssembly binary format to take a module apart and put
//! it back together byte-for-byte.
//!
//! Nothing here knows what a Guild Wars build is. It reads sections, LEB128
//! integers, index vectors and function bodies, and every encoder is the exact
//! inverse of its decoder — which is what lets [`super::rewrite`] pin an output
//! hash and have that mean something.

use super::Outcome;

pub(super) const WASM_HEADER: [u8; 8] = [0, 97, 115, 109, 1, 0, 0, 0];

/// A WebAssembly `u32` LEB cannot use more than five bytes.
const MAX_U32_LEB_BYTES: usize = 5;

pub(super) struct Section {
    pub id: u8,
    pub body: Vec<u8>,
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
    for _ in 0..MAX_U32_LEB_BYTES {
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
