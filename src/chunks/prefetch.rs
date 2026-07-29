//! Everything the store does before it is asked.
//!
//! Three guesses, in the order a session meets them. [`warm_boot`] replays the
//! chunk list an earlier launch left behind, so a boot's round trips overlap
//! instead of queueing one behind the next. [`start_readahead`] follows the
//! client's read cursor for the parts no list predicted. And
//! [`start_full_download`] is the player asking for the whole 4.2 GB up front.
//!
//! All three obey the same rule, which is the only interesting thing about
//! them: a guess must never sit in front of a read the game is blocked on.
//! They share one reserve of permits that can never grow past
//! [`MAX_PREFETCH_FETCHES`], so being wrong costs bandwidth and never latency —
//! and they run at the class [`fetch_class`] picks, which is the launch itself
//! until a frame lands and background work afterwards.
//!
//! [`start_verify`] is the odd one out and lives here anyway, because it is
//! the same shape over the same list asking the opposite question: not what is
//! missing, but whether what is already here is real. It is the only thing in
//! the crate that re-hashes a chunk nobody asked for, and it runs on exactly
//! one occasion — a launch that told the player the download was finished.
//!
//! [`warm_boot`]: ChunkStore::warm_boot
//! [`start_readahead`]: ChunkStore::start_readahead
//! [`start_full_download`]: ChunkStore::start_full_download
//! [`start_verify`]: ChunkStore::start_verify
//! [`fetch_class`]: ChunkStore::fetch_class

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use super::ChunkStore;
use crate::cache::{self, BOOT_LIST, BootList, read_boot_list, write_boot_list};
use crate::qos;

/// Background prefetch never claims more than this share of
/// [`MAX_CONCURRENT_FETCHES`](super::MAX_CONCURRENT_FETCHES), so a full
/// download always leaves slots free for the reads the game is actually blocked
/// on. Without the reserve a sweep of 16000 chunks would sit in front of every
/// demand read and stall rendering.
///
/// Two thirds of the pool, because during the boot warm-up the guesses *are*
/// the demand: the list being replayed is the very set of chunks the client
/// is about to read, so a demand read for a listed chunk coalesces onto the
/// prefetch already carrying it rather than queueing behind it. Sixteen slots
/// still exist that prefetch can never occupy, for the reads no list
/// predicted.
pub(super) const MAX_PREFETCH_FETCHES: usize = 32;

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
/// Sized with [`MAX_CONCURRENT_FETCHES`](super::MAX_CONCURRENT_FETCHES): a
/// window smaller than the pool would idle the slots the pool was widened to
/// fill.
const READAHEAD_CHUNKS: usize = 48;

/// Threads that read the cache back and check it.
///
/// Fewer than [`MAX_PREFETCH_FETCHES`], and not rationed by permits, because
/// this pass never touches the network: it reads the volume and hashes what it
/// finds. Past the point where the disk is saturated more threads only buy
/// context switches, and eight keeps every performance core fed on the
/// machines this ships to.
const VERIFY_WORKERS: usize = 8;

/// The window of chunks readahead workers are allowed to fetch.
///
/// `next` is where the workers have got to and `limit` is one past the last
/// index worth fetching; both move with the client's reads. Behind one mutex
/// rather than two atomics because a seek has to move them together — a worker
/// that saw the new `next` against the old `limit` would fetch into whatever
/// the cursor had just left.
#[derive(Default)]
pub(super) struct Readahead {
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
pub(super) struct Prefetch {
    done: AtomicU64,
    total: AtomicU64,
    running: AtomicBool,
    stop: AtomicBool,
    /// The integrity pass. Its own counters because it runs *before* a sweep
    /// rather than as part of one, and the launcher draws both.
    verify_done: AtomicU64,
    verify_total: AtomicU64,
    verify_repaired: AtomicU64,
    verifying: AtomicBool,
}

impl ChunkStore {
    /// Add `index` to the boot list, if it is still open.
    pub(super) fn note(&self, index: usize) {
        if self.recording.load(Ordering::Relaxed) {
            self.touched.lock().unwrap().insert(index);
        }
    }

    /// Move the readahead window past the chunk a demand read just asked for.
    ///
    /// Called on every read, cached or not: what makes the next read cheap is
    /// knowing where this one was, and a run of cache hits is the clearest
    /// possible signal that the client is walking forwards.
    pub(super) fn advance_readahead(&self, index: usize) {
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

    /// The page is loading from the top again.
    ///
    /// The mirror of the latch [`seal_boot_list`](Self::seal_boot_list) sets.
    /// Whatever was on screen is gone — a ⌘R, or a client that failed and
    /// asked to start over — so a download still running is once again the
    /// thing the player is waiting on rather than a top-up behind a frame.
    /// Without this the class latched at the first frame outlived the game it
    /// was set for, and the reload that exists for a client which has already
    /// gone wrong would fetch from the bottom of the queue.
    pub fn back_to_waiting(&self) {
        self.playing.store(false, Ordering::Relaxed);
    }

    /// Fetch a little way ahead of wherever the client is reading.
    ///
    /// Idle until a read moves the window, so it costs a parked thread and
    /// nothing else on a warm cache. Workers take `prefetch_permits` for the
    /// same reason [`warm_boot`](Self::warm_boot) does: a guess about what
    /// comes next must never sit in front of a read the game is blocked on.
    /// Being wrong costs bandwidth, never latency.
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
        self.start_full_download_with_workers(MAX_PREFETCH_FETCHES)
    }

