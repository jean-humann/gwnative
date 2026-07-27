//! On-demand content-addressed chunk store for `Gw.snapshot`.
//!
//! The snapshot is 4.2 GB in 256 KiB chunks. The client only ever reads the
//! parts of it a session touches, so nothing is downloaded up front: a read
//! pulls the chunks it covers, verifies them, and caches them by content hash.
//! Chunks are deduplicated by construction — the same hash appearing twice in
//! the manifest is stored once.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
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
    /// Which chunks this session has read, while the boot list is still being
    /// recorded. See [`seal_boot_list`](ChunkStore::seal_boot_list).
    touched: Mutex<BTreeSet<usize>>,
    /// Whether `touched` is still accumulating. Checked before the lock, so a
    /// sealed list costs a relaxed load per read rather than a mutex.
    recording: AtomicBool,
    /// Hashes whose cached bytes have been checked against them this session.
    ///
    /// A 4 GB cache sitting on disk for months is exactly where bit rot shows
    /// up, and length alone cannot see it: the file is still the right size, so
    /// the corrupt bytes go straight to the client as game data. Re-hashing on
    /// every read would cost a full 256 KiB read per 472-byte request, which is
    /// the amplification the pread path exists to remove — so each chunk is
    /// checked the first time this session asks for it and preads after that.
    verified: Mutex<HashSet<ContentHash>>,
    /// Cached chunk files, held open across reads.
    ///
    /// What a window read costs is dominated by the syscalls around the
    /// `pread`, not by the `pread`. Measured over 2000 real cached chunks, 20
    /// iterations, 32 KiB windows: `open` + `fstat` + `pread` + `close` costs
    /// **23.56 µs/op**, and the same reads through descriptors already open
    /// cost **2.82 µs** — 8.3x, none of it in the read itself.
    ///
    /// Sound because the cache is content-addressed. The bytes behind a hash
    /// never change, so a descriptor cannot come to name the wrong content; the
    /// worst it can do is outlive an unlink and go on reading exactly the
    /// content that hash names.
    handles: Mutex<HandleCache>,
}

/// How many chunk files to keep open. The process limit is 1048576 here and a
/// descriptor costs a few hundred bytes of kernel state, so this is set by the
/// working set rather than by scarcity: 2048 covers the whole boot list twenty
/// times over, and a session that ranges wider than that is paying one `open`
/// to bring a chunk back, which is what it would have paid every read anyway.
const MAX_OPEN_CHUNKS: usize = 2048;

/// Open chunk files, evicted oldest-first once [`MAX_OPEN_CHUNKS`] are held.
///
/// Insertion order rather than access order: an LRU would need a touch on every
/// hit, and the hit path is the one being made cheap. Chunks are read in bursts
/// as the client walks the snapshot, so what a hit costs matters much more than
/// which entry an eviction picks.
#[derive(Default)]
struct HandleCache {
    open: HashMap<ContentHash, Arc<fs::File>>,
    order: VecDeque<ContentHash>,
}

impl HandleCache {
    fn get(&self, hash: &ContentHash) -> Option<Arc<fs::File>> {
        self.open.get(hash).map(Arc::clone)
    }

    fn put(&mut self, hash: ContentHash, file: Arc<fs::File>) {
        if self.open.insert(hash, file).is_none() {
            self.order.push_back(hash);
        }
        while self.order.len() > MAX_OPEN_CHUNKS {
            if let Some(oldest) = self.order.pop_front() {
                self.open.remove(&oldest);
            }
        }
    }

    fn forget(&mut self, hash: &ContentHash) {
        if self.open.remove(hash).is_some() {
            self.order.retain(|held| held != hash);
        }
    }
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
            touched: Mutex::default(),
            recording: AtomicBool::new(true),
            verified: Mutex::default(),
            handles: Mutex::default(),
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
        self.note(index);
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

