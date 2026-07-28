//! What a launch that cannot continue says to someone who is not watching a
//! terminal.
//!
//! A `cargo run` build reports a failed start on stderr and the person who
//! typed the command reads it. A bundle has no terminal: double-clicking an
//! application that writes a line and exits 1 looks exactly like
//! double-clicking nothing at all, which is the version of this failure a
//! player actually meets.
//!
//! The line is still written, always and first — the log is what ends up
//! attached to an issue, and the modal is what the player sees.

use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{MainThreadMarker, NSString};

/// End the process, having said why.
///
/// `on_screen` is false for the invocations that have a terminal by definition
/// — `serve` and `sync` are run by scripts, and a modal nobody is there to
/// click is not a message, it is a hang.
pub fn fatal(on_screen: bool, message: &str, detail: &str) -> ! {
    note!("[gwnative] {message}: {detail}");
    if on_screen && let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        // The policy is normally set further down `main`, which this failure
        // never reaches. Without it the process is an accessory: the alert
        // opens behind whatever the player was looking at, with nothing in the
        // Dock to say where it went.
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Critical);
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(detail));
        // Named for what it does. The stock button says OK, which is the wrong
        // word for "the application is about to close".
        alert.addButtonWithTitle(&NSString::from_str("Quit"));
        app.activate();
        alert.runModal();
    }
    std::process::exit(1);
}
