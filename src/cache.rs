//! What lives in the cache directory, and the rules it lives by.
//!
//! The store next door decides which chunks are wanted and when. This decides
//! what a chunk looks like on disk: its name, its length, where the directory
//! sits, which files in it are debris and which are the boot list. Those are
//! two different kinds of question, and the second one is the one that has to
//! stay right across upgrades — a rule changed here is a cache the next build
//! reads as empty, or worse, as full of files it will happily serve.
//!
//! Everything is written so that being interrupted leaves a state the next
//! launch can make sense of: temporary names that a sweep can recognise as
//! debris, a boot list that reads as absent unless it is entirely intact, and
//! a migration that refuses rather than merges.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

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
pub fn open_sized(path: &Path, expected: u64) -> Option<fs::File> {
    let file = fs::File::open(path).ok()?;
    (file.metadata().ok()?.len() == expected).then_some(file)
}

/// `take` bytes at `within` of an already-open chunk, in one `pread`.
pub fn read_window(file: &fs::File, within: usize, take: usize) -> Option<Vec<u8>> {
    let mut window = vec![0u8; take];
    file.read_exact_at(&mut window, within as u64).ok()?;
    Some(window)
}

/// Filename of the boot list, inside the cache directory.
pub const BOOT_LIST: &str = "boot-chunks.json";

/// The chunks one session read on its way to a first frame.
pub struct BootList {
    pub chunk_size: u64,
    pub chunks: Vec<usize>,
}

pub fn write_boot_list(path: &Path, list: &BootList) -> std::io::Result<()> {
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
pub fn read_boot_list(path: &Path) -> Option<BootList> {
    parse_boot_list(&fs::read(path).ok()?)
}

/// The boot list an install starts with, before it has recorded its own.
///
/// The chunks that gate a first frame are a property of the game build, not of
/// the player — every blank install walks the same startup file set — so the
/// one boot no recorded list can ever cover, the very first, does not have to
/// go in cold. This is a list recorded from a real cold boot of the current
/// build. When ArenaNet patches, the indices drift and the warm-up fetches
/// some chunks the boot no longer needs — which is the standing contract for
/// every boot list: being wrong costs bandwidth, never latency, and the first
/// frame of that session records a fresh list that replaces this one.
pub fn built_in_boot_list() -> Option<BootList> {
    parse_boot_list(include_bytes!("boot-chunks.json"))
}

fn parse_boot_list(bytes: &[u8]) -> Option<BootList> {
    let raw: serde_json::Value = serde_json::from_slice(bytes).ok()?;
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
pub fn sweep_orphans(cache_dir: &Path) {
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
        note!("[gwnative] cleared {removed} abandoned chunk writes");
    }
}

/// Drop every cached chunk no retained profile manifest can name.
///
/// The cache is content-addressed, which is what makes deduplication free and
/// what makes this necessary: when ArenaNet patches, the chunks whose contents
/// changed get new hashes and the old files are never asked for again. Nothing
/// overwrites them, because nothing writes to those names any more. Before this
/// the cache was a union of every snapshot the machine had ever seen — a second
/// 4.2 GB after the first patch, and another after the next.
///
/// Safe against a fetch happening right now, because the set to keep comes from
/// manifests rather than from a listing: a chunk being written this instant is
/// one a retained manifest named, so it is in `live` whether or not it is yet
/// on disk. Anything that is not a chunk file — the boot list at the top level,
/// a `.tmp` a live writer still owns — fails the name test and is left alone.
///
/// Runs at Utility QoS behind the orphan sweep, so it yields to the boot it is
/// sharing a disk with.
pub fn prune(cache_dir: &Path, live: &HashSet<String>) {
    // A manifest with no chunks in it is a manifest that failed to parse into
    // anything useful, and treating it as authority would empty the cache.
    if live.is_empty() {
        return;
    }
    let Ok(buckets) = fs::read_dir(cache_dir) else {
        return;
    };
    let (mut removed, mut bytes) = (0usize, 0u64);
    for bucket in buckets.flatten() {
        let name = bucket.file_name();
        if !name.to_str().is_some_and(is_bucket_name) {
            continue;
        }
        let Ok(kind) = bucket.file_type() else {
            continue;
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        // Anchor deletion to the directory we checked. If the path is swapped
        // after this open, `unlinkat` below still acts only inside this exact
        // non-symlink directory rather than following the replacement.
        let Ok(directory) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(bucket.path())
        else {
            continue;
        };
        let Ok(entries) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Only ever a name this cache could have written itself: the hex
            // form of a hash, and nothing else in the directory.
            if !is_chunk_name(name) || live.contains(name) {
                continue;
            }
            let Some(size) = unlink_regular_at(&directory, &entry.file_name()) else {
                continue;
            };
            removed += 1;
            bytes += size;
        }
    }
    if removed > 0 {
        note!(
            "[gwnative] dropped {removed} chunks ({:.2} GB) no cached profile uses",
            bytes as f64 / 1e9
        );
    }
}

/// A fan-out bucket is exactly the first byte of a chunk hash.
fn is_bucket_name(name: &str) -> bool {
    name.len() == 2
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Remove one regular file relative to an already-open bucket.
///
/// `fstatat` refuses symlinks without following them; `unlinkat` then removes
/// the same name through the directory descriptor. Returning `None` for every
/// mismatch or failure makes pruning conservative.
fn unlink_regular_at(directory: &fs::File, name: &std::ffi::OsStr) -> Option<u64> {
    let name = std::ffi::CString::new(name.as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `directory` remains open, `name` is NUL-terminated, and `stat`
    // points to writable storage for the duration of the call.
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return None;
    }
    // SAFETY: a successful `fstatat` initialized the structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return None;
    }
    // SAFETY: both descriptor and name are still live, and flags 0 mean a file
    // rather than a directory. The descriptor keeps this inside the bucket.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return None;
    }
    u64::try_from(stat.st_size).ok()
}

