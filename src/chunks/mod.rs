//! On-demand content-addressed chunk store for `Gw.snapshot`.
//!
//! The snapshot is 4.2 GB in 256 KiB chunks. The client only ever reads the
//! parts of it a session touches, so nothing is downloaded up front: a read
//! pulls the chunks it covers, verifies them, and caches them by content hash.
//! Chunks are deduplicated by construction — the same hash appearing twice in
//! the manifest is stored once.
//!
//! One type, [`ChunkStore`], split across four files by what the code is doing
//! rather than by what it touches. This one is the read path the client is
//! blocked on: turn a byte range into chunks, serve them from the cache or
//! fetch them, and let concurrent readers of the same chunk share one download.
//! [`files`] is everything that goes near the cache directory, [`prefetch`] is
//! the background threads that guess ahead of the client, and [`gate`] holds
//! the two synchronisation primitives all of them fetch through.

mod files;
#[cfg(test)]
mod fixture;
mod gate;
mod prefetch;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::cache::{prune, sweep_orphans};
use crate::error::{Error, Result};
use crate::manifest::{ContentHash, Manifest};
use crate::patch::Client;

use files::HandleCache;
use gate::{Permit, Semaphore, Slot};
use prefetch::{MAX_PREFETCH_FETCHES, Prefetch, Readahead};

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
    /// The chunks demand reads have moved past. See
    /// [`advance_readahead`](Self::advance_readahead).
    readahead: Readahead,
    /// Which chunks this session has read, while the boot list is still being
    /// recorded. See [`seal_boot_list`](Self::seal_boot_list).
    touched: Mutex<BTreeSet<usize>>,
    /// Whether `touched` is still accumulating. Checked before the lock, so a
    /// sealed list costs a relaxed load per read rather than a mutex.
    recording: AtomicBool,
    /// Whether anything is on screen yet. Decides what class the fetch threads
    /// run at — see [`fetch_class`](Self::fetch_class).
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
    /// Why the most recent fetch that failed, failed.
    ///
    /// Kept because the page cannot see it otherwise. A chunk read reaches the
    /// client through ArenaNet's own glue, which reports a fatal read by
    /// calling `handleFatalReadError` with no argument at all — so at the one
    /// moment the player is owed a reason, the only side that has one is this
    /// one. Overwritten rather than accumulated: a failing session fails the
    /// same way a thousand times, and it is the last word that is asked for.
    last_failure: Mutex<Option<String>>,
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

/// Where chunks came from. `coalesced` is the count of reads that joined a
/// fetch already in flight instead of starting their own.
#[derive(Default)]
struct Stats {
    from_cache: AtomicU64,
    fetched: AtomicU64,
    coalesced: AtomicU64,
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
            last_failure: Mutex::default(),
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

