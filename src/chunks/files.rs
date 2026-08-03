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
use std::os::unix::fs::MetadataExt;
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

    /// Whether the exact regular cache entry exists at its manifest size.
    ///
    /// A wrong-size or non-regular entry is corrupt cache state, not absence:
    /// remove it before a fetch, and surface a removal failure so no later scan
    /// can count the unusable name as resident.
    pub(super) fn cached_sized(&self, hash: &ContentHash, expected: u64) -> Result<bool> {
        if self.invalid.lock().unwrap().contains(hash) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("cached chunk {hash} is unusable and could not be removed"),
            )
            .into());
        }
        let path = self.cache_path(hash);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(self.mark_invalid(hash, error)),
        };
        if metadata.file_type().is_file() && metadata.len() == expected {
            return Ok(true);
        }
        self.discard_cached(hash, &path)?;
        Ok(false)
    }

    pub(super) fn cache_entry_exists(&self, hash: &ContentHash) -> bool {
        fs::symlink_metadata(self.cache_path(hash)).is_ok()
    }

    /// The whole cached chunk, if it is there and its bytes still hash right.
    ///
    /// The hash is checked the first time this session touches a chunk and
    /// remembered afterwards, which is what lets
    /// [`window_into`](ChunkStore::window_into) pread. A file that fails is
    /// unlinked, so the caller refetches it rather than handing corrupt bytes to
    /// the client and leaving the bad copy behind to fail again next launch.
    pub(super) fn read_cached(&self, hash: &ContentHash, expected: u64) -> Result<Option<Vec<u8>>> {
        let path = self.cache_path(hash);
        if !self.cached_sized(hash, expected)? {
            return Ok(None);
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                // An exact-size name whose bytes cannot be read is not
                // resident. Remove it if possible; if not, the invalid latch
                // keeps every later completion scan from rediscovering it.
                self.discard_cached(hash, &path)?;
                return Ok(None);
            }
        };
        if bytes.len() as u64 != expected {
            self.discard_cached(hash, &path)?;
            return Ok(None);
        }
        if self.verified.lock().unwrap().contains(hash) {
            return Ok(Some(bytes));
        }
        if let Err(e) = crate::patch::verify(&bytes, hash) {
            note!("[gwnative] cached chunk is corrupt, refetching: {e}");
            self.discard_cached(hash, &path)?;
            return Ok(None);
        }
        self.verified.lock().unwrap().insert(*hash);
        Ok(Some(bytes))
    }

    /// Make a bad cache entry impossible to rediscover by name.
    fn discard_cached(&self, hash: &ContentHash, path: &std::path::Path) -> Result<()> {
        self.handles.lock().unwrap().forget(hash);
        self.verified.lock().unwrap().remove(hash);
        self.invalid.lock().unwrap().insert(*hash);
        match cache::remove_regular(path) {
            Ok(()) => {
                self.invalid.lock().unwrap().remove(hash);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.invalid.lock().unwrap().remove(hash);
                Ok(())
            }
            Err(error) => {
                let error = std::io::Error::new(
                    error.kind(),
                    format!(
                        "could not remove unusable cached chunk {}: {error}",
                        path.display()
                    ),
                );
                *self.last_failure.lock().unwrap() = Some(error.to_string());
                Err(error.into())
            }
        }
    }

    fn mark_invalid(&self, hash: &ContentHash, error: std::io::Error) -> crate::error::Error {
        self.handles.lock().unwrap().forget(hash);
        self.verified.lock().unwrap().remove(hash);
        self.invalid.lock().unwrap().insert(*hash);
        let error = std::io::Error::new(
            error.kind(),
            format!("cached chunk {hash} is unreadable: {error}"),
        );
        *self.last_failure.lock().unwrap() = Some(error.to_string());
        error.into()
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
        // Bound so the lock is gone before `open_sized` runs. Opening a file
        // is a syscall, and every other window read is waiting on this mutex.
        let cached = self.handles.lock().unwrap().get(hash);
        if let Some(file) = cached {
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
        self.handles.lock().unwrap().forget(hash);
        self.invalid.lock().unwrap().remove(hash);
        Ok(())
    }

    /// Every chunk file name the cache holds, across the buckets this snapshot
    /// draws from.
    ///
    /// One directory listing per fan-out bucket rather than one `stat` per
    /// chunk: 256 syscalls instead of 16167, over the same directory blocks.
    /// The buckets are gathered as the leading byte, so only the ones this
    /// snapshot could occupy are ever listed.
    fn resident_hashes(&self) -> HashSet<ContentHash> {
        let hashes = &self.manifest.files[&self.snapshot].chunk_hashes;
        let buckets: HashSet<u8> = hashes.iter().map(|h| h.bytes()[0]).collect();
        let mut expected = HashMap::new();
        for (index, &hash) in hashes.iter().enumerate() {
            if let Ok(length) = self.chunk_length(index) {
                expected
                    .entry(hash.hex().as_str().to_owned())
                    .or_insert((hash, length));
            }
        }
        let mut present = HashSet::new();
        for bucket in buckets {
            let bucket_path = self.cache_dir.join(format!("{bucket:02x}"));
            let Ok(before) = fs::symlink_metadata(&bucket_path) else {
                continue;
            };
            if !before.file_type().is_dir() || before.file_type().is_symlink() {
                continue;
            }
            let Ok(entries) = fs::read_dir(&bucket_path) else {
                continue;
            };
            let mut candidates = Vec::new();
            for entry in entries.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                let Some(&(hash, length)) = expected.get(name.as_str()) else {
                    continue;
                };
                let exact = entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_file() && !kind.is_symlink())
                    .and_then(|_| entry.metadata().ok())
                    .is_some_and(|metadata| metadata.len() == length);
                candidates.push((hash, exact));
            }
            // Do not use names observed through a bucket that changed identity
            // while it was listed. This also prevents an external replacement
            // from being counted as cache residency.
            let Ok(after) = fs::symlink_metadata(&bucket_path) else {
                continue;
            };
            if !after.file_type().is_dir()
                || after.file_type().is_symlink()
                || (before.dev(), before.ino()) != (after.dev(), after.ino())
            {
                continue;
            }
            for (hash, exact) in candidates {
                if exact && !self.invalid.lock().unwrap().contains(&hash) {
                    present.insert(hash);
                }
            }
        }
        present
    }

    /// Bitmap of which snapshot chunks are already on disk, LSB first. The
    /// harness seeds `image.isCached` from this so a restart does not re-prefetch
    /// what a previous session already paid for.
    pub fn resident_bitmap(&self) -> Vec<u8> {
        let hashes = &self.manifest.files[&self.snapshot].chunk_hashes;
        let present = self.resident_hashes();
        let mut bits = vec![0u8; hashes.len().div_ceil(8)];
        for (i, hash) in hashes.iter().enumerate() {
            // A `.tmp` left by a write in flight is in the listing too, and does
            // not match a bare hash, which is the answer wanted anyway.
            if present.contains(hash) {
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
        self.residency().0
    }

    /// Exact resident index count and missing distinct bytes from one scan.
    /// Polling code needs both and must not pay two metadata walks per tick.
    pub fn residency(&self) -> (usize, u64) {
        let present = self.resident_hashes();
        let count = self.manifest.files[&self.snapshot]
            .chunk_hashes
            .iter()
            .filter(|hash| present.contains(hash))
            .count();
        let mut seen = HashSet::new();
        let missing = (0..self.chunk_count())
            .filter_map(|index| {
                let hash = self.chunk_hash(index).ok()?;
                (seen.insert(hash) && !present.contains(&hash))
                    .then(|| self.chunk_length(index).ok())
                    .flatten()
            })
            .sum();
        (count, missing)
    }

    /// Exact bytes still missing from this content-addressed snapshot.
    pub fn missing_bytes(&self) -> u64 {
        self.residency().1
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
    fn a_wrong_size_chunk_is_corrupt_not_resident() {
        let temp = TempDir::new("residency-wrong-size");
        let cache = temp.0.join("chunks");
        let store = store_of_five(cache);
        let hash = store.manifest.files["Gw.snapshot"].chunk_hashes[0];
        let path = store.cache_path(&hash);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"short").unwrap();

        assert_eq!(store.resident_count(), 0);
        assert_eq!(store.resident_bitmap(), vec![0]);
        assert!(
            path.exists(),
            "a read-only residency poll must leave repair to demand reads or verification"
        );
    }

    #[test]
    fn residency_polling_cannot_delete_another_store_replacement() {
        let temp = TempDir::new("residency-shared-replacement");
        let cache = temp.0.join("chunks");
        let polling = store_of_five(cache.clone());
        let writing = store_of_five(cache);
        let hash = polling.manifest.files["Gw.snapshot"].chunk_hashes[0];
        let path = polling.cache_path(&hash);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"stale").unwrap();

        assert_eq!(polling.resident_count(), 0);
        assert!(
            path.exists(),
            "polling must not pathname-delete a candidate another store can replace"
        );

        writing.write_cached(&hash, &[0u8; 1024]).unwrap();
        assert_eq!(polling.resident_count(), 1);
        assert_eq!(polling.resident_bitmap(), vec![1]);
    }

    #[test]
    fn residency_never_follows_or_cleans_a_bucket_symlink() {
        let temp = TempDir::new("residency-bucket-symlink");
        let cache = temp.0.join("chunks");
        let outside = temp.0.join("outside");
        let store = store_of_five(cache.clone());
        let hash = store.manifest.files["Gw.snapshot"].chunk_hashes[0];
        let external = outside.join(hash.hex().as_str());
        fs::create_dir_all(&outside).unwrap();
        fs::write(&external, vec![0u8; 1024]).unwrap();
        std::os::unix::fs::symlink(&outside, cache.join(&hash.hex().as_str()[..2])).unwrap();

        assert_eq!(store.resident_count(), 0);
        assert!(
            external.exists(),
            "residency cleanup must remain inside cache buckets"
        );
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
