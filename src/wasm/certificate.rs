//! Signed, data-only certification for ArenaNet client artifact families.
//!
//! ArenaNet can publish a client more often than gwnative can publish an app.
//! Keeping build identities in Rust therefore turns every otherwise compatible
//! client into an application release.  This feed moves only the evidence that
//! changes per client — hashes, semantic call-site identities, and read-only
//! memory offsets — outside the binary.  The transform itself remains compiled
//! Rust code and accepts no instruction bytes from the feed.
//!
//! The feed uses the same Ed25519 key as the Sparkle update feed.  The public
//! half is compiled into the application; a cached or downloaded feed is used
//! only after its detached signature verifies and its schema passes every
//! invariant below.  A bad or unavailable update therefore removes optional
//! compatibility for a new build, never changes how an old one executes.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use base64::Engine;
use ed25519_compact::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::Outcome;

const SCHEMA_VERSION: u32 = 1;
pub(super) const TRANSFORM_ABI: u32 = 2;
const MAX_FAMILIES: usize = 32;
const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 256;
const FEED_NAME: &str = "builds.json";
const SIGNATURE_NAME: &str = "builds.json.sig";
const PUBLIC_KEY: &str = include_str!("../../packaging/sparkle/public-key.txt");
const BUNDLED_FEED: &[u8] = include_bytes!("../../certificates/builds.json");
const REMOTE_FEED: &str =
    "https://raw.githubusercontent.com/jean-humann/gwnative/main/certificates/builds.json";
const REMOTE_SIGNATURE: &str =
    "https://raw.githubusercontent.com/jean-humann/gwnative/main/certificates/builds.json.sig";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Jspi,
    Asyncify,
}