    /// Why the last failed fetch failed, if one has.
    pub fn last_failure(&self) -> Option<String> {
        self.last_failure.lock().unwrap().clone()
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

    /// How many chunks the snapshot is made of.
    pub fn chunk_count(&self) -> usize {
        self.manifest.files[&self.snapshot].chunk_hashes.len()
    }

    /// Validate and seed the cache from a raw local snapshot image.
    ///
    /// The local file is never trusted by name or size alone. Every manifest
    /// chunk is hashed before the ordinary atomic cache write makes it visible,
    /// so a partial, stale, or unrelated image cannot poison later reads.
    pub fn import_image(&self, path: &Path) -> Result<()> {
        let file = fs::File::open(path)?;
        let actual = file.metadata()?.len();
        let expected = self.snapshot_size();
        if actual != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} is {actual} bytes; the current game image is {expected} bytes",
                    path.display()
                ),
            )
            .into());
        }

        let mut input = BufReader::new(file);
        let total = self.chunk_count();
        let mut reported = usize::MAX;
        for index in 0..total {
            let hash = self.chunk_hash(index)?;
            let length = self.chunk_length(index)?;
            let mut bytes = vec![0u8; length as usize];
            input.read_exact(&mut bytes)?;
            crate::patch::verify(&bytes, &hash)?;
            self.write_cached(&hash, &bytes)?;
            self.verified.lock().unwrap().insert(hash);

            let percent = (index + 1).saturating_mul(100) / total.max(1);
            if percent / 5 != reported / 5 {
                reported = percent;
                note!(
                    "[gwnative] importing local game image: {}/{} ({percent}%)",
                    index + 1,
                    total
                );
            }
        }
        Ok(())
    }

    /// Where the cache lives, for anyone who needs to ask the volume about it.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
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

    /// Put every chunk covering `offset..offset + length` on disk, without
    /// producing any of their bytes.
    ///
    /// The client warms ranges it is about to need, and the only thing it wants
    /// back is the knowledge that they are there. Serving that as a range read
    /// meant 256 KiB crossing the loopback socket per chunk and being materialised
    /// into an `ArrayBuffer` the page immediately dropped — measured at boot, some
    /// 1.3 GB of garbage the collector was still catching up with when the
    /// renderer's footprint peaked.
    pub fn warm(&self, offset: u64, length: u64) -> Result<()> {
        let produced = self.readable(offset, length);
        if produced == 0 {
            return Ok(());
        }
        let chunk_size = self.chunk_size();
        let first = (offset / chunk_size) as usize;
        let last = ((offset + produced - 1) / chunk_size) as usize;
        for index in first..=last {
            // The same two calls a real read makes, for the same reason. Warming
            // is the client walking forwards through the image, and it is the
            // strongest statement of where it is going that the store ever gets:
            // without them the readahead window never moves, every chunk waits
            // for the one before it, and a boot that took nine seconds takes
            // twenty-two.
            self.note(index);
            self.advance_readahead(index);
            self.ensure(index)?;
        }
        Ok(())
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
        self.fetch(index, Some(permit)).map(|_| ())
    }

    /// Fetch chunk `index`, from cache if present. Concurrent callers asking
    /// for the same chunk share one fetch rather than racing to download it.
    fn chunk(&self, index: usize) -> Result<Arc<Vec<u8>>> {
        self.fetch(index, None)
    }

    /// The one fetch path, and so the one place a read failure can be caught on
    /// its way out. What it catches is kept for [`last_failure`](Self::last_failure).
    fn fetch<'a>(&'a self, index: usize, permit: Option<Permit<'a>>) -> Result<Arc<Vec<u8>>> {
        self.attempt(index, permit).inspect_err(|e| {
            *self.last_failure.lock().unwrap() = Some(e.to_string());
        })
    }

    /// `permit` is `Some` when the caller already holds one and is handing it
    /// over: if this turns out to coalesce onto somebody else's fetch, the
    /// permit is released rather than held across the wait.
    fn attempt<'a>(&'a self, index: usize, permit: Option<Permit<'a>>) -> Result<Arc<Vec<u8>>> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunks::fixture::store_of;
    use crate::scratch::TempDir;

    #[test]
    fn a_local_image_is_verified_before_it_seeds_the_cache() {
        let temp = TempDir::new("local-image");
        let bytes = vec![0x5au8; 1024];
        let store = crate::chunks::fixture::store_of_content(temp.0.join("chunks"), &[&bytes]);
        let image = temp.0.join("Gw.snapshot");
        fs::write(&image, &bytes).unwrap();

        store.import_image(&image).unwrap();
        assert_eq!(store.resident_count(), 1);

        let other = crate::chunks::fixture::store_of_content(temp.0.join("other"), &[&bytes]);
        fs::write(&image, vec![0xa5u8; 1024]).unwrap();
        assert!(other.import_image(&image).is_err());
        assert_eq!(other.resident_count(), 0);
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
        let free = || store.permits.free();

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

    /// The page has no other source for this. ArenaNet's glue reports a fatal
    /// read by calling `handleFatalReadError` with no argument, so if the store
    /// does not keep why the fetch failed, the sentence the player reads can
    /// only guess — which is what it used to do.
    ///
    /// Failed here by asking for a chunk the manifest does not have, rather
    /// than by letting a fetch fall off the end of a network this store does
    /// not have: the retry ladder makes that take fourteen seconds, and what is
    /// under test is on the way out of `fetch`, which both arrive at.
    #[test]
    fn a_failed_fetch_leaves_its_reason_behind() {
        let temp = TempDir::new("last-failure");
        let store = store_of(temp.0.join("chunks"), 4);

        assert_eq!(store.last_failure(), None, "nothing has failed yet");
        store.fetch(99, None).expect_err("chunk 99 does not exist");

        let reason = store.last_failure().expect("the failure left no reason");
        assert!(
            reason.contains("99"),
            "{reason:?} is not the failure that just happened"
        );
    }
}
