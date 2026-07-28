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
//! A second transform, [`enhancement`], is layered on top of the first when the
//! player has turned one of the GWonMac Tools on. It clones the client's main
//! loop so a companion module can run beside it; the module doc there is the
//! account of why that is the only extension point the client offers. It is
//! chained rather than offered as an alternative, so opting in never costs
//! template save — and it is opt-in rather than always-on because a module that
//! nothing installs a tick into is a strictly larger module for no gain.
//!
//! Split four ways: [`codec`] is the WebAssembly binary format and knows
//! nothing about Guild Wars, [`builds`] is the certified builds described in
//! enough detail that anything else fails closed, and [`rewrite`] and
//! [`enhancement`] are the transforms themselves. What is left here is the
//! cache that keeps their output.

mod builds;
mod codec;
mod enhancement;
mod rewrite;

use std::fs;
use std::path::{Path, PathBuf};

use sha2::Digest;

use builds::{ALL_BRIDGE_KINDS, KnownBuild, find_build, find_enhancement_build};
use enhancement::ENHANCEMENT_TRANSFORM_ABI;
pub use enhancement::{COMPANION_KERNEL, COMPANION_KERNEL_PATH};

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
/// Where the enhancement output lives, under the template-save entry it was
/// derived from. Nested rather than parallel because it is *that* output's
/// transform: an entry orphaned from its input is one nobody can check.
const ENHANCED: &str = "enhanced";

/// How the enhancement layer fared, for the page and for the log.
///
/// A string rather than an enum for the same reason [`Module::template_save`]
/// is one: it is injected into the page, compared there, and never matched on
/// in Rust.
pub mod enhancements {
    /// The player asked for a tool, and the module serving them has the hook.
    pub const READY: &str = "ready";
    /// No tool is on, so nothing was derived. The ordinary state.
    pub const OFF: &str = "off";
    /// A tool is on, but this client build is not one the transform knows.
    pub const UNCERTIFIED: &str = "uncertified";
    /// A tool is on and the transform was tried and did not produce what it
    /// was certified to produce. The template-save module is served instead.
    pub const FAILED: &str = "failed";
}

/// What a launch found when it looked at the client.
pub struct Prepared {
    /// The sha256 of ArenaNet's own `Gw.jspi.wasm`, whatever came of
    /// transforming it.
    ///
    /// The identity of the client build, and the only thing in a launch that
    /// distinguishes one ArenaNet patch from the next. It reaches the page so
    /// that a notice about *this* build can be dismissed for this build and
    /// come back for the following one — see
    /// [`crate::settings::Settings::compatibility_notice_seen_for`].
    pub client: String,
    /// The module to serve instead of the client's own, when this is a build we
    /// certified. `None` is the ordinary state of affairs the day after
    /// ArenaNet ships a patch: the caller serves the untransformed module and
    /// template save goes back to being broken — which is where it started, and
    /// much better than refusing to launch.
    pub derived: Option<PathBuf>,
    /// One of [`enhancements`]. When it is `READY`, [`Self::derived`] is the
    /// enhanced module rather than the template-save one — they are a chain,
    /// not a choice, so there is only ever one file to serve.
    pub enhancements: &'static str,
}

/// What the page is told about the module it is about to run.
///
/// Two facts with one audience. They are carried together because they are read
/// together: the sentence the panel shows is decided by [`Self::template_save`]
/// and how long it stays dismissed is decided by [`Self::build`], and a caller
/// that had one without the other could show a notice it could not remember
/// having shown.
pub struct Module {
    /// [`Prepared::client`], or `None` when the module could not be read at all
    /// — in which case there is no build to name, and nothing to warn about
    /// that a working game would recognise.
    pub build: Option<String>,
    /// `"ready"`, `"uncertified"` or `"failed"`; the three outcomes of
    /// [`prepare`]. `web/settings-panel.js` turns each into a sentence, or into
    /// silence.
    pub template_save: &'static str,
    /// One of [`enhancements`], carried for the same audience and for the same
    /// reason: a player who turned a tool on and got the plain game back is owed
    /// the sentence saying so.
    pub enhancements: &'static str,
}

