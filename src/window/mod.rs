//! Where the window was, across launches.
//!
//! This half is the live window: it opens one, watches it for the moves,
//! resizes and mode changes worth remembering, and writes what it sees. What a
//! written frame *means* — which display it lands on when the one it was saved
//! from is gone — lives in [`state`], which knows nothing about AppKit
//! notifications and can therefore be tested without a window.
//!
//! Writes are coalesced. `NSWindowDidMove` fires continuously while a window is
//! dragged, and the state is small but not free to write, so a drag writes at
//! most once a second and always writes once it settles.

mod state;

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSColor, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{MainThreadMarker, NSObjectNSDelayedPerforming, NSString};
use objc2_web_kit::WKWebView;

use crate::cli::WindowMode;
use crate::notify::{self, Callback, CenterRef};

use state::{Bounds, Mode, State, default_state, fit, load, save, work_areas};

/// At most one write per second while a drag is in progress.
const WRITE_INTERVAL: Duration = Duration::from_secs(1);

thread_local! {
    /// The window being tracked and where its state is written. Main thread by
    /// AppKit's rules, which a thread-local on the main thread enforces.
    static TRACKED: RefCell<Option<Tracker>> = const { RefCell::new(None) };
}

thread_local! {
    /// Where the window should land once it has finished leaving full screen.
    ///
    /// Set only by [`reset`], and only when the window is full screen at the
    /// time. Main thread, like everything else here.
    static PENDING: std::cell::Cell<Option<Bounds>> = const { std::cell::Cell::new(None) };
}

struct Tracker {
    window: Retained<NSWindow>,
    path: PathBuf,
    /// The last frame the window had while it was neither zoomed nor full
    /// screen. What gets written, whatever the window looks like now.
    normal: Bounds,
    /// When this window's state last reached disk, or `None` if it never has.
    /// `None` rather than an instant far enough in the past to look stale:
    /// there is no such instant on a machine that booted a moment ago, and
    /// subtracting one from `Instant::now()` panics there.
    written: Option<Instant>,
    /// The refresh rate last reported for the display the window was on.
    refresh_hz: Option<isize>,
}

/// Say what the display the window is on can actually do.
///
/// `PreferPageRenderingUpdatesNear60FPSEnabled` is off — see [`crate::webview`]
/// — so the client's `requestAnimationFrame` loop runs at whatever the panel
/// refreshes at. A built-in ProMotion display gives 120; nearly every external
/// monitor gives 60. Which means "it is capped at 60 again" is usually a
/// question about which screen the window was dragged to, and nothing in the
/// game can answer it. So the window answers it: once at launch, and again
/// whenever a drag lands it on a panel with a different ceiling.
fn report_refresh_rate(tracker: &mut Tracker) {
    // `None` while the window is off screen or in the middle of being placed.
    // Nothing to report, and nothing to remember either — the next move asks
    // again.
    let Some(screen) = tracker.window.screen() else {
        return;
    };
    let hz = screen.maximumFramesPerSecond();
    if tracker.refresh_hz == Some(hz) {
        return;
    }
    tracker.refresh_hz = Some(hz);
    note!("[gwnative] this display refreshes at {hz} Hz, which is the frame rate ceiling");
}

