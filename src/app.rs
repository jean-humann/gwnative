//! The application delegate: what happens when the player quits.
//!
//! Two behaviours, both of which gwnative did without and both of which cost a
//! player something real.
//!
//! Closing the window left a running process. With no delegate, AppKit's answer
//! to `applicationShouldTerminateAfterLastWindowClosed:` is NO, so the red
//! button produced an app with a menu bar, a Dock icon, no window, and no way
//! to get the game back.
//!
//! Quitting dropped whatever the client had not yet written. The client keeps
//! its account and character record in `app:/Gw.dat` inside IDBFS, and IDBFS
//! only persists a file when the client closes it — which for `Gw.dat` is
//! never, because it stays open for the session. Nothing in this app unloads
//! the page either: closing the window does not deallocate it, and `terminate:`
//! kills the web content process rather than unloading it, so the page's own
//! `pagehide` never fires. The flush therefore has to be pulled from here,
//! while the process is still alive to be asked.

use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;

use block2::{DynBlock, RcBlock};
use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationTerminateReply};
use objc2_foundation::{MainThreadMarker, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_web_kit::{WKContentWorld, WKWebView};

/// How long a quit will wait for the game's files to reach disk.
///
/// The sync itself has a far longer ceiling — 30 s, because a boot that hangs
/// is better than a boot that silently loses a file. A quit is the opposite
/// trade: past a couple of seconds the player is looking at an app that will
/// not close, and IndexedDB either committed long ago or is wedged.
const FLUSH_TIMEOUT_MS: u32 = 2000;

/// How much longer than the script's own timeout the host waits before giving
/// up on hearing back at all. Only reached when the page cannot answer.
const GRACE_MS: u32 = 1000;

/// Ask the page to flush, and answer with whether it did.
///
/// Raced against a timeout inside the page rather than out here, because
/// `callAsyncJavaScript` only calls back when its promise settles: a flush that
/// never resolves would otherwise never reply, and an app that refuses to quit
/// is worse than one that quits a moment early.
const FLUSH_SCRIPT: &str = "\
    const flush = window.gwFlushFilesystem?.(); \
    if (!flush) return 'no filesystem'; \
    const timeout = new Promise((resolve) => setTimeout(() => resolve('timed out'), TIMEOUT)); \
    return await Promise.race([flush.then((ok) => (ok ? 'flushed' : 'failed')), timeout]);";

pub struct Ivars {
    /// The page to pull the flush from.
    webview: Retained<WKWebView>,
    /// Whether the flush has already run. `applicationShouldTerminate:` is
    /// asked again after `replyToApplicationShouldTerminate:`, and without this
    /// the second pass would start a second flush and wait for it too.
    flushed: Cell<bool>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Delegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        /// The window is the app. There is nothing to come back to without it.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn close_is_quit(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, app: &NSApplication) -> NSApplicationTerminateReply {
            if self.ivars().flushed.replace(true) {
                return NSApplicationTerminateReply::TerminateNow;
            }

            let mtm = MainThreadMarker::from(self);
            let script =
                NSString::from_str(&FLUSH_SCRIPT.replace("TIMEOUT", &FLUSH_TIMEOUT_MS.to_string()));

            // Whatever happens, the app has to be told exactly once. A reply
            // that never arrives leaves a process with no window and a menu bar
            // that ignores Quit, which is worse than the lost file this waits
            // for — so there are two callers and a flag deciding which of them
            // is first.
            let reply = {
                let app = app.retain();
                let replied = Rc::new(Cell::new(false));
                move || {
                    if replied.replace(true) {
                        return;
                    }
                    app.replyToApplicationShouldTerminate(true);
                }
            };

            let finish = {
                let reply = reply.clone();
                RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
                    report(result, error);
                    reply();
                })
            };
            // The script has its own timeout, but that only covers a page that
            // is still running JavaScript. If the web content process has died
            // or is wedged, nothing calls the handler above at all.
            after(FLUSH_TIMEOUT_MS + GRACE_MS, move || {
                reply();
            });

            // SAFETY: main thread — AppKit consults its delegate there. The
            // block is retained by WebKit for the duration of the call.
            unsafe {
                self.ivars()
                    .webview
                    .callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                        &script,
                        None,
                        None,
                        &WKContentWorld::pageWorld(mtm),
                        Some(&finish),
                    );
            }
            NSApplicationTerminateReply::TerminateLater
        }
    }
);