/// The derived module for `base`, transforming only when the cache cannot prove
/// it already holds exactly this output.
///
/// `enhance` is whether any GWonMac Tool is on. It is a parameter rather than a
/// setting read in here because the decision belongs to the caller: the module
/// is chosen once, before the page exists, and a launch that changed its mind
/// halfway would be serving one module and describing another.
pub fn prepare(base: &Path, cache_root: &Path, enhance: bool) -> Outcome<Prepared> {
    let input = fs::read(base).map_err(|e| format!("template-save: {}: {e}", base.display()))?;
    let input_hash = digest(&input);
    let Some(build) = find_build(&input_hash) else {
        // Nothing here can serve this input, and the entries are ~8 MB each.
        let _ = fs::remove_dir_all(cache_root);
        return Ok(Prepared {
            client: input_hash,
            derived: None,
            enhancements: if enhance {
                enhancements::UNCERTIFIED
            } else {
                enhancements::OFF
            },
        });
    };

    let dir = cache_root.join(&input_hash).join(TRANSFORM_ABI.to_string());
    let derived = dir.join(DERIVED);
    if !usable(&dir, build) {
        let output = rewrite::rewrite(&input, build)?;

        // Only after a successful transform: a failing one must leave whatever
        // the last good build published exactly where it is.
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
    }

    if !enhance {
        return Ok(Prepared {
            client: input_hash,
            derived: Some(derived),
            enhancements: enhancements::OFF,
        });
    }

    // Past this point a fault is reported rather than returned. The module
    // above is a complete, playable client; losing the game because a tool
    // could not be installed would be the wrong trade by a wide margin, and
    // what the player is owed is the sentence rather than the failure.
    match enhanced(&dir, build) {
        Ok(Some(path)) => Ok(Prepared {
            client: input_hash,
            derived: Some(path),
            enhancements: enhancements::READY,
        }),
        Ok(None) => Ok(Prepared {
            client: input_hash,
            derived: Some(derived),
            enhancements: enhancements::UNCERTIFIED,
        }),
        Err(reason) => {
            note!("[gwnative] {reason}");
            Ok(Prepared {
                client: input_hash,
                derived: Some(derived),
                enhancements: enhancements::FAILED,
            })
        }
    }
}

/// The enhanced module derived from `build`'s template-save output, or `None`
/// when that output is not one the enhancement transform is certified against.
fn enhanced(dir: &Path, build: &KnownBuild) -> Outcome<Option<PathBuf>> {
    let Some(enhancement) = find_enhancement_build(build.output_sha256) else {
        return Ok(None);
    };
    let enhanced_dir = dir
        .join(ENHANCED)
        .join(ENHANCEMENT_TRANSFORM_ABI.to_string());
    let derived = enhanced_dir.join(DERIVED);
    if stamped(&enhanced_dir, enhancement.sha256, enhancement.output_sha256) {
        return Ok(Some(derived));
    }

    // Read back rather than carried down from the caller: this is reached on
    // every launch that found the template-save entry already cached, and the
    // alternative is re-deriving eight megabytes to have them in hand.
    let input = fs::read(dir.join(DERIVED))
        .map_err(|e| format!("enhancement: {}: {e}", dir.join(DERIVED).display()))?;
    let output = enhancement::transform(&input, enhancement)?;

    fs::create_dir_all(&enhanced_dir)
        .map_err(|e| format!("enhancement: {}: {e}", enhanced_dir.display()))?;
    write_atomic(&derived, &output)?;
    write_atomic(
        &enhanced_dir.join(STAMP),
        serde_json::json!({
            "inputSha256": enhancement.sha256,
            "transformAbi": ENHANCEMENT_TRANSFORM_ABI,
            "outputSha256": enhancement.output_sha256,
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
    stamped(dir, build.sha256, build.output_sha256)
}

/// Shared by both transforms. The ABI is not re-checked here because it is part
/// of the path — an entry for a different ABI is a different directory, and one
/// that claimed otherwise in its stamp would still have to hash correctly to be
/// used at all.
fn stamped(dir: &Path, input_sha256: &str, output_sha256: &str) -> bool {
    let Ok(stamp) = fs::read(dir.join(STAMP)) else {
        return false;
    };
    let Ok(stamp) = serde_json::from_slice::<serde_json::Value>(&stamp) else {
        return false;
    };
    if stamp["inputSha256"].as_str() != Some(input_sha256) {
        return false;
    }
    fs::read(dir.join(DERIVED)).is_ok_and(|bytes| digest(&bytes) == output_sha256)
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
