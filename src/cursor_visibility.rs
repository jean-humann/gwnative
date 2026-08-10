//! Keep WebKit from hiding the game cursor after keyboard input.
//!
//! GWOnMac leaves the Guild Wars bitmap on the canvas as a CSS cursor. That is
//! the correct ownership boundary here too: WebKit decodes the image and
//! AppKit presents the resulting hardware cursor. The macOS WebKit port adds
//! one behaviour Chromium does not, though: it calls
//! `NSCursor::setHiddenUntilMouseMoves(true)` while handling keyboard input.
//! A game interprets WASD as movement, not typing, so that policy makes a
//! perfectly valid cursor disappear until the player moves the mouse.
//!
//! This process is a dedicated one-window game host. When the player has the
//! native Guild Wars cursor enabled, replace that one AppKit class method with
//! a filter: requests to hide-until-movement are ignored, requests to reveal
//! are forwarded to AppKit's original implementation. Pointer lock and the
//! game's own hidden state still use `cursor: none` and are unaffected.

use std::sync::{Once, OnceLock};

use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{ClassType, sel};
use objc2_app_kit::NSCursor;

type SetHiddenUntilMouseMoves = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, bool);

static INSTALL: Once = Once::new();
static ORIGINAL: OnceLock<Imp> = OnceLock::new();

unsafe extern "C-unwind" fn keep_game_cursor_visible(
    receiver: *mut AnyObject,
    selector: Sel,
    hidden: bool,
) {
    if hidden {
        if std::env::var("GWNATIVE_CURSOR_AUDIT").is_ok_and(|value| value == "1") {
            note!("[cursor] suppressed WebKit's hide-until-mouse-moves request");
        }
        return;
    }
    let Some(original) = ORIGINAL.get() else {
        return;
    };
    // SAFETY: `original` came from the same Objective-C method this function
    // replaced, whose encoded signature is `v@:c` on this target.
    let original: SetHiddenUntilMouseMoves = unsafe { std::mem::transmute(*original) };
    unsafe { original(receiver, selector, false) };
}

fn replacement() -> Imp {
    // SAFETY: Objective-C IMP erases the arguments. The runtime calls it with
    // the exact class-method signature declared above.
    unsafe { std::mem::transmute::<SetHiddenUntilMouseMoves, Imp>(keep_game_cursor_visible) }
}

/// Install the process-wide policy before the `WKWebView` starts.
pub fn install() {
    // Clear a state inherited from native startup work before the filter is in
    // place. False remains forwardable after installation as well.
    NSCursor::setHiddenUntilMouseMoves(false);
    INSTALL.call_once(|| {
        let method = NSCursor::class()
            .class_method(sel!(setHiddenUntilMouseMoves:))
            .expect("AppKit's documented cursor visibility method is present");
        // SAFETY: `replacement` has the method's exact ABI and accepts every
        // value the original accepted. Installation happens on the main thread
        // before WebKit exists, so no call can observe the handoff halfway.
        let original = unsafe { method.set_implementation(replacement()) };
        ORIGINAL
            .set(original)
            .expect("the cursor visibility filter is installed once");
    });
}

#[cfg(test)]
mod tests {
    use objc2::{ClassType, sel};
    use objc2_app_kit::NSCursor;

    #[test]
    fn appkit_exposes_the_method_the_filter_owns() {
        assert!(
            NSCursor::class()
                .class_method(sel!(setHiddenUntilMouseMoves:))
                .is_some()
        );
    }
}
