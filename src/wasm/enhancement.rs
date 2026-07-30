//! Cloning the client's main loop so a companion module can run beside it.
//!
//! The client has no extension point. There is no callback to register, no
//! export to wrap from JavaScript that runs often enough to be a tick, and no
//! way to read the game's own state from outside its linear memory without
//! copying eight megabytes a frame. What it does have is one exported function
//! that the browser drives — `EmscriptenExeThreadMainLoop` — and a function
//! table with a free slot in it.
//!
//! So: clone that function to a new index and export the clone, overwrite the
//! original's body with a dispatcher, and add one mutable global for the
//! dispatcher to read. With the global at zero the dispatcher calls the clone
//! and returns, which is the untouched game to the byte. With it set to *n* the
//! dispatcher calls table slot *n − 1* instead, and whatever the page put in
//! that slot is now the tick — free to call the exported clone itself, which is
//! how the game still runs.
//!
//! The offset by one is what makes slot 0 usable. Emscripten reserves it for
//! the null function pointer and leaves it empty, so it is the one slot nothing
//! can collide with; a global that meant "slot 0" at zero could not also mean
//! "not installed".
//!
//! None of this is speculative rewriting. The input hash is the template-save
//! transform's own output, every index below was found in that exact module,
//! and the result is compared against a pinned hash before it is handed back —
//! so a module that transformed at all transformed into precisely the one that
//! was certified.

use super::builds::{ENHANCEMENT_LAYOUT_WORDS, EnhancementBuild};
use super::codec::{
    Section, WASM_HEADER, encode_code, encode_index_vector, encode_name, encode_section,
    occupied_table_slots, parse_code, parse_index_vector, parse_table, parse_types, section_by_id,
    sleb, split_sections, uleb, value_type_name, vector_payload,
};
use super::{Outcome, digest};

/// Bumped whenever a derived module stops being interchangeable with one an
/// older build published. Shared with the companion, which refuses a manifest
/// it does not recognise rather than reading the wrong words out of it.
pub(super) const ENHANCEMENT_TRANSFORM_ABI: u32 = 13;

/// The mutable global the dispatcher reads: zero for the untouched game, or one
/// past the table slot holding the tick.
const HOOK_EXPORT: &str = "enhancement_hook_slot";
/// The clone of the main loop, so an installed tick can still run the game.
const ORIGINAL_EXPORT: &str = "enhancement_tick_original";
/// Where the layout below travels. A custom section rather than a file beside
/// the module because the two must not be separable: a manifest describing one
/// build next to the module of another is a companion reading whatever happens
/// to be at those addresses.
const MANIFEST_SECTION: &str = "enhancement_manifest";

/// Shape of the block the companion publishes each tick, and of the cursor
/// block beside it. Named here because they are the manifest's own words and
/// the companion's `Snapshot`/`CursorSnapshot` at once — see
/// `web/companion-snapshot.js`, which reads both back.
const SNAPSHOT_ABI: u32 = 10;
const SNAPSHOT_BYTES: u32 = 56_252;
const CURSOR_SNAPSHOT_ABI: u32 = 1;
const CURSOR_SNAPSHOT_BYTES: u32 = 4160;

/// The companion itself: `src/companion-kernel/lib.rs`, compiled for
/// `wasm32-unknown-unknown` by `build.rs` and carried in this binary.
///
/// It lives here rather than in the server because it is the other half of this
/// file. The transform above writes the manifest; the kernel reads it, and the
/// two agree only because the constants are literally the same lines of source.
/// Embedded rather than shipped in the web root for the same reason: a copy on
/// disk could be older than the host that serves it, and the mismatch would not
/// surface until a player's frame read the wrong words out of the layout.
pub const COMPANION_KERNEL: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/companion-kernel.wasm"));

/// The name the page fetches [`COMPANION_KERNEL`] by.
pub const COMPANION_KERNEL_PATH: &str = "companion-kernel.wasm";

fn fault(message: impl std::fmt::Display) -> String {
    format!("enhancement: {message}")
}

