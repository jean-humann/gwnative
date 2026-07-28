//! The items nothing else can answer.
//!
//! A menu item with no target is performed by whatever is on the responder
//! chain, which is how cut, paste, full screen and quit are answered without
//! this build implementing any of them. What is left over is the handful
//! nobody else can answer — reload the page, reveal the log, open the settings
//! panel, say what this application is — and each of those needs an
//! Objective-C object to receive it. That object is the only reason this half
//! exists; the bar it hangs off is next door.
//!
//! Every one of these runs on the main thread, because that is where AppKit
//! sends a menu action.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion,
    NSAboutPanelOptionCredits, NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication,
    NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSObject, NSObjectProtocol, NSString, NSURL,
};
use objc2_web_kit::WKWebView;

use crate::{app, release, settings, window};

/// Whether a check is already running or already on screen.
///
/// The item is a menu item, so the way to press it twice is to press it twice.
/// [`release::check`] would answer the second press from its cache, but a
/// second alert stacked on the first is still two alerts for one question.
static ASKING: AtomicBool = AtomicBool::new(false);

/// What the About panel says this application is.
///
/// The first line is the manifest's own description, so the sentence in the
/// panel and the sentence in `Cargo.toml` cannot drift apart. The rest is what
/// a player deserves to be told before they type an account name into it:
/// whose game this is, and that nobody official is behind this window.
const CREDITS: &str = concat!(
    env!("CARGO_PKG_DESCRIPTION"),
    ".\n\nGuild Wars is a trademark of NCSOFT Corporation. This is an unofficial \
     client and is not affiliated with, or endorsed by, NCSOFT or ArenaNet.\n\n\
     Licensed ",
    env!("CARGO_PKG_LICENSE"),
    "."
);