/// Remove one regular cache entry without following its bucket if that bucket
/// has been replaced by a symlink.
pub fn remove_regular(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache entry has no bucket"))?;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("cache entry has no name"))?;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::other("cache entry name contains NUL"))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the directory and C string remain live and `stat` is writable.
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful `fstatat` initialized the structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "cache entry is not a regular file",
        ));
    }
    // SAFETY: descriptor and C string remain valid; flags 0 refuse directories.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Whether `name` is one this cache writes: lowercase hex, and as long as one
/// of the digests [`ContentHash`] produces.
fn is_chunk_name(name: &str) -> bool {
    matches!(name.len(), 40 | 64)
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
/// The old location is migrated by [`prepare`] after instance exclusion and
/// while the cache maintenance lock is exclusive. Merely computing this path
/// must not move files underneath an already-running process.
pub fn default_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    Path::new(&home).join("Library/Application Support/gwnative/chunks")
}

fn legacy_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    Path::new(&home).join("Library/Caches/gwnative/chunks")
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
        note!("[chunks] could not prepare {}: {e}", parent.display());
        return;
    }
    match std::fs::rename(legacy, current) {
        Ok(()) => {
            note!(
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
            note!("[chunks] could not move the existing cache ({e}); leaving it in place")
        }
    }
}

/// The file that says "delete the game data before opening it".
///
/// A sentinel rather than a deletion, and that is the whole design. By the time
/// a player can ask for this, the store has a readahead thread, a prefetch
/// thread and up to 48 fetches in flight, all of them holding open descriptors
/// into the directory about to be removed — so deleting it underneath them ends
/// in a launch that half-refetches what it half-deleted. Asking the *next*
/// launch to do it, before anything is opened, costs a restart and is correct by
/// construction.
///
/// It lives beside the directory rather than inside it, because inside it would
/// be deleted by the very sweep it asks for.
fn clear_marker(cache_dir: &Path) -> PathBuf {
    sidecar_path(cache_dir, ".clear")
}