impl Runtime {
    pub fn key(self) -> &'static str {
        match self {
            Self::Jspi => "jspi",
            Self::Asyncify => "asyncify",
        }
    }

    pub fn wasm_name(self) -> &'static str {
        match self {
            Self::Jspi => "Gw.jspi.wasm",
            Self::Asyncify => "Gw.wasm",
        }
    }

    pub fn glue_name(self) -> &'static str {
        match self {
            Self::Jspi => "Gw.jspi.js",
            Self::Asyncify => "Gw.js",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub(super) enum BridgeKind {
    EnsureDirectory,
    FindFiles,
    FileBaseName,
    DeleteFile,
    FileExists,
}

impl BridgeKind {
    pub(super) const ALL: [Self; 5] = [
        Self::EnsureDirectory,
        Self::FindFiles,
        Self::FileBaseName,
        Self::DeleteFile,
        Self::FileExists,
    ];

    pub(super) const fn marker(self) -> i64 {
        match self {
            Self::EnsureDirectory => -70_001,
            Self::FindFiles => -70_002,
            Self::FileBaseName => -70_003,
            Self::DeleteFile => -70_004,
            Self::FileExists => -70_005,
        }
    }

    pub(super) const fn key(self) -> &'static str {
        match self {
            Self::EnsureDirectory => "ensureDirectory",
            Self::FindFiles => "findFiles",
            Self::FileBaseName => "fileBaseName",
            Self::DeleteFile => "deleteFile",
            Self::FileExists => "fileExists",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CertificateFeed {
    pub schema_version: u32,
    pub sequence: u64,
    pub transform_abi: u32,
    pub families: Vec<BuildFamily>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildFamily {
    /// Derived from both exact official runtime pairs. This is an identity,
    /// not publisher metadata: candidate generation and feed validation
    /// independently compute it from the four artifact hashes.
    pub family_id: String,
    pub layout: LayoutCertificate,
    pub runtimes: Vec<RuntimeCertificate>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutCertificate {
    pub snapshot_abi: u32,
    pub snapshot_bytes: u32,
    pub cursor_snapshot_abi: u32,
    pub cursor_snapshot_bytes: u32,
    pub layout_words: Vec<u32>,
    /// Exact section/prefix identities independently checked in both official
    /// runtime artifacts before these read-only offsets may be used.
    pub data_sha256: String,
    pub element_sha256: String,
    pub shared_global_prefix_sha256: String,
    pub shared_global_count: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeCertificate {
    pub runtime: Runtime,
    pub wasm_sha256: String,
    pub glue_sha256: String,
    pub template: TemplateCertificate,
    pub passive_enhancements: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateCertificate {
    pub output_sha256: String,
    pub import_count: u32,
    pub carrier_import: u32,
    pub bridges: Vec<BridgeCertificate>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeCertificate {
    pub kind: BridgeKind,
    /// Code-section local index.  This is deliberately not a global function
    /// index: appending functions cannot move it, and imported functions cannot
    /// accidentally be selected.
    pub stub_function: usize,
    /// Exact body identity where the target is an ArenaNet stub. FileExists is
    /// a real implementation and is instead bound by the signed artifact hash
    /// plus the semantic call-site target.
    pub stub_body_sha256: Option<String>,
    pub call_sites: Vec<CallSiteCertificate>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallSiteCertificate {
    pub local_function: usize,
    /// Zero-based occurrence among calls to this bridge's certified target.
    pub occurrence: usize,
    /// Counted before rewriting.  This prevents a function that gained or lost
    /// another call to the same target from making an occurrence ambiguous.
    pub expected_target_calls: usize,
}

#[derive(Clone)]
pub struct Selected<'a> {
    pub family: &'a BuildFamily,
    pub runtime: &'a RuntimeCertificate,
}

impl CertificateFeed {
    pub fn select(
        &self,
        runtime: Runtime,
        wasm_sha256: &str,
        glue_sha256: &str,
    ) -> Option<Selected<'_>> {
        self.families.iter().find_map(|family| {
            family
                .runtimes
                .iter()
                .find(|candidate| {
                    candidate.runtime == runtime
                        && candidate.wasm_sha256 == wasm_sha256
                        && candidate.glue_sha256 == glue_sha256
                })
                .map(|runtime| Selected { family, runtime })
        })
    }

    pub(super) fn validate(&self) -> Outcome<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "certificate: schema {} is not supported",
                self.schema_version
            ));
        }
        if self.transform_abi != TRANSFORM_ABI {
            return Err(format!(
                "certificate: transform ABI {} is not supported",
                self.transform_abi
            ));
        }
        if self.families.is_empty() || self.families.len() > MAX_FAMILIES {
            return Err("certificate: feed has an invalid artifact-family count".to_owned());
        }

        let mut families = HashSet::new();
        let mut artifacts = HashSet::new();
        for family in &self.families {
            hash("family ID", &family.family_id)?;
            if !families.insert(family.family_id.as_str()) {
                return Err(format!(
                    "certificate: duplicate artifact family {}",
                    family.family_id
                ));
            }
            family.layout.validate()?;
            if family.runtimes.len() != 2 {
                return Err(format!(
                    "certificate: family {} must contain both official runtimes",
                    family.family_id
                ));
            }
            let mut modes = HashSet::new();
            for runtime in &family.runtimes {
                if !modes.insert(runtime.runtime.key()) {
                    return Err(format!(
                        "certificate: family {} repeats runtime {}",
                        family.family_id,
                        runtime.runtime.key()
                    ));
                }
                hash("wasm", &runtime.wasm_sha256)?;
                hash("glue", &runtime.glue_sha256)?;
                if !artifacts.insert(runtime.wasm_sha256.as_str()) {
                    return Err(format!(
                        "certificate: artifact {} appears twice",
                        runtime.wasm_sha256
                    ));
                }
                runtime.template.validate()?;
            }
            if artifact_family_id(&family.runtimes)? != family.family_id {
                return Err(format!(
                    "certificate: family {} does not match its artifacts",
                    family.family_id
                ));
            }
        }
        Ok(())
    }
}

impl LayoutCertificate {
    fn validate(&self) -> Outcome<()> {
        if self.snapshot_abi != 1
            || self.snapshot_bytes != 64
            || self.cursor_snapshot_abi != 1
            || self.cursor_snapshot_bytes != 4160
            || self.layout_words.len() != 29
        {
            return Err("certificate: unsupported companion layout ABI".to_owned());
        }
        for value in [
            &self.data_sha256,
            &self.element_sha256,
            &self.shared_global_prefix_sha256,
        ] {
            hash("layout proof", value)?;
        }
        if self.shared_global_count == 0 {
            return Err("certificate: empty shared-global proof".to_owned());
        }
        Ok(())
    }

    pub fn page_manifest(&self, family_id: &str) -> serde_json::Value {
        serde_json::json!({
            "snapshotAbi": self.snapshot_abi,
            "snapshotBytes": self.snapshot_bytes,
            "cursorSnapshotAbi": self.cursor_snapshot_abi,
            "cursorSnapshotBytes": self.cursor_snapshot_bytes,
            "configBytes": self.layout_words.len() * std::mem::size_of::<u32>(),
            "layoutWords": self.layout_words,
            "familyId": family_id,
        })
    }
}

/// Stable identity for a tested JSPI/Asyncify artifact pair.
///
/// The domain separator and fixed runtime order make this independent of JSON
/// field and array ordering. Hash bytes, rather than a human build label, are
/// the only inputs.
pub(super) fn artifact_family_id(runtimes: &[RuntimeCertificate]) -> Outcome<String> {
    let mut digest = sha2::Sha256::new();
    digest.update(b"gwnative-artifact-family-v1\0");
    for expected in [Runtime::Jspi, Runtime::Asyncify] {
        let mut matches = runtimes
            .iter()
            .filter(|runtime| runtime.runtime == expected);
        let Some(runtime) = matches.next() else {
            return Err(format!(
                "certificate: artifact family must contain one {} runtime",
                expected.key()
            ));
        };
        if matches.next().is_some() {
            return Err(format!(
                "certificate: artifact family repeats the {} runtime",
                expected.key()
            ));
        }
        hash("wasm", &runtime.wasm_sha256)?;
        hash("glue", &runtime.glue_sha256)?;
        digest.update(expected.key().as_bytes());
        digest.update([0]);
        digest.update(
            hex::decode(&runtime.wasm_sha256)
                .map_err(|_| "certificate: malformed wasm sha256".to_owned())?,
        );
        digest.update(
            hex::decode(&runtime.glue_sha256)
                .map_err(|_| "certificate: malformed glue sha256".to_owned())?,
        );
    }
    Ok(hex::encode(digest.finalize()))
}

pub(super) fn validate_candidate(family: &BuildFamily) -> Outcome<()> {
    CertificateFeed {
        schema_version: SCHEMA_VERSION,
        sequence: 1,
        transform_abi: TRANSFORM_ABI,
        families: vec![family.clone()],
    }
    .validate()
}

impl TemplateCertificate {
    fn validate(&self) -> Outcome<()> {
        hash("template output", &self.output_sha256)?;
        if self.import_count == 0 || self.carrier_import >= self.import_count {
            return Err("certificate: invalid template import indices".to_owned());
        }
        if self.bridges.len() != BridgeKind::ALL.len() {
            return Err("certificate: template does not contain five bridges".to_owned());
        }
        let mut kinds = HashSet::new();
        for bridge in &self.bridges {
            if !kinds.insert(bridge.kind) || bridge.call_sites.is_empty() {
                return Err("certificate: duplicate or empty bridge".to_owned());
            }
            if let Some(body) = &bridge.stub_body_sha256 {
                hash("stub body", body)?;
            }
            for site in &bridge.call_sites {
                if site.expected_target_calls == 0 || site.occurrence >= site.expected_target_calls
                {
                    return Err(format!(
                        "certificate: invalid {} call occurrence",
                        bridge.kind.key()
                    ));
                }
            }
        }
        if BridgeKind::ALL.iter().any(|kind| !kinds.contains(kind)) {
            return Err("certificate: template bridge set is incomplete".to_owned());
        }
        Ok(())
    }
}

fn hash(label: &str, value: &str) -> Outcome<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("certificate: malformed {label} sha256"));
    }
    Ok(())
}

