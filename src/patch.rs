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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::manifest::{Compression, ContentHash, HashAlgo, Manifest};
use crate::transport;

const PATCH_ROOT: &str = "https://patching.1.arenanetworks.com";

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

const CLIENT_ARTIFACTS: [&str; 2] = ["Gw.jspi.js", "Gw.jspi.wasm"];
const COMMON_ARTIFACTS: [&str; 1] = ["version.json"];
/// The prebuilt filesystem image the chunk store hydrates IDBFS from. Not part
/// of the startup sync — it is hundreds of megabytes and fetched on demand.
pub const SNAPSHOT: &str = "Gw.snapshot";

pub struct Client {
    root: String,
    access_key: String,
    retries: Retries,
}

/// What the retry ladder in [`Client::fetch`] cost, which is otherwise
/// invisible.
///
/// A transient failure is retried without a word — nothing is logged, and a
/// fetch that eventually succeeds looks from the outside exactly like one that
/// was merely slow. Since the sleeps are seconds long and a demand read blocks
/// behind them, a stalled read cannot be told from a queued one without this.
#[derive(Default)]
pub struct Retries {
    /// Attempts after the first, across every fetch.
    attempts: AtomicU64,
    /// Milliseconds spent asleep between them.
    slept_ms: AtomicU64,
}

impl Client {
    /// Both endpoint and key can be overridden from the environment; neither has
    /// to be, so a clean checkout runs without setup. See [`ACCESS_KEY`].
    pub fn from_env() -> Self {
        let access_key =
            std::env::var("GWNATIVE_ACCESS_KEY").unwrap_or_else(|_| ACCESS_KEY.to_owned());
        let root = std::env::var("GWNATIVE_PATCH_ROOT").unwrap_or_else(|_| PATCH_ROOT.to_owned());
        Self::new(&root, access_key)
    }

    pub fn new(root: &str, access_key: String) -> Self {
        Self {
            root: root.trim_end_matches('/').to_owned(),
            access_key,
            retries: Retries::default(),
        }
    }

    /// Retries so far: attempts after the first, and milliseconds slept between
    /// them. See [`Retries`].
    pub fn retries(&self) -> (u64, u64) {
        (
            self.retries.attempts.load(Ordering::Relaxed),
            self.retries.slept_ms.load(Ordering::Relaxed),
        )
    }

    /// The manifest the service is offering now, stored for the next launch.
    ///
    /// What [`Client::manifest`] falls back to, and what the `sync` command
    /// calls directly: an explicit request to install the client is a request
    /// for whatever is on offer at that moment, so it is the one caller for
    /// which reading the cache — being deliberately a launch behind, everywhere
    /// else — would be the wrong answer. It still *writes* the cache, because
    /// the artifacts it is about to install come from these bytes and the next
    /// launch has to open on the manifest that describes them.
    pub fn fetch_manifest(&self, dir: &Path) -> Result<Manifest> {
        let fetched = self.fetch_with(&self.manifest_url(), MAX_MANIFEST_BYTES, None)?;
        let bytes = fetched.body.expect("an unconditional GET returns a body");
        // Parsed before it is stored: a body that cannot be read is not a copy
        // worth booting the next launch from.
        let manifest = Manifest::parse(&bytes)?;
        write_cache(dir, &self.root, fetched.validator.as_deref(), &bytes);
        Ok(manifest)
    }

    fn manifest_url(&self) -> String {
        format!("{}/manifest.json", self.root)
    }

    /// The manifest this launch should run on, preferring the copy on disk.
    ///
    /// A warm launch has the whole client installed and wants the manifest for
    /// one thing: the snapshot's chunk list, which [`crate::chunks::ChunkStore`]
    /// reads when it opens. That list is 1.2 MB, and fetching it was 120 ms of
    /// the 165 ms this process spent before the window existed — three quarters
    /// of a launch spent asking the service to re-send a file that had not
    /// changed in six days.
    ///
    /// Being a launch behind costs nothing here, which is the part worth being
    /// precise about. The client artifacts on disk were installed from this same
    /// cached manifest and their hashes are checked against the record before
    /// this is called, so the cached manifest describes exactly the client that
    /// is about to run. It is the arrangement this replaces — a *fresh* manifest
    /// paired with a client from whenever the last sync happened — that
    /// describes something nobody has installed.
    ///
    /// The caller revalidates off the launch path. See [`Client::revalidate`].
    pub fn manifest(&self, dir: &Path) -> Result<(Manifest, Source)> {
        if let Some((_, bytes)) = read_cache(dir, &self.root)
            && let Ok(manifest) = Manifest::parse(&bytes)
        {
            return Ok((manifest, Source::Disk));
        }
        Ok((self.fetch_manifest(dir)?, Source::Service))
    }

