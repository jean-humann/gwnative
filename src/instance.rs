//! One app per support directory.
//!
//! Two copies running at once do not merely waste memory. They share a web
//! root, so one can be rewriting `Gw.jspi.wasm` while the other reads it; they
//! share the generation record, the settings file and the boot list, each of
//! which is written whole and would end up holding whichever process finished
//! last; and they share a 4.2 GB snapshot cache. The chunk cache alone survives
//! the collision, because it is content-addressed and written by rename — which
//! is the only reason this was not noticed sooner.
//!
//! An advisory `flock` is the right shape for it: the kernel releases it when
//! the process dies, however it dies, so a crashed instance cannot leave a
//! stale lock behind the way a pid file would.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::path::Path;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

/// How often a patient acquire looks again.
///
/// Short enough that a relaunch is not visibly slower than a launch, long
/// enough that ten seconds of waiting is a few hundred syscalls rather than a
/// spin. Always non-blocking: `flock` can block until the lock is free, but
/// then a predecessor that hangs takes the successor with it and neither can
/// report why.
const POLL: Duration = Duration::from_millis(20);

/// A held lock. Dropping it — or exiting, or crashing — releases it.
pub struct Instance(#[allow(dead_code)] File);

/// Take the lock, or report who already has it.
///
/// `patience` is how long to keep trying. Zero for a launch, where a lock that
/// is held means the app is already open and the answer is to raise that one.
/// A relaunch is the case where it is worth waiting: see [`crate::relaunch`]
/// for why the successor overlaps its predecessor at all.
///
/// The pid in the message comes from the file's contents rather than the lock
/// itself, so it is a hint and not a fact: a lock held by a process that never
/// managed to write is reported as unknown rather than as nobody.
pub fn acquire(path: &Path, patience: Duration) -> Result<Instance, String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;

    let deadline = Instant::now() + patience;
    // SAFETY: `file` owns the descriptor for the whole call.
    while unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        if Instant::now() >= deadline {
            let holder = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok());
            return Err(match holder {
                Some(pid) => format!("another gwnative is already running (pid {pid})"),
                None => "another gwnative is already running".to_owned(),
            });
        }
        std::thread::sleep(POLL);
    }

    // Written after the lock is held, so what is in the file is always the pid
    // of whoever holds it. Failing to write costs the next instance a nicer
    // message and nothing else.
    let _ = file.set_len(0);
    let _ = write!(file, "{}", std::process::id());
    let _ = file.flush();
    Ok(Instance(file))
}

/// The pid recorded in a lock file, if there is one.
pub fn holder(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_one_is_told_who_has_it() {
        let dir = std::env::temp_dir().join(format!(
            "gwnative-instance-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("gwnative.lock");

        let first = acquire(&path, Duration::ZERO).expect("the first instance takes the lock");
        assert_eq!(holder(&path), Some(std::process::id() as i32));

        // Same process, second descriptor: flock is per open file description,
        // so this is the same refusal a second app would get.
        let Err(message) = acquire(&path, Duration::ZERO) else {
            panic!("a second acquire must not succeed while the first is held");
        };
        assert!(
            message.contains(&std::process::id().to_string()),
            "should name the holder: {message}"
        );

        // And the lock is the process's to give back, not a file to clean up.
        drop(first);
        let second = acquire(&path, Duration::ZERO).expect("released on drop");
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_patient_acquire_waits_for_the_holder_to_let_go() {
        let dir = std::env::temp_dir().join(format!(
            "gwnative-patience-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("gwnative.lock");

        let held = acquire(&path, Duration::ZERO).expect("the first instance takes the lock");
        // Impatience is what a launch does, and it is refused immediately.
        assert!(acquire(&path, Duration::ZERO).is_err());

        let releasing = path.clone();
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            drop(held);
            drop(releasing);
        });

        // Patience is what a relaunch does, and it gets the lock the moment the
        // predecessor is finished with it rather than being told it is taken.
        let waited = std::time::Instant::now();
        let successor = acquire(&path, Duration::from_secs(5)).expect("waited it out");
        assert!(
            waited.elapsed() >= Duration::from_millis(100),
            "it should have had to wait for the holder"
        );

        letting_go.join().unwrap();
        drop(successor);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
