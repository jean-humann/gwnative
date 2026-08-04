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

use std::cell::Cell;
use std::sync::{Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

const MAX_ACTIVE_VALUES: usize = 514;
const MAX_MATCH_WORK: usize = 128 * 1024 * 1024;
const MAX_CANONICAL_VARIANTS: usize = 32;

struct Secret(Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

#[derive(Default)]
struct Registry {
    capabilities: Vec<Secret>,
    active: Vec<(Secret, usize)>,
    untrusted_sinks_disabled: bool,
}

fn secrets() -> &'static Mutex<Registry> {
    static VALUES: OnceLock<Mutex<Registry>> = OnceLock::new();
    VALUES.get_or_init(|| Mutex::new(Registry::default()))
}

fn credential_epoch() -> &'static RwLock<()> {
    static EPOCH: OnceLock<RwLock<()>> = OnceLock::new();
    EPOCH.get_or_init(|| RwLock::new(()))
}

/// Holds credential identity stable from request admission through its sink or
/// durable mutation. A concurrent update cannot retire an old registration in
/// the gap between checking a body and using it.
pub struct UntrustedLease {
    _guard: Option<RwLockReadGuard<'static, ()>>,
}

thread_local! {
    static LEASE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

fn lease_epoch() -> UntrustedLease {
    let nested = LEASE_DEPTH.with(|depth| {
        let nested = depth.get() != 0;
        depth.set(depth.get() + 1);
        nested
    });
    let guard = (!nested).then(|| credential_epoch().read().unwrap_or_else(|e| e.into_inner()));
    UntrustedLease { _guard: guard }
}

impl Drop for UntrustedLease {
    fn drop(&mut self) {
        LEASE_DEPTH.with(|depth| {
            debug_assert!(depth.get() > 0);
            depth.set(depth.get().saturating_sub(1));
        });
    }
}

pub fn admit_untrusted(bytes: &[u8]) -> Option<UntrustedLease> {
    admit_untrusted_parts(std::iter::once(bytes))
}

pub fn admit_untrusted_parts<'a>(
    parts: impl IntoIterator<Item = &'a [u8]>,
) -> Option<UntrustedLease> {
    let lease = lease_epoch();
    let registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
    (!registry.untrusted_sinks_disabled
        && !parts
            .into_iter()
            .any(|bytes| registry_contains(&registry, bytes)))
    .then_some(lease)
}

/// Hold the credential identity stable through a host-owned response body.
/// After a renderer credential replacement, even native dynamic bodies close:
/// the retired immutable page value is intentionally no longer retained in the
/// registry. Required control replies use fixed, bodyless acknowledgements and
/// therefore do not need this exception.
#[expect(dead_code)]
pub fn admit_host_output(bytes: &[u8]) -> Option<UntrustedLease> {
    let lease = lease_epoch();
    let registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
    host_output_allowed(&registry, bytes).then_some(lease)
}

#[cfg_attr(not(test), expect(dead_code))]
fn host_output_allowed(registry: &Registry, bytes: &[u8]) -> bool {
    !registry.untrusted_sinks_disabled && !registry_contains(registry, bytes)
}

/// Exclusive side of [`UntrustedLease`], held while the current credential
/// registration changes or is retired.
pub struct CredentialTransition {
    _guard: RwLockWriteGuard<'static, ()>,
}

pub fn credential_transition() -> CredentialTransition {
    CredentialTransition {
        _guard: credential_epoch()
            .write()
            .unwrap_or_else(|e| e.into_inner()),
    }
}

/// Permanently disable diagnostic/export sinks and arbitrary non-credential
/// request bodies for this process. Required closed runtime control remains
/// available through bodyless acknowledgements.
pub fn disable_untrusted_sinks() {
    secrets()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .untrusted_sinks_disabled = true;
}

#[expect(dead_code)]
pub fn untrusted_sinks_disabled() -> bool {
    secrets()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .untrusted_sinks_disabled
}

/// Replace the fixed launch capabilities as one set.
pub fn set_capabilities(values: &[&str]) {
    let mut registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
    registry.capabilities.clear();
    registry.capabilities.extend(
        values
            .iter()
            .filter(|value| !value.is_empty())
            .map(|value| Secret(value.as_bytes().to_vec())),
    );
}

/// Credentials may not take the exact value of a host capability or prepared
/// artifact identity. Without this closed collision check, a later runtime
/// record could become byte-identical to newly received protected input even
/// though the native value was generated first.
pub fn conflicts_capability(values: &[&str]) -> bool {
    let registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
    values
        .iter()
        .filter(|value| !value.is_empty())
        .any(|value| {
            registry
                .capabilities
                .iter()
                .any(|known| known.0 == value.as_bytes())
        })
}