/// Build the window, restoring wherever the last one was left.
///
/// The state file is read here rather than by the caller because the frame has
/// to be known before `initWithContentRect:`: creating a window at one size and
/// moving it afterwards shows the player both.
pub fn open(
    mtm: MainThreadMarker,
    webview: &WKWebView,
    path: PathBuf,
    requested_mode: Option<WindowMode>,
) -> Retained<NSWindow> {
    let (areas, primary) = work_areas(mtm);
    let stored = load(&path);
    let mut state = match stored {
        Some(state) => fit(state, &areas, primary),
        None => default_state(primary),
    };
    state.mode = match requested_mode {
        Some(WindowMode::Windowed) => Mode::Normal,
        Some(WindowMode::Fullscreen) => Mode::Fullscreen,
        None => state.mode,
    };

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;

    // SAFETY: main thread, and the frame is a rectangle `fit` has already
    // bounded against a real work area.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            state.bounds.to_rect(),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    window.setTitle(&NSString::from_str("Guild Wars"));
    // Keep the semantic window title for the Dock, Window menu and
    // accessibility, but do not draw a second copy in the title bar. AppKit
    // can place that label at the leading edge, underneath the
    // close/minimize/full-screen buttons.
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    // If the retained-frame cover cannot be captured, the content view's
    // backing surface should still reveal the same black as the game rather
    // than a system background colour during WebKit's activation handoff.
    window.setBackgroundColor(Some(&NSColor::blackColor()));
    // Without this the green button zooms instead of going full screen, and
    // `toggleFullScreen:` — which is how a stored `fullscreen` is restored —
    // does nothing at all.
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    // Own the WebView's normal-window container rather than making AppKit's
    // private frame view its direct parent. The activation cover can then use
    // this public content view normally and resolve WebKit's current content
    // view if element fullscreen temporarily moves the live WebView elsewhere.
    let container = NSView::initWithFrame(NSView::alloc(mtm), webview.frame());
    window.setContentView(Some(&container));
    webview.setFrame(container.bounds());
    webview.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    container.addSubview(webview);
    // Key events go to the first responder; mouse events are hit-tested and do
    // not. A window that never hands the web view first responder therefore
    // looks alive to the trackpad and deaf to the keyboard, which is exactly
    // what the game does when its canvas never sees a keydown.
    window.setInitialFirstResponder(Some(webview));
    window.makeFirstResponder(Some(webview));

    // `initWithContentRect:` takes a *content* rectangle and the stored frame is
    // the window's, so the window is now a titlebar's height too tall. Setting
    // the frame directly is the correction, and it is done before the window is
    // ever ordered front.
    window.setFrame_display(state.bounds.to_rect(), false);

    // The mode is applied after the frame, so the frame underneath a zoomed or
    // full-screen window is the one the player left rather than whatever the
    // window happened to be created at.
    match state.mode {
        Mode::Normal => {}
        Mode::Maximized => window.zoom(None),
        // Deferred: a window that is not yet on screen cannot enter full
        // screen, and asking it to before `makeKeyAndOrderFront` silently does
        // nothing. `perform` runs it once the run loop turns, by which time it
        // is.
        // SAFETY: `window` is live and this is the main thread, which is where
        // `performSelector:` schedules the send.
        Mode::Fullscreen => unsafe {
            window.performSelector_withObject_afterDelay(objc2::sel!(toggleFullScreen:), None, 0.0);
        },
    }

    TRACKED.with(|tracked| {
        let mut tracker = Tracker {
            window: window.clone(),
            path,
            normal: state.bounds,
            // Never written, so the first move writes rather than being
            // coalesced away.
            written: None,
            refresh_hz: None,
        };
        // Before it is stored, so the report cannot be reading a window some
        // notification has already moved.
        report_refresh_rate(&mut tracker);
        *tracked.borrow_mut() = Some(tracker);
    });
    watch();

    window
}

/// What the tracked window looks like right now.
fn observe(tracker: &mut Tracker) -> State {
    let window = &tracker.window;
    let mode = if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        Mode::Fullscreen
    } else if window.isZoomed() {
        Mode::Maximized
    } else {
        // The only case where the current frame is the one worth keeping. A
        // minimized window is skipped too: its frame is the frame it will be
        // restored to, but AppKit reports it while the window is in the Dock
        // and reading it there has been known to give the docked rectangle.
        if !window.isMiniaturized() {
            tracker.normal = Bounds::from_rect(window.frame());
        }
        Mode::Normal
    };
    State {
        bounds: tracker.normal,
        mode,
    }
}