    /// Every chunk file name the cache holds, across the buckets this snapshot
    /// draws from.
    ///
    /// One directory listing per fan-out bucket rather than one `stat` per
    /// chunk: 256 syscalls instead of 16167, over the same directory blocks.
    /// The buckets are gathered as the leading byte, so only the ones this
    /// snapshot could occupy are ever listed.
    fn resident_names(&self) -> HashSet<String> {
        let hashes = &self.manifest.files[&self.snapshot].chunk_hashes;
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
        present
    }

    /// Bitmap of which snapshot chunks are already on disk, LSB first. The
    /// harness seeds `image.isCached` from this so a restart does not re-prefetch
    /// what a previous session already paid for.
    pub fn resident_bitmap(&self) -> Vec<u8> {
        let hashes = &self.manifest.files[&self.snapshot].chunk_hashes;
        let present = self.resident_names();
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

    /// How many of this snapshot's chunks are already on disk.
    ///
    /// Distinct from [`Self::prefetch_progress`], which counts what the *current*
    /// sweep has fetched and so resets to zero each time one starts. The launcher
    /// needs the other question — how much of the game is already paid for —
    /// which only the cache itself can answer, and which survives restarts.
    pub fn resident_count(&self) -> usize {
        let present = self.resident_names();
        self.manifest.files[&self.snapshot]
            .chunk_hashes
            .iter()
            .filter(|hash| present.contains(hash.hex().as_str()))
            .count()
    }

    /// How many chunks the snapshot is made of.
    pub fn chunk_count(&self) -> usize {
        self.manifest.files[&self.snapshot].chunk_hashes.len()
    }

    /// Where the cache lives, for anyone who needs to ask the volume about it.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Add `index` to the boot list, if it is still open.
    fn note(&self, index: usize) {
        if self.recording.load(Ordering::Relaxed) {
            self.touched.lock().unwrap().insert(index);
        }
    }

    /// Close the boot list and write it out.
    ///
    /// Called when the page reports its first frame, so what it records is
    /// exactly the set of chunks between launch and a usable login screen —
    /// the part of the snapshot every session pays for, and the only part
    /// worth knowing in advance.
    ///
    /// Idempotent, because nothing stops the page saying so twice.
    pub fn seal_boot_list(&self) {
        if !self.recording.swap(false, Ordering::SeqCst) {
            return;
        }
        let chunks: Vec<usize> = self.touched.lock().unwrap().iter().copied().collect();
        if chunks.is_empty() {
            return;
        }
        let count = chunks.len();
        // The indices only mean the byte ranges they meant if the chunks are
        // still the same size. Chunk *count* deliberately is not recorded: the
        // snapshot grows with every patch, and an index below the old count
        // still names the same offset, which is what the client reads by.
        let list = BootList {
            chunk_size: self.chunk_size(),
            chunks,
        };
        match write_boot_list(&self.boot_list_path(), &list) {
            Ok(()) => eprintln!("[gwnative] boot list: {count} chunks recorded"),
            Err(e) => eprintln!("[gwnative] could not write the boot list: {e}"),
        }
    }

    /// Fetch last session's boot chunks in the background, ahead of demand.
    ///
    /// On a warm cache this is a few thousand `stat` calls and nothing else. On
    /// a cold one it is the whole point: the client asks for these chunks one
    /// at a time as it needs them, so without this the boot is a serial chain
    /// of round trips, and with it they overlap. `prefetch_permits` keeps the
    /// warm-up behind whatever the client is actually blocked on, so being
    /// wrong about the list costs bandwidth and never latency.
    pub fn warm_boot(self: &Arc<Self>) {
        let path = self.boot_list_path();
        let Some(list) = read_boot_list(&path) else {
            return;
        };
        if list.chunk_size != self.chunk_size() {
            eprintln!(
                "[gwnative] boot list is for {} KiB chunks, not {} KiB; discarding it",
                list.chunk_size / 1024,
                self.chunk_size() / 1024
            );
            let _ = fs::remove_file(&path);
            return;
        }

        let total = self.chunk_count();
        let chunks: Vec<usize> = list.chunks.into_iter().filter(|&i| i < total).collect();
        if chunks.is_empty() {
            return;
        }
        eprintln!("[gwnative] warming {} boot chunks", chunks.len());
        let chunks = Arc::new(chunks);
        for worker in 0..MAX_PREFETCH_FETCHES {
            let store = Arc::clone(self);
            let chunks = Arc::clone(&chunks);
            thread::spawn(move || {
                crate::qos::set(crate::qos::Class::Utility);
                for &index in chunks.iter().skip(worker).step_by(MAX_PREFETCH_FETCHES) {
                    let _permit = store.prefetch_permits.acquire();
                    if let Err(e) = store.ensure(index) {
                        // The list is a guess about a previous session. A chunk
                        // that is gone or changed is ordinary, and the demand
                        // read that wants it will report it properly.
                        eprintln!("[gwnative] warm chunk {index}: {e}");
                    }
                }
            });
        }
    }

    fn boot_list_path(&self) -> PathBuf {
        // Inside the cache rather than beside it: this describes that cache,
        // and has to travel and be discarded with it.
        self.cache_dir.join(BOOT_LIST)
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
        let file = self.handle(hash, expected)?;
        let window = read_window(&file, within, take);
        if window.is_none() {
            // Not necessarily fatal — the caller falls back to the full read,
            // which re-hashes and repairs. Drop the descriptor so that path
            // starts from the file as it is now.
            self.handles.lock().unwrap().forget(hash);
        }
        window
    }

    /// The open descriptor for a cached chunk, opening it if this is the first
    /// window read to ask.
    ///
    /// The length check happens once, when the descriptor is opened, rather
    /// than on every read: nothing can change the length behind an open
    /// descriptor, because nothing ever writes to a cached chunk in place.
    fn handle(&self, hash: &ContentHash, expected: u64) -> Option<Arc<fs::File>> {
        if let Some(file) = self.handles.lock().unwrap().get(hash) {
            return Some(file);
        }
        let file = Arc::new(open_sized(&self.cache_path(hash), expected)?);
        self.handles.lock().unwrap().put(*hash, Arc::clone(&file));
        Some(file)
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

/// Open `path`, provided it is exactly `expected` bytes long.
///
/// The length is the only check available here — a window read is a fraction of
/// the chunk, so there is nothing to hash it against. Callers must already know
/// the file's contents are good.
///
/// Checking once, at open, is the whole point: nothing writes to a cached chunk
/// in place, so a descriptor's file cannot change length underneath it.
fn open_sized(path: &Path, expected: u64) -> Option<fs::File> {
    let file = fs::File::open(path).ok()?;
    (file.metadata().ok()?.len() == expected).then_some(file)
}

/// `take` bytes at `within` of an already-open chunk, in one `pread`.
fn read_window(file: &fs::File, within: usize, take: usize) -> Option<Vec<u8>> {
    let mut window = vec![0u8; take];
    file.read_exact_at(&mut window, within as u64).ok()?;
    Some(window)
}

/// Filename of the boot list, inside the cache directory.
const BOOT_LIST: &str = "boot-chunks.json";

/// The chunks one session read on its way to a first frame.
struct BootList {
    chunk_size: u64,
    chunks: Vec<usize>,
}

fn write_boot_list(path: &Path, list: &BootList) -> std::io::Result<()> {
    let body = serde_json::json!({
        "chunkSize": list.chunk_size,
        "chunks": list.chunks,
    });
    // Same rename-in discipline as a chunk: a warm-up that read a half-written
    // list would warm a truncated set and never say why.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(body.to_string().as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Read the boot list, or `None` if there is not a usable one. Every failure is
/// the same answer — warm nothing — so none of them is worth distinguishing.
fn read_boot_list(path: &Path) -> Option<BootList> {
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    Some(BootList {
        chunk_size: raw.get("chunkSize")?.as_u64()?,
        chunks: raw
            .get("chunks")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_u64().map(|n| n as usize))
            .collect(),
    })
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
    let mut take = |entry: fs::DirEntry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { return };
        if !name.ends_with(".tmp") || name.contains(&ours) {
            return;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|t| t.elapsed().map_err(std::io::Error::other))
            .is_ok_and(|age| age > ORPHAN_AGE);
        if stale && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    };
    for bucket in buckets.flatten() {
        // The top level holds the boot list as well as the fan-out buckets, and
        // it is written the same rename-in way, so it can leave the same litter.
        let Ok(entries) = fs::read_dir(bucket.path()) else {
            take(bucket);
            continue;
        };
        for entry in entries.flatten() {
            take(entry);
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

    /// A store over a five-chunk snapshot, with no network behind it: every test
    /// here asks what is on disk, which is a question the client never answers.
    fn store_of_five(cache_dir: PathBuf) -> ChunkStore {
        let hashes = ["00", "11", "22", "33", "44"]
            .iter()
            .enumerate()
            .map(|(i, bucket)| {
                format!(
                    r#""{bucket}{}""#,
                    char::from(b'1' + i as u8).to_string().repeat(30)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let manifest = Manifest::parse(
            format!(
                r#"{{"compressionMode":"none","chunkSize":1024,
                     "files":[{{"name":"Gw.snapshot","size":5120,
                                "chunkHashes":[{hashes}]}}]}}"#
            )
            .as_bytes(),
        )
        .expect("the synthetic manifest should parse");
        ChunkStore::open(
            Client::new(String::new(), String::new()),
            manifest,
            cache_dir,
        )
        .expect("the store should open over an empty cache")
    }

    #[test]
    fn residency_is_counted_from_the_same_scan_it_is_drawn_from() {
        let temp = TempDir::new("residency");
        let cache = temp.0.join("chunks");
        let store = store_of_five(cache.clone());
        let hashes = &store.manifest.files["Gw.snapshot"].chunk_hashes;

        assert_eq!(store.resident_count(), 0, "an empty cache holds nothing");
        assert_eq!(store.resident_bitmap(), vec![0u8]);

        for index in [0, 2, 4] {
            let hash = &hashes[index];
            let bucket = cache.join(format!("{:02x}", hash.bytes()[0]));
            fs::create_dir_all(&bucket).unwrap();
            fs::write(bucket.join(hash.hex().as_str()), vec![0u8; 1024]).unwrap();
        }
        // A write in flight leaves one of these beside the chunk it will become.
        // It is in the listing and is not the chunk, and the count has to agree
        // with the bitmap about that rather than each deciding for itself.
        let pending = &hashes[1];
        let bucket = cache.join(format!("{:02x}", pending.bytes()[0]));
        fs::create_dir_all(&bucket).unwrap();
        fs::write(
            bucket.join(format!("{}.7.tmp", pending.hex())),
            vec![0u8; 512],
        )
        .unwrap();

        assert_eq!(store.resident_count(), 3);
        assert_eq!(store.resident_bitmap(), vec![0b0001_0101]);
        assert_eq!(store.chunk_count(), 5);
    }

    #[test]
    fn a_window_reads_only_its_own_span() {
        let temp = TempDir::new("window");
        let chunk: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let path = temp.0.join("chunk");
        fs::write(&path, &chunk).unwrap();

        let file = open_sized(&path, 4096).unwrap();
        assert_eq!(
            read_window(&file, 1000, 472).unwrap(),
            chunk[1000..1472],
            "the pread window must match the same span of the whole chunk"
        );
        // The commonest read of all: the first bytes of a chunk.
        assert_eq!(read_window(&file, 0, 8).unwrap(), chunk[..8]);
        // And the last, which must not read past the end.
        assert_eq!(read_window(&file, 4090, 6).unwrap(), chunk[4090..]);
        // Reads repeat through the one descriptor, which is the point of
        // holding it: the second read of a span must equal the first.
        assert_eq!(read_window(&file, 1000, 472).unwrap(), chunk[1000..1472]);
    }

    #[test]
    fn a_window_refuses_a_file_of_the_wrong_length() {
        let temp = TempDir::new("truncated");
        let path = temp.0.join("chunk");
        fs::write(&path, vec![0u8; 100]).unwrap();

        // A truncated cache file is the case the length check exists for: the
        // pread would happily serve a window that lies wholly within it.
        assert!(open_sized(&path, 4096).is_none());
        assert!(open_sized(&temp.0.join("missing"), 100).is_none());
        // Past the end of a correctly-sized file, `read_exact_at` is the check.
        let file = open_sized(&path, 100).unwrap();
        assert!(read_window(&file, 96, 8).is_none());
    }

    #[test]
    fn the_handle_cache_evicts_oldest_first_and_forgets_on_demand() {
        let mut cache = HandleCache::default();
        let temp = TempDir::new("handles");
        let path = temp.0.join("chunk");
        fs::write(&path, b"held open").unwrap();
        let open = || Arc::new(fs::File::open(&path).unwrap());

        let hash = |n: u32| ContentHash::parse(&format!("{n:040x}")).unwrap();
        for n in 0..=(MAX_OPEN_CHUNKS as u32) {
            cache.put(hash(n), open());
        }
        assert_eq!(cache.order.len(), MAX_OPEN_CHUNKS, "the cap must hold");
        assert_eq!(cache.open.len(), MAX_OPEN_CHUNKS, "and both halves agree");
        assert!(cache.get(&hash(0)).is_none(), "the oldest goes first");

        // Re-inserting a hash already held must not queue it a second time, or
        // eviction would drop a descriptor still in the map.
        let held = hash(5);
        cache.put(held, open());
        cache.put(held, open());
        assert_eq!(
            cache.order.iter().filter(|&&h| h == held).count(),
            1,
            "a repeated insert must not double-queue"
        );

        cache.forget(&held);
        assert!(cache.get(&held).is_none());
        assert!(!cache.order.contains(&held), "forget clears both halves");
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

    #[test]
    fn the_sweep_reaches_the_top_level_too() {
        let temp = TempDir::new("toplevel");
        let stale = temp.0.join("boot-chunks.999999.tmp");
        let list = temp.0.join(BOOT_LIST);
        fs::write(&stale, b"x").unwrap();
        fs::write(&list, b"{}").unwrap();
        let long_ago =
            std::time::SystemTime::now() - ORPHAN_AGE - std::time::Duration::from_secs(60);
        fs::File::open(&stale)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        sweep_orphans(&temp.0);

        assert!(!stale.exists(), "the boot list writes temps here as well");
        assert!(list.exists(), "the boot list itself is not litter");
    }

    #[test]
    fn a_boot_list_survives_the_round_trip() {
        let temp = TempDir::new("bootlist");
        let path = temp.0.join(BOOT_LIST);
        let list = BootList {
            chunk_size: 256 * 1024,
            chunks: vec![0, 1, 7, 16_166],
        };

        write_boot_list(&path, &list).unwrap();
        let read = read_boot_list(&path).unwrap();

        assert_eq!(read.chunk_size, list.chunk_size);
        assert_eq!(read.chunks, list.chunks);
        assert!(
            !path
                .with_extension(format!("{}.tmp", std::process::id()))
                .exists(),
            "the temp should have been renamed away, not left beside the list"
        );
    }

    #[test]
    fn a_boot_list_that_cannot_be_trusted_reads_as_absent() {
        let temp = TempDir::new("bootjunk");
        let path = temp.0.join(BOOT_LIST);

        assert!(read_boot_list(&path).is_none(), "nothing written yet");
        fs::write(&path, b"not json at all").unwrap();
        assert!(read_boot_list(&path).is_none());
        // A list without the chunk size cannot be checked against this
        // snapshot's geometry, so it is worth no more than no list.
        fs::write(&path, br#"{"chunks":[1,2]}"#).unwrap();
        assert!(read_boot_list(&path).is_none());
    }
}
