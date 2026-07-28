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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::cache::{
    self, BOOT_LIST, BootList, open_sized, prune, read_boot_list, read_window, sweep_orphans,
    write_boot_list,
};
use crate::error::{Error, Result};
use crate::manifest::{ContentHash, Manifest};
use crate::patch::Client;
use crate::qos;

/// ArenaNet sees at most this many concurrent requests from us, no matter how
/// many reads the client has in flight.
///
/// On HTTP/2 these are streams on one connection, not connections of their
/// own — the transport pays no handshake for them, and the CDN advertises
/// room for 128. Depth is what buys throughput on this path: a 256 KiB chunk
/// costs one round trip, so at the ~150 ms a cold CloudFront edge answers in,
/// each stream moves under 2 MiB/s and only the number in flight multiplies
/// it. Measured on a blank install against a cold edge, sixteen in flight
/// sustained 13 MiB/s — the arithmetic of that latency, not of the pipe.
/// Eight was tuned for the HTTP/1.1 client this replaced, where every slot
/// cost a socket; forty-eight is a third of what the server offers.
const MAX_CONCURRENT_FETCHES: usize = 48;

/// Background prefetch never claims more than this share of
/// [`MAX_CONCURRENT_FETCHES`], so a full download always leaves slots free for
/// the reads the game is actually blocked on. Without the reserve a sweep of
/// 16000 chunks would sit in front of every demand read and stall rendering —
/// which is the whole reason gwonmac splits its queue by priority.
///
/// Two thirds of the pool, because during the boot warm-up the guesses *are*
/// the demand: the list being replayed is the very set of chunks the client
/// is about to read, so a demand read for a listed chunk coalesces onto the
/// prefetch already carrying it rather than queueing behind it. Sixteen slots
/// still exist that prefetch can never occupy, for the reads no list
/// predicted.
const MAX_PREFETCH_FETCHES: usize = 32;

/// Threads that service the demand readahead window.
///
/// Its own number rather than [`MAX_PREFETCH_FETCHES`], which counts permits:
/// the warm-up and the full download size their worker pools to their permit
/// share because their work is a list known up front, but the readahead
/// window rarely holds more than a few chunks nothing else is already
/// fetching, and a thread parked on a condvar for each permit would be
/// twenty-four threads waiting for work that arrives eight at a time.
const READAHEAD_WORKERS: usize = 8;

/// How far ahead of the client's read cursor to fetch, in chunks.
///
/// [`warm_boot`](ChunkStore::warm_boot) overlaps the round trips a boot spends
/// on the snapshot, but it can only replay a list some earlier session left
/// behind — so the very first launch, the one a new player actually judges the
/// app on, gets nothing from it. Without readahead that launch was one round
/// trip per read, each waiting for the last; with it, the reads the client is
/// about to issue are already in flight.
///
/// 48 chunks is 12 MiB — two full fetch pools of work queued ahead of the
/// cursor, so a pool of sixteen never drains between the client's reads, and
/// still streaming rather than the full download the player did not ask for.
/// Sized with [`MAX_CONCURRENT_FETCHES`]: a window smaller than the pool
/// would idle the slots the pool was widened to fill.
const READAHEAD_CHUNKS: usize = 48;

/// The window of chunks readahead workers are allowed to fetch.
///
/// `next` is where the workers have got to and `limit` is one past the last
/// index worth fetching; both move with the client's reads. Behind one mutex
/// rather than two atomics because a seek has to move them together — a worker
/// that saw the new `next` against the old `limit` would fetch into whatever
/// the cursor had just left.
#[derive(Default)]
struct Readahead {
    window: Mutex<Window>,
    wake: Condvar,
}

