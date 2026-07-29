//! The application menu bar.
//!
//! It is not decoration. A ⌘-key is delivered as a key equivalent, and what
//! turns ⌘V into the `paste:` action is an Edit menu item claiming it — with no
//! main menu, pasting an account name into the login field does nothing, and so
//! does ⌘Q. WKWebView already implements every one of those actions; the menu
//! only supplies the route to them.
//!
//! Most items here have no target at all: AppKit sends a nil-targeted action up
//! the responder chain, so `toggleFullScreen:` finds the key window and `hide:`
//! finds the application without either being named here. So this half is only
//! the shape of the bar — which menus exist, what they are called, which key
//! reaches them. The few items that need someone to answer them live in
//! [`actions`], and this file knows them only as a target to hang off.

mod actions;

use std::sync::Arc;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::sel;
use objc2_app_kit::{NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};
use objc2_web_kit::WKWebView;

use crate::{diagnostics, release, settings, updater};

use actions::Actions;

pub use actions::at_launch as check_for_updates_at_launch;

/// Whether this build can find out about a newer one.
///
/// Two ways to be able to, and the item appears if either holds: the bundle
/// carries the updater, or `Cargo.toml` declares where this was published from
/// and the older check can compare tags. A build with neither does not offer to
/// look, because the alternative is an item that can only ever answer "this
/// build did not come from the release process" — a worse thing to put in a
/// menu than nothing at all.
fn updates_offered() -> bool {
    updater::available() || release::repository().is_some()
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
    recorder: Arc<diagnostics::Recorder>,
) -> Retained<NSMenu> {
    let actions = Actions::new(mtm, webview, settings, recorder);
    let menu = build(mtm, &actions);
    std::mem::forget(actions);
    menu
}

fn build(mtm: MainThreadMarker, actions: &Actions) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.addItem(&application_menu(mtm, actions));
    menu.addItem(&edit_menu(mtm));
    menu.addItem(&view_menu(mtm, actions));
    menu.addItem(&help_menu(mtm, actions));
    menu
}

/// An item AppKit already knows how to perform, sent up the responder chain.
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

/// An item this file performs itself. Distinguished from [`item`] only by
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

/// The first submenu is the application menu whatever it is titled; macOS
/// substitutes the process name for the title it is given.
fn application_menu(mtm: MainThreadMarker, actions: &Actions) -> Retained<NSMenuItem> {
    let command = NSEventModifierFlags::Command;
    let about = ours(mtm, actions, "About Guild Wars", sel!(gwAbout:), "", None);
    // Directly under About, where every Mac application that has one puts it,
    // and so where a player will look without being told.
    let updates = ours(
        mtm,
        actions,
        "Check for Updates…",
        sel!(gwCheckForUpdates:),
        "",
        None,
    );
    let settings = ours(mtm, actions, "Settings…", sel!(gwOpenSettings:), ",", None);
    let hide = item(mtm, "Hide Guild Wars", sel!(hide:), "h", None);
    let hide_others = item(
        mtm,
        "Hide Others",
        sel!(hideOtherApplications:),
        "h",
        Some(command | NSEventModifierFlags::Option),
    );
    let show_all = item(mtm, "Show All", sel!(unhideAllApplications:), "", None);
    let quit = item(mtm, "Quit Guild Wars", sel!(terminate:), "q", None);
    // One each: an item belongs to one menu, and three rules drawn from one
    // object would be one rule drawn three times.
    let rules = [
        NSMenuItem::separatorItem(mtm),
        NSMenuItem::separatorItem(mtm),
        NSMenuItem::separatorItem(mtm),
    ];

    let mut items: Vec<&NSMenuItem> = vec![&about];
    if updates_offered() {
        items.push(&updates);
    }
    items.extend([
        &*rules[0],
        &*settings,
        &*rules[1],
        &*hide,
        &*hide_others,
        &*show_all,
        &*rules[2],
        &*quit,
    ]);
    submenu(mtm, "Guild Wars", &items)
}

/// Cut and copy matter as much as paste: the client's own fields are these
/// proxies, so the player expects the ordinary Mac editing keys in them.
fn edit_menu(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let command = NSEventModifierFlags::Command;
    submenu(
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
    )
}

/// Full screen is the only one of these AppKit performs. It is here because
/// without a menu item there is no ⌃⌘F, and the green button is the only way
/// in — which is a poor only way for a game.
fn view_menu(mtm: MainThreadMarker, actions: &Actions) -> Retained<NSMenuItem> {
    let command = NSEventModifierFlags::Command;
    submenu(
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
                "Companion Tools…",
                sel!(gwOpenTools:),
                "t",
                Some(command | NSEventModifierFlags::Shift),
            ),
            &ours(
                mtm,
                actions,
                "Toggle Diagnostics",
                sel!(gwToggleDiagnostics:),
                "",
                None,
            ),
            // ⌘⇧M, and here rather than in Help because it is pressed *during*
            // a session rather than at the end of one — a player chasing a
            // stutter presses it several times and never opens a menu to do it.
            // The Help item that explains it is next door.
            &ours(
                mtm,
                actions,
                "Mark a Slowdown",
                sel!(gwMarkSlowdown:),
                "m",
                Some(command | NSEventModifierFlags::Shift),
            ),
            &ours(mtm, actions, "Reload Game", sel!(gwReloadGame:), "r", None),
        ],
    )
}

/// Four items, and only three of them always exist.
///
/// The guide, the report and the store are unconditional because none of them
/// depends on this build knowing where it came from: the guide is a page inside
/// the window, the report is a file next to the log, and the store has been at
/// the same address for twenty years. The website is the one that is not, for
/// the reason below.
fn help_menu(mtm: MainThreadMarker, actions: &Actions) -> Retained<NSMenuItem> {
    let guide = ours(mtm, actions, "User Guide", sel!(gwOpenGuide:), "?", None);
    // This replaced an item that only revealed `gwnative.jsonl`. Revealing the
    // raw log asked the player to attach several thousand unlabelled records
    // about a Mac the file never names; the report is that log's tail under a
    // cover sheet, written into the same folder, so the raw file is still one
    // click away for anyone who wants it.
    let report = ours(
        mtm,
        actions,
        "Report a Problem…",
        sel!(gwReportProblem:),
        "",
        None,
    );
    let store = ours(mtm, actions, "Buy Guild Wars", sel!(gwOpenStore:), "", None);
    let website = ours(
        mtm,
        actions,
        "Project Website",
        sel!(gwOpenWebsite:),
        "",
        None,
    );
    let rule = NSMenuItem::separatorItem(mtm);
    let mut items: Vec<&NSMenuItem> = vec![&guide, &report, &rule, &store];
    // Constant, and meant to be: whether the item exists is decided by what
    // `Cargo.toml` declares, not by anything that happens at runtime. Clippy is
    // right that this folds away and wrong that folding away makes it pointless
    // — the whole point is that a build with nowhere to send the player does
    // not offer to send them anywhere.
    #[allow(clippy::const_is_empty)]
    if !release::PROJECT_URL.is_empty() {
        items.push(&website);
    }
    submenu(mtm, "Help", &items)
}
