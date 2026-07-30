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

pub use certificate::Runtime;
use certificate::{CertificateFeed, Selected};

type Fault = String;
type Outcome<T> = std::result::Result<T, Fault>;

fn digest(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

const STAMP: &str = "derived.json";

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

    fn insert(&mut self, runtime: Runtime, path: PathBuf) {
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
    for runtime in [Runtime::Jspi, Runtime::Asyncify] {
        runtimes.insert(
            runtime.key(),
            RuntimeModule {
                build: None,
                template_save: "failed",
                enhancements: if enhance {
                    enhancements::FAILED
                } else {
                    enhancements::OFF
                },
                enhancement_manifest: None,
            },
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
) -> Outcome<Prepared> {
    let feed = certificate::load(certificate_cache)?;
    certificate::spawn_refresh(certificate_cache.to_path_buf(), feed.sequence);

    let mut derived = DerivedModules::default();
    let mut runtimes = BTreeMap::new();
    for runtime in [Runtime::Jspi, Runtime::Asyncify] {
        let (path, module) = prepare_runtime(root, derived_root, &feed, runtime, enhance);
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

/// Build an unsigned, fail-closed candidate from a new official artifact pair.
///
/// The most recent certificate supplies semantic function identities and the
/// read-only layout as review input. Stub body hashes and both transform output
/// hashes are recomputed. Passive enhancements stay disabled until live layout
/// probes have independently certified the copied offsets.
pub fn certificate_candidate(root: &Path) -> Outcome<String> {
    let feed = certificate::bundled()?;
    let prototype = feed
        .families
        .last()
        .ok_or("certificate candidate: no prototype family")?;
    let global_count = prototype.layout.shared_global_count;

    let mut runtimes = Vec::new();
    let mut shared_proof: Option<rewrite::LayoutProof> = None;
    for runtime in [Runtime::Jspi, Runtime::Asyncify] {
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
        let proof = rewrite::layout_proof(&wasm, global_count)?;
        if let Some(shared) = &shared_proof {
            if proof.data_sha256 != shared.data_sha256
                || proof.element_sha256 != shared.element_sha256
                || proof.shared_global_prefix_sha256 != shared.shared_global_prefix_sha256
            {
                return Err(format!(
                    "certificate candidate: {} does not share the artifact-family layout",
                    runtime.key()
                ));
            }
        } else {
            shared_proof = Some(proof);
        }
        let (certificate, _) = rewrite::recertify(&wasm, &glue, prototype_runtime)?;
        runtimes.push(certificate);
    }

    let proof = shared_proof.ok_or("certificate candidate: no runtime artifacts")?;
    let mut layout = prototype.layout.clone();
    layout.data_sha256 = proof.data_sha256;
    layout.element_sha256 = proof.element_sha256;
    layout.shared_global_prefix_sha256 = proof.shared_global_prefix_sha256;
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

fn prepare_runtime(
    root: &Path,
    cache_root: &Path,
    feed: &CertificateFeed,
    runtime: Runtime,
    enhance: bool,
) -> (Option<PathBuf>, RuntimeModule) {
    match prepare_runtime_inner(root, cache_root, feed, runtime, enhance) {
        Ok(result) => result,
        Err(reason) => {
            note!("[gwnative] {}: {reason}", runtime.key());
            (
                None,
                RuntimeModule {
                    build: None,
                    template_save: "failed",
                    enhancements: if enhance {
                        enhancements::FAILED
                    } else {
                        enhancements::OFF
                    },
                    enhancement_manifest: None,
                },
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
) -> Outcome<(Option<PathBuf>, RuntimeModule)> {
    let wasm_path = root.join(runtime.wasm_name());
    let glue_path = root.join(runtime.glue_name());
    let input = fs::read(&wasm_path).map_err(|e| format!("{}: {e}", wasm_path.display()))?;
    let glue = fs::read(&glue_path).map_err(|e| format!("{}: {e}", glue_path.display()))?;
    let wasm_hash = digest(&input);
    let glue_hash = digest(&glue);
    let Some(selected) = feed.select(runtime, &wasm_hash, &glue_hash) else {
        return Ok((
            None,
            RuntimeModule {
                build: Some(wasm_hash),
                template_save: "uncertified",
                enhancements: if enhance {
                    enhancements::UNCERTIFIED
                } else {
                    enhancements::OFF
                },
                enhancement_manifest: None,
            },
        ));
    };

    rewrite::verify_layout(&input, &selected.family.layout)?;
    let derived = derive(cache_root, runtime, &input, &selected)?;
    let enhancement_state = if !enhance {
        enhancements::OFF
    } else if selected.runtime.passive_enhancements {
        enhancements::READY
    } else {
        enhancements::UNCERTIFIED
    };
    let manifest = (enhancement_state == enhancements::READY).then(|| {
        selected
            .family
            .layout
            .page_manifest(&selected.family.family_id)
    });
    Ok((
        Some(derived),
        RuntimeModule {
            build: Some(wasm_hash),
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
) -> Outcome<PathBuf> {
    let certificate = selected.runtime;
    let dir = cache_root
        .join(runtime.key())
        .join(&certificate.wasm_sha256)
        .join(certificate::TRANSFORM_ABI.to_string());
    let path = dir.join(runtime.wasm_name());
    if stamped(&dir, &path, certificate) {
        prune_derived(cache_root, runtime, certificate);
        return Ok(path);
    }

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
    prune_derived(cache_root, runtime, certificate);
    Ok(path)
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
            rewrite::verify_layout(&wasm, &selected.family.layout).unwrap();
            let output = rewrite::candidate(&wasm, selected.runtime).unwrap();
            eprintln!("{} candidate sha256 {}", runtime.key(), digest(&output));
            assert_eq!(digest(&output), selected.runtime.template.output_sha256);
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
        let temporary = crate::scratch::TempDir::new("dual-runtime-derive");
        let prepared = prepare(
            Path::new(&root),
            &temporary.0.join("derived"),
            &temporary.0.join("certificates"),
            true,
        )
        .unwrap();
        assert!(prepared.derived.get(Runtime::Jspi.wasm_name()).is_some());
        assert!(
            prepared
                .derived
                .get(Runtime::Asyncify.wasm_name())
                .is_some()
        );
        let modules: serde_json::Value =
            serde_json::from_str(&prepared.module.runtimes_json()).unwrap();
        for runtime in ["jspi", "asyncify"] {
            assert_eq!(modules[runtime]["templateSave"], "ready");
            assert_eq!(modules[runtime]["enhancements"], "ready");
            let family_id = modules[runtime]["enhancementManifest"]["familyId"]
                .as_str()
                .unwrap();
            assert_eq!(family_id.len(), 64);
            assert!(family_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }
}