fn verify(feed: &[u8], signature: &[u8]) -> Outcome<CertificateFeed> {
    if feed.len() > MAX_FEED_BYTES || signature.len() > MAX_SIGNATURE_BYTES {
        return Err("certificate: signed feed is implausibly large".to_owned());
    }
    let decode = |name: &str, bytes: &[u8]| {
        base64::engine::general_purpose::STANDARD
            .decode(bytes.trim_ascii())
            .map_err(|e| format!("certificate: invalid {name}: {e}"))
    };
    let key = decode("public key", PUBLIC_KEY.as_bytes())?;
    let signature = decode("signature", signature)?;
    verify_signature(&key, feed, &signature)?;
    let parsed: CertificateFeed =
        serde_json::from_slice(feed).map_err(|e| format!("certificate: invalid JSON: {e}"))?;
    parsed.validate()?;
    Ok(parsed)
}

fn verify_signature(key: &[u8], message: &[u8], signature: &[u8]) -> Outcome<()> {
    let key = PublicKey::from_slice(key)
        .map_err(|_| "certificate: public key has the wrong length".to_owned())?;
    let signature = Signature::from_slice(signature)
        .map_err(|_| "certificate: signature has the wrong length".to_owned())?;
    key.verify(message, &signature)
        .map_err(|_| "certificate: signature verification failed".to_owned())
}

pub(super) fn bundled() -> Outcome<CertificateFeed> {
    if BUNDLED_FEED.len() > MAX_FEED_BYTES {
        return Err("certificate: bundled feed is implausibly large".to_owned());
    }
    // These bytes are part of the executable and are authenticated by the
    // application's code signature. Detached verification is for data that can
    // change without replacing that executable: the cache and remote feed.
    let parsed: CertificateFeed = serde_json::from_slice(BUNDLED_FEED)
        .map_err(|e| format!("certificate: invalid bundled JSON: {e}"))?;
    parsed.validate()?;
    Ok(parsed)
}

