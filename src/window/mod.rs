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
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_foundation::{MainThreadMarker, NSObjectNSDelayedPerforming, NSString};
use objc2_web_kit::WKWebView;

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
}

/// Build the window, restoring wherever the last one was left.
///
/// The state file is read here rather than by the caller because the frame has
/// to be known before `initWithContentRect:`: creating a window at one size and
/// moving it afterwards shows the player both.
pub fn open(mtm: MainThreadMarker, webview: &WKWebView, path: PathBuf) -> Retained<NSWindow> {
    let (areas, primary) = work_areas(mtm);
    let stored = load(&path);
    let state = match stored {
        Some(state) => fit(state, &areas, primary),
        None => default_state(primary),
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
    // Without this the green button zooms instead of going full screen, and
    // `toggleFullScreen:` — which is how a stored `fullscreen` is restored —
    // does nothing at all.
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    window.setContentView(Some(webview));
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
        *tracked.borrow_mut() = Some(Tracker {
            window: window.clone(),
            path,
            normal: state.bounds,
            // Never written, so the first move writes rather than being
            // coalesced away.
            written: None,
        });
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
/// the escape hatch for those, and it is why the Electron build has the same
/// item in the same menu.
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
