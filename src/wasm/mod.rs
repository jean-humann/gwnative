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
//! Split three ways: [`codec`] is the WebAssembly binary format and knows
//! nothing about Guild Wars, [`builds`] is the certified build described in
//! enough detail that anything else fails closed, and [`rewrite`] is the
//! transform itself. What is left here is the cache that keeps its output.

mod builds;
mod codec;
mod rewrite;

use std::fs;
use std::path::{Path, PathBuf};

use sha2::Digest;

use builds::{ALL_BRIDGE_KINDS, KnownBuild, find_build};

/// A structural or policy fault. These are all "this is not the build we
/// certified", which is never worth failing a launch over: the caller falls
/// back to the untransformed module and the player loses template save, not the
/// game.
type Fault = String;

type Outcome<T> = std::result::Result<T, Fault>;

fn digest(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

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
    let input = fs::read(base).map_err(|e| format!("template-save: {}: {e}", base.display()))?;
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

    let output = rewrite::rewrite(&input, build)?;

    // Only after a successful transform: a failing one must leave whatever the
    // last good build published exactly where it is.
    let _ = fs::remove_dir_all(cache_root);
    fs::create_dir_all(&dir).map_err(|e| format!("template-save: {}: {e}", dir.display()))?;
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
    fs::write(&temporary, bytes)
        .map_err(|e| format!("template-save: {}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("template-save: {}: {e}", path.display()))
}

/// `{ ensureDirectory: -70001, … }`, for injection into the page.
///
/// Built by the encoder rather than spelled out, like [`crate::layout::as_json`]
/// and every other JSON body in this crate. The keys here are compile-time
/// literals and the values are `i64` constants, so the braces were not wrong —
/// but this string is injected into a `WKUserScript`, which is the last place
/// worth keeping a second escaper that only happens to have nothing to escape.
pub fn markers_json() -> String {
    serde_json::Value::from(
        ALL_BRIDGE_KINDS
            .iter()
            .map(|kind| {
                (
                    kind.key().to_owned(),
                    serde_json::Value::from(kind.marker()),
                )
            })
            .collect::<serde_json::Map<_, _>>(),
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_markers_reach_the_page_as_an_object() {
        let json: serde_json::Value = serde_json::from_str(&markers_json()).unwrap();
        assert_eq!(json["ensureDirectory"], -70_001);
        assert_eq!(json["fileExists"], -70_005);
        assert_eq!(json.as_object().unwrap().len(), ALL_BRIDGE_KINDS.len());
    }
}