    /// Start a full download with an explicit worker bound.
    ///
    /// Command-line image and repair operations use this to honour `--jobs`.
    /// The shared prefetch semaphore remains the hard ceiling, so a requested
    /// value cannot consume capacity reserved for demand reads.
    pub fn start_full_download_with_workers(self: &Arc<Self>, workers: usize) -> bool {
        if self.prefetch.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.prefetch.stop.store(false, Ordering::Relaxed);
        self.prefetch.done.store(0, Ordering::Relaxed);
        self.prefetch
            .total
            .store(self.chunk_count() as u64, Ordering::Relaxed);

        let workers = workers.clamp(1, MAX_PREFETCH_FETCHES);
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

    /// `(checked, total, running, discarded)` for the current or last check.
    pub fn verify_progress(&self) -> (u64, u64, bool, u64) {
        (
            self.prefetch.verify_done.load(Ordering::Relaxed),
            self.prefetch.verify_total.load(Ordering::Relaxed),
            self.prefetch.verifying.load(Ordering::Relaxed),
            self.prefetch.verify_repaired.load(Ordering::Relaxed),
        )
    }

    /// Read every cached chunk back and prove it still hashes right.
    ///
    /// A one-shot beside the sweep rather than a part of it, because the sweep
    /// deliberately trusts a `stat` (see [`ensure`](Self::ensure)) — that is
    /// what makes resuming a 4 GB download cost seconds instead of minutes, and
    /// folding a re-hash into it would undo the decision on every resume. This
    /// runs where the cost is worth paying: once, before a launch that told the
    /// player the network was done with. A filename proves residency; only
    /// this proves the bytes.
    ///
    /// Repair is [`read_cached`](Self::read_cached)'s doing, not this one's. It
    /// already unlinks a chunk that fails, which drops it back out of residency
    /// — so the sweep that follows a non-zero return refetches exactly what was
    /// discarded, and the pass needs no repair path of its own.
    ///
    /// Returns `false` if a check is already running.
    pub fn start_verify(self: &Arc<Self>) -> bool {
        if self.prefetch.verifying.swap(true, Ordering::SeqCst) {
            return false;
        }

        // Deduplicated up front, on this thread, so a chunk that appears at
        // several indices is hashed once rather than once per occurrence.
        //
        // On the snapshot as it ships today this finds nothing: all 16,167
        // hashes are distinct, which is the same fact that denies the download
        // a deduplication discount. It is a `HashSet` over a list already in
        // memory, so keeping it costs nothing measurable, and the alternative
        // is a pass that silently does the repetition factor's worth of extra
        // work the first time a snapshot does repeat.
        let mut seen = HashSet::new();
        let mut work = Vec::new();
        for index in 0..self.chunk_count() {
            let Ok(hash) = self.chunk_hash(index) else {
                continue;
            };
            if seen.insert(hash) {
                work.push(index);
            }
        }
        drop(seen);

        self.prefetch.stop.store(false, Ordering::Relaxed);
        self.prefetch.verify_done.store(0, Ordering::Relaxed);
        self.prefetch.verify_repaired.store(0, Ordering::Relaxed);
        self.prefetch
            .verify_total
            .store(work.len() as u64, Ordering::Relaxed);

        note!("[gwnative] checking {} distinct chunks", work.len());
        let work = Arc::new(work);
        let outstanding = Arc::new(AtomicU64::new(VERIFY_WORKERS as u64));
        for worker in 0..VERIFY_WORKERS {
            let store = Arc::clone(self);
            let work = Arc::clone(&work);
            let outstanding = Arc::clone(&outstanding);
            thread::spawn(move || {
                let mut class = qos::Following::start(store.fetch_class());
                let mut at = worker;
                while at < work.len() {
                    // Shares the sweep's flag, which is what "Switch to Quick
                    // Start" wants: whichever of the two is running, the button
                    // that abandons the full image stops it.
                    if store.prefetch.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    class.now(store.fetch_class());
                    let index = work[at];
                    if let Ok(hash) = store.chunk_hash(index)
                        && let Ok(expected) = store.chunk_length(index)
                        // A chunk that was never fetched is not a failure — it
                        // is the sweep's work, and counting it as damage would
                        // report an interrupted download as a corrupt one.
                        && store.cached_len(&hash) == Some(expected)
                        && store.read_cached(&hash, expected).is_none()
                    {
                        store
                            .prefetch
                            .verify_repaired
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    store.prefetch.verify_done.fetch_add(1, Ordering::Relaxed);
                    at += VERIFY_WORKERS;
                }
                if outstanding.fetch_sub(1, Ordering::SeqCst) == 1 {
                    store.prefetch.verifying.store(false, Ordering::SeqCst);
                    let (done, total, _, repaired) = store.verify_progress();
                    note!("[gwnative] check finished: {done}/{total} chunks, {repaired} discarded");
                }
            });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunks::fixture::{store_of, store_of_content};
    use crate::scratch::TempDir;

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
        store.back_to_waiting();
        assert!(
            matches!(store.fetch_class(), qos::Class::UserInitiated),
            "a reload puts the player back in front of a launch"
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

    /// A store over `chunks`, with those same chunks already in its cache —
    /// the state a resumed full install is in before anything looks at it.
    fn planted(name: &str, chunks: &[&[u8]]) -> (TempDir, Arc<ChunkStore>) {
        let temp = TempDir::new(name);
        let store = Arc::new(store_of_content(temp.0.join("chunks"), chunks));
        for (index, bytes) in chunks.iter().enumerate() {
            let path = store.cache_path(&store.chunk_hash(index).unwrap());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
        }
        (temp, store)
    }

    /// Wait for the pass to report itself finished.
    ///
    /// It runs on eight threads that outlive the call that spawned them, so
    /// there is nothing to join; the flag they clear is the same one the page
    /// polls, which makes waiting on it the thing the tests should assert
    /// against anyway.
    fn settled(store: &ChunkStore) -> (u64, u64, bool, u64) {
        for _ in 0..2000 {
            let progress = store.verify_progress();
            if !progress.2 {
                return progress;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the check never finished");
    }

    #[test]
    fn the_check_leaves_a_cache_that_still_hashes_right_alone() {
        let (_temp, store) = planted("verify-clean", &[&[b'a'; 1024], &[b'b'; 1024]]);

        assert!(store.start_verify());
        let (done, total, _, discarded) = settled(&store);
        assert_eq!((done, total, discarded), (2, 2, 0));
        assert_eq!(store.resident_count(), 2, "nothing was thrown away");
    }

    #[test]
    fn the_check_throws_away_a_chunk_whose_bytes_have_changed() {
        let (_temp, store) = planted("verify-rot", &[&[b'a'; 1024], &[b'b'; 1024]]);
        // Same length, different bytes. This is exactly what a `stat` cannot
        // see and the whole reason the pass exists: to the sweep, and to
        // `resident_count`, this file is a complete chunk.
        let rotten = store.cache_path(&store.chunk_hash(1).unwrap());
        fs::write(&rotten, [b'x'; 1024]).unwrap();
        assert_eq!(store.resident_count(), 2, "residency cannot tell yet");

        assert!(store.start_verify());
        let (done, total, _, discarded) = settled(&store);
        assert_eq!((done, total, discarded), (2, 2, 1));
        assert!(!rotten.exists(), "the bad copy is gone");
        assert_eq!(
            store.resident_count(),
            1,
            "which is what puts it back in front of the sweep"
        );
    }

    #[test]
    fn a_chunk_that_was_never_fetched_is_not_damage() {
        let (_temp, store) = planted("verify-partial", &[&[b'a'; 1024], &[b'b'; 1024]]);
        // Half a download, which is the ordinary state of an interrupted one.
        fs::remove_file(store.cache_path(&store.chunk_hash(1).unwrap())).unwrap();

        assert!(store.start_verify());
        let (done, total, _, discarded) = settled(&store);
        assert_eq!((done, total), (2, 2), "every distinct chunk is looked at");
        assert_eq!(discarded, 0, "an unfinished download is not a corrupt one");
    }

    #[test]
    fn a_chunk_the_snapshot_repeats_is_checked_once() {
        // Four indices over two distinct chunks. Today's real snapshot repeats
        // nothing, so this is the case the deduplication is kept *for* rather
        // than one it is currently earning its place on — which is exactly why
        // it needs a test: nothing in production would notice it breaking.
        let (_temp, store) = planted(
            "verify-dedup",
            &[&[b'a'; 1024], &[b'b'; 1024], &[b'a'; 1024], &[b'b'; 1024]],
        );

        assert_eq!(store.chunk_count(), 4);
        assert!(store.start_verify());
        let (done, total, _, discarded) = settled(&store);
        assert_eq!((done, total, discarded), (2, 2, 0), "two, not four");
    }

    #[test]
    fn a_second_check_does_not_start_on_top_of_the_first() {
        let (_temp, store) = planted("verify-once", &[&[b'a'; 1024]]);

        // The flag is set by hand rather than by racing a real pass: what is
        // being asserted is what the latch does, not how fast eight threads
        // get through one chunk, and a test that depends on losing that race
        // is a test that passes on a slow machine and flakes on a fast one.
        store.prefetch.verifying.store(true, Ordering::SeqCst);
        assert!(!store.start_verify(), "one is already running");

        store.prefetch.verifying.store(false, Ordering::SeqCst);
        assert!(store.start_verify(), "and it clears for the next launch");
        settled(&store);
    }
}
