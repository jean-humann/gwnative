//! What the page is allowed to do, and what happens when it dies.
//!
//! Three jobs, one object, because WebKit asks for all of them through
//! delegates it holds on the same web view.
//!
//! **Navigation.** The page starts at the loopback origin and has no business
//! leaving it. Nothing in `web/` links off-origin, so any navigation that tries
//! to is either the client following something it parsed out of server content
//! or a script that got somewhere it should not have — and in both cases the
//! result of allowing it is the game's window quietly becoming a browser
//! pointed at somebody else's page, with the session token and the injected
//! settings still in the realm it came from. So: same origin or nothing. This
//! sits *underneath* the DNS allowlist in `net`, which governs what the host
//! dials on the client's behalf; this one governs what the window itself
//! becomes.
//!
//! **Recovery.** The web content process is a separate process and it can be
//! killed — by the jetsam pressure that a 4.2 GB streaming game invites, or by
//! a WebGL driver fault. WKWebView does not reload itself when that happens; it
//! leaves a blank white view and no error, which reads exactly like the app
//! hanging. One automatic reload turns that into a re-boot the player watches
//! happen. Only one: a client that crashes its renderer on every boot would
//! otherwise reload forever, and a loop is worse than a message, so the second
//! one says what happened instead.
//!
//! **Pointer lock.** Holding the right button rotates the camera, and the page
//! implements that by locking the pointer and integrating `movementX`/`Y` — the
//! only way to keep turning once the cursor reaches the edge of the window.
//!
//! In Safari a page just gets the lock. In a `WKWebView` it does not: the
//! request is routed to the application's UI delegate, and WebKit's
//! `UIDelegate::UIClient::requestPointerLock` ends in `completionHandler(false)`
//! when no delegate is set or when the one that is set answers neither of the
//! two selectors below. A host that installs no UI delegate therefore denies
//! every request its own page makes — and because the page treats a refusal as
//! "the button is not really down", *every right click died on arrival*, not
//! merely the camera. That is what this delegate is for. Both spellings are
//! implemented because WebKit prefers the completion-handler form and falls
//! back to the older one, and neither is in the public protocol.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSAlert, NSAlertStyle};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKUIDelegate, WKWebView,
};

use crate::{app, generation, relaunch};