pub struct Ivars {
    /// The page, for the items that are really requests to it.
    webview: Retained<WKWebView>,
    /// The settings file, so that a diagnostics overlay switched on from the
    /// menu is still on after a relaunch.
    settings: Arc<settings::Store>,
    /// Where the diagnostics log is written, for Report a Problem.
    log_dir: PathBuf,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Actions` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub(super) struct Actions;

    unsafe impl NSObjectProtocol for Actions {}

    impl Actions {
        /// Open the page's settings panel.
        ///
        /// Sent as a command rather than performed here: every setting in it is
        /// one whose effect the page owns, and a native panel would need a
        /// second copy of the same four values kept in step with the first.
        #[unsafe(method(gwOpenSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            self.evaluate(
                "window.dispatchEvent(new CustomEvent('gw:command', \
                 { detail: { name: 'settings-open' } }));",
            );
        }

        #[unsafe(method(gwResetWindow:))]
        fn reset_window(&self, _sender: Option<&AnyObject>) {
            window::reset(MainThreadMarker::from(self));
        }

        /// Show or hide the overlay, and remember which.
        ///
        /// The setting is what the page reads at boot, so writing it is not
        /// bookkeeping on top of the real action — it *is* half the action. The
        /// other half is the live page, which read the setting once and will
        /// not read it again.
        #[unsafe(method(gwToggleDiagnostics:))]
        fn toggle_diagnostics(&self, _sender: Option<&AnyObject>) {
            let showing = !self.ivars().settings.get().show_diagnostics;
            match self
                .ivars()
                .settings
                .apply(&serde_json::json!({ "showDiagnostics": showing }))
            {
                Ok(_) => self.evaluate(&format!("window.gwLog?.({showing});")),
                Err(e) => note!("[gwnative] the diagnostics setting was not saved: {e}"),
            }
        }

        /// Load the page again from the top.
        ///
        /// Deliberately unguarded, unlike the Electron build, which asks first
        /// when a game socket is open. That question is worth asking there
        /// because ⌘R is a browser reflex and Chromium honours it everywhere.
        /// Here the key equivalent exists only because this item does, so
        /// nobody arrives at it by muscle memory — and the reload is the escape
        /// hatch for a client that has already stopped answering, which is
        /// exactly when a modal asking about sockets is in the way.
        #[unsafe(method(gwReloadGame:))]
        fn reload_game(&self, _sender: Option<&AnyObject>) {
            // SAFETY: main thread — AppKit sends menu actions there.
            unsafe {
                self.ivars().webview.reload();
            }
        }

        /// The standard About panel, told what it is looking at.
        ///
        /// The stock item reads a bundle's Info.plist, and every build that is
        /// not a bundle — `cargo run`, `scripts/signed-run`, every benchmark —
        /// has none, so it announces a program called `gwnative` with no
        /// version and no explanation. Supplying the three values it would have
        /// read makes the two kinds of build say the same thing, and everything
        /// supplied comes from `Cargo.toml`.
        #[unsafe(method(gwAbout:))]
        fn about(&self, _sender: Option<&AnyObject>) {
            let name = NSString::from_str("Guild Wars");
            let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
            // The panel takes credits as an attributed string and nothing else;
            // handed a plain one it shows an empty info area.
            let credits = NSAttributedString::from_nsstring(&NSString::from_str(CREDITS));
            // SAFETY: the three constants are AppKit's own option keys, and the
            // values are of the types their documentation names — two strings
            // and an attributed string.
            unsafe {
                let options = NSDictionary::from_slices(
                    &[
                        NSAboutPanelOptionApplicationName,
                        NSAboutPanelOptionApplicationVersion,
                        NSAboutPanelOptionCredits,
                    ],
                    &[&*name as &AnyObject, &*version, &*credits],
                );
                NSApplication::sharedApplication(MainThreadMarker::from(self))
                    .orderFrontStandardAboutPanelWithOptions(&options);
            }
        }

        #[unsafe(method(gwOpenWebsite:))]
        fn open_website(&self, _sender: Option<&AnyObject>) {
            open(release::PROJECT_URL);
        }

        /// Ask whether the project has published something newer.
        ///
        /// The request takes up to five seconds and this is the thread drawing
        /// the game, so a worker asks and the answer comes back to the main
        /// queue to be shown. Offered only on a build that says where it was
        /// published from — see [`super::updates_offered`].
        #[unsafe(method(gwCheckForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            if ASKING.swap(true, Ordering::SeqCst) {
                return;
            }
            std::thread::spawn(|| {
                let notice = Box::new(release::check());
                // SAFETY: the box is leaked here and rebuilt exactly once, by
                // the function libdispatch hands it to.
                unsafe { app::to_main(Box::into_raw(notice).cast(), present) };
            });
        }

        /// Show the player the diagnostics log rather than exporting one.
        ///
        /// The Electron build builds a report on demand because its diagnostics
        /// live in memory. Here they are already a file, written a line a
        /// second for the whole session, so an export would be a copy of
        /// something the player can attach to an issue directly. Revealing it
        /// selects the file in the Finder, which is where an attachment comes
        /// from anyway.
        #[unsafe(method(gwRevealDiagnostics:))]
        fn reveal_diagnostics(&self, _sender: Option<&AnyObject>) {
            let dir = &self.ivars().log_dir;
            let log = dir.join("gwnative.jsonl");
            // The directory is created at startup and the log appears with the
            // first sample, so both exist in practice. Selecting a file that
            // does not opens the folder, which is the right failure.
            let workspace = NSWorkspace::sharedWorkspace();
            let selected = workspace.selectFile_inFileViewerRootedAtPath(
                Some(&NSString::from_str(&log.to_string_lossy())),
                &NSString::from_str(&dir.to_string_lossy()),
            );
            if !selected {
                note!(
                    "[gwnative] the diagnostics log could not be shown; it is at {}",
                    log.display()
                );
            }
        }
    }
);

/// Hand a URL to whatever the player browses with.
fn open(url: &str) {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
        return;
    };
    NSWorkspace::sharedWorkspace().openURL(&url);
}

/// Put the answer on screen.
///
/// What it says is [`release::wording`]'s business; this is the window around
/// it. A second button exists only when there is somewhere to send the player,
/// which is why the answer is read back from `releases` rather than from the
/// notice again.
extern "C" fn present(context: *mut c_void) {
    // SAFETY: `check_for_updates` leaked exactly this box, and libdispatch runs
    // this once with it.
    let notice = *unsafe { Box::from_raw(context.cast::<release::Notice>()) };
    // SAFETY: this runs on the main queue, which is the main thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    let release::Wording {
        title,
        detail,
        releases,
    } = release::wording(&notice);
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(&detail));
    if releases.is_some() {
        alert.addButtonWithTitle(&NSString::from_str("Show Releases"));
        alert.addButtonWithTitle(&NSString::from_str("Later"));
    }
    note!("[release] {notice:?}");

    // The check is slow enough that the player may have gone elsewhere, and an
    // alert behind another window is the same as no alert.
    NSApplication::sharedApplication(mtm).activate();
    let chosen = alert.runModal();
    // After the modal, not before: until it closes there is still a check on
    // screen, and the answer to being asked again is the one already showing.
    ASKING.store(false, Ordering::SeqCst);
    if chosen == NSAlertFirstButtonReturn
        && let Some(releases) = releases
    {
        open(&releases);
    }
}

impl Actions {
    pub(super) fn new(
        mtm: MainThreadMarker,
        webview: &WKWebView,
        settings: Arc<settings::Store>,
        log_dir: PathBuf,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            webview: webview.retain(),
            settings,
            log_dir,
        });
        // SAFETY: `NSObject`'s designated initializer, on a freshly allocated
        // instance whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }

    /// Run one line in the page. Main thread; every caller is a menu action.
    fn evaluate(&self, script: &str) {
        // SAFETY: main thread — AppKit sends menu actions there.
        unsafe {
            self.ivars()
                .webview
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), None);
        }
    }
}
