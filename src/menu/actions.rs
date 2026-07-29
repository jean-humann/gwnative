//! The items nothing else can answer.
//!
//! A menu item with no target is performed by whatever is on the responder
//! chain, which is how cut, paste, full screen and quit are answered without
//! this build implementing any of them. What is left over is the handful
//! nobody else can answer — reload the page, write a problem report, mark a
//! slowdown, open the settings panel, say what this application is — and each
//! of those needs an
//! Objective-C object to receive it. That object is the only reason this half
//! exists; the bar it hangs off is next door.
//!
//! Every one of these runs on the main thread, because that is where AppKit
//! sends a menu action.

use std::ffi::c_void;
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

use crate::{app, diagnostics, release, report, settings, updater, window};

/// Whether a check is already running or already on screen.
///
/// The item is a menu item, so the way to press it twice is to press it twice.
/// [`release::check`] would answer the second press from its cache, but a
/// second alert stacked on the first is still two alerts for one question.
///
/// Shared with the automatic check, which is the case that needs it most: an
/// opted-in launch and an impatient player pressing the item can otherwise
/// arrive at [`present`] together and stack two alerts about one answer.
static ASKING: AtomicBool = AtomicBool::new(false);

/// Where a player who does not own the game buys it.
///
/// The root rather than a locale path, because the store redirects to the one
/// it thinks the visitor wants and this build has no business choosing an
/// English page for somebody whose Mac is not in English.
const STORE_URL: &str = "https://store.guildwars.com/";

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
    /// menu is still on after a relaunch — and the profile a problem report
    /// prints at the top of itself.
    settings: Arc<settings::Store>,
    /// The diagnostics log, for Report a Problem and Mark a Slowdown. Both
    /// write to it; one of them also reads it back.
    recorder: Arc<diagnostics::Recorder>,
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
        /// Deliberately unguarded. The key equivalent exists only because this
        /// menu item does, so it is not an ambient browser shortcut — and reload
        /// is the escape hatch for a client that has already stopped answering,
        /// exactly when another modal would be in the way.
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

        /// Where the game is sold.
        ///
        /// Not the project's link, and that is why it can be offered on every
        /// build: `PROJECT_URL` may be empty, but ArenaNet's store is where it
        /// has always been. Someone who found this client before they found the
        /// game needs an account, and a login screen that cannot say where one
        /// comes from is a dead end.
        #[unsafe(method(gwOpenStore:))]
        fn open_store(&self, _sender: Option<&AnyObject>) {
            open(STORE_URL);
        }

        /// Show the guide.
        ///
        /// Sent to the page for the same reason Settings is: the guide is a
        /// document about the panel, the overlay and the touch modes, all of
        /// which live there. A build with no repository can still open it,
        /// which a link to a markdown file on GitHub could not.
        #[unsafe(method(gwOpenGuide:))]
        fn open_guide(&self, _sender: Option<&AnyObject>) {
            self.evaluate(
                "window.dispatchEvent(new CustomEvent('gw:command', \
                 { detail: { name: 'guide-open' } }));",
            );
        }

        /// Ask whether the project has published something newer.
        ///
        /// Sparkle answers this where it is loaded, and answers it far better:
        /// a window with the release notes in it, and a button that installs.
        /// What follows is the fallback for every build that is not a bundle —
        /// the request takes up to five seconds and this is the thread drawing
        /// the game, so a worker asks and the answer comes back to the main
        /// queue to be shown. Offered only on a build that can answer at all —
        /// see [`super::updates_offered`].
        #[unsafe(method(gwCheckForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            if updater::check() {
                return;
            }
            ask(Arc::clone(&self.ivars().settings), Requested::ByThePlayer);
        }

        /// Ask which kind of problem it is, then do the thing that suits it.
        ///
        /// The two answers are not variations on each other. A crash, a failed
        /// download or a launch that never finished is already fully described
        /// by what is on disk, so the useful action is to write the report now.
        /// A stutter is not described by anything yet, because the sampler was
        /// running at its resting rate when it happened — so the useful action
        /// is to mark the moment and let the player press ⌘⇧M again each time
        /// it recurs, then export once at the end.
        #[unsafe(method(gwReportProblem:))]
        fn report_problem(&self, _sender: Option<&AnyObject>) {
            let mtm = MainThreadMarker::from(self);
            let alert = NSAlert::new(mtm);
            alert.setAlertStyle(NSAlertStyle::Informational);
            alert.setMessageText(&NSString::from_str("Report a problem"));
            alert.setInformativeText(&NSString::from_str(
                "For a crash, a failed download or a launch that never \
                 finished, save the report now.\n\nFor stutter or slow \
                 frames, mark it while it is happening — press ⌘⇧M each time \
                 — and save the report afterwards.",
            ));
            alert.addButtonWithTitle(&NSString::from_str("Save Report…"));
            alert.addButtonWithTitle(&NSString::from_str("Mark a Slowdown"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            match alert.runModal() {
                r if r == NSAlertFirstButtonReturn => self.save_report(),
                r if r == NSAlertFirstButtonReturn + 1 => self.mark(),
                _ => {}
            }
        }

        /// The moment the game misbehaved, ⌘⇧M.
        ///
        /// A menu item and a key equivalent because both are needed: the item is
        /// how it is discovered, and the key is the only one of the two that can
        /// be used without taking a hand off the game and losing the stutter
        /// that was being reported.
        #[unsafe(method(gwMarkSlowdown:))]
        fn mark_slowdown(&self, _sender: Option<&AnyObject>) {
            self.mark();
        }
    }
);