#[derive(Default)]
struct Window {
    next: usize,
    limit: usize,
}

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
    /// The chunks demand reads have moved past. See [`READAHEAD_CHUNKS`].
    readahead: Readahead,
    /// Which chunks this session has read, while the boot list is still being
    /// recorded. See [`seal_boot_list`](ChunkStore::seal_boot_list).
    touched: Mutex<BTreeSet<usize>>,
    /// Whether `touched` is still accumulating. Checked before the lock, so a
    /// sealed list costs a relaxed load per read rather than a mutex.
    recording: AtomicBool,
    /// Whether anything is on screen yet. Decides what class the fetch threads
    /// run at — see [`fetch_class`](ChunkStore::fetch_class).
    playing: AtomicBool,
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
        // Every hash this manifest can ever ask for, across every file in it —
        // not just the snapshot's. Collected here, on the manifest that was
        // just fetched and is about to become the live one, because that is
        // what makes everything else in the cache provably dead.
        //
        // Owned strings rather than borrowed `Hex`, which is a stack type with
        // no identity: this crosses onto another thread and has to outlive the
        // manifest reference it came from. A 4.2 GB snapshot is ~16k hashes, so
        // the set is about a megabyte and it is dropped as soon as it is used.
        let live: HashSet<String> = manifest
            .files
            .values()
            .flat_map(|file| {
                file.chunk_hashes
                    .iter()
                    .map(|hash| hash.hex().as_str().to_owned())
            })
            .collect();

        // Off the launch path: this walks 256 directories and the game has
        // nothing to gain by waiting for it.
        thread::spawn({
            let cache_dir = cache_dir.clone();
            move || {
                crate::qos::set(crate::qos::Class::Utility);
                sweep_orphans(&cache_dir);
                prune(&cache_dir, &live);
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
            readahead: Readahead::default(),
            touched: Mutex::default(),
            recording: AtomicBool::new(true),
            playing: AtomicBool::new(false),
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

    /// What the transport's retry ladder has cost this session. See
    /// [`Retries`](crate::patch::Retries).
    pub fn retries(&self) -> (u64, u64) {
        self.client.retries()
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
        self.advance_readahead(index);
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
        // Speculative by definition — every caller of this is guessing about a
        // chunk nothing has asked for yet. Take the fetch permit *before*
        // claiming the chunk: a demand read joins whatever fetch is already
        // claimed for a chunk, so a guess that claims one and then queues for a
        // permit makes the read it was meant to save wait behind the entire
        // queue in front of it. Measured, that inversion turned the worst
        // snapshot read of a session from 1.4 s into 16.1 s.
        let permit = self.permits.acquire();
        self.fetch_holding_permit(index, permit).map(|_| ())
    }

    /// Fetch chunk `index`, from cache if present. Concurrent callers asking
    /// for the same chunk share one fetch rather than racing to download it.
    fn chunk(&self, index: usize) -> Result<Arc<Vec<u8>>> {
        self.fetch(index, None)
    }

    /// [`chunk`](Self::chunk) for a caller that already holds a fetch permit,
    /// which it hands over: if this turns out to coalesce onto someone else's
    /// fetch, the permit is released rather than held across the wait.
    fn fetch_holding_permit<'a>(
        &'a self,
        index: usize,
        permit: Permit<'a>,
    ) -> Result<Arc<Vec<u8>>> {
        self.fetch(index, Some(permit))
    }

    fn fetch<'a>(&'a self, index: usize, permit: Option<Permit<'a>>) -> Result<Arc<Vec<u8>>> {
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
            // A waiter has no use for a permit, and a caller that brought one
            // gives it back here rather than hold a fetch slot across a wait
            // for somebody else's download.
            drop(permit);
            self.stats.coalesced.fetch_add(1, Ordering::Relaxed);
            return slot.wait();
        }
        self.stats.fetched.fetch_add(1, Ordering::Relaxed);

        // Only the owner holds a permit, so waiters never occupy one — which is
        // what keeps a burst of reads for one chunk from starving the pool.
        let fetched = {
            let _permit = permit.unwrap_or_else(|| self.permits.acquire());
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
                    Err(e) => note!("[gwnative] chunk cache write failed: {e}"),
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

    /// Move the readahead window past the chunk a demand read just asked for.
    ///
    /// Called on every read, cached or not: what makes the next read cheap is
    /// knowing where this one was, and a run of cache hits is the clearest
    /// possible signal that the client is walking forwards.
    fn advance_readahead(&self, index: usize) {
        let start = index + 1;
        let limit = (start + READAHEAD_CHUNKS).min(self.chunk_count());
        if start >= limit {
            return;
        }
        let mut window = self.readahead.window.lock().unwrap();
        // Outside the window means the client seeked, and the chunks the
        // workers were about to fetch are no longer the ones in front of it.
        // Inside it means they are already ahead, and where they have got to is
        // worth more than starting the run again.
        if window.next < start || window.next > limit {
            window.next = start;
        }
        window.limit = limit;
        drop(window);
        self.readahead.wake.notify_all();
    }

    /// What class the fetch threads should be running at right now.
    ///
    /// Before the first frame there is no game to protect and the player is
    /// watching a progress bar whose whole content is these threads, so they
    /// are the interactive path and say so. After it, the same threads are
    /// topping a cache up behind someone who is playing, and utility is right:
    /// efficiency cores, out of the way of the frame.
    ///
    /// The class buys more than core selection, which is what makes this worth
    /// getting right: on Apple platforms it also decides where the transfer
    /// sits in the system's network scheduling, and the fetch threads are the
    /// ones asking. The symptom that pointed here is measured — a sweep that
    /// had settled into 4 MiB/s, while a `curl` at the default class pulled
    /// the same eight chunks from the same host in 0.23 s, so neither the
    /// network nor the CDN was the limit at that moment. How much of the gap
    /// the class accounts for is what the full-download race decides.
    fn fetch_class(&self) -> qos::Class {
        if self.playing.load(Ordering::Relaxed) {
            qos::Class::Utility
        } else {
            qos::Class::UserInitiated
        }
    }

    /// Fetch a little way ahead of wherever the client is reading.
    ///
    /// Idle until a read moves the window, so it costs a parked thread and
    /// nothing else on a warm cache. Workers take `prefetch_permits` for the
    /// same reason [`warm_boot`](ChunkStore::warm_boot) does: a guess about
    /// what comes next must never sit in front of a read the game is blocked
    /// on. Being wrong costs bandwidth, never latency.
    pub fn start_readahead(self: &Arc<Self>) {
        for _ in 0..READAHEAD_WORKERS {
            let store = Arc::clone(self);
            thread::spawn(move || {
                let mut class = qos::Following::start(store.fetch_class());
                loop {
                    class.now(store.fetch_class());
                    let index = {
                        let mut window = store.readahead.window.lock().unwrap();
                        while window.next >= window.limit {
                            window = store.readahead.wake.wait(window).unwrap();
                        }
                        let index = window.next;
                        window.next += 1;
                        index
                    };
                    let _permit = store.prefetch_permits.acquire();
                    // Best effort by construction: nothing has asked for this
                    // chunk yet, and the demand read that eventually does is
                    // where a failure has to be reported from.
                    let _ = store.ensure(index);
                }
            });
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
        // The same signal decides what the fetch threads run at from here on:
        // there is a game on screen now, and they stop being the thing the
        // player is waiting for. Set before the idempotence check, because a
        // second report still means the frame happened.
        self.playing.store(true, Ordering::Relaxed);
        if !self.recording.swap(false, Ordering::SeqCst) {
            return;
        }
        let mut chunks: BTreeSet<usize> = self.touched.lock().unwrap().clone();
        if chunks.is_empty() {
            return;
        }
        // Union with what is already recorded, never replace. A warm session
        // reaches its first frame from cache having touched a fraction of the
        // real boot set — measured, a 1,389-chunk list shrank to 102 after one
        // warm boot — and a list overwritten down to that fraction warms
        // almost nothing for whoever next loses the cache. Chunks the set no
        // longer needs age out when a patch changes the chunk size and the
        // whole list is discarded.
        if let Some(existing) = read_boot_list(&self.boot_list_path())
            && existing.chunk_size == self.chunk_size()
        {
            chunks.extend(existing.chunks);
        }
        let chunks: Vec<usize> = chunks.into_iter().collect();
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
            Ok(()) => note!("[gwnative] boot list: {count} chunks recorded"),
            Err(e) => note!("[gwnative] could not write the boot list: {e}"),
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
        // A blank install has recorded nothing, which used to mean the first
        // boot — the one a new player judges the app by — was the only boot
        // this could not help. The built-in list recorded from a real cold
        // boot stands in; see `built_in_boot_list` for why that is sound.
        let list = match read_boot_list(&path) {
            Some(list) => list,
            None => match cache::built_in_boot_list() {
                Some(list) => list,
                None => return,
            },
        };
        if list.chunk_size != self.chunk_size() {
            note!(
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
        note!("[gwnative] warming {} boot chunks", chunks.len());
        let chunks = Arc::new(chunks);
        for worker in 0..MAX_PREFETCH_FETCHES {
            let store = Arc::clone(self);
            let chunks = Arc::clone(&chunks);
            thread::spawn(move || {
                // Always the launch path when it runs at all: the warm-up
                // exists to be the thing the first frame is waiting for.
                let mut class = qos::Following::start(store.fetch_class());
                for &index in chunks.iter().skip(worker).step_by(MAX_PREFETCH_FETCHES) {
                    class.now(store.fetch_class());
                    let _permit = store.prefetch_permits.acquire();
                    if let Err(e) = store.ensure(index) {
                        // The list is a guess about a previous session. A chunk
                        // that is gone or changed is ordinary, and the demand
                        // read that wants it will report it properly.
                        note!("[gwnative] warm chunk {index}: {e}");
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
                // whether that is background work depends entirely on when it
                // is running. Started from the launcher's "download everything
                // first", it *is* the launch: nothing else is happening, no
                // frame is being competed with, and the player is watching a
                // bar that measures exactly these threads. Started later, or
                // still running once the client is up, it is a top-up behind a
                // game and belongs on efficiency cores. `fetch_class` is that
                // distinction; `prefetch_permits` rations these threads' share
                // of the network either way.
                let mut class = qos::Following::start(store.fetch_class());
                let total = store.chunk_count();
                let mut index = worker;
                while index < total {
                    if store.prefetch.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    class.now(store.fetch_class());
                    // A cached chunk costs a stat, not a request, so the common
                    // resume case sweeps the whole list almost instantly.
                    let _permit = store.prefetch_permits.acquire();
                    if let Err(e) = store.ensure(index) {
                        // One bad chunk should not abandon the sweep; the game
                        // will ask for it again on demand and surface the error
                        // there, where it can be acted on.
                        note!("[gwnative] prefetch chunk {index}: {e}");
                    }
                    store.prefetch.done.fetch_add(1, Ordering::Relaxed);
                    index += workers;
                }
                if outstanding.fetch_sub(1, Ordering::SeqCst) == 1 {
                    store.prefetch.running.store(false, Ordering::SeqCst);
                    let (done, total, _) = store.prefetch_progress();
                    note!("[gwnative] full download finished: {done}/{total} chunks");
                }
            });
        }
        note!(
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
            note!("[gwnative] cached chunk is corrupt, refetching: {e}");
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
        let tmp = cache::temp_path(parent, &hash.hex());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;
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

    /// A store over `count` chunks, for the arithmetic that only bites on a
    /// snapshot bigger than the readahead window.
    fn store_of(cache_dir: PathBuf, count: usize) -> ChunkStore {
        let hashes = (0..count)
            .map(|i| format!(r#""{i:032x}""#))
            .collect::<Vec<_>>()
            .join(",");
        let manifest = Manifest::parse(
            format!(
                r#"{{"compressionMode":"none","chunkSize":1024,
                     "files":[{{"name":"Gw.snapshot","size":{},
                                "chunkHashes":[{hashes}]}}]}}"#,
                count * 1024
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

    /// The fetch threads are the launch until there is a frame, and background
    /// work afterwards. Getting this backwards is invisible in a test that only
    /// checks bytes: it costs throughput on the one run nobody can afford it
    /// on, so the flip is asserted directly.
    #[test]
    fn the_fetchers_stop_being_the_launch_once_a_frame_lands() {
        let temp = TempDir::new("fetch-class");
        let store = store_of(temp.0.join("chunks"), 8);
        assert!(
            matches!(store.fetch_class(), qos::Class::UserInitiated),
            "before a first frame the fetchers are what the player is waiting for"
        );
        store.seal_boot_list();
        assert!(
            matches!(store.fetch_class(), qos::Class::Utility),
            "after it they are competing with a game"
        );
    }

    #[test]
    fn the_readahead_window_follows_the_reads_and_stops_at_the_end() {
        let temp = TempDir::new("readahead");
        let store = store_of(temp.0.join("chunks"), 200);
        let window = || {
            let w = store.readahead.window.lock().unwrap();
            (w.next, w.limit)
        };

        store.advance_readahead(0);
        assert_eq!(window(), (1, 1 + READAHEAD_CHUNKS), "opens past the read");

        // Workers have got to 10. A read at 5 is behind them, and where they
        // are is worth more than starting the run over — only the far edge
        // moves.
        store.readahead.window.lock().unwrap().next = 10;
        store.advance_readahead(5);
        assert_eq!(window(), (10, 6 + READAHEAD_CHUNKS));

        // A seek out of the window is the case that matters: the chunks the
        // workers were about to fetch are no longer in front of the client.
        store.advance_readahead(150);
        assert_eq!(window(), (151, 151 + READAHEAD_CHUNKS));

        // And a seek backwards far enough to leave them behind it.
        store.advance_readahead(3);
        assert_eq!(window(), (4, 4 + READAHEAD_CHUNKS));
    }

    /// The whole point of taking the permit first. A demand read joins a fetch
    /// already claimed for the chunk it wants, so a speculative fetch holding
    /// that claim while it queues for a permit hands its own wait to the read
    /// it was supposed to save — which measured as a worst-case snapshot read
    /// of 16.1 s against 1.4 s without any readahead at all.
    #[test]
    fn a_speculative_fetch_claims_no_chunk_until_it_can_start() {
        let temp = TempDir::new("no-inversion");
        let store = store_of(temp.0.join("chunks"), 200);

        // Every fetch slot taken, as they are whenever the client is busy.
        let held: Vec<_> = (0..MAX_CONCURRENT_FETCHES)
            .map(|_| store.permits.acquire())
            .collect();

        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                // No network behind this store, so it fails once it starts —
                // starting is the part under test.
                let _ = store.ensure(1);
            });

            // Long enough that a claim-then-queue would have left its mark.
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert!(
                store.inflight.lock().unwrap().is_empty(),
                "a chunk was claimed by a fetch that had not started"
            );

            // Put the chunk where the warm will find it once it is let through,
            // so this ends on the cache rather than on four rounds of retry
            // against a network this store does not have. It could not have
            // been there earlier: `ensure` would have returned on it and never
            // reached the permit this test is about.
            let hash = store.manifest.files["Gw.snapshot"].chunk_hashes[1];
            let path = store.cache_path(&hash);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![0u8; 1024]).unwrap();
            store.verified.lock().unwrap().insert(hash);

            drop(held);
            waiting
                .join()
                .expect("the warm should end, successfully or not");
        });
    }

    /// The other half of taking the permit first. Having taken one, a
    /// speculative fetch that finds the chunk already being downloaded has
    /// nothing left to do but wait, and a fetch slot is far too scarce to spend
    /// on waiting — there are eight for every read the client has in flight.
    #[test]
    fn a_fetch_that_coalesces_gives_its_permit_back() {
        let temp = TempDir::new("coalesce-permit");
        let store = store_of(temp.0.join("chunks"), 200);
        let free = || *store.permits.available.lock().unwrap();

        // Stand in for a download this store already has under way.
        let hash = store.manifest.files["Gw.snapshot"].chunk_hashes[1];
        let slot = Arc::new(Slot::new());
        store
            .inflight
            .lock()
            .unwrap()
            .insert(hash, Arc::clone(&slot));

        // Everything is observed inside the scope and judged outside it. An
        // assertion that fires while the spawned thread is still parked on the
        // slot would hang here rather than fail, because the scope waits for a
        // thread only this test's own later lines will ever release.
        let (coalesced, free_while_waiting) = std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                let _ = store.ensure(1);
            });

            // `ensure` takes its permit before it looks, so the count dipping
            // and coming back is the handover happening.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while store.stats.coalesced.load(Ordering::Relaxed) == 0
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let observed = (store.stats.coalesced.load(Ordering::Relaxed), free());

            slot.fulfil(Ok(Arc::new(vec![0u8; 1024])));
            waiting.join().expect("the wait should end");
            observed
        });

        assert_eq!(coalesced, 1, "the fetch never joined the one in flight");
        assert_eq!(
            free_while_waiting, MAX_CONCURRENT_FETCHES,
            "a fetch slot was held across a wait for somebody else's download"
        );
        assert_eq!(free(), MAX_CONCURRENT_FETCHES, "a permit went missing");
    }

    #[test]
    fn readahead_never_points_past_the_last_chunk() {
        let temp = TempDir::new("readahead-end");
        let store = store_of(temp.0.join("chunks"), 5);
        let window = || {
            let w = store.readahead.window.lock().unwrap();
            (w.next, w.limit)
        };

        store.advance_readahead(0);
        assert_eq!(window(), (1, 5), "a short snapshot clamps to what exists");

        // The last chunk has nothing after it. Leaving `limit` where it was
        // would be worse than doing nothing: the workers would fetch the tail
        // again every time the client read it.
        store.advance_readahead(4);
        assert_eq!(window(), (1, 5), "the end of the file opens no window");
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
}
