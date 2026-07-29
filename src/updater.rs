//! Sparkle, when it is there.
//!
//! [`crate::release`] can tell a player that something newer exists and open a
//! web page. That is the whole of what it can do, and it is not an update: the
//! player still downloads a disk image, still drags it over the copy that is
//! running, still finds out afterwards what changed. This module is the other
//! half — the framework that reads a signed feed, shows the release notes for
//! the version it found, downloads it in the background and installs it on
//! quit.
//!
//! ## Why it is looked up rather than called
//!
//! Nothing here names a Sparkle symbol. Every call goes through the
//! Objective-C runtime by selector, and every entry point starts by asking
//! whether the class exists at all. That is not caution about the framework; it
//! is a fact about this project's builds. `Contents/Frameworks` only exists in
//! a bundle, and most of what gets run here is not one — `cargo run`, the
//! benchmarks, the test harness. `build.rs` links Sparkle weakly so those still
//! start; this file is what makes them still *work*, by answering "no" and
//! letting [`crate::release`] handle the question the older way.
//!
//! So there are two update paths in this application on purpose, and which one
//! a build takes is decided by whether the framework loaded. They are not
//! alternatives to choose between: the second exists because the first cannot
//! be present in a build that was never packaged.
//!
//! ## Who owns the two switches
//!
//! Sparkle keeps `automaticallyChecksForUpdates` and
//! `automaticallyDownloadsUpdates` in the application's user defaults and says,
//! in as many words, not to keep a second copy — because its own interface
//! changes them. The update window carries an "install automatically in the
//! future" checkbox, and a launch that pushed this profile's values over the
//! top would quietly undo the box the player had just ticked.
//!
//! This project does keep a second copy, in `settings.json`, because the
//! settings panel is a web page and cannot read `NSUserDefaults`. What keeps
//! that honest is the direction of the copy. Sparkle's defaults are the truth;
//! [`start`] reads them and writes the profile to match. The profile is pushed
//! the other way in exactly two cases: the first launch after this shipped,
//! where Sparkle has no stored answer and the player's existing opt-in would
//! otherwise be lost, and [`follow`], which is a player moving the switch in the
//! panel a moment ago.
//!
//! What is copied is what the player asked for, not what Sparkle will do about
//! it tonight — the two differ, and copying the wrong one silently discards an
//! opt-in. See [`intent`].

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{MainThreadMarker, NSBundle, NSError, NSString, NSUserDefaults};

use crate::{app, settings};

/// The Info.plist keys Sparkle cannot run without: where the feed is, and the
/// public half of the key every item in it must be signed with. `scripts/bundle`
/// writes both, and only when `packaging/sparkle/public-key.txt` exists — so a
/// bundle built before anyone generated a key has neither, and this module
/// declines to start rather than letting Sparkle put a "contact the developer"
/// alert on a player's screen.
const FEED: &str = "SUFeedURL";
const KEY: &str = "SUPublicEDKey";

/// The user-defaults names behind the two switches. Read to find out whether
/// Sparkle has an answer of its own yet; never written here — the framework
/// owns the writing, through its properties.
const CHECKS_DEFAULT: &str = "SUEnableAutomaticChecks";
const DOWNLOADS_DEFAULT: &str = "SUAutomaticallyUpdate";

