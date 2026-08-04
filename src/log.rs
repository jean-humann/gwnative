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

use std::sync::{Mutex, OnceLock};

const REDACTED: &str = "<redacted>";

struct Secret(Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

fn secrets() -> &'static Mutex<Vec<Secret>> {
    static VALUES: OnceLock<Mutex<Vec<Secret>>> = OnceLock::new();
    VALUES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Add a value that must not cross a diagnostic boundary this session.
pub fn remember(value: &str) {
    if value.is_empty() {
        return;
    }
    let mut values = secrets().lock().unwrap_or_else(|e| e.into_inner());
    if !values.iter().any(|known| known.0 == value.as_bytes()) {
        values.push(Secret(value.as_bytes().to_vec()));
    }
}

/// Replace exact active values, including their JSON-escaped spelling.
pub fn redact(text: &str) -> String {
    let values = secrets().lock().unwrap_or_else(|e| e.into_inner());
    let mut out = text.to_owned();
    for value in values.iter() {
        let Ok(raw) = std::str::from_utf8(&value.0) else {
            continue;
        };
        out = out.replace(raw, REDACTED);
        if let Ok(json) = serde_json::to_string(raw) {
            out = out.replace(&json[1..json.len() - 1], REDACTED);
        }
    }
    out
}

pub fn contains_secret(bytes: &[u8]) -> bool {
    let values = secrets().lock().unwrap_or_else(|e| e.into_inner());
    values.iter().any(|value| {
        !value.0.is_empty() && bytes.windows(value.0.len()).any(|part| part == value.0)
    })
}

/// Overwrite a native secret allocation through volatile stores.
pub fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Print a line to stderr, or don't. Same arguments as [`eprintln!`].
///
/// Available unqualified everywhere in the crate — `main` declares this module
/// with `#[macro_use]` ahead of the rest for exactly that reason.
macro_rules! note {
    ($($arg:tt)*) => {{
        // Locked once for the whole line: two threads writing at the same
        // moment must not interleave halves of their messages.
        use std::io::Write as _;
        let line = crate::log::redact(&format!($($arg)*));
        let _ = writeln!(std::io::stderr().lock(), "{line}");
    }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn active_values_are_removed_from_plain_and_json_diagnostics() {
        super::remember("canary-password-321");
        assert_eq!(
            super::redact(r#"error canary-password-321 "canary-password-321""#),
            "error <redacted> \"<redacted>\""
        );
        assert!(super::contains_secret(b"prefix canary-password-321 suffix"));
    }

    #[test]
    fn wiping_covers_the_entire_allocation() {
        let mut bytes = b"synthetic secret".to_vec();
        super::wipe(&mut bytes);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
}
