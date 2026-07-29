//! What each physical key produces with no modifiers held.
//!
//! macOS treats Option as a text modifier, so a held Option rewrites
//! `KeyboardEvent.key`: on a French layout the key labelled Z arrives as `Ω`
//! rather than `z`. ArenaNet's client identifies keys by that string, so a
//! release pressed under Option never clears the press that registered without
//! it — hold Option to inspect something while running and the movement key
//! stays down for good. The physical key is in `KeyboardEvent.code`, which the
//! modifier does not touch; what the page lacks is the map from that back to
//! the unmodified character.
//!
//! Chromium answers this with `navigator.keyboard.getLayoutMap()`. WebKit
//! implements no Keyboard API at all, so the table has to come from the OS:
//! `TISCopyCurrentKeyboardLayoutInputSource` for the active layout and
//! `UCKeyTranslate` with an empty modifier state for each key it defines. The
//! result is published to the page as `window.__gwnativeLayout` — injected at
//! document start, so it is in place before the first key can arrive, and
//! re-emitted when the player switches input source.

use std::ffi::c_void;

use crate::notify;

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;

// SAFETY of this whole module in one place: `TISCopyCurrentKeyboardLayoutInputSource`
// returns an owned reference that is released below; `TISGetInputSourceProperty`
// returns a borrowed one that must not be. `UCKeyTranslate` writes at most
// `max_length` UTF-16 units and reports how many, and is handed a buffer of
// exactly that size.
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> CFTypeRef;
    fn TISGetInputSourceProperty(source: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    static kTISNotifySelectedKeyboardInputSourceChanged: CFStringRef;
    fn LMGetKbdType() -> u8;
    #[allow(clippy::too_many_arguments)]
    fn UCKeyTranslate(
        layout: *const u8,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: usize,
        actual_length: *mut usize,
        unicode_string: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFDataGetLength(data: CFTypeRef) -> isize;
    fn CFRelease(cf: CFTypeRef);
}

/// Ask for the character the key would show on its cap, rather than the one it
/// would insert. The difference is dead keys: `kUCKeyActionDown` on a French
/// circumflex key returns nothing and arms a dead-key state, which is correct
/// for text and useless for identifying a key.
const ACTION_DISPLAY: u16 = 3;
const NO_DEAD_KEYS: u32 = 1;

/// The character-producing keys, as macOS virtual key codes paired with the
/// `KeyboardEvent.code` values WebKit reports for them.
///
/// Only these matter. Option rewrites the character a key produces, and a key
/// that produces no character — Escape, the arrows, the function row — has
/// nothing to rewrite, so its `key` is already stable. `Space` is deliberately
/// absent for the same reason.
///
/// The codes are the same on every layout: `code` names a position, which is
/// exactly why it survives the rewrite. `IntlBackslash` is the extra key ISO
/// keyboards carry beside the left Shift, and is present on this user's.
const KEYS: &[(u16, &str)] = &[
    (0x00, "KeyA"),
    (0x01, "KeyS"),
    (0x02, "KeyD"),
    (0x03, "KeyF"),
    (0x04, "KeyH"),
    (0x05, "KeyG"),
    (0x06, "KeyZ"),
    (0x07, "KeyX"),
    (0x08, "KeyC"),
    (0x09, "KeyV"),
    (0x0A, "IntlBackslash"),
    (0x0B, "KeyB"),
    (0x0C, "KeyQ"),
    (0x0D, "KeyW"),
    (0x0E, "KeyE"),
    (0x0F, "KeyR"),
    (0x10, "KeyY"),
    (0x11, "KeyT"),
    (0x12, "Digit1"),
    (0x13, "Digit2"),
    (0x14, "Digit3"),
    (0x15, "Digit4"),
    (0x16, "Digit6"),
    (0x17, "Digit5"),
    (0x18, "Equal"),
    (0x19, "Digit9"),
    (0x1A, "Digit7"),
    (0x1B, "Minus"),
    (0x1C, "Digit8"),
    (0x1D, "Digit0"),
    (0x1E, "BracketRight"),
    (0x1F, "KeyO"),
    (0x20, "KeyU"),
    (0x21, "BracketLeft"),
    (0x22, "KeyI"),
    (0x23, "KeyP"),
    (0x25, "KeyL"),
    (0x26, "KeyJ"),
    (0x27, "Quote"),
    (0x28, "KeyK"),
    (0x29, "Semicolon"),
    (0x2A, "Backslash"),
    (0x2B, "Comma"),
    (0x2C, "Slash"),
    (0x2D, "KeyN"),
    (0x2E, "KeyM"),
    (0x2F, "Period"),
    (0x32, "Backquote"),
];

/// The current layout as a `{ code: key }` JSON object, ready to embed.
///
/// Never fails: an input source with no Unicode layout data — most IMEs report
/// none — yields `{}`, and the page falls back to the key the event carried,
/// which is what it would have done anyway.
pub fn as_json() -> String {
    serde_json::Value::from(
        current()
            .into_iter()
            .map(|(code, key)| (code.to_owned(), serde_json::Value::from(key)))
            .collect::<serde_json::Map<_, _>>(),
    )
    .to_string()
}

fn current() -> Vec<(&'static str, String)> {
    // HIToolbox aborts the process — not an error return, `abort()` — if two
    // threads are inside the Text Input Sources API at once: "you must call
    // TIS/TSM API on the main thread". Both callers here are already on it (the
    // web view is built there, and `watch` documents why its notification
    // arrives there), so this lock is never contended in the app. It exists so
    // that a future caller on a request thread degrades to waiting rather than
    // to killing the game.
    static TIS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serialised = TIS.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: `TISCopy…` returns +1 or null. The property is borrowed from it,
    // so it stays valid while `source` does, and every read happens before the
    // release below.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return Vec::new();
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let table = if data.is_null() || CFDataGetLength(data) <= 0 {
            Vec::new()
        } else {
            translate(CFDataGetBytePtr(data))
        };
        CFRelease(source);
        table
    }
}