/// Say what the flush did, once, on the way out.
///
/// Worth a line: this is the only moment the player's saved game reaches disk,
/// and "the login was forgotten again" is otherwise impossible to attribute.
fn report(result: *mut AnyObject, error: *mut NSError) {
    if !error.is_null() {
        // SAFETY: non-null, and WebKit hands the block a live error.
        let error = unsafe { &*error };
        note!("[gwnative] the game files could not be flushed: {error}");
        return;
    }
    if result.is_null() {
        return;
    }
    // SAFETY: non-null, and WebKit hands the block a live object.
    let outcome = unsafe { &*result };
    // The script answers with a string on every path, so anything else means it
    // did not reach its own return — worth saying, and worth not asserting.
    match outcome.downcast_ref::<NSString>() {
        Some(outcome) if outcome.to_string() == "flushed" => {
            note!("[gwnative] the game files are on disk");
        }
        Some(outcome) => note!("[gwnative] the game files were not flushed: {outcome}"),
        None => note!("[gwnative] the flush answered with something unreadable"),
    }
}

impl Delegate {
    fn new(mtm: MainThreadMarker, webview: &WKWebView) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            webview: webview.retain(),
            flushed: Cell::new(false),
        });
        // SAFETY: `NSObject`'s designated initializer, on a freshly allocated
        // instance whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}

/// Own the application's lifecycle. Main thread, before `run`.
///
/// The delegate is deliberately leaked. It has to outlive `run`, and the only
/// thing that could own it is the frame that is about to block in the run loop
/// for the life of the process.
pub fn own_lifecycle(mtm: MainThreadMarker, webview: &WKWebView) {
    let app = NSApplication::sharedApplication(mtm);
    let delegate = Delegate::new(mtm, webview);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    std::mem::forget(delegate);
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    /// `dispatch_get_main_queue()` is an inline function in C over this global,
    /// so there is no symbol to call — the queue itself is the symbol.
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_time(base: u64, delta: i64) -> u64;
    fn dispatch_after(when: u64, queue: *const c_void, block: &DynBlock<dyn Fn()>);
}

/// Run `work` on the main queue after `delay_ms`.
///
/// A dispatch timer rather than an `NSTimer`, because this has to fire during a
/// termination the run loop is already inside — a timer scheduled on the
/// default mode would not.
pub fn after(delay_ms: u32, work: impl Fn() + 'static) {
    let block = RcBlock::new(work);
    // SAFETY: `DISPATCH_TIME_NOW` is 0, the delay is in nanoseconds, and
    // `dispatch_after` copies the block.
    unsafe {
        dispatch_after(
            dispatch_time(0, i64::from(delay_ms) * 1_000_000),
            &raw const _dispatch_main_q,
            &block,
        );
    }
}

/// Hand `work` to the main queue, with `context` for it to take ownership of.
///
/// The one route from a worker thread into AppKit. Everything the loopback
/// server answers runs on a connection thread, and half of what the page asks
/// for — quit, an icon to redraw — is a main-thread-only call at the other end.
///
/// # Safety
///
/// `work` must be the only consumer of `context`, which libdispatch hands it
/// exactly once.
pub unsafe fn to_main(context: *mut c_void, work: extern "C" fn(*mut c_void)) {
    // SAFETY: `_dispatch_main_q` is libdispatch's own main queue object, and
    // the contract on the context is this function's caller's to keep.
    unsafe { dispatch_async_f(&raw const _dispatch_main_q, context, work) };
}

extern "C" fn terminate_on_main(_context: *mut c_void) {
    // SAFETY: this only runs on the main queue, which is the main thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    NSApplication::sharedApplication(mtm).terminate(None);
}

/// Quit, from any thread.
///
/// The client's own Exit is a clean `onExit(0)` in the page, and until now it
/// did nothing at all: the window stayed open on the last frame and the player
/// had to use the menu to quit a game that had already ended. The page reports
/// it over the loopback server, which answers on a worker thread, so the
/// request hops to the main queue before it touches AppKit.
///
/// Routed through `terminate:` rather than `exit` so it goes through the
/// delegate above and the game's files are written like any other quit.
pub fn request_quit() {
    // SAFETY: the work function takes no context, so there is nothing to own.
    unsafe { to_main(std::ptr::null_mut(), terminate_on_main) };
}
