//! The Dock icon while the game image downloads.
//!
//! Downloading all 4.2 GB is the one thing this application does that a player
//! is right to walk away from, and the window they were watching it in is
//! behind whatever they went to do instead. Two things follow. The icon has to
//! carry the progress the window is no longer showing — it is the only part of
//! an application still visible from another app — and the Mac must not decide
//! partway through that an idle machine may as well sleep.
//!
//! Both are tied to the sweep and to nothing else. The tile is installed when a
//! download starts and taken down when it ends, so a game running with no
//! download in it has no periodic work here at all: the tick reschedules itself
//! only while the sweep is running, and the last one clears the icon and lets
//! the machine sleep again.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSBezierPath, NSColor, NSView};
use objc2_foundation::{
    MainThreadMarker, NSActivityOptions, NSObjectProtocol, NSPoint, NSProcessInfo, NSRect, NSSize,
    NSString,
};

use crate::chunks::ChunkStore;

/// How often the icon is redrawn. The Dock renders it at 128 points across, so
/// a percent of a 4.2 GB download is barely a pixel of bar: anything faster
/// redraws the same image.
const TICK_MS: u32 = 1000;

define_class!(
    // SAFETY:
    // - NSView has no subclassing requirements beyond drawing in `drawRect:`.
    // - `Tile` does not implement `Drop`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Cell<f64>]
    struct Tile;

    impl Tile {
        /// The application icon with a progress bar across the bottom of it.
        ///
        /// Drawn rather than badged. A badge is a count — a number in a red
        /// circle — and this is a fraction; the shape people already read as
        /// "this is still going" is a bar, which is what the Dock shows for a
        /// copy in the Finder or a download in Safari.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let mtm = MainThreadMarker::from(self);
            // The content view replaces the icon rather than covering it, so
            // the icon has to be drawn here or the Dock shows a bar floating in
            // nothing.
            if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
                icon.drawInRect(bounds);
            }

            let inset = bounds.size.width * 0.12;
            let height = (bounds.size.height * 0.09).round().max(4.0);
            let width = bounds.size.width - inset * 2.0;
            let track = NSRect::new(
                NSPoint::new(bounds.origin.x + inset, bounds.origin.y + height * 0.9),
                NSSize::new(width, height),
            );
            let radius = height / 2.0;

            // A track under the fill, because the first percent of a bar drawn
            // straight onto an icon is invisible against whatever the icon
            // happens to be behind it.
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.55).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(track, radius, radius).fill();

            let fraction = self.ivars().get().clamp(0.0, 1.0);
            // Never narrower than it is tall: below that the rounded ends meet
            // and the fill stops looking like a bar at all.
            let filled = (width * fraction).max(height).min(width);
            let bar = NSRect::new(track.origin, NSSize::new(filled, height));
            NSColor::controlAccentColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bar, radius, radius).fill();
        }
    }
);

impl Tile {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(0.0));
        // The Dock resizes the content view to its tile; the frame given here
        // only has to be non-empty so the first draw has somewhere to happen.
        unsafe {
            msg_send![super(this), initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(128.0, 128.0),
            )]
        }
    }
}

/// What the icon is showing, for as long as it is showing it.
///
/// Main-thread only, which is why a thread-local is enough: every read and
/// every write below happens on the main queue, and a second thread reaching
/// this would already be a bug about AppKit rather than about this cell.
struct Showing {
    tile: Retained<Tile>,
    /// The promise to the power manager, released when the download ends.
    activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

thread_local! {
    static SHOWING: RefCell<Option<Showing>> = const { RefCell::new(None) };
}

/// Follow `store`'s full download on the Dock icon until it finishes.
///
/// Called from the worker thread that started the sweep, so the work hops to
/// the main queue before it touches AppKit. Starting a second follow while one
/// is running is harmless — the tick that finds the icon already installed
/// reuses it — but there is only one sweep, so it does not happen.
pub fn follow(store: &Arc<ChunkStore>) {
    let context = Box::into_raw(Box::new(Arc::clone(store))).cast::<c_void>();
    // SAFETY: `begin` takes ownership of exactly this box, and libdispatch
    // calls it once.
    unsafe { crate::app::to_main(context, begin) };
}

extern "C" fn begin(context: *mut c_void) {
    // SAFETY: the pointer is the box `follow` leaked, and this runs once.
    let store = unsafe { Box::from_raw(context.cast::<Arc<ChunkStore>>()) };
    tick(&store);
}

fn tick(store: &Arc<ChunkStore>) {
    // SAFETY: only ever reached from the main queue, which is the main thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let (_, _, running) = store.prefetch_progress();
    let total = store.chunk_count();
    if !running || total == 0 {
        clear(mtm);
        return;
    }
    // Residency rather than the sweep's own counter, so the icon and the bar in
    // the window are measuring the same thing: how much of the game is on this
    // Mac, not how far down the list this particular sweep has walked.
    show(mtm, store.resident_count() as f64 / total as f64);
    let store = Arc::clone(store);
    crate::app::after(TICK_MS, move || tick(&store));
}

fn show(mtm: MainThreadMarker, fraction: f64) {
    SHOWING.with_borrow_mut(|showing| {
        let showing = showing.get_or_insert_with(|| {
            let tile = Tile::new(mtm);
            NSApplication::sharedApplication(mtm)
                .dockTile()
                .setContentView(Some(&tile));
            Showing {
                tile,
                // Idle *system* sleep, not display sleep: a download does not
                // need the screen on, and a player who shut the lid asked for
                // something this has no business overriding. The reason string
                // is what `pmset -g assertions` shows, so it is written for
                // someone wondering why their Mac is awake.
                activity: NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                    NSActivityOptions::IdleSystemSleepDisabled
                        | NSActivityOptions::SuddenTerminationDisabled,
                    &NSString::from_str("Downloading the Guild Wars game image"),
                ),
            }
        });
        showing.tile.ivars().set(fraction);
        NSApplication::sharedApplication(mtm).dockTile().display();
    });
}

fn clear(mtm: MainThreadMarker) {
    SHOWING.with_borrow_mut(|showing| {
        let Some(showing) = showing.take() else {
            return;
        };
        let tile = NSApplication::sharedApplication(mtm).dockTile();
        tile.setContentView(None);
        // Without this the Dock keeps compositing the view it was handed, bar
        // and all, until something else invalidates the tile.
        tile.display();
        // SAFETY: the token came from `beginActivityWithOptions:reason:` above
        // and is ended once.
        unsafe { NSProcessInfo::processInfo().endActivity(&showing.activity) };
    });
}
