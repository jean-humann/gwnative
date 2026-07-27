//! ArenaNet patch client.
//!
//! `GET {root}/manifest.json` describes the tree; every file is a list of
//! content hashes fetched as `GET {root}/{hash}.bin`. Chunks arrive gzipped or
//! raw depending on the manifest's compression mode, and **the hash covers the
//! decoded bytes** — decode first, then verify.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::manifest::{Compression, ContentHash, HashAlgo, Manifest};

pub const PATCH_ROOT: &str = "https://patching.1.arenanetworks.com";

/// Identifies this client to ArenaNet. Kept honest on purpose.
const USER_AGENT: &str = "gwnative (Guild Wars interoperability client)";

/// The published client access key.
///
/// It identifies the official Guild Wars client, not a player: it carries no
/// account, grants nothing a public download does not, and is the same value for
/// every installation. That is why it can sit in the source — a player
/// credential never could. `GWNATIVE_ACCESS_KEY` overrides it if ArenaNet
/// rotates the value before this constant catches up.
///
/// Because every installation shares it, request volume is the thing to be
/// careful with; see `PREFETCH_JOBS`.
const ACCESS_KEY: &str = "2043FE79-F32D-4FD7-8C27-0D47231C4F03";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ATTEMPTS: u32 = 4;
const PREFETCH_JOBS: usize = 8;

/// Statuses that mean "the answer will not change" — a wrong key or a hash the
/// service does not have. Retrying these only burns time and looks like abuse.
const FATAL_STATUS: [u16; 3] = [401, 403, 404];

pub const CLIENT_ARTIFACTS: [&str; 2] = ["Gw.jspi.js", "Gw.jspi.wasm"];
pub const COMMON_ARTIFACTS: [&str; 1] = ["version.json"];
/// The prebuilt filesystem image the chunk store hydrates IDBFS from. Not part
/// of the startup sync — it is hundreds of megabytes and fetched on demand.
#[allow(dead_code)]
pub const SNAPSHOT: &str = "Gw.snapshot";

pub struct Client {
    agent: ureq::Agent,
    root: String,
    access_key: String,
}

impl Client {
    /// Both endpoint and key can be overridden from the environment; neither has
    /// to be, so a clean checkout runs without setup. See [`ACCESS_KEY`].
    pub fn from_env() -> Result<Self> {
        let access_key =
            std::env::var("GWNATIVE_ACCESS_KEY").unwrap_or_else(|_| ACCESS_KEY.to_owned());
        let root = std::env::var("GWNATIVE_PATCH_ROOT").unwrap_or_else(|_| PATCH_ROOT.to_owned());
        Ok(Self::new(root, access_key))
    }

