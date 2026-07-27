//! On-demand content-addressed chunk store for `Gw.snapshot`.
//!
//! The snapshot is 4.2 GB in 256 KiB chunks. The client only ever reads the
//! parts of it a session touches, so nothing is downloaded up front: a read
//! pulls the chunks it covers, verifies them, and caches them by content hash.
//! Chunks are deduplicated by construction — the same hash appearing twice in
//! the manifest is stored once.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::error::{Error, Result};
use crate::manifest::{ContentHash, Manifest};
use crate::patch::Client;

/// ArenaNet sees at most this many concurrent requests from us, no matter how
/// many reads the client has in flight.
const MAX_CONCURRENT_FETCHES: usize = 8;

pub struct ChunkStore {
    client: Client,
    manifest: Manifest,
    /// Manifest path of the snapshot, resolved once at construction.
    snapshot: String,
    cache_dir: PathBuf,
    inflight: Mutex<HashMap<ContentHash, Arc<Slot>>>,
    permits: Semaphore,
    stats: Stats,
}

/// Where chunks came from. `coalesced` is the count of reads that joined a
/// fetch already in flight instead of starting their own.
#[derive(Default)]
pub struct Stats {
    pub from_cache: AtomicU64,
    pub fetched: AtomicU64,
    pub coalesced: AtomicU64,
}

