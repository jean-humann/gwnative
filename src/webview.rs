//! The WKWebView the client runs in, and the WebKit defaults it has to undo.
//!
//! Everything here is configuration that must be in place before the first byte
//! of the page is parsed: the feature flags, because a preference set after the
//! web content process exists is not read; and the injected script, because
//! what it carries — the token, the keyboard layout, the settings — is needed by
//! code that runs at document start and could not await a fetch.
//!
//! Almost every call here is `unsafe` only because objc2 marks every message
//! send that way; they are ordinary calls on live objects, made on the main
//! thread, and are not commented one by one. The exception is
//! [`disable_features`], where the selectors are SPI and the return types are
//! declared by hand — that one carries its argument.

use objc2::rc::Retained;
use objc2::{MainThreadOnly, msg_send};
use objc2_foundation::{MainThreadMarker, NSRect, NSString, NSURL, NSURLRequest};
use objc2_web_kit::{WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration};

use crate::{layout, release, settings, wasm};

/// WebKit feature flags this host turns off because each conflicts with a
/// running game's timing or background-download contract.
///
/// The first is the frame-rate cap: WebKit prefers rendering updates near
/// 60 Hz even on displays that refresh faster, and the game's main loop is
/// `requestAnimationFrame`, so this one flag is the difference between 60 and
/// 120 FPS on a ProMotion display.
///
/// The rest are what happens to a hidden page. WebKit does not merely stop
/// frames for an occluded window — it throttles the page's timers and then
/// suppresses the web content process outright, which freezes networking,
/// the session, everything. Measured twice from a blank install: the boot
/// download stalled mid-flight (at 2,093 and at 1,485 of ~6,000 chunks) the
/// moment the machine went unattended, and never resumed — the harness's
/// timer fallback could not save it, because the process its timers lived in
/// was itself suspended. A player who starts the download and walks away must
/// still come back to a finished install; these flags preserve that contract.
const DISABLED_FEATURES: [&str; 5] = [
    "PreferPageRenderingUpdatesNear60FPSEnabled",
    "PageVisibilityBasedProcessSuppressionEnabled",
    "HiddenPageDOMTimerThrottlingEnabled",
    "HiddenPageDOMTimerThrottlingAutoIncreases",
    "BackgroundWebContentRunningBoardThrottlingEnabled",
];

/// Turn off every feature in [`DISABLED_FEATURES`].
///
/// There is no supported API for any of them — the direct setter SPIs of past
/// years (`_setPreferPageRenderingUpdatesNear60FPSEnabled:` and kin) are gone
/// from current WebKit — so the flags are looked up by key in the feature
/// lists WebKit publishes for its own debug menus, which is as close to a
/// stable name as an unstable surface offers. Every step is guarded by a
/// responds-to check, and a key that has vanished is reported rather than
/// crashed on: each of these degrades to WebKit's default behaviour, never to
/// a broken app.
fn disable_features(preferences: &objc2_web_kit::WKPreferences) {
    use objc2::runtime::AnyObject;
    let class = objc2::class!(WKPreferences);
    let mut remaining: Vec<&str> = DISABLED_FEATURES.to_vec();
    // (feature list class method, matching setter taking that feature kind)
    let surfaces: [(objc2::runtime::Sel, objc2::runtime::Sel); 3] = [
        (objc2::sel!(_features), objc2::sel!(_setEnabled:forFeature:)),
        (
            objc2::sel!(_internalDebugFeatures),
            objc2::sel!(_setEnabled:forInternalDebugFeature:),
        ),
        (
            objc2::sel!(_experimentalFeatures),
            objc2::sel!(_setEnabled:forExperimentalFeature:),
        ),
    ];
    // SAFETY: `respondsToSelector:` is `NSObject`'s and always answerable; the
    // pair at the head of the loop is what makes the rest of the sends safe,
    // since a WebKit that has dropped one of these SPIs skips the surface
    // instead of being sent to. What is left is the return types, which are
    // declared rather than inferred because the compiler has no header to read
    // them from and a wrong one is undefined behaviour: `BOOL` from
    // `respondsToSelector:`, an `NSArray` of feature objects from the list
    // method, `NSString` from `key`, `void` from the setters.
    for (list, set) in surfaces {
        if remaining.is_empty() {
            break;
        }
        let listed: bool = unsafe { msg_send![class, respondsToSelector: list] };
        let settable: bool = unsafe { msg_send![preferences, respondsToSelector: set] };
        if !listed || !settable {
            continue;
        }
        let features: Option<Retained<objc2_foundation::NSArray<AnyObject>>> =
            unsafe { msg_send![class, performSelector: list] };
        let Some(features) = features else { continue };
        for feature in features.iter() {
            let key: Option<Retained<NSString>> = unsafe { msg_send![&*feature, key] };
            let Some(key) = key.map(|key| key.to_string()) else {
                continue;
            };
            let Some(position) = remaining.iter().position(|name| **name == key) else {
                continue;
            };
            let () = match set {
                s if s == objc2::sel!(_setEnabled:forFeature:) => unsafe {
                    msg_send![preferences, _setEnabled: false, forFeature: &*feature]
                },
                s if s == objc2::sel!(_setEnabled:forInternalDebugFeature:) => unsafe {
                    msg_send![preferences, _setEnabled: false, forInternalDebugFeature: &*feature]
                },
                _ => unsafe {
                    msg_send![preferences, _setEnabled: false, forExperimentalFeature: &*feature]
                },
            };
            remaining.remove(position);
        }
    }
    for name in remaining {
        note!("[gwnative] WebKit no longer lists {name}; its default behaviour stands");
    }
}