fn lock_path(cache_dir: &Path) -> PathBuf {
    sidecar_path(cache_dir, ".lock")
}

fn sidecar_path(cache_dir: &Path, suffix: &str) -> PathBuf {
    let mut name = cache_dir.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    cache_dir.with_file_name(name)
}

const SHARED: u8 = 0;
const EXCLUSIVE_READY: u8 = 1;
const EXCLUSIVE_ACTIVE: u8 = 2;

struct LeaseInner {
    lock: crate::instance::Instance,
    state: AtomicU8,
}

/// A process's claim on the shared chunk cache and manifest-activation boundary.
///
/// Every profile takes this before it can promote a manifest. Shared leases let
/// profiles play together; an exclusive lease is offered to the first process
/// so it can clear or maintain the cache before admitting peers. Clones hold
/// the same kernel lock, including across background cache work.
#[derive(Clone)]
pub struct Lease(Arc<LeaseInner>);

/// Exclusive ownership temporarily handed to cache maintenance.
///
/// Dropping this guard downgrades to the shared lease retained by the store, so
/// a panic cannot leave peer profiles blocked for the rest of the process.
pub struct Maintenance {
    lease: Lease,
    shared: bool,
}

impl Lease {
    /// Claim the one maintenance opportunity attached to an exclusive launch.
    /// A profile that joined an existing shared user receives `None` and must
    /// defer destructive work.
    pub fn maintenance(&self) -> Option<Maintenance> {
        self.0
            .state
            .compare_exchange(
                EXCLUSIVE_READY,
                EXCLUSIVE_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
            .then(|| Maintenance {
                lease: self.clone(),
                shared: false,
            })
    }
}

impl Maintenance {
    /// Admit peer profiles after exclusive work is complete.
    pub fn finish(mut self) -> std::io::Result<()> {
        self.share()
    }

    fn share(&mut self) -> std::io::Result<()> {
        self.lease
            .0
            .lock
            .downgrade_in_place()
            .map_err(std::io::Error::other)?;
        self.lease.0.state.store(SHARED, Ordering::Release);
        self.shared = true;
        Ok(())
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        if !self.shared
            && let Err(reason) = self.share()
        {
            note!("[chunks] could not admit another profile after cache maintenance: {reason}");
        }
    }
}

/// Lock the cache before any active manifest can change.
///
/// With no peer, the caller keeps an exclusive maintenance opportunity. With
/// a live peer, it joins as a shared reader and must skip destructive work. A
/// pending clear is consumed only under exclusivity, never under another
/// profile's open descriptors or writes.
pub fn prepare(cache_dir: &Path) -> std::io::Result<Lease> {
    let current = default_cache_dir();
    let legacy = (cache_dir == current).then(legacy_cache_dir);
    prepare_with_legacy(cache_dir, legacy.as_deref())
}

fn prepare_with_legacy(cache_dir: &Path, legacy: Option<&Path>) -> std::io::Result<Lease> {
    let lock = lock_path(cache_dir);
    match crate::instance::acquire(&lock, Duration::ZERO) {
        Ok(exclusive) => {
            // `Layout::new` is deliberately path-only. This is the first point
            // after profile instance exclusion where destructive shared-cache
            // work is safe, and the exclusive lock keeps new profile-aware
            // processes out until the rename and clear are complete.
            if let Some(legacy) = legacy {
                migrate_cache(legacy, cache_dir);
            }
            if clear_marker(cache_dir).exists() {
                take_clear_request(cache_dir);
            }
            Ok(Lease(Arc::new(LeaseInner {
                lock: exclusive,
                state: AtomicU8::new(EXCLUSIVE_READY),
            })))
        }
        Err(_) => {
            if clear_marker(cache_dir).exists() {
                note!(
                    "[chunks] cache clear deferred while another profile is using {}",
                    cache_dir.display()
                );
            }
            crate::instance::acquire_shared(&lock, Duration::from_secs(30))
                .map(|shared| {
                    Lease(Arc::new(LeaseInner {
                        lock: shared,
                        state: AtomicU8::new(SHARED),
                    }))
                })
                .map_err(std::io::Error::other)
        }
    }
}

/// Finish the lone launch's safe catalog/cache maintenance before network or
/// client installation work begins, then admit peers for ordinary launch.
pub fn finish_maintenance(
    lease: &Lease,
    cache_dir: &Path,
    protected_chunks: &HashSet<String>,
) -> std::io::Result<bool> {
    let Some(maintenance) = lease.maintenance() else {
        return Ok(false);
    };
    let completed = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                crate::qos::set(crate::qos::Class::Utility);
                sweep_orphans(cache_dir);
                prune(cache_dir, protected_chunks);
            })
            .join()
    });
    if completed.is_err() {
        return Err(std::io::Error::other("cache maintenance panicked"));
    }
    maintenance.finish()?;
    Ok(true)
}

