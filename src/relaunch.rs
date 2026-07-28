//! Quitting and coming back.
//!
//! Two settings — the render scale and the gesture translation — are read once
//! on the way up and cannot change under a running client, so the panel has
//! always saved them and then said "at the next launch". This is that launch,
//! offered rather than left to the player to arrange out of the Dock.
//!
//! The successor has to be started before this process is gone, because the
//! moment it is gone there is nobody left to start it. That leaves the two of
//! them briefly alive at once, on a machine whose whole single-instance rule
//! says they must not be — so the successor is told to wait, and waits on the
//! lock itself rather than on a pid. Waiting on the lock is what makes it
//! correct rather than merely usually right: whatever else happens, the second
//! app runs only once the first has let go, even if some third launch is racing
//! for the same lock.
//!
//! Nothing here quits. Starting the successor can fail — a binary that has been
//! deleted or replaced under the running process, a system refusing to fork —
//! and an app that quits on the strength of a launch that never happened has
//! taken the game away and given nothing back. The caller quits only once this
//! has answered.

use std::process::Command;
use std::time::Duration;

/// Set on the successor, and read by it to know that it is one.
const MARKER: &str = "GWNATIVE_RELAUNCHING";

/// How long a successor waits for its predecessor to let go of the lock.
///
/// Generous on purpose. The predecessor still has to flush the client's files,
/// which is allowed to take three seconds of its own, and the cost of being
/// wrong here is an app that refuses to come back.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// Whether this process was started by [`start`].
///
/// Read in two places, both of which are waiting for the predecessor to finish
/// letting go of something: the instance lock, and the loopback port.
pub fn is_successor() -> bool {
    std::env::var_os(MARKER).is_some()
}

/// Start another copy of this app, and report whether it got off the ground.
///
/// The arguments are carried over so that a relaunched `--headless` run is
/// still headless. The environment is inherited for the same reason: every
/// switch this app reads from it — the port, the web root, the trace flags —
/// is part of what this launch *is*, and a successor that quietly dropped them
/// would be a different app wearing the same window.
pub fn start() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("this app has no path on disk: {e}"))?;
    Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .env(MARKER, "1")
        .spawn()
        .map(|child| note!("[relaunch] started pid {}", child.id()))
        .map_err(|e| format!("{} could not be started: {e}", exe.display()))
}
