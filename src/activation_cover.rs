//! Retained-frame cover for WKWebView's inactive-to-visible surface handoff.
//!
//! The page can submit a healthy WebGL frame a few milliseconds after AppKit
//! activates the application and still flash first: AppKit exposes the native
//! WKWebView layer before JavaScript receives `DidBecomeActive`. The cover
//! snapshots the last complete web view while the app is resigning active or
//! the window is minimizing, installs that image before the app/window becomes
//! visible again, and keeps it there until WebKit has incorporated the next
//! successful logical presentation into a native snapshot.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSImage, NSImageScaling, NSImageView, NSView, NSWindow,
};
use objc2_foundation::{
    MainThreadMarker, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSString,
};
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKSnapshotConfiguration, WKUserContentController,
    WKWebView,
};

use crate::app;
use crate::diagnostics::Recorder;
use crate::notify::{self, Callback, CenterRef};

const MESSAGE_HANDLER: &str = "gwnativeActivationCover";
/// A failed page bridge or WebKit snapshot must not leave a frozen image above
/// a live game indefinitely. Normal removal follows the first presented frame
/// and has measured below 50 ms; this is only a fail-safe.
const RELEASE_FAILSAFE_MS: u32 = 500;

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `PresentedHandler` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct PresentedHandler;

    unsafe impl NSObjectProtocol for PresentedHandler {}

    unsafe impl WKScriptMessageHandler for PresentedHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn received(&self, _controller: &WKUserContentController, message: &WKScriptMessage) {
            // SAFETY: WebKit returns a retained Foundation object. The page
            // posts the decimal generation string injected by this process;
            // anything else is an unrelated or stale message.
            let body = unsafe { message.body() };
            let Some(generation) = body
                .downcast_ref::<NSString>()
                .and_then(|value| value.to_string().parse::<u64>().ok())
            else {
                return;
            };
            release_after_presentation(generation);
        }
    }
);

impl PresentedHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer, on a freshly allocated
        // instance whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY:
    // - NSImageView has no subclassing requirements.
    // - `CoverView` does not implement `Drop`.
    #[unsafe(super(NSImageView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct CoverView;

    impl CoverView {
        /// This is visual cover, not a control. Let a click delivered during
        /// the short handoff continue to the game underneath it.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> *mut NSView {
            std::ptr::null_mut()
        }

        /// The retained pixels are visual continuity, not new content. Keep
        /// the transient image out of VoiceOver's traversal.
        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            false
        }
    }
);

impl CoverView {
    fn new(mtm: MainThreadMarker, frame: NSRect, image: &NSImage) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSImageView's NSView initializer, on a freshly allocated
        // instance whose ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setImage(Some(image));
        view
    }
}

struct State {
    webview: Retained<WKWebView>,
    window: Retained<NSWindow>,
    // A cover inside WKWebView still shares the remote WebKit compositor
    // subtree whose activation handoff the cover is trying to hide.
    // Keep an app-owned sibling above that subtree instead.
    container: Retained<NSView>,
    // Kept explicitly even though WKUserContentController retains registered
    // handlers. The ownership rule is then local rather than an API footnote.
    _handler: Retained<PresentedHandler>,
    recorder: Arc<Recorder>,
    snapshot: Option<Retained<NSImage>>,
    cover: Option<Retained<CoverView>>,
    app_active: bool,
    window_state: WindowState,
    armed_generation: Option<u64>,
    refreshing: bool,
    generation: u64,
    failsafe: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowState {
    Visible,
    Minimizing,
    Minimized,
}

impl WindowState {
    fn is_visible(self) -> bool {
        self == Self::Visible
    }

    fn is_minimized(self) -> bool {
        self == Self::Minimized
    }