pub struct Ivars {
    /// `http://127.0.0.1:38112`, scheme and authority and nothing else. See
    /// [`permits`] for what the absent trailing slash is doing.
    origin: String,
    /// Whether the one automatic reload has been spent.
    recovered: Cell<bool>,
    root: PathBuf,
    generations: Arc<generation::Store>,
    runtime_fingerprint: Option<String>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Guard` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Guard;

    unsafe impl NSObjectProtocol for Guard {}

    unsafe impl WKNavigationDelegate for Guard {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _webview: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: main thread — WebKit calls its navigation delegate there.
            let url = unsafe { action.request().URL() }
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string());

            let allowed = url
                .as_deref()
                .is_some_and(|url| permits(&self.ivars().origin, url));
            if !allowed {
                // The URL is printed because the only way this fires is a bug or
                // an attack, and neither is diagnosable from "a navigation was
                // blocked".
                note!(
                    "[renderer] refused to navigate to {}",
                    url.as_deref().unwrap_or("a request with no URL")
                );
            }
            // The handler must be called exactly once, on every path. Failing to
            // call it wedges the web view's navigation state permanently, which
            // looks like the page having frozen.
            handler.call((if allowed {
                WKNavigationActionPolicy::Allow
            } else {
                WKNavigationActionPolicy::Cancel
            },));
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, webview: &WKWebView) {
            if self.ivars().recovered.replace(true) {
                note!("[renderer] the web content process died again; not reloading");
                explain(
                    "The game's renderer closed twice in a row, so it was not restarted again. \
                     Quit and reopen Guild Wars to try once more.",
                );
                return;
            }
            let _ = self.ivars().generations.recover(&self.ivars().root);
            let fingerprint = self
                .ivars()
                .generations
                .runtime_fingerprint(&self.ivars().root);
            if fingerprint != self.ivars().runtime_fingerprint {
                note!("[renderer] the runtime context changed; starting a fresh app realm");
                match relaunch::start() {
                    Ok(()) => app::request_quit(),
                    Err(reason) => {
                        note!("[renderer] fresh-realm relaunch failed: {reason}");
                        explain(
                            "The renderer closed before the selected runtime was proven, and the \
                             fresh-runtime restart could not be started. Quit and reopen Guild Wars \
                             to continue the fallback safely.",
                        );
                    }
                }
                return;
            }
            if fingerprint.is_none() {
                explain(
                    "The renderer closed while its client files could not be verified. Quit and \
                     reopen Guild Wars to avoid mixing runtime files.",
                );
                return;
            }
            note!("[renderer] the proven web content process died; reloading once");
            // `reload` rather than `reloadFromOrigin`: the artifacts are served
            // with `no-cache`, so they revalidate anyway, and reloading from
            // origin would throw away the compiled form of an 8.2 MB module
            // that is almost certainly still correct.
            //
            // SAFETY: main thread, and the view outlives this call — WebKit
            // holds it while delivering to its delegate.
            let _ = unsafe { webview.reload() };
        }
    }

    // Empty, and required: `setUIDelegate:` takes an `id<WKUIDelegate>`, and
    // every method in the protocol is optional. What WebKit actually calls are
    // the two private selectors below, which it finds by `respondsToSelector:`
    // when the delegate is installed.
    unsafe impl WKUIDelegate for Guard {}

    /// The pointer-lock answer WebKit asks for on current systems.
    ///
    /// Granted unconditionally. There is one page, it is ours, it is on the
    /// loopback origin the navigation delegate above pins it to, and the only
    /// thing that ever asks is the right-drag camera. A prompt here would be a
    /// prompt in the middle of turning around.
    impl Guard {
        #[unsafe(method(_webView:requestPointerLockWithCompletionHandler:))]
        fn request_pointer_lock(&self, _webview: &WKWebView, handler: &DynBlock<dyn Fn(Bool)>) {
            handler.call((Bool::YES,));
        }

        /// The same answer for the older spelling, which has no handler to call
        /// — WebKit takes the mere presence of this method as consent. Kept
        /// because `respondsToSelector:` is what decides, so a system that
        /// stopped asking the first way still gets an answer rather than the
        /// silent `completionHandler(false)` that having neither means.
        #[unsafe(method(_webViewDidRequestPointerLock:))]
        fn did_request_pointer_lock(&self, _webview: &WKWebView) {}
    }
);

impl Guard {
    fn new(
        mtm: MainThreadMarker,
        origin: String,
        root: PathBuf,
        generations: Arc<generation::Store>,
    ) -> Retained<Self> {
        let runtime_fingerprint = generations.runtime_fingerprint(&root);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            origin,
            recovered: Cell::new(false),
            root,
            generations,
            runtime_fingerprint,
        });
        // SAFETY: `NSObject`'s designated initializer, on a freshly allocated
        // instance whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}

/// Whether `url` is on `origin` — scheme and authority, no trailing slash.
///
/// A free function rather than a method on the delegate, so that the rule the
/// window enforces is the rule the tests below exercise. As a method it needed a
/// main thread, a web view and an Objective-C runtime to call, none of which has
/// anything to do with comparing two strings, so the tests carried a second copy
/// of the body — and a copy of a security rule is a rule that can be tightened
/// in one place and still pass.
///
/// `about:blank` is permitted because WebKit navigates to it as part of tearing
/// a frame down, and refusing that would print a line on every ordinary close.
///
/// The length test is what the missing trailing slash buys: the character after
/// the origin has to be `/`, so `http://127.0.0.1:381120/` is not on
/// `http://127.0.0.1:38112` however much of it matches.
fn permits(origin: &str, url: &str) -> bool {
    if url == "about:blank" {
        return true;
    }
    url.len() > origin.len() && url.starts_with(origin) && url.as_bytes()[origin.len()] == b'/'
}

/// Say, once, that the game stopped — because the second crash leaves a blank
/// window and no other explanation of it exists.
///
/// Nothing about the guard is involved: which crash this is has already been
/// decided by the caller, and what is left is a modal that belongs to the
/// application.
fn explain(detail: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Critical);
    alert.setMessageText(&NSString::from_str("Guild Wars stopped unexpectedly"));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.runModal();
}

