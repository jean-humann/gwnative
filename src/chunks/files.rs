//! What the store does with the cache directory.
//!
//! [`cache`](crate::cache) owns the rules — where a chunk lives, what it is
//! called, which files in there are debris. This is the store applying them:
//! the full read that checks a chunk's hash the first time a session wants it,
//! the `pread` window every read after that, the descriptor cache that makes
//! the window cheap, the write that renames a fetched chunk into place, and the
//! directory scans that answer "how much of the game is already paid for".

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;

use super::ChunkStore;
use crate::cache::{self, open_sized, read_window};
use crate::error::Result;
use crate::manifest::ContentHash;

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
pub(super) struct HandleCache {
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

impl ChunkStore {
    pub(super) fn cache_path(&self, hash: &ContentHash) -> PathBuf {
        // Two-level fan-out: 16k files in one directory is fine on APFS, but
        // this keeps directory listings usable when debugging by hand.
        let hex = hash.hex();
        self.cache_dir.join(&hex[..2]).join(hex.as_str())
    }

    /// The size of the cached file for `hash`, if there is one.
    pub(super) fn cached_len(&self, hash: &ContentHash) -> Option<u64> {
        fs::metadata(self.cache_path(hash)).ok().map(|m| m.len())
    }

    /// The whole cached chunk, if it is there and its bytes still hash right.
    ///
    /// The hash is checked the first time this session touches a chunk and
    /// remembered afterwards, which is what lets
    /// [`window_into`](ChunkStore::window_into) pread. A file that fails is
    /// unlinked, so the caller refetches it rather than handing corrupt bytes to
    /// the client and leaving the bad copy behind to fail again next launch.
    pub(super) fn read_cached(&self, hash: &ContentHash, expected: u64) -> Option<Vec<u8>> {
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
    pub(super) fn read_cached_window(
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

    pub(super) fn write_cached(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
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
            push_to_device(&file);
            drop(file);
            fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written?;
        Ok(())
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
    /// Distinct from [`prefetch_progress`](ChunkStore::prefetch_progress), which
    /// counts what the *current* sweep has fetched and so resets to zero each
    /// time one starts. The launcher needs the other question — how much of the
    /// game is already paid for — which only the cache itself can answer, and
    /// which survives restarts.
    pub fn resident_count(&self) -> usize {
        let present = self.resident_names();
        self.manifest.files[&self.snapshot]
            .chunk_hashes
            .iter()
            .filter(|hash| present.contains(hash.hex().as_str()))
            .count()
    }
}

/// Hand a freshly written chunk's bytes to the device, and do not wait for the
/// device to empty its own write cache.
///
/// Every route to durability in std — `sync_all`, `sync_data` — is
/// `F_FULLFSYNC` on macOS, which is a barrier the whole drive queues behind.
/// Measured on this machine, writing 256 KiB chunks one after another: 6.74 ms
/// each with `sync_all` plus a directory sync, 0.41 ms with this, 0.36 ms with
/// no flush at all. That is 37 MiB/s against 608, and it was the full
/// download's ceiling — the CDN path serves 47 MiB/s and the store was reaching
/// 38, because the barrier serialises at the device no matter how many fetch
/// threads are queued behind it.
///
/// What the stronger barrier buys is a chunk surviving a power cut, and this
/// store is the wrong place to pay for that. Chunks are content-addressed and
/// re-downloadable: `read_cached` hashes each one the first time a session
/// touches it, unlinks it if it fails, and refetches, so a chunk lost or rotted
/// by a power cut costs one 256 KiB request and nothing else. A process that
/// merely crashes loses nothing either way — the bytes are the kernel's from
/// `write_all` onwards, and only the hardware losing power can take them back.
///
/// The rename is left unflushed for the same reason. Losing it strands blocks
/// under a name nobody looks up, which `sweep_orphans` collects and the next
/// read refetches.
fn push_to_device(file: &fs::File) {
    // SAFETY: `fsync` reads a descriptor and returns an int. The borrow keeps
    // the file open across the call, so the descriptor cannot be closed or
    // reused underneath it. A failure leaves the bytes in the page cache, which
    // is where they would have been without the call.
    unsafe { libc::fsync(file.as_raw_fd()) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunks::fixture::store_of_five;
    use crate::scratch::TempDir;

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
