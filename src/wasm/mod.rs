//! Certified compatibility transforms for both official client runtimes.
//!
//! ArenaNet publishes a JSPI build and an Asyncify build from the same source.
//! Their data layout is shared, but Asyncify rewrites the suspendable call graph
//! and cannot inherit JSPI byte offsets or output hashes. A signed artifact-family
//! certificate therefore binds both exact artifacts to one independently
//! checked read-only layout while giving each runtime its own semantic
//! template-save anchors and output hash.
//!
//! Optional tools no longer rewrite or call through the game's main loop. The
//! companion is a passive observer driven from JavaScript only while the game
//! is not in an Asyncify unwind/rewind. That makes the layout certificate
//! reusable across the two runtimes without claiming their control flow is the
//! same.

mod certificate;
mod codec;
mod rewrite;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::Digest;

use crate::generation;

pub use certificate::Runtime;
use certificate::{CertificateFeed, Selected};

type Fault = String;
type Outcome<T> = std::result::Result<T, Fault>;

fn digest(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Stable identity for one transform attempt on an exact official runtime pair.
///
/// ArenaNet may change generated glue without changing the Wasm, so the Wasm
/// hash alone is neither a compatibility identity nor a safe transform-refusal
/// key. The transform ABI is included so an application update that fixes the
/// transformer gets one clean retry instead of inheriting an older version's
/// local refusal forever. A selected output hash does the same for a corrected
/// data-only certificate that keeps the compiled ABI.
fn runtime_compatibility_id(
    runtime: Runtime,
    wasm_sha256: &str,
    glue_sha256: &str,
    transform_abi: u32,
    output_sha256: Option<&str>,
) -> String {
    let mut digest = sha2::Sha256::new();
    digest.update(b"gwnative-runtime-compatibility-v1\0");
    digest.update(runtime.key().as_bytes());
    digest.update([0]);
    digest.update(wasm_sha256.as_bytes());
    digest.update([0]);
    digest.update(glue_sha256.as_bytes());
    digest.update([0]);
    digest.update(transform_abi.to_le_bytes());
    digest.update([0]);
    digest.update(output_sha256.unwrap_or("uncertified").as_bytes());
    hex::encode(digest.finalize())
}

const STAMP: &str = "derived.json";
const BENCHMARK_API_ABI: u32 = 2;

pub const COMPANION_KERNEL: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/companion-kernel.wasm"));
pub const COMPANION_KERNEL_PATH: &str = "companion-kernel.wasm";

pub mod enhancements {
    pub const READY: &str = "ready";
    pub const OFF: &str = "off";
    pub const UNCERTIFIED: &str = "uncertified";
    pub const FAILED: &str = "failed";
}

#[derive(Default)]
pub struct DerivedModules {
    by_name: BTreeMap<&'static str, PathBuf>,
}

impl DerivedModules {
    pub fn get(&self, request_path: &str) -> Option<&PathBuf> {
        self.by_name.get(request_path)
    }

    pub(crate) fn insert(&mut self, runtime: Runtime, path: PathBuf) {
        self.by_name.insert(runtime.wasm_name(), path);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeModule {
    build: Option<String>,
    template_save: &'static str,
    enhancements: &'static str,
    enhancement_manifest: Option<serde_json::Value>,
}

impl RuntimeModule {
    fn unavailable(build: Option<String>, state: &'static str, enhance: bool) -> Self {
        Self {
            build,
            template_save: state,
            enhancements: if enhance { state } else { enhancements::OFF },
            enhancement_manifest: None,
        }
    }
}

/// Per-runtime launch facts injected before the page chooses JSPI or Asyncify.
pub struct Module {
    runtimes: BTreeMap<&'static str, RuntimeModule>,
}

impl Module {
    pub fn runtimes_json(&self) -> String {
        serde_json::to_string(&self.runtimes).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn logs(&self) {
        for (runtime, module) in &self.runtimes {
            note!(
                "[gwnative] {runtime}: template save {}, enhancements {}",
                module.template_save,
                module.enhancements
            );
        }
    }
}

pub struct Prepared {
    pub derived: DerivedModules,
    pub module: Module,
}

pub fn failed(enhance: bool) -> Prepared {
    let mut runtimes = BTreeMap::new();
    for runtime in Runtime::ALL {
        runtimes.insert(
            runtime.key(),
            RuntimeModule::unavailable(None, enhancements::FAILED, enhance),
        );
    }
    Prepared {
        derived: DerivedModules::default(),
        module: Module { runtimes },
    }
}

/// Prepare both official runtime modules against one verified certificate feed.
///
/// Downloaded certificates are refreshed after this function returns and take
/// effect on the next launch. No network request is on the game boot path.
pub fn prepare(
    root: &Path,
    derived_root: &Path,
    certificate_cache: &Path,
    enhance: bool,
    benchmark_api: bool,
    generations: &generation::Store,
) -> Outcome<Prepared> {
    let feed = certificate::load(certificate_cache)?;
    certificate::spawn_refresh(certificate_cache.to_path_buf(), feed.sequence);
    let benchmark_targets = if benchmark_api {
        Some(benchmark_targets(root, &feed)?)
    } else {
        None
    };

    let mut derived = DerivedModules::default();
    let mut runtimes = BTreeMap::new();
    for runtime in Runtime::ALL {
        let (path, module) = prepare_runtime(
            root,
            derived_root,
            &feed,
            runtime,
            enhance,
            benchmark_targets,
            generations,
        );
        if let Some(path) = path {
            derived.insert(runtime, path);
        }
        runtimes.insert(runtime.key(), module);
    }
    Ok(Prepared {
        derived,
        module: Module { runtimes },
    })
}

fn benchmark_targets(root: &Path, feed: &CertificateFeed) -> Outcome<rewrite::BenchmarkTargets> {
    let load = |runtime: Runtime| -> Outcome<(Vec<u8>, u32)> {
        let wasm_path = root.join(runtime.wasm_name());
        let glue_path = root.join(runtime.glue_name());
        let wasm = fs::read(&wasm_path).map_err(|e| format!("{}: {e}", wasm_path.display()))?;
        let glue = fs::read(&glue_path).map_err(|e| format!("{}: {e}", glue_path.display()))?;
        let selected = feed
            .select(runtime, &digest(&wasm), &digest(&glue))
            .ok_or_else(|| {
                format!(
                    "{} benchmark API requires a certified artifact pair",
                    runtime.key()
                )
            })?;
        Ok((wasm, selected.runtime.template.import_count))
    };
    let (jspi, jspi_imports) = load(Runtime::Jspi)?;
    let (asyncify, asyncify_imports) = load(Runtime::Asyncify)?;
    rewrite::benchmark_target_pair(&jspi, jspi_imports, &asyncify, asyncify_imports)
}

/// Build an unsigned, fail-closed candidate from a new official artifact pair.
///
/// The most recent certificate supplies semantic function identities and the
/// read-only layout as review input. Every target body must retain its exact
/// anchor; only the two transform output hashes are recomputed. Passive
/// enhancements are inherited only when both new runtimes reproduce every
/// exact section identity that authorized the reviewed layout. A changed proof
/// keeps template saving certifiable while leaving the observer disabled.
pub fn certificate_candidate(root: &Path) -> Outcome<String> {
    let feed = certificate::bundled()?;
    let prototype = feed
        .families
        .last()
        .ok_or("certificate candidate: no prototype family")?;
    // Template anchors follow the newest family, but one changed layout proof
    // must not erase the last layout that was actually certified. A later
    // ArenaNet build may return to those exact bytes.
    let layout_prototype = feed
        .families
        .iter()
        .rev()
        .find_map(|family| family.layout.clone());
    let global_count = layout_prototype
        .as_ref()
        .map(|layout| layout.shared_global_count);

    let mut runtimes = Vec::new();
    let mut layout_proofs = Vec::new();
    for runtime in Runtime::ALL {
        let prototype_runtime = prototype
            .runtimes
            .iter()
            .find(|candidate| candidate.runtime == runtime)
            .ok_or_else(|| {
                format!(
                    "certificate candidate: prototype has no {} runtime",
                    runtime.key()
                )
            })?;
        let wasm_path = root.join(runtime.wasm_name());
        let glue_path = root.join(runtime.glue_name());
        let wasm = fs::read(&wasm_path)
            .map_err(|e| format!("certificate candidate: {}: {e}", wasm_path.display()))?;
        let glue = fs::read(&glue_path)
            .map_err(|e| format!("certificate candidate: {}: {e}", glue_path.display()))?;
        if let Some(global_count) = global_count {
            layout_proofs.push(rewrite::layout_proof(&wasm, global_count));
        }
        let (certificate, _) = rewrite::recertify(&wasm, &glue, prototype_runtime)?;
        runtimes.push(certificate);
    }

    let layout = inherit_layout(layout_prototype, layout_proofs.as_slice());
    let passive_enhancements = layout.is_some();
    for runtime in &mut runtimes {
        runtime.passive_enhancements = passive_enhancements;
    }
    let family_id = certificate::artifact_family_id(&runtimes)?;
    let family = certificate::BuildFamily {
        family_id,
        layout,
        runtimes,
    };
    certificate::validate_candidate(&family)?;
    serde_json::to_string_pretty(&family)
        .map_err(|e| format!("certificate candidate: cannot encode JSON: {e}"))
}

fn inherit_layout(
    prototype: Option<certificate::LayoutCertificate>,
    proofs: &[Outcome<rewrite::LayoutProof>],
) -> Option<certificate::LayoutCertificate> {
    match (prototype, proofs) {
        (Some(layout), [Ok(jspi), Ok(asyncify)])
            if jspi.data_sha256 == asyncify.data_sha256
                && jspi.element_sha256 == asyncify.element_sha256
                && jspi.shared_global_prefix_sha256 == asyncify.shared_global_prefix_sha256
                && jspi.data_sha256 == layout.data_sha256
                && jspi.element_sha256 == layout.element_sha256
                && jspi.shared_global_prefix_sha256 == layout.shared_global_prefix_sha256 =>
        {
            Some(layout)
        }
        _ => {
            note!(
                "[gwnative] certificate candidate: no shared passive-observer layout; \
                 template transforms remain independently certifiable"
            );
            None
        }
    }
}

fn prepare_runtime(
    root: &Path,
    cache_root: &Path,
    feed: &CertificateFeed,
    runtime: Runtime,
    enhance: bool,
    benchmark_targets: Option<rewrite::BenchmarkTargets>,
    generations: &generation::Store,
) -> (Option<PathBuf>, RuntimeModule) {
    match prepare_runtime_inner(
        root,
        cache_root,
        feed,
        runtime,
        enhance,
        benchmark_targets,
        generations,
    ) {
        Ok(result) => result,
        Err(reason) => {
            note!("[gwnative] {}: {reason}", runtime.key());
            (
                None,
                RuntimeModule::unavailable(None, enhancements::FAILED, enhance),
            )
        }
    }
}

fn prepare_runtime_inner(
    root: &Path,
    cache_root: &Path,
    feed: &CertificateFeed,
    runtime: Runtime,
    enhance: bool,
    benchmark_targets: Option<rewrite::BenchmarkTargets>,
    generations: &generation::Store,
) -> Outcome<(Option<PathBuf>, RuntimeModule)> {
    let wasm_path = root.join(runtime.wasm_name());
    let glue_path = root.join(runtime.glue_name());
    let input = fs::read(&wasm_path).map_err(|e| format!("{}: {e}", wasm_path.display()))?;
    let glue = fs::read(&glue_path).map_err(|e| format!("{}: {e}", glue_path.display()))?;
    let wasm_hash = digest(&input);
    let glue_hash = digest(&glue);
    let Some(selected) = feed.select(runtime, &wasm_hash, &glue_hash) else {
        let compatibility_id = runtime_compatibility_id(
            runtime,
            &wasm_hash,
            &glue_hash,
            certificate::TRANSFORM_ABI,
            None,
        );
        return Ok((
            None,
            RuntimeModule::unavailable(Some(compatibility_id), enhancements::UNCERTIFIED, enhance),
        ));
    };
    let compatibility_id = runtime_compatibility_id(
        runtime,
        &wasm_hash,
        &glue_hash,
        certificate::TRANSFORM_ABI,
        Some(&selected.runtime.template.output_sha256),
    );
    if generations.transform_disabled(runtime.key(), &compatibility_id) {
        return Ok((
            None,
            RuntimeModule::unavailable(Some(compatibility_id), enhancements::FAILED, enhance),
        ));
    }

    let derived = derive(cache_root, runtime, &input, &selected, benchmark_targets)?;
    let (enhancement_state, manifest) = if !enhance {
        (enhancements::OFF, None)
    } else if !selected.runtime.passive_enhancements {
        (enhancements::UNCERTIFIED, None)
    } else if let Some(layout) = &selected.family.layout {
        match rewrite::verify_layout(&input, layout) {
            Ok(()) => (
                enhancements::READY,
                Some(layout.page_manifest(&selected.family.family_id)),
            ),
            Err(reason) => {
                note!("[gwnative] {} enhancements: {reason}", runtime.key());
                (enhancements::FAILED, None)
            }
        }
    } else {
        (enhancements::UNCERTIFIED, None)
    };
    Ok((
        Some(derived),
        RuntimeModule {
            build: Some(compatibility_id),
            template_save: "ready",
            enhancements: enhancement_state,
            enhancement_manifest: manifest,
        },
    ))
}

fn derive(
    cache_root: &Path,
    runtime: Runtime,
    input: &[u8],
    selected: &Selected<'_>,
    benchmark_targets: Option<rewrite::BenchmarkTargets>,
) -> Outcome<PathBuf> {
    let certificate = selected.runtime;
    let dir = cache_root
        .join(runtime.key())
        .join(&certificate.wasm_sha256)
        .join(certificate::TRANSFORM_ABI.to_string());
    let path = dir.join(runtime.wasm_name());
    if !stamped(&dir, &path, certificate) {
        let output = rewrite::rewrite(input, certificate)?;
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        write_atomic(&path, &output)?;
        write_atomic(
            &dir.join(STAMP),
            serde_json::json!({
                "inputSha256": certificate.wasm_sha256,
                "glueSha256": certificate.glue_sha256,
                "transformAbi": certificate::TRANSFORM_ABI,
                "outputSha256": certificate.template.output_sha256,
            })
            .to_string()
            .as_bytes(),
        )?;
    }
    prune_derived(cache_root, runtime, certificate);
    let Some(benchmark_targets) = benchmark_targets else {
        return Ok(path);
    };

    let benchmark_dir = dir.join(format!("benchmark-{BENCHMARK_API_ABI}"));
    let benchmark_path = benchmark_dir.join(runtime.wasm_name());
    let transformed = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let benchmark_runtime = match runtime {
        Runtime::Jspi => rewrite::BenchmarkRuntime::Jspi,
        Runtime::Asyncify => rewrite::BenchmarkRuntime::Asyncify,
    };
    let transformed = rewrite::add_benchmark_api(
        &transformed,
        certificate.template.import_count,
        benchmark_targets,
        benchmark_runtime,
    )?;
    fs::create_dir_all(&benchmark_dir).map_err(|e| format!("{}: {e}", benchmark_dir.display()))?;
    write_atomic(&benchmark_path, &transformed)?;
    prune_sibling_directories(&dir, &format!("benchmark-{BENCHMARK_API_ABI}"));
    Ok(benchmark_path)
}

/// Keep only the active artifact and transform ABI for each runtime.
///
/// ArenaNet publishes often and the Asyncify output alone is about 28 MB. This
/// cache is app-owned derived data, so retaining every historical pair would
/// turn fast certificate updates into unbounded disk growth.
fn prune_derived(
    cache_root: &Path,
    runtime: Runtime,
    certificate: &certificate::RuntimeCertificate,
) {
    let runtime_root = cache_root.join(runtime.key());
    prune_sibling_directories(&runtime_root, &certificate.wasm_sha256);
    prune_sibling_directories(
        &runtime_root.join(&certificate.wasm_sha256),
        &certificate::TRANSFORM_ABI.to_string(),
    );
}

fn prune_sibling_directories(parent: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == keep
            || !entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(entry.path()) {
            note!(
                "[gwnative] could not prune derived client {}: {error}",
                entry.path().display()
            );
        }
    }
}

fn stamped(dir: &Path, derived: &Path, certificate: &certificate::RuntimeCertificate) -> bool {
    let Ok(stamp) = fs::read(dir.join(STAMP)) else {
        return false;
    };
    let Ok(stamp) = serde_json::from_slice::<serde_json::Value>(&stamp) else {
        return false;
    };
    if stamp["inputSha256"].as_str() != Some(certificate.wasm_sha256.as_str())
        || stamp["glueSha256"].as_str() != Some(certificate.glue_sha256.as_str())
        || stamp["transformAbi"].as_u64() != Some(u64::from(certificate::TRANSFORM_ABI))
    {
        return false;
    }
    fs::read(derived).is_ok_and(|bytes| digest(&bytes) == certificate.template.output_sha256)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Outcome<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|e| format!("{}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn markers_json() -> String {
    certificate::markers_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_reach_the_page_as_an_object() {
        let json: serde_json::Value = serde_json::from_str(&markers_json()).unwrap();
        assert_eq!(json["ensureDirectory"], -70_001);
        assert_eq!(json["fileExists"], -70_005);
        assert_eq!(json.as_object().unwrap().len(), 5);
    }

    #[test]
    fn runtime_compatibility_identity_covers_pair_runtime_and_transform_abi() {
        let wasm = "a".repeat(64);
        let glue = "b".repeat(64);
        let output = "d".repeat(64);
        let jspi = runtime_compatibility_id(Runtime::Jspi, &wasm, &glue, 2, Some(&output));
        assert_eq!(jspi.len(), 64);
        assert_ne!(
            jspi,
            runtime_compatibility_id(Runtime::Jspi, &wasm, &"c".repeat(64), 2, Some(&output))
        );
        assert_ne!(
            jspi,
            runtime_compatibility_id(Runtime::Asyncify, &wasm, &glue, 2, Some(&output))
        );
        assert_ne!(
            jspi,
            runtime_compatibility_id(Runtime::Jspi, &wasm, &glue, 3, Some(&output))
        );
        assert_ne!(
            jspi,
            runtime_compatibility_id(Runtime::Jspi, &wasm, &glue, 2, Some(&"e".repeat(64)))
        );
    }

    #[test]
    fn passive_layout_is_inherited_only_from_two_exact_proofs() {
        let layout = certificate::bundled().unwrap().families[0]
            .layout
            .clone()
            .unwrap();
        let proof = || rewrite::LayoutProof {
            data_sha256: layout.data_sha256.clone(),
            element_sha256: layout.element_sha256.clone(),
            shared_global_prefix_sha256: layout.shared_global_prefix_sha256.clone(),
        };
        let inherited = inherit_layout(Some(layout.clone()), &[Ok(proof()), Ok(proof())]).unwrap();
        assert_eq!(inherited.layout_words, layout.layout_words);

        let mut changed = proof();
        changed.data_sha256 = "0".repeat(64);
        assert!(inherit_layout(Some(layout.clone()), &[Ok(proof()), Ok(changed)]).is_none());
        assert!(
            inherit_layout(
                Some(layout.clone()),
                &[Ok(proof()), Ok(proof()), Ok(proof())]
            )
            .is_none()
        );
    }

    #[test]
    fn companion_cannot_reenter_the_game() {
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        let mut globals = Vec::new();
        let mut data_segments = 0;
        let mut has_start = false;
        for payload in wasmparser::Parser::new(0).parse_all(COMPANION_KERNEL) {
            match payload.unwrap() {
                wasmparser::Payload::ImportSection(reader) => {
                    for import in reader.into_imports() {
                        let import = import.unwrap();
                        imports.push((import.module.to_owned(), import.name.to_owned()));
                    }
                }
                wasmparser::Payload::ExportSection(reader) => {
                    for export in reader {
                        let export = export.unwrap();
                        exports.push((export.name.to_owned(), export.kind, export.index));
                    }
                }
                wasmparser::Payload::GlobalSection(reader) => {
                    globals.extend(reader.into_iter().map(|global| global.unwrap().ty.mutable));
                }
                wasmparser::Payload::DataSection(reader) => {
                    data_segments += reader.count();
                }
                wasmparser::Payload::StartSection { .. } => has_start = true,
                _ => {}
            }
        }
        assert_eq!(imports, [("env".to_owned(), "memory".to_owned())]);
        assert!(!has_start, "instantiation must not run companion code");
        assert_eq!(
            data_segments, 0,
            "instantiating the companion must not initialize the game's memory"
        );
        assert!(exports.iter().any(|(name, kind, _)| {
            name == "companion_init" && *kind == wasmparser::ExternalKind::Func
        }));
        assert!(exports.iter().any(|(name, kind, _)| {
            name == "companion_observe" && *kind == wasmparser::ExternalKind::Func
        }));
        assert!(exports.iter().any(|(name, kind, _)| {
            name == "companion_runtime_size" && *kind == wasmparser::ExternalKind::Func
        }));
        let (_, _, stack_index) = exports
            .iter()
            .find(|(name, kind, _)| {
                name == "__stack_pointer" && *kind == wasmparser::ExternalKind::Global
            })
            .expect("the page must be able to relocate the companion stack");
        assert_eq!(globals.get(*stack_index as usize), Some(&true));
        assert!(!exports.iter().any(|(name, _, _)| name.contains("tick")));
    }

    #[test]
    fn derived_cache_retains_only_the_active_artifact_and_abi() {
        let temporary = crate::scratch::TempDir::new("derived-prune");
        let feed = certificate::bundled().unwrap();
        let certificate = feed.families[0]
            .runtimes
            .iter()
            .find(|candidate| candidate.runtime == Runtime::Jspi)
            .unwrap();
        let runtime_root = temporary.0.join(Runtime::Jspi.key());
        let active = runtime_root
            .join(&certificate.wasm_sha256)
            .join(certificate::TRANSFORM_ABI.to_string());
        let old_artifact = runtime_root.join("old-artifact").join("1");
        let old_abi = runtime_root.join(&certificate.wasm_sha256).join("1");
        for path in [&active, &old_artifact, &old_abi] {
            fs::create_dir_all(path).unwrap();
        }

        prune_derived(&temporary.0, Runtime::Jspi, certificate);

        assert!(active.is_dir());
        assert!(!old_artifact.exists());
        assert!(!old_abi.exists());
    }

    #[test]
    fn unknown_official_pairs_run_unmodified_with_optional_features_disabled() {
        let temporary = crate::scratch::TempDir::new("unknown-runtime-pairs");
        let root = temporary.0.join("web");
        fs::create_dir_all(&root).unwrap();
        let feed = certificate::bundled().unwrap();
        let generations = generation::Store::open(temporary.0.join("support").join("generations"));

        for runtime in Runtime::ALL {
            fs::write(
                root.join(runtime.wasm_name()),
                format!("new {} wasm", runtime.key()),
            )
            .unwrap();
            fs::write(
                root.join(runtime.glue_name()),
                format!("new {} glue", runtime.key()),
            )
            .unwrap();
            let (derived, module) = prepare_runtime(
                &root,
                &temporary.0.join("derived"),
                &feed,
                runtime,
                true,
                None,
                &generations,
            );
            assert!(
                derived.is_none(),
                "an unknown pair must serve ArenaNet's exact module"
            );
            assert_eq!(module.template_save, "uncertified");
            assert_eq!(module.enhancements, enhancements::UNCERTIFIED);
            assert!(module.enhancement_manifest.is_none());
            assert_eq!(module.build.as_deref().map(str::len), Some(64));
        }
    }

    #[test]
    fn a_locally_failed_exact_transform_serves_the_official_module() {
        let temporary = crate::scratch::TempDir::new("disabled-runtime-transform");
        let root = temporary.0.join("web");
        fs::create_dir_all(&root).unwrap();
        let runtime = Runtime::Jspi;
        let wasm = b"official wasm";
        let glue = b"official glue";
        fs::write(root.join(runtime.wasm_name()), wasm).unwrap();
        fs::write(root.join(runtime.glue_name()), glue).unwrap();

        let mut feed = certificate::bundled().unwrap();
        let certified = feed.families[0]
            .runtimes
            .iter_mut()
            .find(|candidate| candidate.runtime == runtime)
            .unwrap();
        certified.wasm_sha256 = digest(wasm);
        certified.glue_sha256 = digest(glue);
        let compatibility_id = runtime_compatibility_id(
            runtime,
            &certified.wasm_sha256,
            &certified.glue_sha256,
            certificate::TRANSFORM_ABI,
            Some(&certified.template.output_sha256),
        );
        let generations = generation::Store::open(temporary.0.join("support").join("generations"));
        generations
            .disable_transform(runtime.key(), &compatibility_id)
            .unwrap();

        let (derived, module) = prepare_runtime(
            &root,
            &temporary.0.join("derived"),
            &feed,
            runtime,
            true,
            None,
            &generations,
        );
        assert!(derived.is_none());
        assert_eq!(module.build.as_deref(), Some(compatibility_id.as_str()));
        assert_eq!(module.template_save, "failed");
        assert_eq!(module.enhancements, enhancements::FAILED);
    }

    #[test]
    fn external_official_pairs_produce_valid_candidates() {
        let external = std::env::var("GWNATIVE_CERTIFY_FEED").is_ok();
        let feed = match std::env::var("GWNATIVE_CERTIFY_FEED") {
            Ok(path) => {
                serde_json::from_slice::<CertificateFeed>(&fs::read(path).unwrap()).unwrap()
            }
            Err(_) => certificate::bundled().unwrap(),
        };
        feed.validate().unwrap();
        let mut verified = 0;
        for (runtime, wasm_variable, glue_variable) in [
            (
                Runtime::Jspi,
                "GWNATIVE_CERTIFY_JSPI_WASM",
                "GWNATIVE_CERTIFY_JSPI_GLUE",
            ),
            (
                Runtime::Asyncify,
                "GWNATIVE_CERTIFY_ASYNCIFY_WASM",
                "GWNATIVE_CERTIFY_ASYNCIFY_GLUE",
            ),
        ] {
            let paths = (std::env::var(wasm_variable), std::env::var(glue_variable));
            let (wasm_path, glue_path) = match paths {
                (Ok(wasm), Ok(glue)) => (wasm, glue),
                _ if !external => continue,
                _ => panic!(
                    "external certification requires both {wasm_variable} and {glue_variable}"
                ),
            };
            let wasm = fs::read(wasm_path).unwrap();
            let glue = fs::read(glue_path).unwrap();
            let selected = feed
                .select(runtime, &digest(&wasm), &digest(&glue))
                .expect("the exact official pair is in the certificate");
            if let Some(layout) = &selected.family.layout {
                rewrite::verify_layout(&wasm, layout).unwrap();
            }
            let output = rewrite::candidate(&wasm, selected.runtime).unwrap();
            let output_hash = digest(&output);
            eprintln!("{} candidate sha256 {output_hash}", runtime.key());
            assert_eq!(output_hash, selected.runtime.template.output_sha256);
            verified += 1;
        }
        if external {
            assert_eq!(verified, 2, "both official runtime pairs must be verified");
        }
    }

    #[test]
    fn external_runtime_pair_prepares_both_derived_modules() {
        let Ok(root) = std::env::var("GWNATIVE_CERTIFY_ROOT") else {
            return;
        };
        let feed = match std::env::var("GWNATIVE_CERTIFY_FEED") {
            Ok(path) => {
                serde_json::from_slice::<CertificateFeed>(&fs::read(path).unwrap()).unwrap()
            }
            Err(_) => certificate::bundled().unwrap(),
        };
        feed.validate().unwrap();
        let temporary = crate::scratch::TempDir::new("dual-runtime-derive");
        let generations = generation::Store::open(temporary.0.join("support").join("generations"));
        let targets = benchmark_targets(Path::new(&root), &feed).unwrap();
        for runtime in Runtime::ALL {
            let (derived, module) = prepare_runtime(
                Path::new(&root),
                &temporary.0.join("derived"),
                &feed,
                runtime,
                true,
                Some(targets),
                &generations,
            );
            assert!(derived.is_some());
            assert_eq!(module.template_save, "ready");
            let selected = feed
                .select(
                    runtime,
                    &digest(&fs::read(Path::new(&root).join(runtime.wasm_name())).unwrap()),
                    &digest(&fs::read(Path::new(&root).join(runtime.glue_name())).unwrap()),
                )
                .unwrap();
            if selected.runtime.passive_enhancements {
                assert_eq!(module.enhancements, enhancements::READY);
                assert!(module.enhancement_manifest.is_some());
            } else {
                assert_eq!(module.enhancements, enhancements::UNCERTIFIED);
                assert!(module.enhancement_manifest.is_none());
            }
        }
    }
}
