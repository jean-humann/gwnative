//! One file a player can attach to a bug report.
//!
//! The diagnostics log already holds everything worth knowing, and that is the
//! problem: it is a JSONL file of several thousand machine-written records with
//! no header, no environment and no indication of which end is recent. Asking
//! somebody to attach it is asking them to attach a wall of numbers about a Mac
//! nobody can identify.
//!
//! So this is not an export in that sense. It is a cover sheet: what machine,
//! what build, what settings, and the tail of the log underneath it, redacted
//! and written beside the original so the raw file is still one click away.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::diagnostics::{self, Recorder};
use crate::settings;

/// How much of the log to carry.
///
/// A stutter reported now is described by the last few minutes, not by the
/// download that happened an hour ago, and a report nobody opens because it is
/// 5 MiB helps nobody. At the resting rate this is about six minutes of samples,
/// and rather less when a burst or a chatty boot is in it — which is the right
/// bias, because those are the records somebody asked for.
const TAIL_RECORDS: usize = 400;

/// Where a redacted value went.
const REDACTED: &str = "<redacted>";

/// Write a report next to the diagnostics log and return where it went.
pub fn write(recorder: &Recorder, profile: &settings::Settings) -> std::io::Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = recorder
        .dir()
        .join(format!("problem-report-{}.txt", stamp(now, Format::File)));
    fs::write(&path, compose(recorder.dir(), profile, now))?;
    Ok(path)
}

/// Build the text. Separated from writing it so the shape can be tested without
/// a filesystem being the subject of the test.
fn compose(dir: &Path, profile: &settings::Settings, now: u64) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Guild Wars problem report");
    let _ = writeln!(out, "Written {}", stamp(now, Format::Readable));
    let _ = writeln!(out);

    let _ = writeln!(out, "This machine");
    describe(&mut out, &diagnostics::environment());
    let _ = writeln!(out);

    let _ = writeln!(out, "Settings");
    describe(
        &mut out,
        &serde_json::to_value(profile).unwrap_or(serde_json::Value::Null),
    );
    let _ = writeln!(out);

    let log = dir.join("gwnative.jsonl");
    let _ = writeln!(
        out,
        "Diagnostics — the last {TAIL_RECORDS} records of {}",
        log.display()
    );
    let _ = writeln!(
        out,
        "Anything shaped like an email address has been replaced with \
         {REDACTED}. Read this file before attaching it anywhere public."
    );
    let _ = writeln!(out);
    match fs::read_to_string(&log) {
        Ok(body) => {
            let lines: Vec<&str> = body.lines().filter(|line| !line.is_empty()).collect();
            let tail = lines.len().saturating_sub(TAIL_RECORDS);
            if tail > 0 {
                let _ = writeln!(out, "  ({tail} earlier records not included)");
            }
            for line in &lines[tail..] {
                // Active-value redaction covers credentials and capabilities;
                // the shape-based pass remains useful for an older log whose
                // account is no longer active in this process.
                let _ = writeln!(out, "{}", redact(&crate::log::redact(line)));
            }
        }
        // Said in the report rather than refused: everything above it is still
        // worth sending, and "there was no log" is itself a finding.
        Err(e) => {
            let _ = writeln!(out, "  The log could not be read: {e}");
        }
    }
    out
}

/// Lay a flat JSON object out as aligned `name  value` lines.
fn describe(out: &mut String, value: &serde_json::Value) {
    let Some(object) = value.as_object() else {
        let _ = writeln!(out, "  {value}");
        return;
    };
    let width = object.keys().map(String::len).max().unwrap_or(0);
    for (name, value) in object {
        // Strings unquoted; everything else as JSON writes it, so `null` reads
        // as null and 2.0 does not become "2".
        let shown = match value.as_str() {
            Some(text) => text.to_owned(),
            None => value.to_string(),
        };
        let _ = writeln!(out, "  {name:<width$}  {shown}");
    }
}

/// Replace anything shaped like an email address.
///
/// The log carries every line the client printed, and the client is the half of
/// this application that is handed an account name. Nothing observed proves it
/// prints one — but the file this appears in exists to be attached to a public
/// issue, and the cost of being wrong is one player's login in a bug tracker
/// forever. A cheap shape match against the one identifier this application ever
/// sees is the right trade.
///
/// Deliberately not a general secret scrubber. The password never reaches the
/// page — it goes from the keychain into the client over a route that does not
/// log — and pretending to catch things this does not catch would be worse than
/// saying plainly what it does.
fn redact(line: &str) -> String {
    if !line.contains('@') {
        return line.to_owned();
    }
    let atom = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '@' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        // A local part before it and a dotted domain after it, or it is just an
        // at sign in a sentence and stays one.
        let start = (0..index)
            .rev()
            .take_while(|&i| atom(chars[i]))
            .last()
            .unwrap_or(index);
        let mut end = index + 1;
        while end < chars.len() && (atom(chars[end]) || chars[end] == '.') {
            end += 1;
        }
        let domain: String = chars[index + 1..end].iter().collect();
        if start == index || !domain.contains('.') || domain.ends_with('.') {
            out.push('@');
            index += 1;
            continue;
        }
        // The local part is already in `out`; take it back off.
        let local: usize = chars[start..index].iter().map(|c| c.len_utf8()).sum();
        out.truncate(out.len() - local);
        out.push_str(REDACTED);
        index = end;
    }
    out
}

