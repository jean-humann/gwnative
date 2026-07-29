//! Trusted, finite keyboard delivery for signed end-to-end tests.
//!
//! Constructed DOM keyboard events are intentionally untrusted in WebKit. They
//! can exercise JavaScript handlers, but they are not a faithful test of the
//! native input path and the generated client may reject them at transitions
//! such as login and character selection. This module starts with an AppKit
//! `NSEvent`, which enters WKWebView through its normal responder chain.
//!
//! The surface remains deliberately narrow: the loopback hub has already
//! validated one named gameplay key and a bounded hold where applicable. There
//! is no arbitrary key, text, coordinate or script interface.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::Message;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSEvent, NSEventModifierFlags, NSEventType,
    NSRunningApplication, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSProcessInfo, NSString};
use objc2_web_kit::WKWebView;

use crate::app;
use crate::e2e_api::{Action, Hub};

const RETURN_KEY_CODE: u16 = 36;
const UP_ARROW_KEY_CODE: u16 = 126;
const DOWN_ARROW_KEY_CODE: u16 = 125;
const LEFT_ARROW_KEY_CODE: u16 = 123;
const RIGHT_ARROW_KEY_CODE: u16 = 124;
const TAB_KEY_CODE: u16 = 48;
const SPACE_KEY_CODE: u16 = 49;
const ESCAPE_KEY_CODE: u16 = 53;
const ONE_KEY_CODE: u16 = 18;

struct Dispatcher {
    window: Retained<NSWindow>,
    webview: Retained<WKWebView>,
    hub: Arc<Hub>,
    busy: Cell<bool>,
}

thread_local! {
    static DISPATCHER: RefCell<Option<Rc<Dispatcher>>> = const { RefCell::new(None) };
}

static INSTALLED: AtomicBool = AtomicBool::new(false);

pub fn install(window: &NSWindow, webview: &WKWebView, hub: Arc<Hub>) {
    let dispatcher = Rc::new(Dispatcher {
        window: window.retain(),
        webview: webview.retain(),
        hub,
        busy: Cell::new(false),
    });
    DISPATCHER.with(|slot| {
        *slot.borrow_mut() = Some(Rc::clone(&dispatcher));
    });
    INSTALLED.store(true, Ordering::Release);
    activate(&dispatcher.window);
    // If the page completed its focus handshake before AppKit finished
    // installing the dispatcher, the hub will expose that prepared action now.
    drain(dispatcher);
}

/// Wake the installed E2E dispatcher for a page-prepared action.
///
/// Safe from a loopback worker thread: the only state touched here is atomic,
/// and the dispatcher itself is recovered only after libdispatch reaches the
/// main queue.
pub fn wake() {
    if !INSTALLED.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: null carries no ownership, and `wake_on_main` consumes nothing.
    unsafe { app::to_main(std::ptr::null_mut(), wake_on_main) };
}

extern "C" fn wake_on_main(_context: *mut c_void) {
    DISPATCHER.with(|slot| {
        if let Some(dispatcher) = slot.borrow().as_ref() {
            drain(Rc::clone(dispatcher));
        }
    });
}

fn drain(dispatcher: Rc<Dispatcher>) {
    if dispatcher.busy.get() {
        return;
    }
    let Some(action) = dispatcher.hub.take_native_action() else {
        return;
    };
    dispatcher.busy.set(true);
    deliver(dispatcher, action);
}