    fn can_install_cover(self) -> bool {
        self != Self::Minimizing
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn remove_cover(state: &mut State) {
    // Invalidate either timeout associated with the cover being removed.
    state.failsafe = state.failsafe.wrapping_add(1);
    state.armed_generation = None;
    state.refreshing = false;
    if let Some(cover) = state.cover.take() {
        cover.removeFromSuperview();
    }
}

fn arm_failsafe(state: &mut State) -> (u64, u64) {
    state.failsafe = state.failsafe.wrapping_add(1);
    (state.generation, state.failsafe)
}

fn is_current_presentation(generation: u64, armed: Option<u64>, received: u64) -> bool {
    generation == received && armed == Some(received)
}

fn should_arm_visible_failsafe(
    window_state: WindowState,
    visible_transition: bool,
    cover_present: bool,
) -> bool {
    cover_present && visible_transition && window_state.is_visible()
}

fn schedule_failsafe(generation: u64, failsafe: u64) {
    app::after(RELEASE_FAILSAFE_MS, move || {
        STATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(state) = slot.as_mut() else { return };
            if state.generation != generation || state.failsafe != failsafe || state.cover.is_none()
            {
                return;
            }
            remove_cover(state);
            state.snapshot = None;
            state.refreshing = false;
            state
                .recorder
                .metrics
                .count("gw.frame.activation.cover.failsafe", 1.0);
            note!("[gwnative] activation cover removed by {RELEASE_FAILSAFE_MS} ms fail-safe");
        });
    });
}

/// Put the retained complete frame above WebKit before AppKit exposes it.
/// Called before application visibility and once a window is fully minimized.
/// A cover retained inside a minimized app may wait indefinitely for a Dock
/// restore; only a transition that can expose the window gets a fail-safe now.
fn prepare_activation(visible_transition: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let failsafe = STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let state = slot.as_mut()?;
        if state.cover.is_some() {
            return should_arm_visible_failsafe(state.window_state, visible_transition, true)
                .then(|| arm_failsafe(state));
        }
        if !state.window_state.can_install_cover() {
            return None;
        }
        let snapshot = state.snapshot.as_ref()?;
        let cover = CoverView::new(mtm, state.webview.frame(), snapshot);
        cover.setImageScaling(NSImageScaling::ScaleAxesIndependently);
        cover.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        state.container.addSubview(&cover);
        state.cover = Some(cover);
        state
            .recorder
            .metrics
            .count("gw.frame.activation.cover.installed", 1.0);
        note!("[gwnative] activation cover installed before visibility");
        should_arm_visible_failsafe(state.window_state, visible_transition, true)
            .then(|| arm_failsafe(state))
    });
    if let Some((generation, failsafe)) = failsafe {
        // WillUnhide can occur without a later activation. Bound that state as
        // well as the normal active handoff so a stale image can never remain
        // indefinitely above a live but inactive window.
        schedule_failsafe(generation, failsafe);
    }
}

/// Retain the last native frame. Application deactivation leaves the inactive
/// live window alone; minimization installs the image only after the window is
/// off screen so it is ready before a Dock restore.
fn capture() {
    let Some((webview, generation)) = STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let state = slot.as_mut()?;
        state.generation = state.generation.wrapping_add(1);
        state.snapshot = None;
        remove_cover(state);
        Some((state.webview.clone(), state.generation))
    }) else {
        return;
    };

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let started = Instant::now();
    let configuration = unsafe { WKSnapshotConfiguration::new(mtm) };
    // Capture what WebKit has already composited rather than waiting for a
    // post-deactivation page update that may never run.
    unsafe { configuration.setAfterScreenUpdates(false) };
    let completion = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        if image.is_null() {
            STATE.with(|slot| {
                if let Some(state) = slot.borrow().as_ref() {
                    state
                        .recorder
                        .metrics
                        .count("gw.frame.activation.cover.snapshot-failed", 1.0);
                }
            });
            if !error.is_null() {
                // SAFETY: WebKit supplied a non-null error for this callback.
                let error = unsafe { &*error };
                note!("[gwnative] activation cover snapshot failed: {error}");
            }
            return;
        }
        // SAFETY: WebKit supplies a live image for the duration of the callback;
        // retaining it lets the image survive until the next activation.
        let Some(image) = (unsafe { Retained::retain(image) }) else {
            return;
        };
        let prepare_minimized = STATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(state) = slot.as_mut() else {
                return false;
            };
            if state.generation != generation
                || (state.app_active && state.window_state.is_visible())
            {
                return false;
            }
            state.snapshot = Some(image);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            state
                .recorder
                .metrics
                .count("gw.frame.activation.cover.captured", 1.0);
            state
                .recorder
                .metrics
                .count("gw.frame.activation.cover.capture.ms.total", ms);
            state
                .recorder
                .metrics
                .peak("gw.frame.activation.cover.capture.ms.max", ms);
            note!("[gwnative] activation cover captured in {:.2} ms", ms);
            state.window_state.is_minimized()
        });
        if prepare_minimized {
            // DidMiniaturize may have arrived before the asynchronous snapshot.
            // Install it now while the window is still off screen.
            prepare_activation(false);
        }
    });
    // SAFETY: main thread; WebKit copies the completion block for the async
    // snapshot and both objects stay retained through the call.
    unsafe {
        webview.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &completion)
    };
}