/// Put the window back where a first launch would have put it.
///
/// `fit` already rescues the frames it can reason about — a window on a display
/// that has been unplugged, one too small to grab. What it cannot rescue is the
/// window a player has merely lost: dragged mostly off the edge, or left full
/// screen on a machine whose second display went away between sessions. This is
/// the explicit escape hatch for those cases.
pub fn reset(mtm: MainThreadMarker) {
    // Cloned out rather than used inside the borrow: every AppKit call below
    // posts a window notification synchronously, and the handlers for those
    // borrow `TRACKED` again.
    let window = TRACKED.with(|tracked| {
        tracked
            .borrow()
            .as_ref()
            .map(|tracker| tracker.window.clone())
    });
    let Some(window) = window else { return };

    let (_, primary) = work_areas(mtm);
    let bounds = default_state(primary).bounds;

    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        // Leaving full screen is an animation, and it ends by restoring the
        // frame the window had before it started — which is the frame being
        // replaced. Setting one now would be setting it twice, the second time
        // by AppKit. So it is remembered and applied on the way out.
        PENDING.with(|pending| pending.set(Some(bounds)));
        window.toggleFullScreen(None);
        return;
    }
    if window.isZoomed() {
        window.zoom(None);
    }
    window.setFrame_display_animate(bounds.to_rect(), true, true);
}

/// Write the tracked window's state now, whatever the coalescing interval says.
///
/// Called on the notifications that mean a gesture has finished, and on the way
/// out. Cheap enough that calling it once too often costs nothing.
pub fn flush() {
    TRACKED.with(|tracked| {
        if let Some(tracker) = tracked.borrow_mut().as_mut() {
            let state = observe(tracker);
            save(&tracker.path, state);
            tracker.written = Some(Instant::now());
        }
    });
}

/// Note the window's state, writing only if the last write is old enough.
fn touch() {
    TRACKED.with(|tracked| {
        if let Some(tracker) = tracked.borrow_mut().as_mut() {
            // A move is the only notice a screen change gives — AppKit posts no
            // "drag finished" — so the check rides the moves and is silent
            // until the ceiling actually differs.
            report_refresh_rate(tracker);
            let state = observe(tracker);
            if tracker
                .written
                .is_none_or(|at| at.elapsed() >= WRITE_INTERVAL)
            {
                save(&tracker.path, state);
                tracker.written = Some(Instant::now());
            }
        }
    });
}

/// Follow the window without owning a delegate.
///
/// None of these needs an object, only a function, so they are read as C
/// callbacks — see [`crate::notify`].
fn watch() {
    // Moves and live resizes arrive continuously and are coalesced; the rest
    // are the moments a gesture ends, and each writes.
    for (name, callback) in [
        ("NSWindowDidMoveNotification", moved as Callback),
        ("NSWindowDidResizeNotification", moved),
        // A window that has not been ordered front yet is on no screen at all,
        // so the report at launch can come up empty; these are where it lands
        // on one. `DidChangeScreen` is also the only notice of a display whose
        // mode changed underneath a window that never moved.
        ("NSWindowDidBecomeKeyNotification", screened),
        ("NSWindowDidChangeScreenNotification", screened),
        ("NSWindowDidEndLiveResizeNotification", settled),
        ("NSWindowDidEnterFullScreenNotification", settled),
        ("NSWindowDidExitFullScreenNotification", left_full_screen),
        ("NSWindowDidResignKeyNotification", settled),
        ("NSApplicationWillTerminateNotification", settled),
    ] {
        notify::local(name, callback);
    }
}

/// Delivered on the main thread: AppKit posts window notifications there.
extern "C" fn moved(
    _center: CenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    touch();
}

/// The window has a screen, or a different one. Nothing to write — where a
/// window is was already written by the move that took it there.
extern "C" fn screened(
    _center: CenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    TRACKED.with(|tracked| {
        if let Some(tracker) = tracked.borrow_mut().as_mut() {
            report_refresh_rate(tracker);
        }
    });
}

extern "C" fn settled(
    _center: CenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    flush();
}

/// The other half of a [`reset`] that had to leave full screen first.
///
/// A separate callback rather than a check inside `settled`, because `settled`
/// answers to five other notifications and a pending frame must be spent on
/// exactly the one it was queued for.
extern "C" fn left_full_screen(
    _center: CenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    if let Some(bounds) = PENDING.with(std::cell::Cell::take) {
        let window = TRACKED.with(|tracked| {
            tracked
                .borrow()
                .as_ref()
                .map(|tracker| tracker.window.clone())
        });
        if let Some(window) = window {
            window.setFrame_display_animate(bounds.to_rect(), true, true);
        }
    }
    flush();
}