/// A bounded, reference-counted set of values that are live only while its
/// owner is live. Dropping the owner removes and wipes the registry copies.
pub struct Registration {
    values: Vec<Vec<u8>>,
}

impl Registration {
    pub fn matches(&self, values: &[&str]) -> bool {
        self.values.len() == values.iter().filter(|value| !value.is_empty()).count()
            && values
                .iter()
                .filter(|value| !value.is_empty())
                .all(|value| self.values.iter().any(|known| known == value.as_bytes()))
    }
}

pub fn register(values: &[&str]) -> Result<Registration, String> {
    let mut values = values
        .iter()
        .filter(|value| !value.is_empty())
        .map(|value| value.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let mut registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
    let additions = values
        .iter()
        .filter(|value| !registry.active.iter().any(|(known, _)| known.0 == **value))
        .count();
    if registry.active.len() + additions > MAX_ACTIVE_VALUES {
        values.iter_mut().for_each(|value| wipe(value));
        return Err("too many credentials are active".into());
    }
    for value in &values {
        if let Some((_, count)) = registry
            .active
            .iter_mut()
            .find(|(known, _)| known.0 == *value)
        {
            *count += 1;
        } else {
            registry.active.push((Secret(value.clone()), 1));
        }
    }
    Ok(Registration { values })
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
        for value in &mut self.values {
            if let Some(index) = registry
                .active
                .iter()
                .position(|(known, _)| known.0 == *value)
            {
                if registry.active[index].1 == 1 {
                    registry.active.remove(index);
                } else {
                    registry.active[index].1 -= 1;
                }
            }
            wipe(value);
        }
    }
}