/// Who wanted the answer. What it decides is whether a check that found nothing
/// is allowed to interrupt.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Requested {
    /// The menu item. All three answers go on screen, because an item that
    /// sometimes does nothing visible reads as a broken item.
    ByThePlayer,
    /// An opted-in launch. Only [`release::interrupts`] reaches the screen; the
    /// rest is a log line, which is what keeps a permission to check from
    /// becoming a permission to talk.
    ByTheLaunch,
}

/// Ask on a worker thread and bring the answer back to the main queue.
///
/// The request takes up to five seconds and the caller is on the thread drawing
/// the game, so nothing here may run there. The time is recorded whichever way
/// the answer went and whoever asked for it: a manual check counts against the
/// daily automatic one, because a player who has just looked does not need the
/// next launch to look again on their behalf.
fn ask(settings: Arc<settings::Store>, who: Requested) {
    if ASKING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let notice = release::check();
        settings.remember_update_check(settings::now());
        if who == Requested::ByTheLaunch && !release::interrupts(&notice) {
            note!("[release] {notice:?} (checked at launch; nothing to say)");
            ASKING.store(false, Ordering::SeqCst);
            return;
        }
        let notice = Box::new(notice);
        // SAFETY: the box is leaked here and rebuilt exactly once, by the
        // function libdispatch hands it to.
        unsafe { app::to_main(Box::into_raw(notice).cast(), present) };
    });
}

/// Ask GitHub about this build, if this profile has asked to be asked and the
/// last answer is a day old.
///
/// Called once, after the window is up. Returns immediately on every launch
/// that is not due, which is all of them by default — see
/// [`crate::settings::Settings::auto_check_updates`], off in a fresh profile.
pub fn at_launch(settings: &Arc<settings::Store>) {
    // Sparkle keeps its own schedule, in its own preference, and started it
    // when the updater did. Asking GitHub as well would be a second request
    // about the same question, answered in a worse alert.
    if updater::started() {
        return;
    }
    // Nothing to compare against, so there is no request worth making. The same
    // rule that keeps the menu item off this build.
    if !super::updates_offered() {
        return;
    }
    let profile = settings.get();
    if !release::due(
        profile.auto_check_updates,
        profile.last_update_check_at,
        settings::now(),
    ) {
        return;
    }
    ask(Arc::clone(settings), Requested::ByTheLaunch);
}

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
        recorder: Arc<diagnostics::Recorder>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            webview: webview.retain(),
            settings,
            recorder,
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

    /// Record the moment, on both sides.
    ///
    /// The page is told as well as the file, because the counters that describe
    /// a stutter — frames, frame time, the audio underruns that go with it —
    /// are the page's, batched on a one-second timer. Left alone, the batch
    /// carrying the stutter would be flushed up to a second after the mark and
    /// might not be the batch the stutter happened in. Asking for it now is what
    /// makes the mark and the numbers describe the same instant.
    fn mark(&self) {
        self.ivars().recorder.mark("the player marked a slowdown");
        self.evaluate(
            "window.dispatchEvent(new CustomEvent('gw:command', \
             { detail: { name: 'diagnostics-mark' } }));",
        );
    }

    /// Write the report and select it in the Finder, which is where an
    /// attachment is picked up from anyway.
    fn save_report(&self) {
        let ivars = self.ivars();
        let dir = ivars.recorder.dir().to_owned();
        let path = match report::write(&ivars.recorder, &ivars.settings.get()) {
            Ok(path) => path,
            Err(e) => {
                note!("[report] the problem report was not written: {e}");
                // A modal, not a log line: the player pressed a button and is
                // waiting to be shown a file.
                let mtm = MainThreadMarker::from(self);
                let alert = NSAlert::new(mtm);
                alert.setAlertStyle(NSAlertStyle::Warning);
                alert.setMessageText(&NSString::from_str("The report was not written"));
                alert.setInformativeText(&NSString::from_str(&format!("{dir:?}: {e}")));
                alert.runModal();
                return;
            }
        };
        note!("[report] {}", path.display());
        let selected = NSWorkspace::sharedWorkspace().selectFile_inFileViewerRootedAtPath(
            Some(&NSString::from_str(&path.to_string_lossy())),
            &NSString::from_str(&dir.to_string_lossy()),
        );
        if !selected {
            note!(
                "[report] it could not be shown in the Finder; it is at {}",
                path.display()
            );
        }
    }
}
