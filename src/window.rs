//! Where the window was, across launches.
//!
//! Restoring a frame is not merely storing four numbers and setting them back.
//! The display the window was on may be gone, may have moved relative to the
//! others, or may have changed resolution, and a frame that made sense on
//! Friday can put the titlebar under the menu bar or entirely off-screen on
//! Monday — at which point the window cannot be dragged back, because the part
//! you drag it by is what is missing. So a saved frame is a *request*, and
//! [`fit`] decides what it means on the displays that exist now.
//!
//! What is stored is the last *normal* frame, never the zoomed or full-screen
//! one, plus which of the three the window was in. Storing the full-screen
//! frame would restore a window the size of the display and then leave it that
//! size when the player exits full screen, which is how a window ends up with
//! no way back to a usable size.
//!
//! Writes are coalesced. `NSWindowDidMove` fires continuously while a window is
//! dragged, and the state is small but not free to write, so a drag writes at
//! most once a second and always writes once it settles.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBackingStoreType, NSScreen, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSObjectNSDelayedPerforming, NSPoint, NSRect, NSSize, NSString,
};
use objc2_web_kit::WKWebView;

use serde::{Deserialize, Serialize};

/// The shape this build writes. Same contract as `settings`: a version this
/// build does not know is refused rather than reinterpreted, and a refused file
/// costs a default window rather than a window placed from a shape nothing
/// understood.
const FORMAT: u32 = 1;

/// The window a first launch gets, before it has been anywhere.
const DEFAULT_SIZE: (f64, f64) = (1280.0, 800.0);

/// Never restore a window smaller than this, whatever the file says. Below it
/// the client's canvas is unusable and the launcher's buttons do not fit, and a
/// window nobody can read is indistinguishable from a window that did not open.
const MIN_SIZE: (f64, f64) = (800.0, 600.0);

/// Refuse a stored size outside this. Not a real display's limit — a bound past
/// which the number is evidence of a corrupt file rather than of a large
/// monitor.
const MAX_EDGE: f64 = 32_768.0;

/// How much of the work area a default window leaves clear, so it reads as a
/// window rather than as something that failed to go full screen.
const DEFAULT_MARGIN: f64 = 64.0;

/// At most one write per second while a drag is in progress.
const WRITE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Normal,
    Maximized,
    Fullscreen,
}

/// An AppKit frame: origin at the bottom-left, y increasing upwards.
///
/// Stored in AppKit's own coordinates rather than converted to a screen-relative
/// or top-left form, because every producer and consumer here is AppKit and a
/// conversion is one more place to have the sign wrong.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    fn from_rect(rect: NSRect) -> Self {
        Self {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        }
    }

    fn to_rect(self) -> NSRect {
        NSRect::new(
            NSPoint::new(self.x, self.y),
            NSSize::new(self.width, self.height),
        )
    }

    /// Area shared with `other`. Zero when they do not touch, which is how a
    /// window on a display that has since been unplugged is recognised.
    fn overlap(self, other: Bounds) -> f64 {
        let width = (self.x + self.width).min(other.x + other.width) - self.x.max(other.x);
        let height = (self.y + self.height).min(other.y + other.height) - self.y.max(other.y);
        width.max(0.0) * height.max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    pub bounds: Bounds,
    pub mode: Mode,
}

/// Every field optional so serde's type checking does the rejecting, and a
/// `formatVersion` from a later build is caught before any of it is believed.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    format_version: Option<u32>,
    bounds: Option<Bounds>,
    mode: Option<Mode>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Written {
    format_version: u32,
    bounds: Bounds,
    mode: Mode,
}

