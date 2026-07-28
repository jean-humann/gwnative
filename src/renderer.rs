//! What the page is allowed to do, and what happens when it dies.
//!
//! Two jobs, one object, because WebKit asks for both through the same delegate
//! protocol.
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

use std::cell::Cell;

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSAlert, NSAlertStyle};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKWebView,
};

pub struct Ivars {
    /// `http://127.0.0.1:38112`, scheme and authority and nothing else. Compared
    /// as a prefix against the candidate URL, which is why it carries no
    /// trailing slash: `http://127.0.0.1:38112/index.html` starts with it and
    /// `http://127.0.0.1:381120/` does not, because the character after the
    /// prefix must be `/`.
    origin: String,
    /// Whether the one automatic reload has been spent.
    recovered: Cell<bool>,
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

            let allowed = url.as_deref().is_some_and(|url| self.permits(url));
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
                explain();
                return;
            }
            note!("[renderer] the web content process died; reloading");
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
);

impl Guard {
    fn new(mtm: MainThreadMarker, origin: String) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            origin,
            recovered: Cell::new(false),
        });
        // SAFETY: `NSObject`'s designated initializer, on a freshly allocated
        // instance whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }

    /// Whether `url` is on the origin this window was opened at.
    ///
    /// `about:blank` is permitted because WebKit navigates to it as part of
    /// tearing a frame down, and refusing that would print a line on every
    /// ordinary close.
    fn permits(&self, url: &str) -> bool {
        if url == "about:blank" {
            return true;
        }
        let origin = &self.ivars().origin;
        url.len() > origin.len()
            && url.starts_with(origin.as_str())
            && url.as_bytes()[origin.len()] == b'/'
    }

}

/// Say, once, that the game stopped — because the second crash leaves a blank
/// window and no other explanation of it exists.
///
/// Nothing about the guard is involved: which crash this is has already been
/// decided by the caller, and what is left is a modal that belongs to the
/// application.
fn explain() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Critical);
    alert.setMessageText(&NSString::from_str("Guild Wars stopped unexpectedly"));
    alert.setInformativeText(&NSString::from_str(
        "The game's renderer closed twice in a row, so it was not restarted \
         again. Quit and reopen Guild Wars to try once more.",
    ));
    alert.runModal();
}

thread_local! {
    /// The live delegate. WKWebView holds its navigation delegate *weakly*, so
    /// something on this side has to keep it alive — dropping it here would
    /// leave the web view with a dangling delegate and every policy decision
    /// back to WebKit's permissive default.
    static GUARD: std::cell::RefCell<Option<Retained<Guard>>> =
        const { std::cell::RefCell::new(None) };
}

/// Confine `webview` to `origin` and give it one free reload. Main thread.
///
/// `origin` is scheme and authority with no trailing slash, as
/// `http://127.0.0.1:38112`.
pub fn guard(mtm: MainThreadMarker, webview: &WKWebView, origin: &str) {
    let guard = Guard::new(mtm, origin.to_owned());
    // SAFETY: main thread, and the delegate is kept alive below for as long as
    // the process runs.
    unsafe {
        webview.setNavigationDelegate(Some(ProtocolObject::from_ref(&*guard)));
    }
    GUARD.with(|held| *held.borrow_mut() = Some(guard));
}

#[cfg(test)]
mod tests {
    /// The same rule the delegate applies, extracted so it can be tested
    /// without a main thread, a web view or an Objective-C runtime.
    fn permits(origin: &str, url: &str) -> bool {
        if url == "about:blank" {
            return true;
        }
        url.len() > origin.len() && url.starts_with(origin) && url.as_bytes()[origin.len()] == b'/'
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