impl ChunkStore {
    pub fn open(client: Client, manifest: Manifest, cache_dir: PathBuf) -> Result<Self> {
        let snapshot = manifest.require_unique(crate::patch::SNAPSHOT)?.to_owned();
        fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            client,
            manifest,
            snapshot,
            cache_dir,
            inflight: Mutex::new(HashMap::new()),
            permits: Semaphore::new(MAX_CONCURRENT_FETCHES),
            stats: Stats::default(),
        })
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        (
            self.stats.from_cache.load(Ordering::Relaxed),
            self.stats.fetched.load(Ordering::Relaxed),
            self.stats.coalesced.load(Ordering::Relaxed),
        )
    }

    pub fn snapshot_size(&self) -> u64 {
        self.manifest.files[&self.snapshot].size
    }

    pub fn chunk_size(&self) -> u64 {
        self.manifest.chunk_size
    }

    /// Read `length` bytes at `offset` from the snapshot, fetching whatever
    /// chunks are not cached. A read past the end is clamped, not an error.
    pub fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>> {
        let size = self.snapshot_size();
        if offset >= size {
            return Ok(Vec::new());
        }
        let end = size.min(offset.saturating_add(length));
        let chunk_size = self.chunk_size();

        let mut out = Vec::with_capacity((end - offset) as usize);
        let mut cursor = offset;
        while cursor < end {
            let index = (cursor / chunk_size) as usize;
            let chunk = self.chunk(index)?;
            let within = (cursor % chunk_size) as usize;
            let take = ((end - cursor) as usize).min(chunk.len() - within);
            out.extend_from_slice(&chunk[within..within + take]);
            cursor += take as u64;
        }
        Ok(out)
    }

    /// Fetch chunk `index`, from cache if present. Concurrent callers asking
    /// for the same chunk share one fetch rather than racing to download it.
    fn chunk(&self, index: usize) -> Result<Arc<Vec<u8>>> {
        let entry = &self.manifest.files[&self.snapshot];
        let hash = entry
            .chunk_hashes
            .get(index)
            .ok_or_else(|| Error::ManifestFormat(format!("chunk {index} out of range")))?
            .clone();
        let expected = self
            .manifest
            .chunk_length(&self.snapshot, index)
            .expect("index checked above");

        if let Some(cached) = self.read_cached(&hash, expected) {
            self.stats.from_cache.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::new(cached));
        }

        // Claim the fetch, or find that someone else already has.
        let (slot, owned) = {
            let mut inflight = self.inflight.lock().unwrap();
            match inflight.get(&hash) {
                Some(existing) => (Arc::clone(existing), false),
                None => {
                    let slot = Arc::new(Slot::new());
                    inflight.insert(hash.clone(), Arc::clone(&slot));
                    (slot, true)
                }
            }
        };

        if !owned {
            self.stats.coalesced.fetch_add(1, Ordering::Relaxed);
            return slot.wait();
        }
        self.stats.fetched.fetch_add(1, Ordering::Relaxed);

        // Only the owner holds a permit, so waiters never occupy one — which is
        // what keeps a burst of reads for one chunk from starving the pool.
        let fetched = {
            let _permit = self.permits.acquire();
            self.client
                .fetch_chunk(&hash, expected, self.manifest.compression)
        };

        self.inflight.lock().unwrap().remove(&hash);

        match fetched {
            Ok(bytes) => {
                // Cache failures are not read failures: a full or read-only
                // cache should cost speed, not correctness.
                if let Err(e) = self.write_cached(&hash, &bytes) {
                    eprintln!("[gwnative] chunk cache write failed: {e}");
                }
                let shared = Arc::new(bytes);
                slot.fulfil(Ok(Arc::clone(&shared)));
                Ok(shared)
            }
            Err(e) => {
                slot.fulfil(Err(e.to_string()));
                Err(e)
            }
        }
    }

    fn cache_path(&self, hash: &ContentHash) -> PathBuf {
        // Two-level fan-out: 16k files in one directory is fine on APFS, but
        // this keeps directory listings usable when debugging by hand.
        let hex = hash.as_str();
        self.cache_dir.join(&hex[..2]).join(hex)
    }

    fn read_cached(&self, hash: &ContentHash, expected: u64) -> Option<Vec<u8>> {
        let bytes = fs::read(self.cache_path(hash)).ok()?;
        // Length is the cheap integrity check on the hot path; the hash was
        // verified when the chunk was written.
        (bytes.len() as u64 == expected).then_some(bytes)
    }

    fn write_cached(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
        let path = self.cache_path(hash);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Rename in: a reader must never observe a half-written chunk.
        let tmp = path.with_extension("partial");
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// A chunk fetch that several readers may be waiting on.
struct Slot {
    state: Mutex<Option<std::result::Result<Arc<Vec<u8>>, String>>>,
    ready: Condvar,
}

impl Slot {
    fn new() -> Self {
        Self {
            state: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn fulfil(&self, result: std::result::Result<Arc<Vec<u8>>, String>) {
        *self.state.lock().unwrap() = Some(result);
        self.ready.notify_all();
    }

    fn wait(&self) -> Result<Arc<Vec<u8>>> {
        let mut state = self.state.lock().unwrap();
        while state.is_none() {
            state = self.ready.wait(state).unwrap();
        }
        match state.as_ref().expect("filled") {
            Ok(bytes) => Ok(Arc::clone(bytes)),
            Err(detail) => Err(Error::Transport {
                url: "chunk".into(),
                detail: detail.clone(),
            }),
        }
    }
}

struct Semaphore {
    available: Mutex<usize>,
    released: Condvar,
}

impl Semaphore {
    fn new(count: usize) -> Self {
        Self {
            available: Mutex::new(count),
            released: Condvar::new(),
        }
    }

    fn acquire(&self) -> Permit<'_> {
        let mut available = self.available.lock().unwrap();
        while *available == 0 {
            available = self.released.wait(available).unwrap();
        }
        *available -= 1;
        Permit { semaphore: self }
    }
}

struct Permit<'a> {
    semaphore: &'a Semaphore,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.semaphore.available.lock().unwrap() += 1;
        self.semaphore.released.notify_one();
    }
}

/// `~/Library/Caches/gwnative/chunks`, the conventional home for data that is
/// expensive to refetch but safe to lose.
pub fn default_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    Path::new(&home).join("Library/Caches/gwnative/chunks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn semaphore_bounds_concurrency() {
        let sem = Arc::new(Semaphore::new(2));
        let peak = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));

        std::thread::scope(|scope| {
            for _ in 0..16 {
                let (sem, peak, live) = (Arc::clone(&sem), Arc::clone(&peak), Arc::clone(&live));
                scope.spawn(move || {
                    let _permit = sem.acquire();
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    live.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn slot_wakes_every_waiter() {
        let slot = Arc::new(Slot::new());
        let payload = Arc::new(vec![7u8; 4]);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let slot = Arc::clone(&slot);
                scope.spawn(move || assert_eq!(*slot.wait().unwrap(), vec![7u8; 4]));
            }
            slot.fulfil(Ok(payload));
        });
    }

    #[test]
    fn slot_propagates_failure() {
        let slot = Slot::new();
        slot.fulfil(Err("boom".into()));
        assert!(slot.wait().is_err());
    }
}
