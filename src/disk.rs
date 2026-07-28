//! How much room is left, as macOS counts it.
//!
//! Deliberately not `statvfs`. On APFS a large share of a volume is purgeable —
//! local Time Machine snapshots, caches the system will evict when something
//! important needs the space — and `f_bavail` counts none of it, so a Mac with
//! 40 GB genuinely available can report 3. Asking a user to free space they
//! already have is worse than not asking. `NSURLVolumeAvailableCapacityForImportantUsageKey`
//! is the number the system itself uses to answer "can I write this file",
//! which is exactly the question here.

use std::path::Path;

use objc2_foundation::{
    NSArray, NSNumber, NSString, NSURL, NSURLVolumeAvailableCapacityForImportantUsageKey,
};

/// Bytes that could be written under `path` if it mattered, or `None` when the
/// volume declines to say.
pub fn available(path: &Path) -> Option<u64> {
    // The path itself need not exist yet — the cache directory is created
    // lazily — but its volume has to be findable, so ask about the nearest
    // ancestor that is really there.
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }

    // SAFETY: `probe` exists, so it names a real file URL, and the key is
    // Foundation's own constant. Every value that comes back is checked before
    // it is used — including the downcast, which is what makes a volume that
    // answers with something other than a number a `None` rather than a crash.
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&probe.to_string_lossy()));
        let key = NSURLVolumeAvailableCapacityForImportantUsageKey;
        let values = url
            .resourceValuesForKeys_error(&NSArray::from_slice(&[key]))
            .ok()?;
        let number = values.objectForKey(key)?;
        let number = number.downcast_ref::<NSNumber>()?;
        u64::try_from(number.longLongValue()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_volume_has_a_number_and_it_is_not_a_silly_one() {
        let free = available(&std::env::temp_dir()).expect("a volume should answer");
        assert!(free > 1 << 20, "{free} bytes is not a plausible free space");

        // A path that does not exist yet still answers, via its nearest real
        // ancestor — the chunk cache asks before it has been created, and the
        // volume is the same either way.
        let unborn = std::env::temp_dir().join("gwnative-no-such-dir/and/deeper");
        assert!(available(&unborn).is_some());

        // Nothing to walk up to, so nothing to report.
        assert_eq!(available(Path::new("")), None);
    }
}