/// # Safety
/// `layout` must point at a live `UCKeyboardLayout`, borrowed from the input
/// source it came from and outliving this call.
unsafe fn translate(layout: *const u8) -> Vec<(&'static str, String)> {
    // SAFETY: `LMGetKbdType` reads a global the OS maintains and takes no
    // arguments.
    let keyboard = unsafe { LMGetKbdType() } as u32;
    let mut table = Vec::with_capacity(KEYS.len());

    for &(virtual_key, code) in KEYS {
        // Four is enough for any single key: the longest thing a key produces
        // unmodified is one character, and the surrogate pair that would need
        // two is already covered twice over.
        let mut buffer = [0u16; 4];
        let mut length = 0usize;
        let mut dead = 0u32;

        // SAFETY: buffer and its length agree, and the two out-parameters are
        // live locals for the duration of the call.
        let status = unsafe {
            UCKeyTranslate(
                layout,
                virtual_key,
                ACTION_DISPLAY,
                0,
                keyboard,
                NO_DEAD_KEYS,
                &mut dead,
                buffer.len(),
                &mut length,
                buffer.as_mut_ptr(),
            )
        };
        if status != 0 || length == 0 || length > buffer.len() {
            continue;
        }

        let Ok(key) = String::from_utf16(&buffer[..length]) else {
            continue;
        };
        // A layout that maps a key to a control character has not given us a
        // name the client could match against; skip it rather than publish one.
        if key.chars().any(|c| c.is_control()) {
            continue;
        }
        table.push((code, key));
    }

    table
}

static ON_CHANGE: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

extern "C" fn layout_changed(
    _center: notify::CenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _user_info: *const c_void,
) {
    if let Some(handler) = ON_CHANGE.get() {
        handler(&as_json());
    }
}

/// Call `on_change` with a fresh table whenever the player switches layout.
///
/// Must be called from the main thread and before the app's run loop starts:
/// CoreFoundation delivers a distributed notification on the run loop of the
/// thread that registered for it, and `on_change` ends in `evaluateJavaScript`,
/// which is main-thread-only.
///
/// The notification matters while the game is already frontmost: polling only
/// when focus changes would miss a layout switch made in place.
pub fn watch(on_change: fn(&str)) {
    if ON_CHANGE.set(on_change).is_err() {
        return;
    }
    // SAFETY: the name is HIToolbox's constant CFString, which outlives the
    // process.
    unsafe {
        if kTISNotifySelectedKeyboardInputSourceChanged.is_null() {
            return;
        }
        // Immediately, rather than coalesced until the app comes forward: the
        // player switches layout with the game in front of them, but they may
        // also do it from another app and come back, and coalescing that away
        // would leave the stale table in place until the next focus refresh.
        notify::distributed(
            kTISNotifySelectedKeyboardInputSourceChanged,
            layout_changed,
            notify::DELIVER_IMMEDIATELY,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_named_once() {
        let mut codes: Vec<_> = KEYS.iter().map(|(_, code)| *code).collect();
        codes.sort_unstable();
        let count = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), count, "a KeyboardEvent.code is listed twice");

        let mut virtual_keys: Vec<_> = KEYS.iter().map(|(key, _)| *key).collect();
        virtual_keys.sort_unstable();
        let count = virtual_keys.len();
        virtual_keys.dedup();
        assert_eq!(
            virtual_keys.len(),
            count,
            "a virtual key code is listed twice"
        );
    }

    #[test]
    fn the_active_layout_names_the_movement_keys() {
        let table = current();
        // A machine with no Unicode layout data has nothing to assert about,
        // and CI may well be one. Anywhere a layout exists, the keys the game
        // steers with have to be in it, or the Option fix silently does nothing.
        if table.is_empty() {
            return;
        }
        for code in ["KeyW", "KeyA", "KeyS", "KeyD", "KeyQ", "KeyZ"] {
            let found = table.iter().find(|(name, _)| *name == code);
            assert!(found.is_some(), "{code} missing from the layout table");
            let (_, key) = found.unwrap();
            assert_eq!(key.chars().count(), 1, "{code} produced {key:?}");
        }
    }

    #[test]
    fn json_is_an_object_of_strings() {
        let parsed: serde_json::Value = serde_json::from_str(&as_json()).unwrap();
        let object = parsed.as_object().expect("an object");
        for (code, key) in object {
            assert!(!code.is_empty());
            assert!(key.is_string(), "{code} mapped to {key}");
        }
    }
}