fn deliver(dispatcher: Rc<Dispatcher>, action: Action) {
    let key = match action.action.as_str() {
        "activate" => Key {
            characters: "\r",
            code: RETURN_KEY_CODE,
            modifiers: NSEventModifierFlags::empty(),
        },
        "move-forward" => Key {
            // AppKit's private-use character for the physical Up Arrow key.
            characters: "\u{f700}",
            code: UP_ARROW_KEY_CODE,
            modifiers: NSEventModifierFlags::Function,
        },
        "move-backward" => Key {
            characters: "\u{f701}",
            code: DOWN_ARROW_KEY_CODE,
            modifiers: NSEventModifierFlags::Function,
        },
        "turn-left" => Key {
            characters: "\u{f702}",
            code: LEFT_ARROW_KEY_CODE,
            modifiers: NSEventModifierFlags::Function,
        },
        "turn-right" => Key {
            characters: "\u{f703}",
            code: RIGHT_ARROW_KEY_CODE,
            modifiers: NSEventModifierFlags::Function,
        },
        "target-next" => Key {
            characters: "\t",
            code: TAB_KEY_CODE,
            modifiers: NSEventModifierFlags::empty(),
        },
        "interact" => Key {
            characters: " ",
            code: SPACE_KEY_CODE,
            modifiers: NSEventModifierFlags::empty(),
        },
        "cancel" => Key {
            characters: "\u{1b}",
            code: ESCAPE_KEY_CODE,
            modifiers: NSEventModifierFlags::empty(),
        },
        "skill-1" => Key {
            characters: "1",
            code: ONE_KEY_CODE,
            modifiers: NSEventModifierFlags::empty(),
        },
        _ => {
            publish(
                &dispatcher.hub,
                &action,
                false,
                "native action is not allowed",
            );
            dispatcher.busy.set(false);
            drain(dispatcher);
            return;
        }
    };

    activate(&dispatcher.window);
    dispatcher
        .window
        .makeFirstResponder(Some(&dispatcher.webview));
    let Some(down) = event(&dispatcher.window, key, NSEventType::KeyDown) else {
        publish(&dispatcher.hub, &action, false, "AppKit refused key-down");
        dispatcher.busy.set(false);
        drain(dispatcher);
        return;
    };
    let mtm = MainThreadMarker::new().expect("native E2E delivery is on the main thread");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&down, false);

    let duration = u32::try_from(action.duration_ms).unwrap_or(u32::MAX);
    app::after(duration, move || {
        if let Some(up) = event(&dispatcher.window, key, NSEventType::KeyUp) {
            let mtm = MainThreadMarker::new().expect("native E2E release is on the main thread");
            NSApplication::sharedApplication(mtm).postEvent_atStart(&up, false);
            publish(&dispatcher.hub, &action, true, "");
        } else {
            publish(&dispatcher.hub, &action, false, "AppKit refused key-up");
        }
        dispatcher.busy.set(false);
        drain(Rc::clone(&dispatcher));
    });
}

fn activate(window: &NSWindow) {
    window.orderFrontRegardless();
    window.makeKeyAndOrderFront(None);
    NSRunningApplication::currentApplication()
        .activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
}

#[derive(Clone, Copy)]
struct Key {
    characters: &'static str,
    code: u16,
    modifiers: NSEventModifierFlags,
}

fn event(window: &NSWindow, key: Key, kind: NSEventType) -> Option<Retained<NSEvent>> {
    let characters = NSString::from_str(key.characters);
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        kind,
        NSPoint::new(0.0, 0.0),
        key.modifiers,
        NSProcessInfo::processInfo().systemUptime(),
        window.windowNumber(),
        None,
        &characters,
        &characters,
        false,
        key.code,
    )
}

fn publish(hub: &Hub, action: &Action, succeeded: bool, message: &str) {
    let (kind, detail) = if succeeded {
        (
            "action-complete",
            serde_json::json!({
                "actionSequence": action.sequence,
                "action": action.action,
                "target": "native-window",
                "activeTarget": "native-window",
            }),
        )
    } else {
        (
            "action-fail",
            serde_json::json!({
                "actionSequence": action.sequence,
                "action": action.action,
                "target": "native-window",
                "activeTarget": "native-window",
                "message": message,
            }),
        )
    };
    if let Ok(bytes) = serde_json::to_vec(&serde_json::json!({ "kind": kind, "detail": detail })) {
        let _ = hub.publish_event(&bytes);
    }
}
