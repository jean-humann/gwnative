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
const MAX_MATCH_WORK: usize = 128 * 1024 * 1024;
const MAX_CANONICAL_VARIANTS: usize = 32;

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

fn contains_spelling(haystack: &[u8], value: &[u8]) -> bool {
    if value.is_empty() {
        return false;
    }
    // Linear-time substring search. Inputs can be an 8 MiB proxy body and a
    // 4 KiB credential; repeated-prefix data must not become quadratic work.
    let mut prefix = vec![0usize; value.len()];
    for index in 1..value.len() {
        let mut matched = prefix[index - 1];
        while matched != 0 && value[index] != value[matched] {
            matched = prefix[matched - 1];
        }
        if value[index] == value[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    let mut matched = 0usize;
    for byte in haystack {
        while matched != 0 && *byte != value[matched] {
            matched = prefix[matched - 1];
        }
        if *byte == value[matched] {
            matched += 1;
        }
        if matched == value.len() {
            return true;
        }
    }
    false
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn json_unescape_once(input: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len());
    let mut at = 0;
    let mut changed = false;
    while at < input.len() {
        if input[at] != b'\\' || at + 1 >= input.len() {
            output.push(input[at]);
            at += 1;
            continue;
        }
        let simple = match input[at + 1] {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'/' => Some(b'/'),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            _ => None,
        };
        if let Some(decoded) = simple {
            output.push(decoded);
            at += 2;
            changed = true;
            continue;
        }
        let unit = (input.get(at + 1) == Some(&b'u'))
            .then(|| json_unit(input.get(at + 2..at + 6)?))
            .flatten();
        let Some(unit) = unit else {
            output.push(input[at]);
            at += 1;
            continue;
        };
        let (scalar, width) = if (0xd800..=0xdbff).contains(&unit)
            && input.get(at + 6..at + 8) == Some(&b"\\u"[..])
            && let Some(low) = input.get(at + 8..at + 12).and_then(json_unit)
            && (0xdc00..=0xdfff).contains(&low)
        {
            (
                0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00),
                12,
            )
        } else if (0xd800..=0xdfff).contains(&unit) {
            output.extend_from_slice(&input[at..at + 6]);
            at += 6;
            continue;
        } else {
            (u32::from(unit), 6)
        };
        let Some(character) = char::from_u32(scalar) else {
            output.extend_from_slice(&input[at..at + width]);
            at += width;
            continue;
        };
        let mut encoded = [0u8; 4];
        output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        wipe(&mut encoded);
        at += width;
        changed = true;
    }
    if changed {
        Some(output)
    } else {
        wipe(&mut output);
        None
    }
}

fn json_unit(bytes: &[u8]) -> Option<u16> {
    if bytes.len() != 4 {
        return None;
    }
    bytes.iter().try_fold(0u16, |value, byte| {
        Some((value << 4) | u16::from(hex(*byte)?))
    })
}

fn percent_unescape_once(input: &[u8], form: bool) -> Option<Vec<u8>> {
    if !input.contains(&b'%') && (!form || !input.contains(&b'+')) {
        return None;
    }
    let mut output = Vec::with_capacity(input.len());
    let mut at = 0;
    let mut changed = false;
    while at < input.len() {
        if input[at] == b'%'
            && let (Some(high), Some(low)) = (
                input.get(at + 1).and_then(|byte| hex(*byte)),
                input.get(at + 2).and_then(|byte| hex(*byte)),
            )
        {
            output.push((high << 4) | low);
            at += 3;
            changed = true;
        } else if form && input[at] == b'+' {
            output.push(b' ');
            at += 1;
            changed = true;
        } else {
            output.push(input[at]);
            at += 1;
        }
    }
    if changed {
        Some(output)
    } else {
        wipe(&mut output);
        None
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
    let mut work = 0usize;
    let contains = |candidate: &[u8], work: &mut usize| {
        *work = work.saturating_add(candidate.len().saturating_mul(values.len()));
        *work > MAX_MATCH_WORK
            || values
                .iter()
                .any(|value| contains_spelling(candidate, &value.0))
    };
    if contains(bytes, &mut work) {
        return true;
    }
    let mut variants = [
        json_unescape_once(bytes),
        percent_unescape_once(bytes, false),
        percent_unescape_once(bytes, true),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let mut examined = 1usize;
    while let Some(mut candidate) = variants.pop() {
        examined += 1;
        if examined > MAX_CANONICAL_VARIANTS || contains(&candidate, &mut work) {
            wipe(&mut candidate);
            variants.iter_mut().for_each(|remaining| wipe(remaining));
            return true;
        }
        variants.extend(
            [
                json_unescape_once(&candidate),
                percent_unescape_once(&candidate, false),
                percent_unescape_once(&candidate, true),
            ]
            .into_iter()
            .flatten(),
        );
        wipe(&mut candidate);
    }
    false
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

    #[test]
    fn encoded_active_values_are_detected() {
        super::remember("player+one@example.com");
        assert!(super::contains_secret(b"name=player%2Bone%40example.com"));
        assert!(super::contains_secret(
            br#"{"name":"player+on\\u0065@example.com"}"#
        ));
        assert!(super::contains_secret(
            b"name=%2570layer%2Bone%40example.com"
        ));
        super::remember("player-🚀");
        assert!(super::contains_secret(br#"player-\uD83D\uDE80"#));
    }
}
