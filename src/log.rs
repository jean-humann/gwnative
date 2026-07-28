//! Saying things without betting the process on being heard.
//!
//! `println!` and `eprintln!` treat a failed write as unrecoverable: they
//! panic, and in a release build — where a panic aborts — the app dies. What
//! makes that a real bug rather than a theoretical one is who owns the far end
//! of the pipe. A launcher, a benchmark harness, a shell the player closed:
//! when any of them goes away, the next line anyone writes takes the whole app
//! down. It happened here, mid-download, from a worker thread answering a
//! loopback request: `SIGABRT`, a crash report, and a game that had been
//! running fine for three minutes.
//!
//! Nothing this program says about itself is worth that. [`note!`] writes the
//! line to stderr and discards whatever the write says back — a listener that
//! has stopped listening is not an error condition, it is Tuesday.
//!
//! Stderr, specifically, and not stdout: the one thing on stdout is the
//! headless handshake line in `main`, which a harness parses. Diagnostics
//! sharing that channel would be read as a handshake that never arrived.

/// Print a line to stderr, or don't. Same arguments as [`eprintln!`].
///
/// Available unqualified everywhere in the crate — `main` declares this module
/// with `#[macro_use]` ahead of the rest for exactly that reason.
macro_rules! note {
    ($($arg:tt)*) => {{
        // Locked once for the whole line: two threads writing at the same
        // moment must not interleave halves of their messages.
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}