/// What the page is handed before it has run a line of its own.
///
/// The token goes out of band rather than over the loopback origin, which would
/// put it where any local process could simply ask for it — the exposure the
/// token exists to close.
///
/// The keyboard layout rides the same channel for a different reason: it has to
/// be in place before the page can see a keydown, and a fetch at boot would not
/// be. See `layout` for what the page does with it.
///
/// Settings ride it for that same reason. The render scale is read by the
/// client's first call into the graphics host and the touch mode decides which
/// listeners `input.js` installs, so both are needed before anything the page
/// could await. `PUT /__settings` is what changes them afterwards; this is only
/// the value they start at.
///
/// The template-save state is settled before the page exists — it is which
/// module the server is about to hand out — so it travels with the rest rather
/// than costing the panel a round trip the first time it opens. The client build
/// beside it is what the page compares against
/// [`settings::Settings::compatibility_notice_seen_for`], and is `null` on the
/// one launch where the module could not be read at all. Enhancement state rides
/// alongside for the same reason and one more: after runtime initialization
/// the page installs a passive observer from this exact preselected layout.
/// A later round trip could pair a refreshed certificate with the already
/// instantiated artifact, so the immutable launch snapshot is injected here.
fn preamble(token: &str, settings: &settings::Settings, module: &wasm::Module) -> String {
    let forced_runtime = std::env::var("GWNATIVE_CLIENT_RUNTIME")
        .ok()
        .filter(|value| value == "jspi" || value == "asyncify");
    format!(
        "window.__gwnativeToken = {};\nwindow.__gwnativeLayout = {};\n\
         window.__gwnativeBridgeMarkers = {};\nwindow.__gwnativeSettings = {};\n\
         window.__gwnativeRuntimeCapabilities = {};\n\
         window.__gwnativeTemplateSave = \"uncertified\";\nwindow.__gwnativeClientBuild = null;\n\
         window.__gwnativeUpdates = {};\nwindow.__gwnativeAutoInstall = {};\n\
         window.__gwnativeEnhancements = \"off\";\n\
         window.__gwnativeEnhancementManifest = null;\nwindow.__gwnativeClientRuntime = {};",
        serde_json::Value::from(token),
        layout::as_json(),
        wasm::markers_json(),
        serde_json::to_string(settings).unwrap_or_else(|_| "{}".to_owned()),
        module.runtimes_json(),
        // The same question the Help menu asks itself before offering "Check for
        // Updates…", answered once and injected so the settings panel does not
        // offer a switch for something this build cannot do. A page that guessed
        // would offer it on every build, including the ones with nowhere to look.
        serde_json::Value::from(crate::updater::available() || release::repository().is_some()),
        // The narrower of the two. Checking can be offered by either update
        // path; installing without being asked is Sparkle's alone, and a switch
        // for it on a build that only knows how to open a web page would be a
        // promise nothing could keep.
        serde_json::Value::from(crate::updater::available()),
        serde_json::Value::from(forced_runtime),
    )
}

pub fn make(
    mtm: MainThreadMarker,
    frame: NSRect,
    url: &str,
    token: &str,
    settings: &settings::Settings,
    module: &wasm::Module,
) -> Retained<WKWebView> {
    let config = unsafe { WKWebViewConfiguration::new(mtm) };

    // See DISABLED_FEATURES: the 60 FPS cap and the hidden-page throttling
    // ladder, both of which Chromium-based rivals never had.
    let preferences = unsafe { config.preferences() };
    disable_features(&preferences);

    unsafe {
        let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
            WKUserScript::alloc(mtm),
            &NSString::from_str(&preamble(token, settings, module)),
            WKUserScriptInjectionTime::AtDocumentStart,
            true,
        );
        config.userContentController().addUserScript(&script);
    }

    let webview =
        unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config) };

    // The stock WKWebView agent string has no Version/Safari token, and
    // Emscripten glue is known to branch on it.
    unsafe {
        webview.setCustomUserAgent(Some(&NSString::from_str(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
             AppleWebKit/605.1.15 (KHTML, like Gecko) Version/27.0 Safari/605.1.15",
        )));
    }

    let nsurl = NSURL::URLWithString(&NSString::from_str(url))
        .expect("the caller builds this from the loopback address, so it always parses");
    let request = NSURLRequest::requestWithURL(&nsurl);
    unsafe { webview.loadRequest(&request) };

    webview
}
