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

use std::path::PathBuf;
use std::sync::Arc;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::sel;
use objc2_app_kit::{NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};
use objc2_web_kit::WKWebView;

use crate::settings;

use actions::{Actions, WEBSITE};

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
    submenu(
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
    )
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
                "Toggle Diagnostics",
                sel!(gwToggleDiagnostics:),
                "",
                None,
            ),
            &ours(mtm, actions, "Reload Game", sel!(gwReloadGame:), "r", None),
        ],
    )
}

fn help_menu(mtm: MainThreadMarker, actions: &Actions) -> Retained<NSMenuItem> {
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
    let mut items: Vec<&NSMenuItem> = vec![&log];
    // Constant, and meant to be: whether the item exists is decided by what
    // `Cargo.toml` declares, not by anything that happens at runtime. Clippy is
    // right that this folds away and wrong that folding away makes it pointless
    // — the whole point is that a build with nowhere to send the player does
    // not offer to send them anywhere.
    #[allow(clippy::const_is_empty)]
    if !WEBSITE.is_empty() {
        items.push(&website);
    }
    submenu(mtm, "Help", &items)
}
