//! On-demand content-addressed chunk store for `Gw.snapshot`.
//!
//! The snapshot is 4.2 GB in 256 KiB chunks. The client only ever reads the
//! parts of it a session touches, so nothing is downloaded up front: a read
//! pulls the chunks it covers, verifies them, and caches them by content hash.
//! Chunks are deduplicated by construction — the same hash appearing twice in
//! the manifest is stored once.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::error::{Error, Result};
use crate::manifest::{ContentHash, Manifest};
use crate::patch::Client;

/// ArenaNet sees at most this many concurrent requests from us, no matter how
/// many reads the client has in flight.
const MAX_CONCURRENT_FETCHES: usize = 8;

/// Background prefetch never claims more than this share of
/// [`MAX_CONCURRENT_FETCHES`], so a full download always leaves slots free for
/// the reads the game is actually blocked on. Without the reserve a sweep of
/// 16000 chunks would sit in front of every demand read and stall rendering —
/// which is the whole reason gwonmac splits its queue by priority.
const MAX_PREFETCH_FETCHES: usize = 3;

/// Progress of a background full download.
///
/// `total` is 0 when no sweep has ever run, which is how the page tells "idle"
/// apart from "finished".
#[derive(Default)]
pub struct Prefetch {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub running: AtomicBool,
    stop: AtomicBool,
}