thread_local! {
    /// The live delegate. WKWebView holds both of its delegates *weakly*, so
    /// something on this side has to keep it alive — dropping it here would
    /// leave the web view with a dangling delegate and every policy decision
    /// back to WebKit's permissive default, and every pointer-lock request back
    /// to WebKit's refusing one.
    static GUARD: std::cell::RefCell<Option<Retained<Guard>>> =
        const { std::cell::RefCell::new(None) };
}

/// Confine `webview` to `origin`, give it one free reload, and let it lock the
/// pointer. Main thread.
///
/// `origin` is scheme and authority with no trailing slash, as
/// `http://127.0.0.1:38112`.
pub fn guard(
    mtm: MainThreadMarker,
    webview: &WKWebView,
    origin: &str,
    root: PathBuf,
    generations: Arc<generation::Store>,
) {
    let guard = Guard::new(mtm, origin.to_owned(), root, generations);
    let object = ProtocolObject::from_ref(&*guard);
    // SAFETY: main thread, and the delegate is kept alive below for as long as
    // the process runs.
    unsafe {
        webview.setNavigationDelegate(Some(object));
        webview.setUIDelegate(Some(ProtocolObject::from_ref(&*guard)));
    }
    GUARD.with(|held| *held.borrow_mut() = Some(guard));
}

#[cfg(test)]
mod tests {
    use super::{Guard, permits};
    use objc2::ClassType;
    use objc2::runtime::AnyClass;
    use objc2::sel;

    /// Both pointer-lock selectors are private, so nothing at compile time
    /// checks that they are spelled the way WebKit looks them up — and a
    /// misspelling is not a crash or a warning. WebKit asks
    /// `respondsToSelector:`, gets no, and answers its own page
    /// `completionHandler(false)`; the game then behaves as though the right
    /// mouse button were broken. So the spelling is asserted the same way
    /// WebKit reads it.
    #[test]
    fn webkit_can_find_both_pointer_lock_selectors() {
        let class: &AnyClass = Guard::class();
        assert!(
            class.responds_to(sel!(_webView:requestPointerLockWithCompletionHandler:)),
            "the form current WebKit prefers"
        );
        assert!(
            class.responds_to(sel!(_webViewDidRequestPointerLock:)),
            "the fallback WebKit uses when the first is absent"
        );
    }

    #[test]
    fn only_the_origin_the_window_was_opened_at() {
        let origin = "http://127.0.0.1:38112";

        assert!(permits(origin, "http://127.0.0.1:38112/index.html"));
        assert!(permits(origin, "http://127.0.0.1:38112/Gw.snapshot"));
        assert!(permits(origin, "http://127.0.0.1:38112/"));
        assert!(permits(origin, "about:blank"));

        // A longer port that shares the prefix. This is the case the `/` test
        // exists for: a plain `starts_with` would allow it.
        assert!(!permits(origin, "http://127.0.0.1:381120/evil.html"));
        // Another port on the same host is another origin.
        assert!(!permits(origin, "http://127.0.0.1:8080/index.html"));
        // Another host that happens to embed ours.
        assert!(!permits(origin, "http://127.0.0.1:38112.evil.com/"));
        // A different scheme to the same authority is a different origin.
        assert!(!permits(origin, "https://127.0.0.1:38112/index.html"));
        // The ordinary off-origin cases.
        assert!(!permits(origin, "https://example.com/"));
        assert!(!permits(origin, "file:///etc/passwd"));
        assert!(!permits(origin, "javascript:alert(1)"));
        // The origin itself, with nothing after it, is not a navigation target:
        // WebKit always supplies at least a path.
        assert!(!permits(origin, origin));
    }
}