/// Ask the page to acknowledge its next successful logical presentation.
/// `graphics.js` disarms the flag before posting, so at most one message crosses
/// the process boundary for each visibility transition.
fn arm_after_transition() {
    let armed = STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let state = slot.as_mut()?;
        if !state.app_active || !state.window_state.is_visible() {
            return None;
        }
        if state.cover.is_none() {
            state.snapshot = None;
            return None;
        }
        state.armed_generation = Some(state.generation);
        let (generation, failsafe) = arm_failsafe(state);
        Some((state.webview.clone(), generation, failsafe))
    });
    let Some((webview, generation, failsafe)) = armed else {
        return;
    };

    // SAFETY: lifecycle notifications arrive on the main thread. The decimal
    // generation is produced locally and quoted, so the script contains no
    // untrusted interpolation.
    unsafe {
        webview.evaluateJavaScript_completionHandler(
            &NSString::from_str(&format!(
                "window.__gwnativeActivationCoverArmed = \"{generation}\";"
            )),
            None,
        );
    }

    schedule_failsafe(generation, failsafe);
}

/// A successful logical swap has crossed from the page. Ask WebKit for a
/// snapshot that includes screen updates; its completion is the native-side
/// evidence that the fresh frame is ready underneath the cover.
fn release_after_presentation(generation: u64) {
    let Some(webview) = STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let state = slot.as_mut()?;
        if !state.app_active
            || !state.window_state.is_visible()
            || !is_current_presentation(state.generation, state.armed_generation, generation)
            || state.cover.is_none()
            || state.refreshing
        {
            return None;
        }
        state.refreshing = true;
        Some(state.webview.clone())
    }) else {
        return;
    };

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let started = Instant::now();
    let configuration = unsafe { WKSnapshotConfiguration::new(mtm) };
    unsafe { configuration.setAfterScreenUpdates(true) };
    let completion = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        STATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(state) = slot.as_mut() else { return };
            if !state.app_active
                || !state.window_state.is_visible()
                || !is_current_presentation(state.generation, state.armed_generation, generation)
            {
                return;
            }
            state.refreshing = false;
            if image.is_null() {
                state
                    .recorder
                    .metrics
                    .count("gw.frame.activation.cover.refresh-failed", 1.0);
                if !error.is_null() {
                    // SAFETY: WebKit supplied a non-null error for this callback.
                    let error = unsafe { &*error };
                    note!("[gwnative] activation refresh snapshot failed: {error}");
                }
                return;
            }
            remove_cover(state);
            state.snapshot = None;
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            state
                .recorder
                .metrics
                .count("gw.frame.activation.cover.released", 1.0);
            state
                .recorder
                .metrics
                .count("gw.frame.activation.cover.release.ms.total", ms);
            state
                .recorder
                .metrics
                .peak("gw.frame.activation.cover.release.ms.max", ms);
            note!(
                "[gwnative] activation cover released after fresh presentation in {:.2} ms",
                ms
            );
        });
    });
    // SAFETY: main thread; WebKit copies the completion block for the async
    // snapshot and both objects stay retained through the call.
    unsafe {
        webview.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &completion)
    };
}

extern "C" fn will_resign_active(
    _center: CenterRef,
    _observer: *mut std::ffi::c_void,
    _name: *const std::ffi::c_void,
    _object: *const std::ffi::c_void,
    _info: *const std::ffi::c_void,
) {
    let should_capture = STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return false;
        };
        state.app_active = false;
        // A minimized window already has a retained cover. Snapshotting its
        // offscreen WebView again could replace good pixels with a failure.
        state.window_state.is_visible()
    });
    if should_capture {
        capture();
    }
}

extern "C" fn will_activate(
    _center: CenterRef,
    _observer: *mut std::ffi::c_void,
    _name: *const std::ffi::c_void,
    _object: *const std::ffi::c_void,
    _info: *const std::ffi::c_void,
) {
    prepare_activation(true);
}

extern "C" fn did_become_active(
    _center: CenterRef,
    _observer: *mut std::ffi::c_void,
    _name: *const std::ffi::c_void,
    _object: *const std::ffi::c_void,
    _info: *const std::ffi::c_void,
) {
    STATE.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            state.app_active = true;
        }
    });
    // Covers the narrow race where the async inactive snapshot completed
    // after both will-unhide and will-become-active were delivered.
    prepare_activation(true);
    arm_after_transition();
}

fn is_game_window(object: *const std::ffi::c_void) -> bool {
    if object.is_null() {
        return false;
    }
    STATE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|state| {
            std::ptr::eq(
                object.cast::<NSWindow>(),
                std::ptr::from_ref(&*state.window),
            )
        })
    })
}

extern "C" fn will_miniaturize(
    _center: CenterRef,
    _observer: *mut std::ffi::c_void,
    _name: *const std::ffi::c_void,
    object: *const std::ffi::c_void,
    _info: *const std::ffi::c_void,
) {
    if !is_game_window(object) {
        return;
    }
    STATE.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            state.window_state = WindowState::Minimizing;
        }
    });
    capture();
}