fn contains_spelling(haystack: &[u8], value: &[u8]) -> bool {
    if value.is_empty() {
        return false;
    }
    // Linear-time substring search. Inputs can be an 8 MiB proxy body and a
    // 4 KiB credential; repeated-prefix data must not turn exact-value
    // admission into quadratic work.
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

/// Decode one layer of JSON string escapes without requiring the surrounding
/// diagnostic/request bytes to be one JSON string. This deliberately scans all
/// text: false positives omit a sink, while restricting decoding to a parsed
/// schema lets an arbitrary `\u0065` scalar become a credential only after the
/// admission decision.
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

/// Suppress a complete diagnostic when any raw or repeatedly JSON-escaped
/// spelling is present. Whole-message suppression is the only marker that is
/// safe even when a valid credential is itself `redacted`, `<redacted>`, or a
/// single character shared with a replacement marker.
pub fn redact(text: &str) -> String {
    if rejects_untrusted(text.as_bytes()) {
        String::new()
    } else {
        text.to_owned()
    }
}

fn registry_contains(registry: &Registry, bytes: &[u8]) -> bool {
    let pattern_count = registry.capabilities.len() + registry.active.len();
    if pattern_count == 0 {
        return false;
    }
    // Credential history is bounded, but an 8 MiB proxy body times the whole
    // bound would still make a rejection path do billions of comparisons while
    // holding the registry mutex. Ambiguous work above this fixed ceiling is
    // itself rejected: availability must not turn the disclosure guard into a
    // denial-of-service primitive.
    let mut work = 0usize;
    let mut contains = |candidate: &[u8]| {
        work = work.saturating_add(candidate.len().saturating_mul(pattern_count));
        if work > MAX_MATCH_WORK {
            return true;
        }
        registry
            .capabilities
            .iter()
            .chain(registry.active.iter().map(|(value, _)| value))
            .any(|value| contains_spelling(candidate, &value.0))
    };
    if contains(bytes) {
        return true;
    }
    if !bytes.contains(&b'\\') && !bytes.contains(&b'%') && !bytes.contains(&b'+') {
        return false;
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
        if examined > MAX_CANONICAL_VARIANTS || contains(&candidate) {
            wipe(&mut candidate);
            for remaining in &mut variants {
                wipe(remaining);
            }
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

pub fn contains_secret(bytes: &[u8]) -> bool {
    let registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
    registry_contains(&registry, bytes)
}

/// One atomic decision across the permanent suppression bit and the exact
/// active-value registry. Credential replacement cannot slip between two locks
/// and make an old value temporarily look safe.
pub fn rejects_untrusted(bytes: &[u8]) -> bool {
    let registry = secrets().lock().unwrap_or_else(|e| e.into_inner());
    registry.untrusted_sinks_disabled || registry_contains(&registry, bytes)
}

/// Serialize a structured diagnostic only when it contains no protected
/// spelling. Omitting the complete record preserves both secrecy and schema;
/// textual replacement could corrupt JSON, while a scalar replacement would
/// violate every object-shaped API contract.
#[cfg_attr(not(test), expect(dead_code))]
pub fn redact_json(value: &serde_json::Value) -> Option<Vec<u8>> {
    redact_json_bytes(serde_json::to_vec(value).unwrap_or_default())
}

#[cfg_attr(not(test), expect(dead_code))]
pub fn redact_json_bytes(mut encoded: Vec<u8>) -> Option<Vec<u8>> {
    if !encoded.is_empty() && !rejects_untrusted(&encoded) {
        return Some(encoded);
    }
    wipe(&mut encoded);
    None
}

/// Overwrite a native secret allocation through volatile stores.
pub fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

pub fn wipe_string(value: &mut String) {
    // SAFETY: bytes are replaced with valid UTF-8 NULs and the length and
    // capacity are unchanged.
    unsafe { wipe(value.as_mut_vec()) };
}

/// A transient sensitive text allocation that clears itself on every exit.
#[expect(dead_code)]
pub struct SecretText(String);

impl SecretText {
    #[expect(dead_code)]
    pub fn new(value: String) -> Self {
        Self(value)
    }
}

impl AsRef<str> for SecretText {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretText {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
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
        let mut formatted = format!($($arg)*);
        if let Some(_lease) = crate::log::admit_untrusted(formatted.as_bytes()) {
            let _ = writeln!(std::io::stderr().lock(), "{formatted}");
            crate::log::wipe_string(&mut formatted);
        } else {
            crate::log::wipe_string(&mut formatted);
        }
    }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn active_values_are_removed_from_plain_and_json_diagnostics() {
        let _registration = super::register(&["canary-password-321"]).unwrap();
        assert_eq!(super::redact("error canary-password-321"), "");
        assert!(super::contains_secret(b"prefix canary-password-321 suffix"));
        let _quoted = super::register(&["quoted\"credential"]).unwrap();
        assert!(super::contains_secret(
            br#"{"value":"quoted\\\"credential"}"#
        ));
        let _url = super::register(&["player+one@example.com"]).unwrap();
        assert!(super::contains_secret(b"name=player%2Bone%40example.com"));
        assert!(super::contains_secret(
            b"name=%70%6c%61%79%65%72%2b%6f%6e%65%40%65%78%61%6d%70%6c%65%2e%63%6f%6d"
        ));
        assert!(super::contains_secret(
            b"name=p%6Ca%79er%2Bone%40example%2Ecom"
        ));
        assert!(super::contains_secret(
            br#"{"name":"player+on\u0065@example.com"}"#
        ));
        assert!(super::contains_secret(
            br#"{"name":"player+on\\u0065@example.com"}"#
        ));
        assert!(super::contains_secret(
            b"name=%2570layer%2Bone%40example.com"
        ));
        assert!(super::contains_secret(
            br#"{"name":"player+on%5cu0065@example.com"}"#
        ));
        let _unicode = super::register(&["player-🚀"]).unwrap();
        assert!(super::contains_secret(br#"player-\uD83D\uDE80"#));
    }

    #[test]
    fn pathological_and_overlapping_values_fail_closed() {
        assert!(super::contains_spelling(b"a", b"a"));
        for value in ["redacted", "<redacted>"] {
            let registration = super::register(&[value]).unwrap();
            assert_eq!(super::redact(&format!("before {value} after")), "");
            drop(registration);
        }
        let _registration = super::register(&["user", "user-password"]).unwrap();
        assert_eq!(super::redact("login user-password"), "");
    }

    #[test]
    fn structured_redaction_remains_valid_json() {
        let _registration = super::register(&["::canary::", "quoted\"credential"]).unwrap();
        let encoded = super::redact_json(&serde_json::json!({
            "line": "quoted\\\"credential",
        }));
        assert!(encoded.is_none());
        drop(_registration);
        let ordinary = super::redact_json(&serde_json::json!({"kind": "sample"})).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&ordinary).is_ok());
    }

    #[test]
    fn wiping_covers_the_entire_allocation() {
        let mut bytes = b"synthetic secret".to_vec();
        super::wipe(&mut bytes);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn dynamic_host_responses_close_after_a_credential_replacement() {
        let mut registry = super::Registry {
            untrusted_sinks_disabled: true,
            ..super::Registry::default()
        };
        assert!(!super::host_output_allowed(&registry, b"203.0.113.7"));
        registry
            .active
            .push((super::Secret(b"protected".to_vec()), 1));
        assert!(!super::host_output_allowed(
            &registry,
            b"protected response"
        ));
    }

    #[test]
    fn adversarial_registry_work_fails_closed() {
        let registry = super::Registry {
            active: (0..super::MAX_ACTIVE_VALUES)
                .map(|index| (super::Secret(format!("value-{index}").into_bytes()), 1))
                .collect(),
            ..super::Registry::default()
        };
        let ordinary = vec![b'x'; super::MAX_MATCH_WORK / super::MAX_ACTIVE_VALUES + 1];
        assert!(super::registry_contains(&registry, &ordinary));
    }
}