/// Best signed feed already on this Mac. A remote refresh is deliberately not
/// on the launch path: [`spawn_refresh`] writes it for the next launch.
pub fn load(cache: &Path) -> Outcome<CertificateFeed> {
    let bundled = bundled()?;
    let Ok(bytes) = fs::read(cache.join(FEED_NAME)) else {
        return Ok(bundled);
    };
    let Ok(signature) = fs::read(cache.join(SIGNATURE_NAME)) else {
        return Ok(bundled);
    };
    match verify(&bytes, &signature) {
        Ok(cached) if cached.sequence >= bundled.sequence => Ok(cached),
        Ok(_) => {
            note!("[gwnative] certificate: ignoring a signed rollback");
            Ok(bundled)
        }
        Err(reason) => {
            note!("[gwnative] {reason}; using bundled certificates");
            Ok(bundled)
        }
    }
}

/// Fetch a signed update in the background and make it available next launch.
///
/// The two files are fetched and verified as a pair before either cache entry
/// is replaced.  Publishing JSON before its signature, or vice versa, is
/// therefore merely a missed refresh and cannot poison the cache.
pub fn spawn_refresh(cache: PathBuf, minimum_sequence: u64) {
    let _ = thread::Builder::new()
        .name("gwnative-certificates".into())
        .spawn(move || {
            if let Err(reason) = refresh(&cache, minimum_sequence) {
                note!("[gwnative] certificate refresh: {reason}");
            }
        });
}

fn refresh(cache: &Path, minimum_sequence: u64) -> Outcome<()> {
    const HEADERS: &[(&str, &str)] = &[(
        "user-agent",
        concat!("gwnative/", env!("CARGO_PKG_VERSION")),
    )];
    let get = |url: &str, limit: usize| -> Outcome<Vec<u8>> {
        let response = crate::transport::fetch("GET", url, HEADERS, None, Duration::from_secs(5))
            .map_err(|e| format!("{url}: {e}"))?;
        if response.status != 200 {
            return Err(format!("{url}: HTTP {}", response.status));
        }
        if response.body.len() > limit {
            return Err(format!("{url}: response is too large"));
        }
        Ok(response.body)
    };
    let feed = get(REMOTE_FEED, MAX_FEED_BYTES)?;
    let signature = get(REMOTE_SIGNATURE, MAX_SIGNATURE_BYTES)?;
    let parsed = verify(&feed, &signature)?;
    if parsed.sequence < minimum_sequence {
        return Err(format!(
            "refusing sequence {} below {}",
            parsed.sequence, minimum_sequence
        ));
    }
    fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    write_atomic(&cache.join(FEED_NAME), &feed)?;
    write_atomic(&cache.join(SIGNATURE_NAME), &signature)?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Outcome<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|e| format!("{}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn markers_json() -> String {
    serde_json::Value::from(
        BridgeKind::ALL
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
    fn bundled_feed_is_self_consistent() {
        let feed = bundled().unwrap();
        assert!(!feed.families.is_empty());
    }

    #[test]
    fn unsigned_remote_data_is_never_accepted() {
        assert!(verify(BUNDLED_FEED, b"not a signature").is_err());
    }

    #[test]
    fn ed25519_verification_matches_the_rfc8032_vector() {
        let key = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        let signature = hex::decode(concat!(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        ))
        .unwrap();
        verify_signature(&key, b"", &signature).unwrap();
        assert!(verify_signature(&key, b"changed", &signature).is_err());
    }

    #[test]
    fn bridge_markers_are_distinct_and_not_real_descriptors() {
        let mut seen = HashSet::new();
        for kind in BridgeKind::ALL {
            assert!(seen.insert(kind.marker()));
            assert!(kind.marker() < -1000);
        }
    }

    #[test]
    fn the_signed_feed_has_a_bounded_build_history() {
        let mut feed = bundled().unwrap();
        let family = feed.families[0].clone();
        feed.families = (1..=MAX_FAMILIES)
            .map(|generation| {
                let mut next = family.clone();
                for (runtime, certificate) in next.runtimes.iter_mut().enumerate() {
                    certificate.wasm_sha256 = format!("{:064x}", generation * 2 + runtime);
                }
                next.family_id = artifact_family_id(&next.runtimes).unwrap();
                next
            })
            .collect();
        feed.validate().unwrap();
        feed.families.push(family);
        assert!(feed.validate().is_err());
    }

    #[test]
    fn artifact_family_identity_is_derived_and_order_independent() {
        let family = bundled().unwrap().families.remove(0);
        let expected = artifact_family_id(&family.runtimes).unwrap();
        assert_eq!(expected, family.family_id);

        let mut reversed = family.runtimes;
        reversed.reverse();
        assert_eq!(artifact_family_id(&reversed).unwrap(), expected);
    }

    #[test]
    fn artifact_family_identity_cannot_be_supplied_independently() {
        let mut feed = bundled().unwrap();
        feed.families[0].family_id =
            "0000000000000000000000000000000000000000000000000000000000000000".to_owned();
        assert!(feed.validate().is_err());
    }
}