pub struct ChunkStore {
    client: Client,
    manifest: Manifest,
    /// Manifest path of the snapshot, resolved once at construction.
    snapshot: String,
    cache_dir: PathBuf,
    inflight: Mutex<HashMap<ContentHash, Arc<Slot>>>,
    permits: Semaphore,
    /// Held *in addition to* `permits` by prefetch workers only. See
    /// [`MAX_PREFETCH_FETCHES`].
    prefetch_permits: Semaphore,
    stats: Stats,
    prefetch: Prefetch,
    /// Hashes whose cached bytes have been checked against them this session.
    ///
    /// A 4 GB cache sitting on disk for months is exactly where bit rot shows
    /// up, and length alone cannot see it: the file is still the right size, so
    /// the corrupt bytes go straight to the client as game data. Re-hashing on
    /// every read would cost a full 256 KiB read per 472-byte request, which is
    /// the amplification the pread path exists to remove — so each chunk is
    /// checked the first time this session asks for it and preads after that.
    verified: Mutex<HashSet<ContentHash>>,
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
        // Off the launch path: this walks 256 directories and the game has
        // nothing to gain by waiting for it.
        thread::spawn({
            let cache_dir = cache_dir.clone();
            move || {
                crate::qos::set(crate::qos::Class::Utility);
                sweep_orphans(&cache_dir)
            }
        });
        Ok(Self {
            client,
            manifest,
            snapshot,
            cache_dir,
            inflight: Mutex::new(HashMap::new()),
            permits: Semaphore::new(MAX_CONCURRENT_FETCHES),
            prefetch_permits: Semaphore::new(MAX_PREFETCH_FETCHES),
            stats: Stats::default(),
            prefetch: Prefetch::default(),
            verified: Mutex::default(),
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

    /// How many bytes a read at `offset` for `length` will actually produce.
    ///
    /// Arithmetic, not I/O. The range handler needs a Content-Length before it
    /// has read a byte, so it can send the head and stream the body straight to
    /// the socket instead of buffering the whole span to measure it.
    pub fn readable(&self, offset: u64, length: u64) -> u64 {
        let size = self.snapshot_size();
        if offset >= size {
            0
        } else {
            length.min(size - offset)
        }
    }

    /// Write `length` bytes at `offset` from the snapshot to `out`, fetching
    /// whatever chunks are not cached. A read past the end is clamped, not an
    /// error.
    ///
    /// Returns the number of bytes written, which always equals
    /// [`readable`](Self::readable) when it returns `Ok`. A failure part way
    /// through has already written to `out`, so a caller that has committed to a
    /// response cannot recover — it has to close the connection.
    pub fn read_into(&self, offset: u64, length: u64, out: &mut impl Write) -> Result<u64> {
        let produced = self.readable(offset, length);
        let end = offset + produced;
        let chunk_size = self.chunk_size();

        let mut cursor = offset;
        while cursor < end {
            let index = (cursor / chunk_size) as usize;
            let within = (cursor % chunk_size) as usize;
            let resident = self.chunk_length(index)?;
            // A manifest whose chunk is shorter than the offset into it would
            // otherwise underflow, or leave `take` at zero and spin here.
            let rest = resident
                .checked_sub(within as u64)
                .filter(|&rest| rest > 0)
                .ok_or_else(|| {
                    Error::ManifestFormat(format!(
                        "chunk {index} is {resident} bytes, shorter than the offset {within} into it"
                    ))
                })?;
            let take = ((end - cursor) as usize).min(rest as usize);
            self.window_into(index, within, take, out)?;
            cursor += take as u64;
        }
        Ok(produced)
    }

    /// Write `take` bytes starting `within` chunk `index` to `out`.
    ///
    /// The cached path reads exactly that window with one `pread`. It matters
    /// more than it looks: the client's median snapshot read is 9 KB and ten
    /// thousand of them a session are *472 bytes*, so pulling the enclosing
    /// 256 KiB file in to answer each one turned 6 MB of wanted data into 3 GB
    /// of read data.
    fn window_into(
        &self,
        index: usize,
        within: usize,
        take: usize,
        out: &mut impl Write,
    ) -> Result<()> {
        let hash = self.chunk_hash(index)?;
        let expected = self.chunk_length(index)?;
        // Only chunks already hashed this session may be served by the window;
        // the full read in `chunk` is what earns them that place.
        if self.verified.lock().unwrap().contains(&hash)
            && let Some(window) = self.read_cached_window(&hash, expected, within, take)
        {
            self.stats.from_cache.fetch_add(1, Ordering::Relaxed);
            out.write_all(&window)?;
            return Ok(());
        }
        let chunk = self.chunk(index)?;
        out.write_all(&chunk[within..within + take])?;
        Ok(())
    }

    fn chunk_hash(&self, index: usize) -> Result<ContentHash> {
        self.manifest.files[&self.snapshot]
            .chunk_hashes
            .get(index)
            .copied()
            .ok_or_else(|| Error::ManifestFormat(format!("chunk {index} out of range")))
    }

    /// How long chunk `index` is — the last one is short.
    fn chunk_length(&self, index: usize) -> Result<u64> {
        self.manifest
            .chunk_length(&self.snapshot, index)
            .ok_or_else(|| Error::ManifestFormat(format!("chunk {index} out of range")))
    }

    /// Make sure chunk `index` is on disk, without reading its bytes.
    ///
    /// A resume sweep walks all 16167 chunks, and asking each one for its
    /// contents would read — and, the first time, hash — the entire 4 GB cache
    /// to learn what a `stat` already says. Integrity is still checked, just at
    /// the point where the game actually reads the chunk, which is the only
    /// place a corrupt one can do harm.
    fn ensure(&self, index: usize) -> Result<()> {
        let hash = self.chunk_hash(index)?;
        if self.cached_len(&hash) == Some(self.chunk_length(index)?) {
            self.stats.from_cache.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        self.chunk(index).map(|_| ())
    }

    /// Fetch chunk `index`, from cache if present. Concurrent callers asking
    /// for the same chunk share one fetch rather than racing to download it.
    fn chunk(&self, index: usize) -> Result<Arc<Vec<u8>>> {
        let hash = self.chunk_hash(index)?;
        let expected = self.chunk_length(index)?;

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
                    inflight.insert(hash, Arc::clone(&slot));
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
                match self.write_cached(&hash, &bytes) {
                    // `fetch_chunk` hashed these bytes before returning them, so
                    // what just landed on disk is known good and later reads can
                    // go straight to the pread window.
                    Ok(()) => {
                        self.verified.lock().unwrap().insert(hash);
                    }
                    Err(e) => eprintln!("[gwnative] chunk cache write failed: {e}"),
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

    /// Bitmap of which snapshot chunks are already on disk, LSB first. The
    /// harness seeds `image.isCached` from this so a restart does not re-prefetch
    /// what a previous session already paid for.
    pub fn resident_bitmap(&self) -> Vec<u8> {
        let hashes = &self.manifest.files[&self.snapshot].chunk_hashes;

        // One directory listing per fan-out bucket rather than one `stat` per
        // chunk: 256 syscalls instead of 16167, over the same directory blocks.
        // Gathered as the leading byte, so only the buckets that exist are ever
        // rendered rather than a name per chunk.
        let buckets: HashSet<u8> = hashes.iter().map(|h| h.bytes()[0]).collect();
        let mut present: HashSet<String> = HashSet::new();
        for bucket in buckets {
            let Ok(entries) = fs::read_dir(self.cache_dir.join(format!("{bucket:02x}"))) else {
                continue;
            };
            present.extend(
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok()),
            );
        }

        let mut bits = vec![0u8; hashes.len().div_ceil(8)];
        for (i, hash) in hashes.iter().enumerate() {
            // A `.tmp` left by a write in flight is in the listing too, and does
            // not match a bare hash, which is the answer wanted anyway.
            if present.contains(hash.hex().as_str()) {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        bits
    }

    /// How many chunks the snapshot is made of.
    pub fn chunk_count(&self) -> usize {
        self.manifest.files[&self.snapshot].chunk_hashes.len()
    }

    /// `(done, total, running)` for the current or last full download.
    pub fn prefetch_progress(&self) -> (u64, u64, bool) {
        (
            self.prefetch.done.load(Ordering::Relaxed),
            self.prefetch.total.load(Ordering::Relaxed),
            self.prefetch.running.load(Ordering::Relaxed),
        )
    }

    /// Ask a running full download to stop at the next chunk boundary.
    pub fn stop_full_download(&self) {
        self.prefetch.stop.store(true, Ordering::Relaxed);
    }

    /// Start downloading every chunk that is not already cached.
    ///
    /// Returns `false` if a sweep is already running — starting a second one
    /// would double the request rate against a shared access key for no gain.
    /// Workers walk disjoint strides of the chunk list so no two of them race
    /// for the same hash, and each one is throttled by `prefetch_permits` so
    /// demand reads keep their share of the pool.
    pub fn start_full_download(self: &Arc<Self>) -> bool {
        if self.prefetch.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.prefetch.stop.store(false, Ordering::Relaxed);
        self.prefetch.done.store(0, Ordering::Relaxed);
        self.prefetch
            .total
            .store(self.chunk_count() as u64, Ordering::Relaxed);

        let workers = MAX_PREFETCH_FETCHES;
        let outstanding = Arc::new(AtomicU64::new(workers as u64));
        for worker in 0..workers {
            let store = Arc::clone(self);
            let outstanding = Arc::clone(&outstanding);
            thread::spawn(move || {
                // The sweep decompresses and hashes its way through 4 GB, and
                // nobody is waiting on any particular chunk of it. At the
                // default class the scheduler puts that on performance cores
                // beside WebContent and the GPU process; utility puts it on
                // efficiency cores, where a throughput job with a progress bar
                // belongs. `prefetch_permits` rations these threads' share of
                // the network; this rations their share of the package.
                crate::qos::set(crate::qos::Class::Utility);
                let total = store.chunk_count();
                let mut index = worker;
                while index < total {
                    if store.prefetch.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    // A cached chunk costs a stat, not a request, so the common
                    // resume case sweeps the whole list almost instantly.
                    let _permit = store.prefetch_permits.acquire();
                    if let Err(e) = store.ensure(index) {
                        // One bad chunk should not abandon the sweep; the game
                        // will ask for it again on demand and surface the error
                        // there, where it can be acted on.
                        eprintln!("[gwnative] prefetch chunk {index}: {e}");
                    }
                    store.prefetch.done.fetch_add(1, Ordering::Relaxed);
                    index += workers;
                }
                if outstanding.fetch_sub(1, Ordering::SeqCst) == 1 {
                    store.prefetch.running.store(false, Ordering::SeqCst);
                    let (done, total, _) = store.prefetch_progress();
                    eprintln!("[gwnative] full download finished: {done}/{total} chunks");
                }
            });
        }
        eprintln!(
            "[gwnative] full download started: {} chunks, {workers} workers",
            self.chunk_count()
        );
        true
    }

    fn cache_path(&self, hash: &ContentHash) -> PathBuf {
        // Two-level fan-out: 16k files in one directory is fine on APFS, but
        // this keeps directory listings usable when debugging by hand.
        let hex = hash.hex();
        self.cache_dir.join(&hex[..2]).join(hex.as_str())
    }

    /// The size of the cached file for `hash`, if there is one.
    fn cached_len(&self, hash: &ContentHash) -> Option<u64> {
        fs::metadata(self.cache_path(hash)).ok().map(|m| m.len())
    }

    /// The whole cached chunk, if it is there and its bytes still hash right.
    ///
    /// The hash is checked the first time this session touches a chunk and
    /// remembered afterwards, which is what lets [`window_into`](Self::window_into)
    /// pread. A file that fails is unlinked, so the caller refetches it rather
    /// than handing corrupt bytes to the client and leaving the bad copy behind
    /// to fail again next launch.
    fn read_cached(&self, hash: &ContentHash, expected: u64) -> Option<Vec<u8>> {
        let path = self.cache_path(hash);
        let bytes = fs::read(&path).ok()?;
        if bytes.len() as u64 != expected {
            let _ = fs::remove_file(&path);
            return None;
        }
        if self.verified.lock().unwrap().contains(hash) {
            return Some(bytes);
        }
        if let Err(e) = crate::patch::verify(&bytes, hash) {
            eprintln!("[gwnative] cached chunk is corrupt, refetching: {e}");
            let _ = fs::remove_file(&path);
            return None;
        }
        self.verified.lock().unwrap().insert(*hash);
        Some(bytes)
    }

    /// `take` bytes at `within` from the cached chunk, in one `pread`.
    ///
    /// Only sound for a hash already in `verified`: the length check inside
    /// would not see rot, and the window is too small a sample to hash.
    fn read_cached_window(
        &self,
        hash: &ContentHash,
        expected: u64,
        within: usize,
        take: usize,
    ) -> Option<Vec<u8>> {
        read_window(&self.cache_path(hash), expected, within, take)
    }

    fn write_cached(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
        let path = self.cache_path(hash);
        let parent = path.parent().expect("cache paths always have a bucket");
        fs::create_dir_all(parent)?;

        // Rename in: a reader must never observe a half-written chunk. The temp
        // name carries the pid because the fixed `.partial` it replaces meant
        // two instances sharing this cache — a second launch, or the launcher
        // beside the game — wrote the same file over each other and renamed in
        // a blend of the two, which then failed its hash on the next read.
        let tmp = parent.join(format!(
            "{}.{}.{:08x}.tmp",
            hash.hex(),
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        let written = (|| -> Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written?;

        // The rename is metadata, and `sync_all` above only covered the data.
        // Without this a power cut can leave the directory entry missing while
        // the blocks it would have pointed at are safely on disk — a chunk paid
        // for and lost.
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// Distinguishes the temp files of one process from each other; the pid
/// distinguishes them from another instance's.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// `take` bytes at `within` of `path`, in one `pread`, provided the file is
/// exactly `expected` bytes long.
///
/// The length is the only check available here — the window is a fraction of
/// the chunk, so there is nothing to hash it against. Callers must already know
/// the file's contents are good.
fn read_window(path: &Path, expected: u64, within: usize, take: usize) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() != expected {
        return None;
    }
    let mut window = vec![0u8; take];
    file.read_exact_at(&mut window, within as u64).ok()?;
    Some(window)
}

/// Anything left over from a crashed write. Older than this and no live writer
/// can still own it: a chunk is 256 KiB, so a write that has not finished in an
/// hour is not going to.
const ORPHAN_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Delete temp files a previous run died holding.
///
/// Every one is a quarter-megabyte that nothing will ever read, and nothing
/// else removes them — before this the cache grew by one per crash, forever.
/// Our own pid is skipped outright and the rest have to be stale, so a second
/// instance downloading right now keeps its files.
fn sweep_orphans(cache_dir: &Path) {
    let ours = format!(".{}.", std::process::id());
    let Ok(buckets) = fs::read_dir(cache_dir) else {
        return;
    };
    let mut removed = 0usize;
    for bucket in buckets.flatten() {
        let Ok(entries) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.ends_with(".tmp") || name.contains(&ours) {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .and_then(|t| t.elapsed().map_err(std::io::Error::other))
                .is_ok_and(|age| age > ORPHAN_AGE);
            if stale && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        eprintln!("[gwnative] cleared {removed} abandoned chunk writes");
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

/// `~/Library/Application Support/gwnative/chunks`.
///
/// Not `~/Library/Caches`, where this used to live. That directory is the
/// conventional home for data that is expensive to refetch but safe to lose,
/// and the second half of that is false here: macOS purges it under disk
/// pressure without asking, and what it would be purging is up to 4 GB of game
/// data over a metered connection. The name says cache, but the durability
/// required is that of user data.
///
/// The old location is moved rather than abandoned, so nobody re-downloads what
/// they already have.
pub fn default_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    let home = Path::new(&home);
    let current = home.join("Library/Application Support/gwnative/chunks");
    let legacy = home.join("Library/Caches/gwnative/chunks");
    migrate_cache(&legacy, &current);
    current
}

/// Move a pre-existing cache to its durable home, once.
///
/// A rename, so several gigabytes cost one directory entry and no copy — both
/// paths are under `~/Library` and so on one volume. Everything here is
/// best-effort: a failure leaves the old directory where it is and costs a
/// re-download, which is the same outcome as never having tried, so nothing is
/// worth aborting a launch over.
fn migrate_cache(legacy: &Path, current: &Path) {
    if current.exists() || !legacy.exists() {
        return;
    }
    let Some(parent) = current.parent() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(parent) {
        eprintln!("[chunks] could not prepare {}: {e}", parent.display());
        return;
    }
    match std::fs::rename(legacy, current) {
        Ok(()) => {
            eprintln!(
                "[chunks] moved the cache out of ~/Library/Caches, which macOS may purge, \
                 to {}",
                current.display()
            );
            // Tidy the directory that held it, but only if the move emptied
            // it: WebKit keeps its own cache for this executable under the
            // same name, and that one belongs where it is. `remove_dir`
            // refuses a non-empty directory, which is exactly the test wanted.
            if let Some(old) = legacy.parent() {
                let _ = std::fs::remove_dir(old);
            }
        }
        Err(e) => {
            eprintln!("[chunks] could not move the existing cache ({e}); leaving it in place")
        }
    }
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

    /// A temporary directory that removes itself, so a failing assertion cannot
    /// leave one behind.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gwnative-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn migration_carries_an_existing_cache_across() {
        let temp = TempDir::new("migrate");
        let legacy = temp.0.join("Caches/gwnative/chunks");
        let current = temp.0.join("Application Support/gwnative/chunks");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("abc"), b"a cached chunk").unwrap();

        migrate_cache(&legacy, &current);

        assert_eq!(fs::read(current.join("abc")).unwrap(), b"a cached chunk");
        assert!(!legacy.exists(), "the old cache should not be left behind");
    }

    #[test]
    fn migration_never_overwrites_a_cache_already_there() {
        let temp = TempDir::new("keep");
        let legacy = temp.0.join("Caches/gwnative/chunks");
        let current = temp.0.join("Application Support/gwnative/chunks");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(legacy.join("abc"), b"stale").unwrap();
        fs::write(current.join("abc"), b"in use").unwrap();

        migrate_cache(&legacy, &current);

        assert_eq!(fs::read(current.join("abc")).unwrap(), b"in use");
    }

    #[test]
    fn migration_is_silent_when_there_is_nothing_to_move() {
        let temp = TempDir::new("absent");
        let legacy = temp.0.join("Caches/gwnative/chunks");
        let current = temp.0.join("Application Support/gwnative/chunks");

        migrate_cache(&legacy, &current);

        assert!(!current.exists(), "nothing to move should create nothing");
    }

    #[test]
    fn a_window_reads_only_its_own_span() {
        let temp = TempDir::new("window");
        let chunk: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let path = temp.0.join("chunk");
        fs::write(&path, &chunk).unwrap();

        assert_eq!(
            read_window(&path, 4096, 1000, 472).unwrap(),
            chunk[1000..1472],
            "the pread window must match the same span of the whole chunk"
        );
        // The commonest read of all: the first bytes of a chunk.
        assert_eq!(read_window(&path, 4096, 0, 8).unwrap(), chunk[..8]);
        // And the last, which must not read past the end.
        assert_eq!(read_window(&path, 4096, 4090, 6).unwrap(), chunk[4090..]);
    }

    #[test]
    fn a_window_refuses_a_file_of_the_wrong_length() {
        let temp = TempDir::new("truncated");
        let path = temp.0.join("chunk");
        fs::write(&path, vec![0u8; 100]).unwrap();

        // A truncated cache file is the case the length check exists for: the
        // pread inside would happily serve a window that lies wholly within it.
        assert!(read_window(&path, 4096, 0, 8).is_none());
        assert!(read_window(&temp.0.join("missing"), 100, 0, 8).is_none());
        // Past the end of a correctly-sized file, `read_exact_at` is the check.
        assert!(read_window(&path, 100, 96, 8).is_none());
    }

    #[test]
    fn the_sweep_takes_stale_temps_and_leaves_everything_else() {
        let temp = TempDir::new("orphans");
        let bucket = temp.0.join("ab");
        fs::create_dir_all(&bucket).unwrap();

        let ours = bucket.join(format!("abcd.{}.00000001.tmp", std::process::id()));
        let theirs_fresh = bucket.join("abce.999999.00000001.tmp");
        let theirs_stale = bucket.join("abcf.999999.00000002.tmp");
        let chunk = bucket.join("abc0");
        for path in [&ours, &theirs_fresh, &theirs_stale, &chunk] {
            fs::write(path, b"x").unwrap();
        }
        // Only mtime distinguishes the two foreign temps, so back one of them up
        // past the cutoff rather than waiting an hour for it.
        let long_ago =
            std::time::SystemTime::now() - ORPHAN_AGE - std::time::Duration::from_secs(60);
        fs::File::open(&theirs_stale)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        sweep_orphans(&temp.0);

        assert!(
            !theirs_stale.exists(),
            "a crashed write should be reclaimed"
        );
        assert!(ours.exists(), "our own write is still in progress");
        assert!(
            theirs_fresh.exists(),
            "another instance may be downloading this right now"
        );
        assert!(chunk.exists(), "a cached chunk is not a temp file");
    }
}