/// The body that replaces the main loop.
///
/// ```wat
/// global.get $hook          ;; 0 when nothing is installed
/// i32.eqz
/// if
///   local.get 0 … ; call $original ; return
/// end
/// local.get 0 …
/// global.get $hook
/// i32.const 1
/// i32.sub                   ;; the slot itself
/// call_indirect (type $tick)
/// ```
///
/// The arguments are pushed twice rather than once before the branch because
/// the `if` has no result type: a value left on the stack across it would not
/// validate. Costing a few bytes to keep the block empty-typed is cheaper than
/// the block type that would otherwise have to describe the whole signature.
fn dispatcher(
    param_count: usize,
    type_index: u32,
    original_index: u32,
    hook_global: u32,
) -> Vec<u8> {
    let push_args = |body: &mut Vec<u8>| {
        for index in 0..param_count {
            body.push(0x20); // local.get
            body.extend_from_slice(&uleb(index as u64));
        }
    };
    let global_get = |body: &mut Vec<u8>| {
        body.push(0x23);
        body.extend_from_slice(&uleb(u64::from(hook_global)));
    };

    let mut body = uleb(0); // no locals
    global_get(&mut body);
    body.extend_from_slice(&[0x45, 0x04, 0x40]); // i32.eqz; if (void)
    push_args(&mut body);
    body.push(0x10); // call
    body.extend_from_slice(&uleb(u64::from(original_index)));
    body.extend_from_slice(&[0x0f, 0x0b]); // return; end

    push_args(&mut body);
    global_get(&mut body);
    body.push(0x41); // i32.const
    body.extend_from_slice(&sleb(1));
    body.extend_from_slice(&[0x6b, 0x11]); // i32.sub; call_indirect
    body.extend_from_slice(&uleb(u64::from(type_index)));
    body.extend_from_slice(&uleb(0)); // table 0
    body.push(0x0b); // end
    body
}

/// The manifest, spelled out rather than encoded.
///
/// Written by hand because the key order is part of the module's bytes and
/// therefore part of its hash, and `serde_json` sorts its keys — a map would
/// produce valid JSON that hashes to something the pin does not know. Every
/// value here is an integer, so there is nothing to escape and nothing an
/// encoder would do differently.
fn manifest_section(build: &EnhancementBuild) -> Section {
    let words = build.layout.words();
    let layout_words = words
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        concat!(
            r#"{{"transformAbi":{},"snapshotAbi":{},"snapshotBytes":{},"#,
            r#""cursorSnapshotAbi":{},"cursorSnapshotBytes":{},"configBytes":{},"#,
            r#""programId":{},"buildId":{},"tableSlot":{},"layoutWords":[{}]}}"#,
        ),
        ENHANCEMENT_TRANSFORM_ABI,
        SNAPSHOT_ABI,
        SNAPSHOT_BYTES,
        CURSOR_SNAPSHOT_ABI,
        CURSOR_SNAPSHOT_BYTES,
        ENHANCEMENT_LAYOUT_WORDS * 4,
        build.program_id,
        build.build_id,
        build.table_slot,
        layout_words,
    );
    let mut body = encode_name(MANIFEST_SECTION);
    body.extend_from_slice(json.as_bytes());
    Section { id: 0, body }
}