    pub fn new(root: String, access_key: String) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            // Redirects off: a patch endpoint that suddenly wants to send us
            // elsewhere is a signal to stop, not to follow.
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        Self {
            agent,
            root: root.trim_end_matches('/').to_owned(),
            access_key,
        }
    }

    pub fn fetch_manifest(&self) -> Result<Manifest> {
        let url = format!("{}/manifest.json", self.root);
        let bytes = self.fetch(&url, MAX_MANIFEST_BYTES)?;
        Manifest::parse(&bytes)
    }

    /// Fetch one chunk and return its decoded bytes, verified against `hash`.
    pub fn fetch_chunk(
        &self,
        hash: &ContentHash,
        expected_len: u64,
        compression: Compression,
    ) -> Result<Vec<u8>> {
        let url = format!("{}/{hash}.bin", self.root);
        let encoded = self.fetch(&url, encoded_limit(expected_len, compression))?;
        let decoded = decode_chunk(&encoded, expected_len, compression)?;
        verify(&decoded, hash)?;
        Ok(decoded)
    }

    /// Download `path` from the manifest into `dest`, replacing it atomically.
    /// Chunks are fetched `PREFETCH_JOBS` at a time and written in order, so
    /// peak memory is bounded by the batch rather than the file size.
    pub fn download(&self, manifest: &Manifest, path: &str, dest: &Path) -> Result<()> {
        let entry = manifest
            .files
            .get(path)
            .ok_or_else(|| Error::ManifestFormat(format!("manifest is missing {path}")))?;

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = temp_path(dest);
        let mut out = fs::File::create(&tmp)?;

        let result = (|| -> Result<()> {
            for (batch, hashes) in entry.chunk_hashes.chunks(PREFETCH_JOBS).enumerate() {
                let base = batch * PREFETCH_JOBS;
                let slots: Vec<Mutex<Option<Vec<u8>>>> =
                    (0..hashes.len()).map(|_| Mutex::new(None)).collect();
                let next = AtomicUsize::new(0);
                let failure: Mutex<Option<Error>> = Mutex::new(None);

                std::thread::scope(|scope| {
                    for _ in 0..hashes.len().min(PREFETCH_JOBS) {
                        scope.spawn(|| {
                            loop {
                                let i = next.fetch_add(1, Ordering::Relaxed);
                                if i >= hashes.len() || failure.lock().unwrap().is_some() {
                                    return;
                                }
                                let len = manifest
                                    .chunk_length(path, base + i)
                                    .expect("chunk index within file");
                                match self.fetch_chunk(&hashes[i], len, manifest.compression) {
                                    Ok(bytes) => *slots[i].lock().unwrap() = Some(bytes),
                                    Err(e) => {
                                        let mut slot = failure.lock().unwrap();
                                        if slot.is_none() {
                                            *slot = Some(e);
                                        }
                                        return;
                                    }
                                }
                            }
                        });
                    }
                });

                if let Some(e) = failure.into_inner().unwrap() {
                    return Err(e);
                }
                for slot in slots {
                    let bytes = slot.into_inner().unwrap().expect("chunk filled on success");
                    out.write_all(&bytes)?;
                }
            }
            out.flush()?;
            out.sync_all()?;
            Ok(())
        })();

        drop(out);
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        fs::rename(&tmp, dest)?;
        Ok(())
    }

    /// GET `url` with the patch headers, retrying transient failures with
    /// exponential backoff. Fatal statuses and redirects abort immediately.
    fn fetch(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let mut last = None;
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(1 << attempt));
            }
            match self.get_once(url, limit) {
                Ok(bytes) => return Ok(bytes),
                Err(e @ Error::Http { .. }) => return Err(e),
                Err(e @ Error::TooLarge { .. }) => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("at least one attempt"))
    }

    fn get_once(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let mut response = self
            .agent
            .get(url)
            .header("X-Access-Key", &self.access_key)
            // Identity encoding: chunks are already compressed per the manifest,
            // and a transfer-level layer would defeat the byte budget below.
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|e| Error::Transport {
                url: url.to_owned(),
                detail: e.to_string(),
            })?;

        let status = response.status().as_u16();
        if status != 200 {
            let fatal = FATAL_STATUS.contains(&status) || (300..400).contains(&status);
            return Err(if fatal {
                Error::Http {
                    url: url.to_owned(),
                    status,
                }
            } else {
                Error::Transport {
                    url: url.to_owned(),
                    detail: format!("HTTP {status}"),
                }
            });
        }

        if let Some(declared) = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            && declared > limit
        {
            return Err(Error::TooLarge {
                url: url.to_owned(),
                limit,
            });
        }

        // Read one byte past the budget so an undeclared overrun is caught
        // rather than silently truncated into a hash mismatch.
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit + 1)
            .read_to_end(&mut body)
            .map_err(|e| Error::Transport {
                url: url.to_owned(),
                detail: e.to_string(),
            })?;
        if body.len() as u64 > limit {
            return Err(Error::TooLarge {
                url: url.to_owned(),
                limit,
            });
        }
        Ok(body)
    }
}