/// Ask the next launch to start from an empty cache.
///
/// The caller relaunches; nothing here does. Failing to write the marker is
/// reported and not fatal — what follows is a restart that keeps the game data,
/// which is the state the player was already in.
pub fn request_clear(cache_dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = clear_marker(cache_dir).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(clear_marker(cache_dir), b"")
}

/// Whether a clear was asked for, and take the request either way.
///
/// The marker is removed first and the directory second, so a launch that dies
/// mid-delete comes back to a partly-emptied cache rather than to a request it
/// will honour forever. A partly-emptied cache is a state the store already
/// handles — every chunk is content-addressed and re-fetched when absent — and
/// an unclearable request is not.
///
/// Called before anything opens the directory. Nothing else may call it: it
/// consumes the request.
pub fn take_clear_request(cache_dir: &Path) -> bool {
    let marker = clear_marker(cache_dir);
    if !marker.exists() {
        return false;
    }
    if let Err(e) = fs::remove_file(&marker) {
        // The request cannot be consumed, so honouring it would mean clearing
        // the cache at this launch and at every launch after it.
        note!("[chunks] the clear request could not be taken ({e}); leaving the cache alone");
        return false;
    }
    match fs::remove_dir_all(cache_dir) {
        Ok(()) => note!("[chunks] cleared {} as asked", cache_dir.display()),
        // Including "it was not there", which is the same outcome asked for.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => note!("[chunks] could not clear {}: {e}", cache_dir.display()),
    }
    true
}