    /// Refresh the cached manifest if the service has a different one, and say
    /// whether it did.
    ///
    /// Conditional, so the ordinary answer is 304 and a few hundred bytes rather
    /// than 1.2 MB. Every installation shares one access key, so what this costs
    /// the service is multiplied by everyone running it — see [`ACCESS_KEY`].
    ///
    /// Nothing is applied to the running app, and that is deliberate: the client
    /// this process is running came from the old manifest, and swapping the
    /// chunk list underneath a live game would pair a running client with a
    /// snapshot it was not built against.
    ///
    /// What it does *not* do is install anything, and it does not have to:
    /// storing the manifest is what installs it, one launch later. The next
    /// launch opens on these bytes, `install_client` reads the build they offer
    /// with [`crate::generation::identify`], and a build that is not the one on
    /// disk is fetched then — with the whole comparison done from a file that
    /// was already going to be read. That is why the check runs here, behind a
    /// launch that is already serving, instead of in front of one that is not.
    pub fn revalidate(&self, dir: &Path) -> Result<bool> {
        let Some((known, _)) = read_cache(dir, &self.root) else {
            return Ok(false);
        };
        let fetched =
            self.fetch_with(&self.manifest_url(), MAX_MANIFEST_BYTES, known.as_deref())?;
        let Some(bytes) = fetched.body else {
            return Ok(false);
        };
        // A service with no ETag answers every conditional request in full, so
        // compare the bytes rather than trusting the 200: without this, a
        // validator-less service would look like it patched on every launch.
        if read_cache(dir, &self.root).is_some_and(|(_, cached)| cached == bytes) {
            return Ok(false);
        }
        Manifest::parse(&bytes)?;
        write_cache(dir, &self.root, fetched.validator.as_deref(), &bytes);
        Ok(true)
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
        // Unconditional, so the service has nothing to answer 304 to and a
        // success always carries bytes.
        let fetched = self.fetch_with(url, limit, None)?;
        Ok(fetched.body.expect("an unconditional GET returns a body"))
    }

    /// [`Client::fetch`], plus the validator, and with `If-None-Match` when
    /// `known` is set. See [`Fetched`].
    fn fetch_with(&self, url: &str, limit: u64, known: Option<&str>) -> Result<Fetched> {
        let mut last = None;
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                let nap = Duration::from_secs(1 << attempt);
                self.retries.attempts.fetch_add(1, Ordering::Relaxed);
                self.retries
                    .slept_ms
                    .fetch_add(nap.as_millis() as u64, Ordering::Relaxed);
                std::thread::sleep(nap);
            }
            match self.get_once(url, limit, known) {
                Ok(fetched) => return Ok(fetched),
                Err(e @ (Error::Http { .. } | Error::TooLarge { .. })) => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("at least one attempt"))
    }

    fn get_once(&self, url: &str, limit: u64, known: Option<&str>) -> Result<Fetched> {
        // Redirects come back as their 3xx and land in FATAL below: a patch
        // endpoint that suddenly wants to send us elsewhere is a signal to
        // stop, not to follow. Identity encoding because chunks are already
        // compressed per the manifest, and a transfer-level layer would defeat
        // the byte budget below.
        let mut headers = vec![
            ("X-Access-Key", self.access_key.as_str()),
            ("Accept-Encoding", "identity"),
            ("User-Agent", USER_AGENT),
        ];
        if let Some(known) = known {
            headers.push(("If-None-Match", known));
        }
        let response =
            transport::fetch("GET", url, &headers, None, REQUEST_TIMEOUT).map_err(|detail| {
                Error::Transport {
                    url: url.to_owned(),
                    detail,
                }
            })?;

        let validator = response
            .headers
            .iter()
            .find(|(name, _)| name == "etag")
            .map(|(_, value)| value.clone());

        let status = response.status;
        // Only when we asked: 304 answers `If-None-Match` and nothing else, so
        // an unsolicited one is a service doing something we did not ask for and
        // stays in the 3xx refusal below with the redirects.
        if known.is_some() && status == 304 {
            return Ok(Fetched {
                body: None,
                validator,
            });
        }
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

        // The budget is enforced after receipt rather than during it — the OS
        // stack buffers the body before this code sees a byte, so an overrun
        // costs its size in memory once, but it is still refused rather than
        // truncated into a hash mismatch.
        if response.body.len() as u64 > limit {
            return Err(Error::TooLarge {
                url: url.to_owned(),
                limit,
            });
        }
        Ok(Fetched {
            body: Some(response.body),
            validator,
        })
    }
}

