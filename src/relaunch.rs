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

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

/// Set on the successor, and read by it to know that it is one.
const MARKER: &str = "GWNATIVE_RELAUNCHING";

/// Inherited only by the one successor created for a renderer-process crash.
/// A second automatic recovery must stop visibly instead of starting an
/// unbounded chain of fresh native processes.
const RENDERER_RECOVERY: &str = "GWNATIVE_RENDERER_RECOVERY";

/// Inherited only by a successor started because the official client asked to
/// reload itself.
///
/// A page reload cannot replace the client files underneath the running host,
/// and the ordinary launch-time manifest check is deliberately asynchronous.
/// The successor carrying this marker therefore fetches the current manifest
/// before it opens another window instead of racing that background check.
const CLIENT_REFRESH: &str = "GWNATIVE_CLIENT_REFRESH";

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

/// Whether this successor must synchronously check for a current client.
pub fn client_refresh_requested() -> bool {
    std::env::var_os(CLIENT_REFRESH).is_some()
}

/// Start another copy of this app, and report whether it got off the ground.
///
/// Non-credential arguments are carried over so that a relaunched `serve` run
/// still serves rather than opening a window. The environment is inherited for
/// the same reason: every
/// switch this app reads from it — the port, the web root, the trace flags —
/// is part of what this launch *is*, and a successor that quietly dropped them
/// would be a different app wearing the same window.
pub fn start() -> Result<(), String> {
    start_with_options(false, false)
}

/// Start a fresh process that updates the official client before reopening it.
///
/// This is the native meaning of a reload requested from inside the game. A
/// same-document reload would reuse the one-shot launch identity and could not
/// activate a manifest published after this process started.
pub fn refresh_client() -> Result<(), String> {
    start_with_options(false, true)
}

/// Start the single automatic successor allowed after WebKit's content process
/// disappears. Ordinary user/settings/runtime relaunches clear this marker;
/// only a repeated renderer crash consumes the same recovery budget.
pub fn recover_renderer() -> Result<(), String> {
    if std::env::var_os(RENDERER_RECOVERY).is_some() {
        return Err("the automatic renderer recovery was already used".into());
    }
    start_with_options(true, false)
}

fn start_with_options(renderer_recovery: bool, refresh_client: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("this app has no path on disk: {e}"))?;
    let mut command = Command::new(&exe);
    command
        .args(relaunch_args(std::env::args_os().skip(1)))
        .env(MARKER, "1")
        // The original anonymous pipe belongs only to this process. Even if
        // the parent still has a same-numbered descriptor open, the successor
        // must not publish a second launch's capabilities into it.
        .env_remove("GWNATIVE_CONTROL_FD");
    if renderer_recovery {
        command.env(RENDERER_RECOVERY, "1");
    } else {
        command.env_remove(RENDERER_RECOVERY);
    }
    if refresh_client {
        command.env(CLIENT_REFRESH, "1");
    } else {
        // A refresh is one launch's work, not a mode inherited by every later
        // user, settings, runtime, or renderer restart.
        command.env_remove(CLIENT_REFRESH);
    }
    command
        .spawn()
        .map(|child| note!("[relaunch] started pid {}", child.id()))
        .map_err(|e| format!("{} could not be started: {e}", exe.display()))
}

/// Carry launch behavior forward without copying invocation credentials into a
/// successor process. A replacement must load the just-written Keychain value,
/// and a clear must stay clear; replaying the original argv would do neither.
fn relaunch_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut kept = Vec::new();
    let mut omit_value = false;
    for argument in args {
        if omit_value {
            wipe_argument(argument);
            omit_value = false;
            continue;
        }
        let bytes = argument.as_os_str().as_bytes();
        if matches!(bytes, b"-email" | b"-password") {
            omit_value = true;
            wipe_argument(argument);
        } else if bytes.starts_with(b"-email=") || bytes.starts_with(b"-password=") {
            wipe_argument(argument);
        } else {
            kept.push(argument);
        }
    }
    kept
}

#[cfg(unix)]
fn wipe_argument(argument: std::ffi::OsString) {
    let mut bytes = argument.into_vec();
    crate::log::wipe(&mut bytes);
}

#[cfg(not(unix))]
fn wipe_argument(argument: std::ffi::OsString) {
    drop(argument);
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn successors_never_inherit_invocation_credentials() {
        let args = [
            "serve",
            "-email",
            "old@example.test",
            "-password=old-password",
            "--profile",
            "benchmark",
            "-password",
            "second-password",
            "--offline",
        ]
        .map(OsString::from);
        assert_eq!(
            relaunch_args(args),
            ["serve", "--profile", "benchmark", "--offline"]
                .map(OsString::from)
                .to_vec()
        );
    }
}