/// Which client artifacts are absent from `dest`.
pub fn missing_artifacts(dest: &Path) -> Vec<&'static str> {
    CLIENT_ARTIFACTS
        .iter()
        .chain(COMMON_ARTIFACTS.iter())
        .copied()
        .filter(|name| !dest.join(name).is_file())
        .collect()
}

/// Fetch the current manifest and write every client artifact into `dest`,
/// returning each name with its byte size.
pub fn sync(dest: &Path) -> Result<Vec<(&'static str, u64)>> {
    let client = Client::from_env()?;
    let manifest = client.fetch_manifest()?;

    let mut written = Vec::new();
    for name in CLIENT_ARTIFACTS.iter().chain(COMMON_ARTIFACTS.iter()) {
        // The manifest is a tree; these artifacts must resolve to exactly one
        // path in it, or we would be guessing which copy the client wants.
        let path = manifest.require_unique(name)?.to_owned();
        client.download(&manifest, &path, &dest.join(name))?;
        written.push((*name, manifest.files[&path].size));
    }
    Ok(written)
}

/// Upper bound on the wire size of a chunk. Gzip can expand incompressible
/// input slightly, so allow headroom rather than assuming it always shrinks.
fn encoded_limit(expected_len: u64, compression: Compression) -> u64 {
    match compression {
        Compression::None => expected_len,
        Compression::Gzip => expected_len + (64 * 1024).max(expected_len.div_ceil(16)),
    }
}

fn decode_chunk(encoded: &[u8], expected_len: u64, compression: Compression) -> Result<Vec<u8>> {
    let decoded = match compression {
        Compression::None => encoded.to_vec(),
        Compression::Gzip => {
            let mut out = Vec::with_capacity(expected_len as usize);
            flate2::read::GzDecoder::new(encoded)
                // Cap inflation so a zip bomb cannot allocate past the chunk size.
                .take(expected_len + 1)
                .read_to_end(&mut out)
                .map_err(|e| Error::Decode(e.to_string()))?;
            out
        }
    };
    if decoded.len() as u64 != expected_len {
        return Err(Error::Decode(format!(
            "expected {expected_len} bytes, got {}",
            decoded.len()
        )));
    }
    Ok(decoded)
}

fn verify(bytes: &[u8], hash: &ContentHash) -> Result<()> {
    use md5::Digest as _;
    let actual = match hash.algo() {
        HashAlgo::Md5 => hex::encode(md5::Md5::digest(bytes)),
        HashAlgo::Sha1 => hex::encode(sha1::Sha1::digest(bytes)),
        HashAlgo::Sha256 => hex::encode(sha2::Sha256::digest(bytes)),
    };
    if actual != hash.as_str() {
        return Err(Error::HashMismatch {
            expected: hash.to_string(),
            actual,
        });
    }
    Ok(())
}

fn temp_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn hash_covers_decoded_bytes() {
        let plain = b"guild wars".to_vec();
        let hash = ContentHash::parse(&hex::encode({
            use sha2::Digest as _;
            sha2::Sha256::digest(&plain)
        }))
        .unwrap();
        let decoded = decode_chunk(&gzip(&plain), plain.len() as u64, Compression::Gzip).unwrap();
        assert_eq!(decoded, plain);
        verify(&decoded, &hash).unwrap();
    }

    #[test]
    fn rejects_wrong_decoded_length() {
        let err = decode_chunk(&gzip(b"short"), 99, Compression::Gzip);
        assert!(matches!(err, Err(Error::Decode(_))));
    }

    #[test]
    fn rejects_hash_mismatch() {
        let hash = ContentHash::parse(&"0".repeat(64)).unwrap();
        assert!(matches!(
            verify(b"anything", &hash),
            Err(Error::HashMismatch { .. })
        ));
    }

    #[test]
    fn gzip_limit_allows_incompressible_growth() {
        assert!(encoded_limit(1024, Compression::Gzip) > 1024);
        assert_eq!(encoded_limit(1024, Compression::None), 1024);
    }
}