/// A response the patch client accepted.
struct Fetched {
    /// `None` only when the service answered 304 — the caller's copy stands.
    body: Option<Vec<u8>>,
    /// The service's `ETag` for these bytes, when it published one. Kept beside
    /// the body it names so the two can be stored together; see [`CACHE_FILE`].
    validator: Option<String>,
}

/// Where [`Client::manifest`] came from.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// Fetched from the service, because there was no readable local copy.
    /// Already current — nothing to revalidate.
    Service,
    /// Read from disk, and therefore as old as the last launch that stored one.
    Disk,
}

/// The cached manifest: the service it came from, its validator, then the bytes
/// that validator names, each on its own line.
///
/// One file rather than three, and that is the whole reason it has a format at
/// all. Split across a body file and an ETag file, a crash between the two
/// writes leaves a validator naming bytes that are no longer there — and the
/// next launch would send it as `If-None-Match`, be told "still fresh", and keep
/// a body the service never associated with that tag. Written together, they
/// cannot disagree: either the rename lands or it does not.
///
/// The root is there because everything after it is only true of that root. An
/// ETag means nothing to a service that did not issue it, and `GWNATIVE_PATCH_ROOT`
/// exists precisely so a run can be pointed somewhere else — without this, doing
/// so would boot the app on the other service's manifest and offer the other
/// service's ETag back to it.
///
/// Neither a URL nor a header value can contain a newline, so splitting on the
/// first two leaves the manifest's own bytes untouched.
const CACHE_FILE: &str = "manifest.cache";

/// The cached validator — `None` if the service published none — and the bytes,
/// if what is stored was stored for `root`.
///
/// Every failure here reads as "no cache": absent, truncated by a full disk,
/// left by a run pointed at another service, written by a version that spelled
/// it differently. The caller's answer to all of those is the same, and it is
/// the answer that was correct before this cache existed — fetch it.
fn read_cache(dir: &Path, root: &str) -> Option<(Option<String>, Vec<u8>)> {
    let raw = fs::read(dir.join(CACHE_FILE)).ok()?;
    let (stored, rest) = split_line(&raw)?;
    if stored != root {
        return None;
    }
    let (validator, bytes) = split_line(rest)?;
    Some((
        (!validator.is_empty()).then(|| validator.to_owned()),
        bytes.to_vec(),
    ))
}

/// Everything before the first newline as text, and everything after it as
/// bytes. `None` when there is no newline, or when what precedes it is not text.
fn split_line(raw: &[u8]) -> Option<(&str, &[u8])> {
    let split = raw.iter().position(|b| *b == b'\n')?;
    Some((std::str::from_utf8(&raw[..split]).ok()?, &raw[split + 1..]))
}

/// Store `bytes`, the service they came from and the validator that names them,
/// atomically.
///
/// Failures are logged and dropped. What a failed write costs is one launch
/// that fetches the manifest the old way, which is the behaviour this replaced
/// and is not worth refusing to start over.
fn write_cache(dir: &Path, root: &str, validator: Option<&str>, bytes: &[u8]) {
    let path = dir.join(CACHE_FILE);
    let tmp = temp_path(&path);
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(root.as_bytes())?;
        file.write_all(b"\n")?;
        file.write_all(validator.unwrap_or_default().as_bytes())?;
        file.write_all(b"\n")?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        note!("[patch] could not store the manifest: {e}");
    }
}

/// Every artifact a sync writes, in the order it writes them.
pub fn artifacts() -> Vec<&'static str> {
    CLIENT_ARTIFACTS
        .iter()
        .chain(COMMON_ARTIFACTS.iter())
        .copied()
        .collect()
}