/// A name for a chunk that is still being written.
///
/// The `.tmp` suffix is the contract with [`sweep_orphans`]: it is the whole of
/// how an interrupted write is told apart from a finished chunk, which is why
/// this is here rather than at the call site. The pid and the counter keep two
/// writers — two threads, or two instances sharing a cache — from choosing the
/// same path for different bytes.
pub fn temp_path(parent: &Path, hex: &str) -> PathBuf {
    parent.join(format!(
        "{hex}.{}.{:08x}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;

    /// The one thing the sentinel must do, and the one thing it must not: clear
    /// the cache once, and never twice.
    #[test]
    fn a_clear_is_asked_for_once_and_honoured_once() {
        let temp = TempDir::new("clear");
        let cache = temp.0.join("chunks");
        let bucket = cache.join("11");
        fs::create_dir_all(&bucket).unwrap();
        fs::write(bucket.join("11".to_owned() + &"a".repeat(62)), b"a chunk").unwrap();

        // Nothing asked, so nothing happens — which is every ordinary launch.
        assert!(!take_clear_request(&cache));
        assert!(cache.exists(), "an unasked launch must keep the game data");

        request_clear(&cache).unwrap();
        // Beside the directory, not inside it: inside, the deletion below would
        // take the request with it and the next launch would clear again.
        assert!(!cache.join(".clear").exists());
        assert!(cache.exists(), "arming is not deleting");

        assert!(take_clear_request(&cache));
        assert!(!cache.exists());

        // The launch after the one that cleared. A request that survived it
        // would mean a profile that can never keep game data again.
        assert!(!take_clear_request(&cache));
    }

    /// A clear asked for and then interrupted before the store existed leaves a
    /// directory that is not there. Re-asking has to be an ordinary success:
    /// "already gone" is the outcome that was wanted.
    #[test]
    fn clearing_a_cache_that_is_not_there_is_not_a_failure() {
        let temp = TempDir::new("clear-missing");
        let cache = temp.0.join("never-opened");
        request_clear(&cache).unwrap();
        assert!(take_clear_request(&cache));
        assert!(!cache.exists());
    }

    #[test]
    fn a_clear_waits_until_every_profile_releases_the_shared_cache() {
        let temp = TempDir::new("clear-shared");
        let cache = temp.0.join("chunks");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("kept-while-live"), b"data").unwrap();

        let first = prepare(&cache).expect("first profile opens the cache");
        first
            .maintenance()
            .expect("the first profile starts exclusive")
            .finish()
            .unwrap();
        request_clear(&cache).unwrap();
        let second = prepare(&cache).expect("a second profile may share the cache");

        assert!(cache.exists(), "an active profile keeps the cache intact");
        assert!(clear_marker(&cache).exists(), "the request remains pending");
        assert!(
            second.maintenance().is_none(),
            "a shared profile must not receive destructive ownership"
        );

        drop(first);
        drop(second);
        let after = prepare(&cache).expect("the next lone profile performs the clear");
        assert!(!cache.exists());
        assert!(!clear_marker(&cache).exists());
        drop(after);
    }

    #[test]
    fn dotted_cache_names_get_distinct_sidecars() {
        let temp = TempDir::new("dotted-sidecars");
        let first = temp.0.join("cache.one");
        let second = temp.0.join("cache.two");

        assert_ne!(clear_marker(&first), clear_marker(&second));
        assert_ne!(lock_path(&first), lock_path(&second));
        assert_eq!(clear_marker(&first).file_name().unwrap(), "cache.one.clear");
        assert_eq!(lock_path(&second).file_name().unwrap(), "cache.two.lock");

        request_clear(&first).unwrap();
        assert!(clear_marker(&first).exists());
        assert!(!clear_marker(&second).exists());
    }

    #[test]
    fn a_peer_joins_while_the_first_profile_continues_ordinary_launch_work() {
        let temp = TempDir::new("shared-after-maintenance");
        let cache = temp.0.join("chunks");
        let first = prepare(&cache).expect("the first profile enters exclusively");

        assert!(finish_maintenance(&first, &cache, &HashSet::new()).unwrap());
        let peer = prepare(&cache).expect("non-destructive launch work is shared");
        assert!(
            peer.maintenance().is_none(),
            "the first profile still lives, so the peer must be shared"
        );

        drop(peer);
        drop(first);
    }

    #[test]
    fn a_prune_keeps_every_chunk_named_by_the_profile_union() {
        let temp = TempDir::new("prune");
        let cache = temp.0.join("chunks");

        let kept = "11".to_owned() + &"a".repeat(62);
        let other_profile = "22".to_owned() + &"b".repeat(62);
        let stale = "33".to_owned() + &"c".repeat(62);
        let short_kept = "44".to_owned() + &"d".repeat(38);
        for name in [&kept, &other_profile, &stale, &short_kept] {
            let bucket = cache.join(&name[..2]);
            fs::create_dir_all(&bucket).unwrap();
            fs::write(bucket.join(name), vec![0u8; 1000]).unwrap();
        }

        let bucket = cache.join("33");
        fs::write(bucket.join("in-flight.9999.tmp"), b"half a chunk").unwrap();
        fs::write(bucket.join("notes.txt"), b"by hand").unwrap();
        fs::write(cache.join(BOOT_LIST), b"[1,2,3]").unwrap();

        let live: HashSet<String> =
            [kept.clone(), other_profile.clone(), short_kept.clone()].into();
        prune(&cache, &live);

        assert!(cache.join(&kept[..2]).join(&kept).exists());
        assert!(
            cache
                .join(&other_profile[..2])
                .join(&other_profile)
                .exists(),
            "a second profile's retained chunk must survive"
        );
        assert!(cache.join(&short_kept[..2]).join(&short_kept).exists());
        assert!(!cache.join(&stale[..2]).join(&stale).exists());
        assert!(bucket.join("in-flight.9999.tmp").exists());
        assert!(bucket.join("notes.txt").exists());
        assert!(cache.join(BOOT_LIST).exists());

        prune(&cache, &HashSet::new());
        assert!(cache.join(&kept[..2]).join(&kept).exists());
    }

    #[test]
    fn a_prune_never_follows_a_bucket_symlink() {
        let temp = TempDir::new("prune-bucket-symlink");
        let cache = temp.0.join("chunks");
        let outside = temp.0.join("outside");
        let stale = "aa".to_owned() + &"b".repeat(62);
        fs::create_dir_all(&cache).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join(&stale), b"belongs outside the cache").unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("aa")).unwrap();

        prune(&cache, &HashSet::from(["cc".to_owned() + &"d".repeat(62)]));

        assert_eq!(
            fs::read(outside.join(stale)).unwrap(),
            b"belongs outside the cache",
            "a directory symlink must never turn pruning into an external delete"
        );
    }

    #[test]
    fn a_live_profile_defers_pruning_across_manifest_and_chunk_writes() {
        let temp = TempDir::new("concurrent-catalog");
        let cache = temp.0.join("chunks");
        let catalog = temp.0.join("profiles/other/manifest.cache");
        let new_chunk = "66".to_owned() + &"e".repeat(62);
        let new_path = cache.join(&new_chunk[..2]).join(&new_chunk);
        fs::create_dir_all(new_path.parent().unwrap()).unwrap();
        fs::create_dir_all(catalog.parent().unwrap()).unwrap();

        let live = crate::instance::acquire_shared(&lock_path(&cache), Duration::ZERO)
            .expect("the existing profile holds the catalog/cache lease");
        let newcomer = prepare(&cache).expect("a second profile joins as shared");
        let ready = Arc::new(std::sync::Barrier::new(2));

        std::thread::scope(|scope| {
            let ready_writer = Arc::clone(&ready);
            let new_path = new_path.clone();
            let catalog = catalog.clone();
            scope.spawn(move || {
                let _live = live;
                fs::write(&catalog, format!("manifest names {new_chunk}")).unwrap();
                fs::write(&new_path, b"new profile chunk").unwrap();
                ready_writer.wait();
                ready_writer.wait();
            });

            ready.wait();
            assert!(
                !finish_maintenance(&newcomer, &cache, &HashSet::from(["old".into()])).unwrap(),
                "a shared launch must defer pruning instead of using a stale union"
            );
            ready.wait();
        });

        assert!(catalog.exists());
        assert!(
            new_path.exists(),
            "the concurrent profile's chunk must survive"
        );
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
    fn migration_runs_inside_the_exclusive_maintenance_lease() {
        let temp = TempDir::new("migrate-locked");
        let legacy = temp.0.join("Caches/gwnative/chunks");
        let current = temp.0.join("Application Support/gwnative/chunks");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("abc"), b"a cached chunk").unwrap();

        let lease = prepare_with_legacy(&current, Some(&legacy)).unwrap();

        assert_eq!(fs::read(current.join("abc")).unwrap(), b"a cached chunk");
        assert!(!legacy.exists());
        assert!(
            crate::instance::acquire(&lock_path(&current), Duration::ZERO).is_err(),
            "migration returns with the exclusive maintenance lease still held"
        );
        drop(lease);
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
