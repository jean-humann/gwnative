//! The host's channel to the running page.
//!
//! Almost everything the page needs it asks for over the loopback origin. What
//! cannot work that way is anything only AppKit knows about, because the page
//! is never told and has nothing to poll: the window resigning key, the
//! keyboard layout changing. Those are pushed here, by evaluating a one-line
//! dispatch into the page's realm.
//!
//! The vocabulary lives in `web/commands.js`. This side names a command and
//! sends it; what the name does is the page's business.

use std::ffi::c_void;

use objc2::Message;
use objc2::rc::Retained;
use objc2_app_kit::NSApplication;
use objc2_foundation::{MainThreadMarker, NSString};
use objc2_web_kit::WKWebView;

use crate::layout;

thread_local! {
    /// The live web view. Main-thread-only by WebKit's rules, which is exactly
    /// what a thread-local on the main thread enforces.
    static PAGE: std::cell::RefCell<Option<Retained<WKWebView>>> =
        const { std::cell::RefCell::new(None) };
}

/// Adopt `webview` as the page every later push goes to, and start watching the
/// things AppKit has to forward. Main thread.
pub fn attach(webview: &WKWebView) {
    PAGE.with(|page| *page.borrow_mut() = Some(webview.retain()));
    watch_keyboard_layout();
    watch_focus();
}

/// Run `script` in the page, if there is one yet.
///
/// Silent when there is not: every caller here is a notification that can
/// arrive before the first load or after a crash, and none of them is worth
/// failing a launch over.
fn evaluate(script: &str) {
    PAGE.with(|page| {
        if let Some(webview) = page.borrow().as_ref() {
            // SAFETY: main thread — every caller is a notification delivered
            // there, which each documents.
            unsafe {
                webview.evaluateJavaScript_completionHandler(&NSString::from_str(script), None);
            }
        }
    });
}

/// Send a named command to the page.
pub fn send(name: &str) {
    // The name is a literal from this file, never anything a page or a server
    // supplied, so there is nothing here to quote or escape.
    evaluate(&format!(
        "window.dispatchEvent(new CustomEvent('gw:command', {{ detail: {{ name: '{name}' }} }}));"
    ));
}

/// Keep `window.__gwnativeLayout` current when the player switches input source.
///
/// The injected copy is correct at document start and stale the moment somebody
/// presses ⌃Space. Switching layout mid-session is ordinary on a Mac — this
/// user runs French AZERTY — and a stale table would restate an Option-held key
/// as the character some *other* layout puts on it, which is worse than not
/// restating it at all.
fn watch_keyboard_layout() {
    layout::watch(|json| {
        evaluate(&format!(
            "window.__gwnativeLayout = {json}; \
             window.dispatchEvent(new Event('gw:layout-changed'));"
        ));
    });
}

type CFNotificationCenterRef = *const c_void;

type CFNotificationCallback = extern "C" fn(
    center: CFNotificationCenterRef,
    observer: *mut c_void,
    name: *const c_void,
    object: *const c_void,
    user_info: *const c_void,
);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    /// This process's own notifications. Distinct from the distributed centre
    /// `layout` watches, which carries notifications between applications.
    fn CFNotificationCenterGetLocalCenter() -> CFNotificationCenterRef;
    fn CFNotificationCenterAddObserver(
        center: CFNotificationCenterRef,
        observer: *const c_void,
        callback: CFNotificationCallback,
        name: *const c_void,
        object: *const c_void,
        suspension_behavior: i32,
    );
}

/// Tell the page when the window gains or loses key status.
///
/// Two things ride on this. Input: `input.js` already releases everything on
/// `blur`, and that covers most of it, but not the case that actually strands a
/// key — ⌘Tab away mid-stride and the keyup goes to whatever took focus, so the
/// client walks into a wall until the key is pressed and released again. AppKit
/// sees the window resign key every time, including the times the page's own
/// blur does not fire.
///
/// Audio: the Electron build goes quiet when another window is selected. That
/// is the client's own doing — it registers a blur callback on the canvas, and
/// Chromium blurs the focused element when the window resigns key. WebKit only
/// fires blur on the window, so the client never hears it and keeps playing.
/// The page has a fallback on `window` blur, but this is the reliable half, for
/// the same reason it is the reliable half for input.
///
/// Registered as a pair, deliberately: a mute with no matching unmute is a game
/// that is silent until relaunch.
fn watch_focus() {
    // AppKit posts to the default `NSNotificationCenter`, which is the same
    // centre as Core Foundation's local one — the two are bridged. Reading it
    // this way avoids declaring an Objective-C class purely to own a selector.
    for (name, callback) in [
        (
            "NSWindowDidResignKeyNotification",
            resigned_key as CFNotificationCallback,
        ),
        ("NSWindowDidBecomeKeyNotification", became_key),
    ] {
        let name = NSString::from_str(name);
        // SAFETY: `NSString` is toll-free bridged to `CFStringRef`. The name is
        // deliberately leaked: the observer is never removed, because it is
        // wanted for as long as the process runs, so the name has to outlive
        // it. The observer pointer is null and is only ever handed back to the
        // callbacks, which ignore it.
        unsafe {
            let name = Retained::into_raw(name).cast::<c_void>();
            CFNotificationCenterAddObserver(
                CFNotificationCenterGetLocalCenter(),
                std::ptr::null(),
                callback,
                name,
                std::ptr::null(),
                // Local notifications are not suspended, so the behaviour is
                // moot; 0 is `CFNotificationSuspensionBehaviorDrop`, which the
                // local centre ignores.
                0,
            );
        }
    }
}

/// Delivered on the main thread: AppKit posts window notifications there.
extern "C" fn resigned_key(
    _center: CFNotificationCenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    send("input-reset");
    // The observer is registered for any window in the process, so a host panel
    // taking key — the crash alert in `renderer`, a save dialog — arrives here
    // too. Losing the key window while the application is still frontmost is
    // not the player switching away, and ducking the game for it would be
    // wrong. `NSApp.isActive` is the distinction, and it is one the page cannot
    // make.
    if !application_is_active() {
        send("audio-mute");
    }
}

extern "C" fn became_key(
    _center: CFNotificationCenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    send("audio-unmute");
}

/// Whether this application is the frontmost one. Main thread.
fn application_is_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
}