/// Write every client artifact described by `manifest` into `dest`, returning
/// each name with its byte size.
///
/// The manifest is a parameter rather than something fetched here because the
/// caller has a decision to make from it before any of this runs — see
/// `generation::identify`, which needs to know which build is on offer while
/// declining it is still cheap.
///
/// The set is failure-atomic: every artifact is downloaded and verified in a
/// staging directory before any live file moves. Promotion keeps a private copy
/// of the set it is replacing and restores it if any rename fails, so an `Err`
/// means `dest` still holds the generation it held on entry.
pub fn sync_with(
    client: &Client,
    manifest: &Manifest,
    dest: &Path,
) -> Result<Vec<(&'static str, u64)>> {
    let staging = dest.join(format!(".gwnative-client-sync-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let incoming = staging.join("incoming");
    fs::create_dir_all(&incoming)?;
    let _cleanup = RemoveDir(staging.clone());

    let names = artifacts();
    let mut written = Vec::new();
    for &name in &names {
        // The manifest is a tree; these artifacts must resolve to exactly one
        // path in it, or we would be guessing which copy the client wants.
        let path = manifest.require_unique(name)?.to_owned();
        client.download(manifest, &path, &incoming.join(name))?;
        written.push((name, manifest.files[&path].size));
    }
    promote_set(&incoming, &staging.join("replaced"), dest, &names)?;
    Ok(written)
}

/// Promote a complete staged set, putting the old set back on any failure.
fn promote_set(incoming: &Path, replaced: &Path, dest: &Path, names: &[&str]) -> Result<()> {
    fs::create_dir_all(replaced)?;
    let mut existed = Vec::with_capacity(names.len());
    // Finish the backup before the first live path changes.
    for name in names {
        let live = dest.join(name);
        let present = live.exists();
        if present {
            fs::copy(&live, replaced.join(name))?;
        }
        existed.push(present);
    }

    for (index, name) in names.iter().enumerate() {
        if let Err(error) = fs::rename(incoming.join(name), dest.join(name)) {
            for (restore_index, restore_name) in names[..index].iter().enumerate() {
                let live = dest.join(restore_name);
                let restored = if existed[restore_index] {
                    fs::copy(replaced.join(restore_name), &live).map(|_| ())
                } else {
                    fs::remove_file(&live)
                };
                if let Err(restore_error) = restored {
                    note!(
                        "[patch] could not restore {restore_name} after promotion failed: \
                         {restore_error}"
                    );
                }
            }
            return Err(error.into());
        }
    }
    Ok(())
}

/// A staging directory is never part of the installed client.
struct RemoveDir(PathBuf);

impl Drop for RemoveDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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

pub fn verify(bytes: &[u8], hash: &ContentHash) -> Result<()> {
    use md5::Digest as _;
    // Compared as bytes, in a buffer the width of the widest digest. This runs
    // once per chunk over the whole 4.2 GB snapshot, so the hex round trip it
    // replaces was 16167 allocations to answer a question about 32 bytes.
    let mut actual = [0u8; 32];
    let width = hash.bytes().len();
    let actual = &mut actual[..width];
    match hash.algo() {
        HashAlgo::Md5 => actual.copy_from_slice(&md5::Md5::digest(bytes)[..]),
        HashAlgo::Sha1 => actual.copy_from_slice(&sha1::Sha1::digest(bytes)[..]),
        HashAlgo::Sha256 => actual.copy_from_slice(&sha2::Sha256::digest(bytes)[..]),
    }
    if actual != hash.bytes() {
        return Err(Error::HashMismatch {
            expected: hash.to_string(),
            actual: hex::encode(actual),
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

    use crate::scratch::TempDir;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// The launch path reads this file and boots on what comes back, so a body
    /// that survives the round trip only most of the time is a client that
    /// starts only most of the time. A manifest is 1.2 MB of arbitrary bytes and
    /// the format puts a newline in the middle of the file on purpose — these
    /// are the cases where a split on the wrong newline shows up.
    #[test]
    fn a_stored_manifest_comes_back_byte_for_byte() {
        let dir = TempDir::new("manifest-cache");
        for (validator, body) in [
            (Some("\"8fa40eee\""), &b"{\"files\":{}}"[..]),
            // The manifest is JSON, and JSON is routinely pretty-printed. Every
            // one of these newlines is a place a second split would land.
            (
                Some("\"tagged\""),
                &b"{\n  \"files\": {\n    \"a\": 1\n  }\n}\n"[..],
            ),
            // A service that publishes no ETag still gets a usable cache; the
            // validator is simply absent and revalidation goes unconditional.
            (None, &b"{\"files\":{}}"[..]),
            // Nothing says a manifest cannot start with a newline, and a naive
            // reader would hand back an empty body for this one.
            (Some("\"leading\""), &b"\n{\"files\":{}}"[..]),
            (None, &b""[..]),
        ] {
            write_cache(&dir.0, PATCH_ROOT, validator, body);
            let (stored, bytes) = read_cache(&dir.0, PATCH_ROOT).expect("just written");
            assert_eq!(stored.as_deref(), validator);
            assert_eq!(bytes, body);
        }
    }

    /// Absent, truncated, or written by something else: all of them have to read
    /// as "no cache" rather than as a cache holding nonsense, because the caller
    /// boots on whatever this returns.
    #[test]
    fn an_unreadable_cache_is_no_cache() {
        let dir = TempDir::new("manifest-cache-bad");
        assert!(read_cache(&dir.0, PATCH_ROOT).is_none());

        // One newline where the format wants two: a root and no validator, so
        // there is no body either.
        fs::write(dir.0.join(CACHE_FILE), format!("{PATCH_ROOT}\n")).unwrap();
        assert!(read_cache(&dir.0, PATCH_ROOT).is_none());

        // No newline anywhere: there is no validator and no body, only bytes.
        fs::write(dir.0.join(CACHE_FILE), b"no newline here").unwrap();
        assert!(read_cache(&dir.0, PATCH_ROOT).is_none());

        // A URL is ASCII, and so is a validator. Bytes that are not valid UTF-8
        // mean this file is not one of ours.
        fs::write(dir.0.join(CACHE_FILE), b"\xff\xfe\n\n{}").unwrap();
        assert!(read_cache(&dir.0, PATCH_ROOT).is_none());
    }

    /// The cache belongs to the service that filled it. `GWNATIVE_PATCH_ROOT`
    /// exists so a run can be pointed at a local one, and without this the run
    /// after it would boot on the local manifest and offer the local ETag back
    /// to ArenaNet.
    #[test]
    fn a_cache_written_for_one_service_is_not_read_for_another() {
        let dir = TempDir::new("manifest-cache-root");
        write_cache(&dir.0, "http://127.0.0.1:8080", Some("\"local\""), b"{}");

        assert!(read_cache(&dir.0, PATCH_ROOT).is_none());
        assert!(read_cache(&dir.0, "http://127.0.0.1:8081").is_none());
        assert!(read_cache(&dir.0, "http://127.0.0.1:8080").is_some());
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

    #[test]
    fn a_failed_promotion_restores_the_whole_live_set() {
        let temp = TempDir::new("patch-promote");
        let live = temp.0.join("live");
        let incoming = temp.0.join("incoming");
        let replaced = temp.0.join("replaced");
        fs::create_dir_all(&live).unwrap();
        fs::create_dir_all(&incoming).unwrap();
        for name in artifacts() {
            fs::write(live.join(name), format!("old {name}")).unwrap();
        }
        // The second rename fails after the first one has already landed.
        fs::write(incoming.join(CLIENT_ARTIFACTS[0]), b"new glue").unwrap();

        assert!(promote_set(&incoming, &replaced, &live, &artifacts()).is_err());
        for name in artifacts() {
            assert_eq!(
                fs::read(live.join(name)).unwrap(),
                format!("old {name}").as_bytes()
            );
        }
    }

    /// The ladder itself is not exercised here — one round of it sleeps for two
    /// seconds and a full one for fourteen, which is not a price a unit suite
    /// should pay. What is checked is the wiring, which is where a counter that
    /// silently reads zero forever would actually come from.
    #[test]
    fn the_retry_ladder_is_reported_from_the_counters_it_increments() {
        let client = Client::new("", String::new());
        assert_eq!(
            client.retries(),
            (0, 0),
            "a fresh client has retried nothing"
        );

        client.retries.attempts.fetch_add(3, Ordering::Relaxed);
        client.retries.slept_ms.fetch_add(14_000, Ordering::Relaxed);
        assert_eq!(client.retries(), (3, 14_000));
    }

    /// The sleeps are the whole reason a retry is visible in a range time:
    /// `1 << attempt` over attempts 1..4 is 2 s, 4 s and 8 s, so a fetch that
    /// uses the whole ladder cannot come in under fourteen seconds.
    #[test]
    fn the_backoff_ladder_sums_to_fourteen_seconds() {
        let slept: u64 = (1..MAX_ATTEMPTS).map(|attempt| 1u64 << attempt).sum();
        assert_eq!(slept, 14);
    }
}