extern "C" fn did_miniaturize(
    _center: CenterRef,
    _observer: *mut std::ffi::c_void,
    _name: *const std::ffi::c_void,
    object: *const std::ffi::c_void,
    _info: *const std::ffi::c_void,
) {
    if !is_game_window(object) {
        return;
    }
    STATE.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            state.window_state = WindowState::Minimized;
        }
    });
    // No timeout while the cover is safely inside an offscreen window.
    prepare_activation(false);
}

extern "C" fn did_deminiaturize(
    _center: CenterRef,
    _observer: *mut std::ffi::c_void,
    _name: *const std::ffi::c_void,
    object: *const std::ffi::c_void,
    _info: *const std::ffi::c_void,
) {
    if !is_game_window(object) {
        return;
    }
    STATE.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            state.window_state = WindowState::Visible;
        }
    });
    prepare_activation(true);
    arm_after_transition();
}

/// Install the native activation cover unless explicitly disabled for recovery
/// or comparison.
pub fn install(webview: &WKWebView, window: &NSWindow, recorder: Arc<Recorder>) {
    if std::env::var("GWNATIVE_ACTIVATION_COVER").is_ok_and(|value| value == "0") {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // `window::open` has already installed WKWebView inside an app-owned
    // container. A cover added there is a native sibling above WebKit's remote
    // layer tree without relying on AppKit's private title/frame hierarchy.
    // SAFETY: main thread, and `webview` is retained by the caller and already
    // installed as this window's live content view.
    let Some(container) = (unsafe { webview.superview() }) else {
        note!("[gwnative] activation cover unavailable: WKWebView has no parent view");
        return;
    };

    let handler = PresentedHandler::new(mtm);
    // SAFETY: main thread, a non-empty unique handler name, and the handler is
    // retained both by our state and WKUserContentController.
    unsafe {
        webview
            .configuration()
            .userContentController()
            .addScriptMessageHandler_name(
                ProtocolObject::from_ref(&*handler),
                &NSString::from_str(MESSAGE_HANDLER),
            );
    }
    STATE.with(|slot| {
        *slot.borrow_mut() = Some(State {
            webview: webview.retain(),
            window: window.retain(),
            container,
            _handler: handler,
            recorder,
            snapshot: None,
            cover: None,
            app_active: true,
            window_state: WindowState::Visible,
            armed_generation: None,
            refreshing: false,
            generation: 0,
            failsafe: 0,
        });
    });
    for (name, callback) in [
        (
            "NSApplicationWillResignActiveNotification",
            will_resign_active as Callback,
        ),
        ("NSApplicationWillBecomeActiveNotification", will_activate),
        ("NSApplicationWillUnhideNotification", will_activate),
        (
            "NSApplicationDidBecomeActiveNotification",
            did_become_active,
        ),
        ("NSWindowWillMiniaturizeNotification", will_miniaturize),
        ("NSWindowDidMiniaturizeNotification", did_miniaturize),
        ("NSWindowDidDeminiaturizeNotification", did_deminiaturize),
    ] {
        notify::local(name, callback);
    }
    note!("[gwnative] native retained-frame activation cover enabled");
}

#[cfg(test)]
mod tests {
    use super::{
        CoverView, PresentedHandler, WindowState, is_current_presentation,
        should_arm_visible_failsafe,
    };
    use objc2::ClassType;
    use objc2::runtime::AnyClass;
    use objc2::sel;

    #[test]
    fn webkit_can_find_the_presentation_handler() {
        let class: &AnyClass = PresentedHandler::class();
        assert!(class.responds_to(sel!(
            userContentController:didReceiveScriptMessage:
        )));
    }

    #[test]
    fn cover_does_not_intercept_game_input() {
        let class: &AnyClass = CoverView::class();
        assert!(class.responds_to(sel!(hitTest:)));
    }

    #[test]
    fn an_old_presentation_cannot_release_a_new_cover() {
        assert!(!is_current_presentation(8, Some(8), 7));
        assert!(!is_current_presentation(8, Some(7), 8));
        assert!(is_current_presentation(8, Some(8), 8));
    }

    #[test]
    fn a_fast_minimize_snapshot_waits_until_the_window_is_offscreen() {
        assert!(!WindowState::Minimizing.is_minimized());
        assert!(!WindowState::Minimizing.can_install_cover());
    }

    #[test]
    fn a_late_minimize_snapshot_installs_after_the_window_is_offscreen() {
        assert!(WindowState::Minimized.is_minimized());
        assert!(WindowState::Minimized.can_install_cover());
    }

    #[test]
    fn a_minimized_cover_gets_a_failsafe_when_it_becomes_visible() {
        assert!(!should_arm_visible_failsafe(
            WindowState::Minimized,
            false,
            true
        ));
        assert!(should_arm_visible_failsafe(
            WindowState::Visible,
            true,
            true
        ));
    }
}