enum Format {
    /// For a person reading the top of the report.
    Readable,
    /// For a filename. No colons: the Finder shows one as a slash.
    File,
}

/// UTC, because a report is read by somebody in a different place than it was
/// written and an unlabelled local time is worse than a labelled foreign one.
fn stamp(seconds: u64, format: Format) -> String {
    let time = seconds as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `gmtime_r` reads the time through the first pointer and fills the
    // caller's `tm` through the second; both are live locals here.
    if unsafe { libc::gmtime_r(&time, &mut tm) }.is_null() {
        return seconds.to_string();
    }
    let (year, month, day) = (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday);
    let (hour, minute, second) = (tm.tm_hour, tm.tm_min, tm.tm_sec);
    match format {
        Format::Readable => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
        }
        Format::File => format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;

    #[test]
    fn an_account_name_does_not_reach_the_report() {
        assert_eq!(
            redact("[warn] login failed for player.one+gw@example.co.uk after 3 tries"),
            format!("[warn] login failed for {REDACTED} after 3 tries")
        );
        assert_eq!(
            redact(r#"{"kind":"page","line":"account: a@b.com"}"#),
            format!(r#"{{"kind":"page","line":"account: {REDACTED}"}}"#)
        );
    }

    #[test]
    fn an_active_non_email_secret_does_not_reach_an_export() {
        let dir = TempDir::new("report-active-secret");
        let _registration = crate::log::register(&["report-canary-password"]).unwrap();
        fs::write(
            dir.0.join("gwnative.jsonl"),
            r#"{"kind":"page","line":"report-canary-password"}"#,
        )
        .unwrap();

        let text = compose(&dir.0, &settings::Settings::default(), 0);
        assert!(!text.contains("report-canary-password"), "{text}");
        assert!(text.contains(REDACTED), "{text}");
    }

    #[test]
    fn an_at_sign_that_is_not_an_address_is_left_alone() {
        // The uncaught-error handler writes this shape on every page error, and
        // a redactor that ate the file and line number would cost more than it
        // saved.
        let line = "[uncaught] boom @ http://127.0.0.1:53019/harness.js:412";
        assert_eq!(redact(line), line);
        for line in ["@ start", "trailing@", "no dots@localhost", "a@b."] {
            assert_eq!(redact(line), line, "{line}");
        }
    }

    #[test]
    fn the_report_leads_with_the_machine_and_ends_with_the_log() {
        let dir = TempDir::new("report-shape");
        fs::write(
            dir.0.join("gwnative.jsonl"),
            "{\"kind\":\"sample\",\"cpuPercent\":12}\n{\"kind\":\"page\",\"line\":\"hi\"}\n",
        )
        .unwrap();

        let text = compose(&dir.0, &settings::Settings::default(), 1_770_000_000);
        assert!(text.starts_with("Guild Wars problem report\n"));
        assert!(text.contains("Written 2026-02-02 02:40:00 UTC"), "{text}");
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        // Every setting, by the name the page and the file both use for it.
        assert!(text.contains("touchMode"), "{text}");
        assert!(text.contains("compatibilityNoticeSeenFor"), "{text}");
        assert!(text.contains("\"cpuPercent\":12"), "{text}");
        assert!(text.contains("\"line\":\"hi\""), "{text}");
    }

    #[test]
    fn only_the_tail_of_a_long_log_is_carried() {
        let dir = TempDir::new("report-tail");
        let body: String = (0..TAIL_RECORDS + 50)
            .map(|n| format!("{{\"n\":{n}}}\n"))
            .collect();
        fs::write(dir.0.join("gwnative.jsonl"), body).unwrap();

        let text = compose(&dir.0, &settings::Settings::default(), 0);
        assert!(text.contains("(50 earlier records not included)"), "{text}");
        assert!(!text.contains("{\"n\":49}"), "the oldest are gone");
        assert!(text.contains("{\"n\":50}"), "and the tail is all there");
        assert!(text.contains(&format!("{{\"n\":{}}}", TAIL_RECORDS + 49)));
    }

    #[test]
    fn a_missing_log_still_produces_a_report() {
        let dir = TempDir::new("report-nolog");
        let text = compose(&dir.0, &settings::Settings::default(), 0);
        assert!(text.contains("The log could not be read"), "{text}");
        assert!(text.contains("This machine"), "and the rest survives it");
    }

    #[test]
    fn the_filename_is_sortable_and_has_no_colons() {
        let name = stamp(1_770_000_000, Format::File);
        assert_eq!(name, "20260202-024000");
        assert!(!name.contains(':'), "the Finder would show one as a slash");
    }
}