/// Rewrite `input` — the template-save transform's output — into the module the
/// companion can be installed into.
///
/// As in [`super::rewrite`], there is no `WebAssembly.validate` here and it
/// would add nothing: the output hash is pinned, so a byte-exact match proves
/// this is the module the transform's own certification already validated.
pub(super) fn transform(input: &[u8], build: &EnhancementBuild) -> Outcome<Vec<u8>> {
    let input_hash = digest(input);
    if input_hash != build.sha256 {
        return Err(fault(format!("unsupported input {input_hash}")));
    }

    let sections = split_sections(input)?;
    let types = parse_types(section_by_id(&sections, 1)?)?;
    let function_types = parse_index_vector(section_by_id(&sections, 3)?)?;
    let bodies = parse_code(section_by_id(&sections, 10)?)?;
    if function_types.len() != bodies.len() {
        return Err(fault("function and code sections disagree"));
    }

    let local_index = build
        .hook_function
        .checked_sub(build.import_count)
        .filter(|index| (*index as usize) < bodies.len())
        .ok_or_else(|| fault("the hook function is out of range"))? as usize;
    let type_index = function_types[local_index];
    let hook_type = types
        .get(type_index as usize)
        .ok_or_else(|| fault("the hook references an unknown type"))?;
    if hook_type.params != build.hook_params || hook_type.results != build.hook_results {
        let show = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| value_type_name(*byte))
                .collect::<Vec<_>>()
                .join(",")
        };
        return Err(fault(format!(
            "the hook signature is ({}) -> ({}), expected ({}) -> ({})",
            show(&hook_type.params),
            show(&hook_type.results),
            show(build.hook_params),
            show(build.hook_results),
        )));
    }

    // The slot has to exist and be empty. A dispatcher pointed at a slot the
    // game itself fills would call one of its functions with the main loop's
    // argument the moment the global was set.
    let table = parse_table(section_by_id(&sections, 4)?)?;
    if build.table_slot >= table.min || table.max.is_some_and(|max| build.table_slot >= max) {
        return Err(fault("the hook table slot is outside the table limits"));
    }
    if occupied_table_slots(section_by_id(&sections, 9)?)?.contains(&build.table_slot) {
        return Err(fault(format!(
            "table slot {} is already occupied",
            build.table_slot
        )));
    }

    let (global_count, global_entries) = vector_payload(section_by_id(&sections, 6)?)?;
    let (export_count, export_entries) = vector_payload(section_by_id(&sections, 7)?)?;
    let original_index = build.import_count + bodies.len() as u32;
    let hook_global = global_count;

    let mut next_types = function_types;
    next_types.push(type_index);
    let mut next_bodies = bodies;
    next_bodies.push(next_bodies[local_index].clone());
    next_bodies[local_index] = dispatcher(
        hook_type.params.len(),
        type_index,
        original_index,
        hook_global,
    );

    // `i32`, mutable, `i32.const 0`, end.
    let mut next_globals = uleb(u64::from(global_count) + 1);
    next_globals.extend_from_slice(global_entries);
    next_globals.extend_from_slice(&[0x7f, 0x01, 0x41, 0x00, 0x0b]);

    let mut next_exports = uleb(u64::from(export_count) + 2);
    next_exports.extend_from_slice(export_entries);
    next_exports.extend_from_slice(&encode_name(HOOK_EXPORT));
    next_exports.push(0x03); // a global
    next_exports.extend_from_slice(&uleb(u64::from(hook_global)));
    next_exports.extend_from_slice(&encode_name(ORIGINAL_EXPORT));
    next_exports.push(0x00); // a function
    next_exports.extend_from_slice(&uleb(u64::from(original_index)));

    let mut output = WASM_HEADER.to_vec();
    for section in &sections {
        let rewritten = match section.id {
            3 => Section {
                id: 3,
                body: encode_index_vector(&next_types),
            },
            6 => Section {
                id: 6,
                body: next_globals.clone(),
            },
            7 => Section {
                id: 7,
                body: next_exports.clone(),
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
    output.extend_from_slice(&encode_section(&manifest_section(build)));

    let output_hash = digest(&output);
    if output_hash != build.output_sha256 {
        return Err(fault(format!("unexpected output {output_hash}")));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::super::builds::{ENHANCEMENT_BUILDS, find_enhancement_build};
    use super::super::codec::read_uleb;
    use super::*;

    /// The dispatcher's whole reason for existing is that an uninstalled hook
    /// is the untouched game, so the branch that decides it is worth reading
    /// byte by byte rather than only through the pinned hash.
    #[test]
    fn the_dispatcher_falls_straight_through_when_nothing_is_installed() {
        let body = dispatcher(1, 11, 12_345, 7);
        assert_eq!(body[0], 0x00, "the dispatcher declares locals");
        assert_eq!(*body.last().unwrap(), 0x0b, "the dispatcher does not end");

        // global.get 7; i32.eqz; if; local.get 0; call 12345; return; end
        let mut expected = vec![0x23, 0x07, 0x45, 0x04, 0x40, 0x20, 0x00, 0x10];
        expected.extend_from_slice(&uleb(12_345));
        expected.extend_from_slice(&[0x0f, 0x0b]);
        assert_eq!(body[1..1 + expected.len()], expected[..]);

        // And the tail dispatches through the table one below the global.
        let tail = &body[1 + expected.len()..];
        assert_eq!(
            tail,
            &[
                0x20, 0x00, 0x23, 0x07, 0x41, 0x01, 0x6b, 0x11, 0x0b, 0x00, 0x0b
            ]
        );
    }

    /// Read a name-prefixed vector entry: `(name, rest)`.
    fn name(bytes: &[u8]) -> (String, &[u8]) {
        let mut cursor = 0;
        let length = read_uleb(bytes, &mut cursor).expect("a name is length-prefixed") as usize;
        let (text, rest) = bytes[cursor..].split_at(length);
        (
            String::from_utf8(text.to_vec()).expect("names are utf-8"),
            rest,
        )
    }

    /// The two halves of the enhancement are compiled from different files by
    /// different compilers into different modules, and nothing links them: the
    /// page hands one's export to the other's import *by name*, at runtime, and
    /// a rename on either side would fail there rather than here.
    ///
    /// So this checks the joins. `env.memory` is the one that matters most and
    /// is invisible in the source: it exists only because `build.rs` passes
    /// `--import-memory`, and a kernel that lost that flag would compile, load,
    /// instantiate, and then read a private memory full of zeroes instead of
    /// the game's.
    #[test]
    fn the_companion_asks_for_exactly_what_the_transform_publishes() {
        assert_eq!(
            COMPANION_KERNEL.get(..8),
            Some(&WASM_HEADER[..]),
            "build.rs did not produce a WebAssembly module",
        );
        let sections = split_sections(COMPANION_KERNEL).expect("the kernel parses");

        let imports = section_by_id(&sections, 2).expect("the kernel imports");
        let (count, mut rest) = vector_payload(imports).expect("the import vector parses");
        let mut imported = Vec::new();
        for _ in 0..count {
            let (module, tail) = name(rest);
            let (field, tail) = name(tail);
            // kind byte, then a descriptor whose length depends on it. Only
            // memories and functions appear here, and both are short.
            let (kind, tail) = tail.split_first().expect("an import has a kind");
            let mut cursor = 0;
            let skip = match kind {
                // A function's descriptor is one type index.
                0x00 => {
                    read_uleb(tail, &mut cursor).expect("a type index");
                    cursor
                }
                // A memory's is limits: a flag byte and one or two counts.
                0x02 => {
                    let flags = read_uleb(tail, &mut cursor).expect("memory limits");
                    read_uleb(tail, &mut cursor).expect("a minimum");
                    if flags & 1 != 0 {
                        read_uleb(tail, &mut cursor).expect("a maximum");
                    }
                    cursor
                }
                other => panic!("the kernel imported an unexpected kind {other:#04x}"),
            };
            imported.push((module, field));
            rest = &tail[skip..];
        }
        assert!(
            imported.iter().any(|(m, f)| m == "env" && f == "memory"),
            "the kernel does not import the game's memory, so build.rs lost \
             --import-memory: {imported:?}",
        );
        assert!(
            imported
                .iter()
                .any(|(m, f)| m == "game" && f == ORIGINAL_EXPORT),
            "the kernel does not call the clone this transform exports as \
             {ORIGINAL_EXPORT}: {imported:?}",
        );

        let exports = section_by_id(&sections, 7).expect("the kernel exports");
        let (count, mut rest) = vector_payload(exports).expect("the export vector parses");
        let mut exported = Vec::new();
        for _ in 0..count {
            let (field, tail) = name(rest);
            let mut cursor = 1; // the kind byte
            read_uleb(tail, &mut cursor).expect("an export index");
            exported.push(field);
            rest = &tail[cursor..];
        }
        for wanted in ["companion_init", "companion_tick"] {
            assert!(
                exported.iter().any(|name| name == wanted),
                "the page installs {wanted}, which the kernel does not export: {exported:?}",
            );
        }
    }

    /// A loop with no arguments still has to be dispatched correctly, and the
    /// only thing that varies with the parameter count is how many `local.get`s
    /// each side of the branch pushes.
    #[test]
    fn the_dispatcher_forwards_exactly_as_many_arguments_as_the_loop_takes() {
        for params in [0usize, 1, 3] {
            let body = dispatcher(params, 0, 1, 0);
            assert_eq!(
                body.iter().filter(|byte| **byte == 0x20).count(),
                params * 2,
                "{params} arguments are not pushed on both paths",
            );
        }
    }

    /// The manifest is the companion's only description of the build it is
    /// reading, and its bytes are part of the module's hash. Key order is
    /// therefore load-bearing, which is not something a reader of the
    /// `format!` above would guess.
    #[test]
    fn the_manifest_says_what_the_companion_needs_in_the_order_it_was_certified_in() {
        let section = manifest_section(&ENHANCEMENT_BUILDS[0]);
        assert_eq!(section.id, 0, "the manifest is not a custom section");
        let body = String::from_utf8(section.body.clone()).unwrap();
        let json = body
            .strip_prefix(&format!("\u{14}{MANIFEST_SECTION}",))
            .expect("the section does not start with its own name");
        assert!(
            json.starts_with(r#"{"transformAbi":13,"snapshotAbi":10,"snapshotBytes":56252,"#),
            "the key order changed, and with it the module's hash: {json}",
        );
        assert!(json.contains(r#""configBytes":792,"programId":1,"buildId":38771,"tableSlot":0,"#));
        assert!(json.contains(r#""layoutWords":[5901856,5918104,5912716,5912712,6,"#));

        // Still valid JSON, and still the 198 words the companion expects.
        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        let words = parsed["layoutWords"].as_array().unwrap();
        assert_eq!(words.len(), ENHANCEMENT_LAYOUT_WORDS);
        assert_eq!(
            parsed["configBytes"].as_u64(),
            Some(ENHANCEMENT_LAYOUT_WORDS as u64 * 4),
        );
    }

    #[test]
    fn an_uncertified_input_is_refused_rather_than_guessed_at() {
        let build = &ENHANCEMENT_BUILDS[0];
        assert!(transform(b"\0asm\x01\0\0\0", build).is_err());
        assert!(find_enhancement_build("not a hash").is_none());
    }

    /// The whole transform in one assertion, exactly as
    /// `rewrite::the_real_client_transforms_to_the_certified_output` is: the
    /// output hash comes from a transform certified against this input, so
    /// reproducing it byte-for-byte proves the dispatcher body, the appended
    /// global, both exports and the manifest at once.
    ///
    /// It runs on the *template-save output*, so it needs that transform to
    /// have happened first — which is why it derives it here rather than
    /// looking for a cached copy that a clean checkout would not have.
    #[test]
    fn the_certified_client_transforms_to_the_certified_output() {
        let base = std::env::var_os("GWNATIVE_CLIENT_WASM")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("web/Gw.jspi.wasm"));
        let Ok(official) = fs::read(&base) else {
            note!("skipping: {} has not been fetched", base.display());
            return;
        };
        let Some(template_save) = super::super::builds::find_build(&digest(&official)) else {
            note!("skipping: the fetched client is not a certified build");
            return;
        };
        let input = super::super::rewrite::rewrite(&official, template_save)
            .expect("the certified build rewrites");
        let Some(build) = find_enhancement_build(&digest(&input)) else {
            note!("skipping: the template-save output is not a certified enhancement input");
            return;
        };
        let output = transform(&input, build).expect("the certified build transforms");
        assert_eq!(digest(&output), build.output_sha256);
        // One cloned body, one global, two exports and the manifest. Nothing is
        // removed, and the dispatcher is shorter than the body it replaces.
        assert!(output.len() > input.len());
    }
}
