//! The application menu bar.
//!
//! It is not decoration. A ⌘-key is delivered as a key equivalent, and what
//! turns ⌘V into the `paste:` action is an Edit menu item claiming it — with no
//! main menu, pasting an account name into the login field does nothing, and so
//! does ⌘Q. WKWebView already implements every one of those actions; the menu
//! only supplies the route to them.
//!
//! The rest of it is things a player has no other way to reach. Most menu items
//! here have no target: AppKit sends a nil-targeted action up the responder
//! chain, so `toggleFullScreen:` finds the key window and `hide:` finds the
//! application without either being named. The ones that do have a target are
//! the ones nothing in AppKit implements, and they are why this file owns a
//! class at all.

use std::path::PathBuf;
use std::sync::Arc;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion,
    NSAboutPanelOptionCredits, NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSObject, NSObjectProtocol, NSString, NSURL,
};
use objc2_web_kit::WKWebView;

use crate::{settings, window};

/// Where the project lives, taken from `Cargo.toml` rather than written here.
///
/// It is empty until the package declares a `repository`, and the Help item is
/// left out while it is. A menu that offers to open the project website and
/// then opens nothing — or worse, opens someone else's repository because the
/// URL was guessed — is worse than a Help menu with one item in it.
const WEBSITE: &str = env!("CARGO_PKG_REPOSITORY");

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
    struct Actions;

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
            let Some(url) = NSURL::URLWithString(&NSString::from_str(WEBSITE)) else {
                return;
            };
            NSWorkspace::sharedWorkspace().openURL(&url);
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

impl Actions {
    fn new(
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

/// Build the menu bar and hand it to the application. Main thread, before
/// `run`.
///
/// The target is deliberately leaked, for the same reason the app delegate is:
/// it has to outlive `run`, and the only thing that could own it is the frame
/// about to block in the run loop for the life of the process.
pub fn install(
    mtm: MainThreadMarker,
    webview: &WKWebView,
    settings: Arc<settings::Store>,
    log_dir: PathBuf,
) -> Retained<NSMenu> {
    let actions = Actions::new(mtm, webview, settings, log_dir);
    let menu = build(mtm, &actions);
    std::mem::forget(actions);
    menu
}

fn build(mtm: MainThreadMarker, actions: &Actions) -> Retained<NSMenu> {
    /// An item AppKit already knows how to perform, sent up the responder
    /// chain.
    fn item(
        mtm: MainThreadMarker,
        title: &str,
        action: Sel,
        key: &str,
        modifiers: Option<NSEventModifierFlags>,
    ) -> Retained<NSMenuItem> {
        // SAFETY: the selectors are AppKit's own first-responder actions, sent
        // down the responder chain rather than to a specific object.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        if let Some(modifiers) = modifiers {
            item.setKeyEquivalentModifierMask(modifiers);
        }
        item
    }

    /// An item this file performs itself. Distinguished from `item` only by
    /// having a target: with one, AppKit stops walking the responder chain and
    /// sends the action straight there.
    fn ours(
        mtm: MainThreadMarker,
        actions: &Actions,
        title: &str,
        action: Sel,
        key: &str,
        modifiers: Option<NSEventModifierFlags>,
    ) -> Retained<NSMenuItem> {
        let item = item(mtm, title, action, key, modifiers);
        // SAFETY: the target is leaked by `install`, so it outlives every menu
        // item holding this weak reference to it.
        unsafe { item.setTarget(Some(actions)) };
        item
    }

    fn submenu(mtm: MainThreadMarker, title: &str, items: &[&NSMenuItem]) -> Retained<NSMenuItem> {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for item in items {
            menu.addItem(item);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        holder
    }

    let command = NSEventModifierFlags::Command;
    let menu = NSMenu::new(mtm);

    // The first submenu is the application menu whatever it is titled; macOS
    // substitutes the process name for the title it is given.
    menu.addItem(&submenu(
        mtm,
        "Guild Wars",
        &[
            &ours(mtm, actions, "About Guild Wars", sel!(gwAbout:), "", None),
            &NSMenuItem::separatorItem(mtm),
            &ours(mtm, actions, "Settings…", sel!(gwOpenSettings:), ",", None),
            &NSMenuItem::separatorItem(mtm),
            &item(mtm, "Hide Guild Wars", sel!(hide:), "h", None),
            &item(
                mtm,
                "Hide Others",
                sel!(hideOtherApplications:),
                "h",
                Some(command | NSEventModifierFlags::Option),
            ),
            &item(mtm, "Show All", sel!(unhideAllApplications:), "", None),
            &NSMenuItem::separatorItem(mtm),
            &item(mtm, "Quit Guild Wars", sel!(terminate:), "q", None),
        ],
    ));

    // Cut and copy matter as much as paste: the client's own fields are these
    // proxies, so the player expects the ordinary Mac editing keys in them.
    menu.addItem(&submenu(
        mtm,
        "Edit",
        &[
            &item(mtm, "Undo", sel!(undo:), "z", None),
            &item(
                mtm,
                "Redo",
                sel!(redo:),
                "z",
                Some(command | NSEventModifierFlags::Shift),
            ),
            &NSMenuItem::separatorItem(mtm),
            &item(mtm, "Cut", sel!(cut:), "x", None),
            &item(mtm, "Copy", sel!(copy:), "c", None),
            &item(mtm, "Paste", sel!(paste:), "v", None),
            &item(mtm, "Select All", sel!(selectAll:), "a", None),
        ],
    ));

    // Full screen is the only one of these AppKit performs. It is here because
    // without a menu item there is no ⌃⌘F, and the green button is the only way
    // in — which is a poor only way for a game.
    menu.addItem(&submenu(
        mtm,
        "View",
        &[
            &item(
                mtm,
                "Enter Full Screen",
                sel!(toggleFullScreen:),
                "f",
                Some(command | NSEventModifierFlags::Control),
            ),
            &ours(
                mtm,
                actions,
                "Reset Window Size and Position",
                sel!(gwResetWindow:),
                "",
                None,
            ),
            &NSMenuItem::separatorItem(mtm),
            &ours(
                mtm,
                actions,
                "Toggle Diagnostics",
                sel!(gwToggleDiagnostics:),
                "",
                None,
            ),
            &ours(mtm, actions, "Reload Game", sel!(gwReloadGame:), "r", None),
        ],
    ));

    let log = ours(
        mtm,
        actions,
        "Show Diagnostics Log…",
        sel!(gwRevealDiagnostics:),
        "",
        None,
    );
    let website = ours(
        mtm,
        actions,
        "Project Website",
        sel!(gwOpenWebsite:),
        "",
        None,
    );
    let mut help: Vec<&NSMenuItem> = vec![&log];
    // Constant, and meant to be: whether the item exists is decided by what
    // `Cargo.toml` declares, not by anything that happens at runtime. Clippy is
    // right that this folds away and wrong that folding away makes it pointless
    // — the whole point is that a build with nowhere to send the player does
    // not offer to send them anywhere.
    #[allow(clippy::const_is_empty)]
    if !WEBSITE.is_empty() {
        help.push(&website);
    }
    menu.addItem(&submenu(mtm, "Help", &help));

    menu
}