/// Read a stored state, or nothing.
///
/// A file that cannot be read is removed rather than left to be re-read and
/// re-rejected on every launch: it has already failed to mean anything, and the
/// next save writes a version that does.
pub fn load(path: &Path) -> Option<State> {
    let text = std::fs::read_to_string(path).ok()?;
    match parse(&text) {
        Ok(state) => Some(state),
        Err(reason) => {
            note!("[window] ignoring {}: {reason}", path.display());
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn parse(text: &str) -> Result<State, String> {
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(version) = wire.format_version
        && version != FORMAT
    {
        return Err(format!("formatVersion {version} is not readable"));
    }
    let bounds = wire.bounds.ok_or_else(|| "no bounds".to_owned())?;
    let mode = wire.mode.ok_or_else(|| "no mode".to_owned())?;
    let sane = [bounds.x, bounds.y, bounds.width, bounds.height]
        .iter()
        .all(|value| value.is_finite());
    if !sane || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Err(format!("bounds {bounds:?} are not a rectangle"));
    }
    if bounds.width > MAX_EDGE || bounds.height > MAX_EDGE {
        return Err(format!("bounds {bounds:?} are implausibly large"));
    }
    Ok(State { bounds, mode })
}

/// Write `state`, atomically enough that a crash mid-write cannot leave a file
/// that parses as something else.
pub fn save(path: &Path, state: State) {
    let written = Written {
        format_version: FORMAT,
        bounds: state.bounds,
        mode: state.mode,
    };
    let Ok(json) = serde_json::to_vec(&written) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, &json).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

/// Place `state` on the displays that exist now.
///
/// The window goes to whichever work area it overlaps most. If it overlaps
/// none — the display it was on is gone — it is centred on `primary` at its
/// remembered size rather than being dragged to the nearest edge, because a
/// window that reappears in the middle of a screen reads as "the app opened"
/// and a window jammed into a corner reads as a bug.
pub fn fit(state: State, areas: &[Bounds], primary: Bounds) -> State {
    let best = areas
        .iter()
        .map(|area| (*area, state.bounds.overlap(*area)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, overlap)| *overlap > 0.0);
    let area = best.map_or(primary, |(area, _)| area);

    let width = state.bounds.width.max(MIN_SIZE.0).min(area.width);
    let height = state.bounds.height.max(MIN_SIZE.1).min(area.height);
    let (x, y) = match best {
        Some(_) => (
            state.bounds.x.clamp(area.x, area.x + area.width - width),
            state.bounds.y.clamp(area.y, area.y + area.height - height),
        ),
        None => (
            area.x + (area.width - width) / 2.0,
            area.y + (area.height - height) / 2.0,
        ),
    };
    State {
        bounds: Bounds {
            x: x.round(),
            y: y.round(),
            width: width.round(),
            height: height.round(),
        },
        mode: state.mode,
    }
}

/// The window a profile with no stored state gets: centred on the primary work
/// area, at the default size or as much of it as fits.
pub fn default_state(primary: Bounds) -> State {
    let width = DEFAULT_SIZE
        .0
        .min((primary.width - DEFAULT_MARGIN).max(MIN_SIZE.0.min(primary.width)));
    let height = DEFAULT_SIZE
        .1
        .min((primary.height - DEFAULT_MARGIN).max(MIN_SIZE.1.min(primary.height)));
    State {
        bounds: Bounds {
            x: (primary.x + (primary.width - width) / 2.0).round(),
            y: (primary.y + (primary.height - height) / 2.0).round(),
            width: width.round(),
            height: height.round(),
        },
        mode: Mode::Normal,
    }
}

/// Every connected display's usable area, and the primary one's.
///
/// `visibleFrame`, not `frame`: it excludes the menu bar and the Dock, so a
/// window placed inside it is a window whose titlebar can be grabbed.
pub fn work_areas(mtm: MainThreadMarker) -> (Vec<Bounds>, Bounds) {
    let screens = NSScreen::screens(mtm);
    let areas: Vec<Bounds> = screens
        .iter()
        .map(|screen| Bounds::from_rect(screen.visibleFrame()))
        .collect();
    // `screens[0]` is the screen with the menu bar, which is what "primary"
    // means for placement. A machine with no screens at all is not a machine
    // running this, but it costs one line not to panic on it.
    let primary = areas.first().copied().unwrap_or(Bounds {
        x: 0.0,
        y: 0.0,
        width: DEFAULT_SIZE.0,
        height: DEFAULT_SIZE.1,
    });
    (areas, primary)
}

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
    written: Instant,
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
        Mode::Fullscreen => unsafe {
            window.performSelector_withObject_afterDelay(objc2::sel!(toggleFullScreen:), None, 0.0);
        },
    }

    TRACKED.with(|tracked| {
        *tracked.borrow_mut() = Some(Tracker {
            window: window.clone(),
            path,
            normal: state.bounds,
            // Far enough in the past that the first move writes rather than
            // being coalesced away.
            written: Instant::now() - WRITE_INTERVAL,
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
            tracker.written = Instant::now();
        }
    });
}

/// Note the window's state, writing only if the last write is old enough.
fn touch() {
    TRACKED.with(|tracked| {
        if let Some(tracker) = tracked.borrow_mut().as_mut() {
            let state = observe(tracker);
            if tracker.written.elapsed() >= WRITE_INTERVAL {
                save(&tracker.path, state);
                tracker.written = Instant::now();
            }
        }
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

/// Follow the window without owning a delegate.
///
/// AppKit posts window notifications to the default `NSNotificationCenter`,
/// which is Core Foundation's local centre — the two are bridged. Reading them
/// this way is the same trick `commands` uses, and for the same reason: none of
/// these needs an object, only a function.
fn watch() {
    // Moves and live resizes arrive continuously and are coalesced; the rest
    // are the moments a gesture ends, and each writes.
    for (name, callback) in [
        (
            "NSWindowDidMoveNotification",
            moved as CFNotificationCallback,
        ),
        ("NSWindowDidResizeNotification", moved),
        ("NSWindowDidEndLiveResizeNotification", settled),
        ("NSWindowDidEnterFullScreenNotification", settled),
        ("NSWindowDidExitFullScreenNotification", left_full_screen),
        ("NSWindowDidResignKeyNotification", settled),
        ("NSApplicationWillTerminateNotification", settled),
    ] {
        let name = NSString::from_str(name);
        // SAFETY: `NSString` is toll-free bridged to `CFStringRef`. The name is
        // deliberately leaked: the observer is never removed, because it is
        // wanted for as long as the process runs, so the name has to outlive
        // it. The observer pointer is null and is handed back only to callbacks
        // that ignore it.
        unsafe {
            let name = Retained::into_raw(name).cast::<c_void>();
            CFNotificationCenterAddObserver(
                CFNotificationCenterGetLocalCenter(),
                std::ptr::null(),
                callback,
                name,
                std::ptr::null(),
                0,
            );
        }
    }
}

/// Delivered on the main thread: AppKit posts window notifications there.
extern "C" fn moved(
    _center: CFNotificationCenterRef,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    touch();
}

extern "C" fn settled(
    _center: CFNotificationCenterRef,
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
    _center: CFNotificationCenterRef,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: f64, y: f64, width: f64, height: f64) -> Bounds {
        Bounds {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_window_on_a_display_that_is_gone_comes_back_in_the_middle_of_one_that_is_not() {
        let laptop = area(0.0, 0.0, 1512.0, 916.0);
        // Where a second display used to be, far to the right.
        let stored = State {
            bounds: area(3000.0, 100.0, 1200.0, 800.0),
            mode: Mode::Normal,
        };

        let fitted = fit(stored, &[laptop], laptop);
        assert!(
            fitted.bounds.overlap(laptop) > 0.0,
            "must land somewhere visible: {fitted:?}"
        );
        // Centred, not shoved against the right edge.
        assert_eq!(fitted.bounds.x, ((1512.0 - 1200.0) / 2.0f64).round());
        assert_eq!(fitted.bounds.width, 1200.0);
        assert_eq!(fitted.mode, Mode::Normal);
    }

    #[test]
    fn a_window_that_still_overlaps_keeps_where_it_was() {
        let laptop = area(0.0, 0.0, 1512.0, 916.0);
        let stored = State {
            bounds: area(200.0, 50.0, 1000.0, 700.0),
            mode: Mode::Maximized,
        };
        let fitted = fit(stored, &[laptop], laptop);
        assert_eq!(fitted.bounds, stored.bounds);
        // The mode survives the fit: a maximized window comes back maximized
        // over the frame it would return to.
        assert_eq!(fitted.mode, Mode::Maximized);
    }

    #[test]
    fn a_window_bigger_than_the_display_is_cut_down_to_it() {
        let small = area(0.0, 0.0, 1280.0, 720.0);
        let stored = State {
            bounds: area(-200.0, -100.0, 2560.0, 1440.0),
            mode: Mode::Normal,
        };
        let fitted = fit(stored, &[small], small);
        assert_eq!(fitted.bounds.width, 1280.0);
        assert_eq!(fitted.bounds.height, 720.0);
        assert_eq!(fitted.bounds.x, 0.0);
        assert_eq!(fitted.bounds.y, 0.0);
    }

    #[test]
    fn the_display_with_the_most_of_the_window_wins() {
        let left = area(0.0, 0.0, 1512.0, 916.0);
        let right = area(1512.0, 0.0, 2560.0, 1440.0);
        // Mostly on the right-hand display.
        let stored = State {
            bounds: area(1400.0, 100.0, 1000.0, 700.0),
            mode: Mode::Normal,
        };
        let fitted = fit(stored, &[left, right], left);
        assert_eq!(fitted.bounds.x, 1512.0, "clamped into the right display");
        assert!(fitted.bounds.overlap(right) > fitted.bounds.overlap(left));
    }

    #[test]
    fn a_file_that_cannot_be_believed_is_refused_rather_than_repaired() {
        assert!(parse("{}").is_err(), "no bounds and no mode");
        assert!(
            parse(r#"{"formatVersion":2,"bounds":{"x":0,"y":0,"width":800,"height":600},"mode":"normal"}"#)
                .is_err(),
            "a later format is not reinterpreted"
        );
        assert!(
            parse(r#"{"bounds":{"x":0,"y":0,"width":0,"height":600},"mode":"normal"}"#).is_err(),
            "a zero-width window is not a window"
        );
        assert!(
            parse(r#"{"bounds":{"x":0,"y":0,"width":1e9,"height":600},"mode":"normal"}"#).is_err(),
            "an implausible edge is corruption, not a monitor"
        );
        assert!(
            parse(r#"{"bounds":{"x":0,"y":0,"width":800,"height":600},"mode":"minimized"}"#)
                .is_err(),
            "a mode this build does not have is not silently a default"
        );
        // No `formatVersion` is the shape the first build wrote, and it is the
        // same shape, so it still restores a window.
        let ok =
            parse(r#"{"bounds":{"x":10,"y":20,"width":800,"height":600},"mode":"fullscreen"}"#)
                .expect("v0 and v1 agree");
        assert_eq!(ok.mode, Mode::Fullscreen);
        assert_eq!(ok.bounds.x, 10.0);
    }

    #[test]
    fn what_is_written_is_what_comes_back() {
        let dir = std::env::temp_dir().join(format!(
            "gwnative-window-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("window.json");

        assert_eq!(load(&path), None, "nothing stored yet");
        let state = State {
            bounds: area(12.0, 34.0, 1280.0, 800.0),
            mode: Mode::Maximized,
        };
        save(&path, state);
        assert_eq!(load(&path), Some(state));

        // And a file that cannot be read does not survive to be re-read.
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(load(&path), None);
        assert!(!path.exists(), "a refused file is removed, not kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_default_window_fits_the_display_it_is_centred_on() {
        // A 13-inch laptop: the default 1280x800 fits with room to spare.
        let laptop = area(0.0, 0.0, 1512.0, 916.0);
        let state = default_state(laptop);
        assert_eq!(state.mode, Mode::Normal);
        assert_eq!(state.bounds.width, 1280.0);
        assert_eq!(state.bounds.height, 800.0);
        assert!(state.bounds.x > 0.0 && state.bounds.y > 0.0);

        // A display smaller than the default: the window shrinks to fit rather
        // than hanging off the bottom.
        let tiny = area(0.0, 0.0, 1024.0, 640.0);
        let state = default_state(tiny);
        assert!(state.bounds.width <= tiny.width, "{state:?}");
        assert!(state.bounds.height <= tiny.height, "{state:?}");
        assert!(state.bounds.x >= 0.0 && state.bounds.y >= 0.0, "{state:?}");
    }
}
