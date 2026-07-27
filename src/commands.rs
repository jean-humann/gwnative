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
use objc2_foundation::NSString;
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

/// Tell the page when the window stops being the key window.
///
/// `input.js` already releases everything on `blur`, and that covers most of
/// it. What it does not cover is the case that actually strands a key: ⌘Tab
/// away mid-stride and the keyup goes to whatever took focus, so the client
/// walks into a wall until the key is pressed and released again. AppKit sees
/// the window resign key every time, including the times the page's own blur
/// does not fire.
fn watch_focus() {
    // AppKit posts to the default `NSNotificationCenter`, which is the same
    // centre as Core Foundation's local one — the two are bridged. Reading it
    // this way avoids declaring an Objective-C class purely to own a selector.
    let name = NSString::from_str("NSWindowDidResignKeyNotification");
    // SAFETY: `NSString` is toll-free bridged to `CFStringRef`. The name is
    // deliberately leaked: the observer is never removed, because it is wanted
    // for as long as the process runs, so the name has to outlive it. The
    // observer pointer is null and is only ever handed back to `resigned_key`,
    // which ignores it.
    unsafe {
        let name = Retained::into_raw(name).cast::<c_void>();
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetLocalCenter(),
            std::ptr::null(),
            resigned_key,
            name,
            std::ptr::null(),
            // Local notifications are not suspended, so the behaviour is moot;
            // 0 is `CFNotificationSuspensionBehaviorDrop`, which the local
            // centre ignores.
            0,
        );
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
}