thread_local! {
    /// The running updater, for the life of the process.
    ///
    /// Thread-local rather than a `static`, and the type is doing the work:
    /// `SPUUpdater` must be used from the main thread, `Retained` is not
    /// `Send`, and a thread local is the one container that cannot be reached
    /// from the wrong thread to begin with. Every accessor below therefore
    /// needs no lock and no marker — being able to see this at all is proof of
    /// where you are.
    static RUNNING: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// Whether this build could run Sparkle at all.
///
/// Three things have to be true and each is false for a different reason: the
/// class is absent from anything that is not a bundle, and the two Info.plist
/// keys are absent from a bundle built without a signing key. Answered here so
/// that the settings panel and the menu ask one question rather than three.
pub fn available() -> bool {
    AnyClass::get(c"SPUUpdater").is_some() && info_has(FEED) && info_has(KEY)
}

/// Whether the updater is up. False in every build [`available`] is false in,
/// and in one it is true in: a bundle whose `startUpdater:` failed.
pub fn started() -> bool {
    RUNNING.with(|running| running.borrow().is_some())
}

/// Start the updater, and settle the two switches between it and the profile.
///
/// Called once, from the main thread, before the run loop. Sparkle schedules
/// its first check on a timer rather than performing one here, so this returns
/// immediately and the network happens after the window is up.
///
/// Returns whether the updater is now running, which is what decides whether
/// [`crate::release`] does anything this launch.
pub fn start(_mtm: MainThreadMarker, store: &Arc<settings::Store>) -> bool {
    if !available() {
        return false;
    }
    let Some(updater) = build() else {
        return false;
    };

    // The migration case, and only it. See the module docs: Sparkle's stored
    // answer wins wherever there is one, and the profile fills in where there
    // is not — which is the launch after this shipped, and a fresh install.
    let profile = store.get();
    let stored = intent(&updater);
    let wanted = reconcile(
        (profile.auto_check_updates, profile.auto_install_updates),
        stored,
        (default_set(CHECKS_DEFAULT), default_set(DOWNLOADS_DEFAULT)),
    );
    if wanted != stored {
        set_switches(&updater, wanted);
    }

    // Read back rather than assume: `set_switches` may not have run, and where
    // it did the framework is entitled to have stored something else. [`intent`]
    // rather than [`switches`], because this is what the settings panel will
    // show and the panel shows what was asked for.
    let (checks, downloads) = intent(&updater);
    store.remember_update_preferences(checks, downloads);
    note!(
        "[sparkle] the updater is running (checks: {}, installs on its own: {})",
        if checks { "on" } else { "off" },
        if downloads { "on" } else { "off" },
    );

    RUNNING.with(|running| *running.borrow_mut() = Some(updater));
    true
}

/// The Help menu's item: check now, verbosely, with the release notes.
///
/// Returns whether Sparkle took it. False means this build has no updater and
/// the caller should fall back to [`crate::release`], which is the whole reason
/// this answers rather than just doing nothing.
pub fn check() -> bool {
    RUNNING.with(|running| {
        let Some(updater) = running.borrow().clone() else {
            return false;
        };
        // SAFETY: main thread, by the thread local. `checkForUpdates` takes
        // nothing and returns nothing.
        unsafe {
            let _: () = msg_send![&*updater, checkForUpdates];
        }
        true
    })
}

/// Let the updater follow a settings change.
///
/// Called for every accepted patch rather than only the two that matter, which
/// is why [`apply`] compares before it writes: setting either property resets
/// Sparkle's schedule, and changing the render scale should not postpone a
/// check. The hop exists because the panel's change arrives on a connection
/// thread and these properties are main-thread-only.
pub fn follow(checks: bool, downloads: bool) {
    if !started() && !available() {
        return;
    }
    let wanted = Box::new((checks, downloads));
    // SAFETY: the box is leaked here and rebuilt exactly once, by the function
    // libdispatch hands it to.
    unsafe { app::to_main(Box::into_raw(wanted).cast(), apply) };
}

/// The main-thread half of [`follow`].
extern "C" fn apply(context: *mut c_void) {
    // SAFETY: `follow` leaked exactly this box, and libdispatch runs this once
    // with it.
    let wanted = *unsafe { Box::from_raw(context.cast::<(bool, bool)>()) };
    RUNNING.with(|running| {
        if let Some(updater) = running.borrow().as_ref()
            && intent(updater) != wanted
        {
            set_switches(updater, wanted);
            note!(
                "[sparkle] checks: {}, installs on its own: {}",
                if wanted.0 { "on" } else { "off" },
                if wanted.1 { "on" } else { "off" },
            );
        }
    });
}

/// Make the updater and its interface, and start it.
///
/// `SPUUpdater` rather than `SPUStandardUpdaterController`, which is the
/// documented shortcut for exactly this pair. The shortcut's `startUpdater`
/// returns nothing and answers a misconfigured bundle by putting an alert in
/// front of the player telling them to contact the developer — the one outcome
/// worth avoiding here, because "misconfigured" covers every build that simply
/// has no signing key yet. This one hands the error to the log.
fn build() -> Option<Retained<AnyObject>> {
    let updater_class = AnyClass::get(c"SPUUpdater")?;
    let driver_class = AnyClass::get(c"SPUStandardUserDriver")?;
    let bundle = NSBundle::mainBundle();

    // SAFETY: both initialisers are the ones the framework's headers declare,
    // sent to a freshly allocated instance of the class that declares them,
    // with an object for every object argument and nil for the two delegates —
    // documented nullable, and this build has nothing to say to either.
    unsafe {
        let driver: Allocated<AnyObject> = msg_send![driver_class, alloc];
        let driver: Retained<AnyObject> =
            msg_send![driver, initWithHostBundle: &*bundle, delegate: None::<&AnyObject>];

        let updater: Allocated<AnyObject> = msg_send![updater_class, alloc];
        let updater: Retained<AnyObject> = msg_send![
            updater,
            initWithHostBundle: &*bundle,
            applicationBundle: &*bundle,
            userDriver: &*driver,
            delegate: None::<&AnyObject>,
        ];

        let mut error: *mut NSError = ptr::null_mut();
        let started: Bool = msg_send![&*updater, startUpdater: &mut error];
        if !started.as_bool() {
            // The error is autoreleased and this frame drains no pool, so it is
            // alive for as long as it takes to read.
            let why = error.as_ref().map_or_else(
                || "no reason given".to_owned(),
                |e| e.localizedDescription().to_string(),
            );
            note!("[sparkle] the updater did not start: {why}");
            return None;
        }
        Some(updater)
    }
}

/// Read both switches as Sparkle will act on them.
fn switches(updater: &AnyObject) -> (bool, bool) {
    // SAFETY: main thread; two `BOOL` properties the header declares, neither
    // taking an argument.
    unsafe {
        let checks: Bool = msg_send![updater, automaticallyChecksForUpdates];
        let downloads: Bool = msg_send![updater, automaticallyDownloadsUpdates];
        (checks.as_bool(), downloads.as_bool())
    }
}

/// Read both switches as the player last set them, which is not the same thing.
///
/// `automaticallyDownloadsUpdates` answers with the effective behaviour rather
/// than the stored one: it is false whenever checking is off, however it was
/// last set. That is the right answer to "will anything download tonight" and
/// the wrong one to write into the profile, because writing it loses the
/// opt-in. A player who turns checking off has a launch persist "installs on
/// its own: no" over their yes, and turning checking back on then finds the
/// answer already changed for them — measured, not theorised.
///
/// So the stored value is read from the default Sparkle itself writes it to,
/// and the property is consulted only where nothing is stored. Checking needs
/// none of this; its property reports what it was set to.
fn intent(updater: &AnyObject) -> (bool, bool) {
    let (checks, downloads) = switches(updater);
    if !default_set(DOWNLOADS_DEFAULT) {
        return (checks, downloads);
    }
    let key = NSString::from_str(DOWNLOADS_DEFAULT);
    let stored = NSUserDefaults::standardUserDefaults().boolForKey(&key);
    (checks, stored)
}

/// Which of the two answers to keep, for each switch independently.
///
/// `answered` is whether Sparkle has a stored answer at all. Where it has one it
/// wins, because its own interface can change it and this profile must not
/// overwrite what the player just ticked there. Where it has none — a fresh
/// install, or the first launch after Sparkle shipped — the profile is all there
/// is, and it carries an opt-in that predates the framework.
fn reconcile(profile: (bool, bool), stored: (bool, bool), answered: (bool, bool)) -> (bool, bool) {
    (
        if answered.0 { stored.0 } else { profile.0 },
        if answered.1 { stored.1 } else { profile.1 },
    )
}

/// Write both switches. Persists in the application's user defaults, which is
/// where Sparkle reads them from at the next launch.
fn set_switches(updater: &AnyObject, (checks, downloads): (bool, bool)) {
    // SAFETY: main thread; the setters for the two properties above.
    unsafe {
        let _: () = msg_send![updater, setAutomaticallyChecksForUpdates: Bool::new(checks)];
        let _: () = msg_send![updater, setAutomaticallyDownloadsUpdates: Bool::new(downloads)];
    }
}

/// Whether the main bundle's Info.plist carries `key`.
fn info_has(key: &str) -> bool {
    let key = NSString::from_str(key);
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(&key)
        .is_some()
}

/// Whether Sparkle has already stored an answer for `key`.
///
/// `objectForKey:` rather than `boolForKey:` on purpose — the difference
/// between "off" and "never asked" is the whole question, and `boolForKey:`
/// answers both with false. An Info.plist default does not count as an answer,
/// which is right: it is what the developer wanted, not what the player said.
fn default_set(key: &str) -> bool {
    let key = NSString::from_str(key);
    NSUserDefaults::standardUserDefaults()
        .objectForKey(&key)
        .is_some()
}

// Only [`reconcile`] is testable here, and it is the only part worth testing:
// everything else in this module is a message to a class that exists in a
// bundle and nowhere else, so a test binary can reach none of it. What these
// cover is the decision that runs once per launch and is invisible when it is
// wrong — the profile and the framework disagreeing about who asked for what.
#[cfg(test)]
mod tests {
    use super::reconcile;

    const NEITHER: (bool, bool) = (false, false);
    const BOTH: (bool, bool) = (true, true);

    // The launch after Sparkle shipped. The framework has stored nothing, so
    // whatever the player opted into before it existed is what starts.
    #[test]
    fn an_opt_in_that_predates_sparkle_survives_meeting_it() {
        assert_eq!(reconcile((true, false), NEITHER, NEITHER), (true, false));
        assert_eq!(reconcile(BOTH, NEITHER, NEITHER), BOTH);
        assert_eq!(reconcile(NEITHER, BOTH, NEITHER), NEITHER);
    }

    // Sparkle's update window can turn both of these on itself, and a launch
    // that pushed the profile over the top would undo the box the player ticked
    // there a moment ago.
    #[test]
    fn what_sparkle_has_been_told_outranks_the_profile() {
        assert_eq!(reconcile(NEITHER, BOTH, BOTH), BOTH);
        assert_eq!(reconcile(BOTH, NEITHER, BOTH), NEITHER);
    }

    // Two switches, two independent answers: Sparkle is asked about checking
    // when it is first run and about installing only later, so one being stored
    // says nothing about the other.
    #[test]
    fn each_switch_is_decided_on_its_own() {
        assert_eq!(reconcile(BOTH, NEITHER, (true, false)), (false, true));
        assert_eq!(reconcile(BOTH, NEITHER, (false, true)), (true, false));
    }

    // The regression this module was fixed for. Checking off does not mean the
    // player took back the install opt-in — Sparkle reports it as off because
    // nothing can install when nothing checks, and persisting that report would
    // turn "not now" into "no", so that turning checking back on would find the
    // answer already changed.
    #[test]
    fn turning_checking_off_does_not_take_back_the_install_opt_in() {
        assert_eq!(reconcile(BOTH, (false, true), BOTH), (false, true));
        // And back on again, with the opt-in still where the player left it.
        assert_eq!(reconcile((true, true), (true, true), BOTH), BOTH);
    }
}
