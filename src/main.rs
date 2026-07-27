//! Native macOS host for the Guild Wars WebAssembly client.
//!
//! ArenaNet ships `Gw.jspi.js` alongside `Gw.jspi.wasm` and regenerates both on
//! every patch, so their JavaScript has to run as-is — in an engine with JSPI
//! and WebGL. On macOS 27 that is WKWebView. Everything outside that realm
//! (patching, chunk storage, sockets, credentials, windowing) is Rust.

mod chunks;
mod error;
mod keychain;
mod layout;
mod manifest;
mod net;
mod patch;
mod proxy;
mod qos;
mod server;
mod sockets;
mod ws;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadOnly, Message, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventModifierFlags, NSMenu,
    NSMenuItem, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest};
use objc2_web_kit::{WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration};

/// `~/Library/Application Support/gwnative`, the one place this app writes.
///
/// The chunk cache is already a directory inside it — see
/// [`chunks::default_cache_dir`], which explains why it is here rather than in
/// `~/Library/Caches`.
fn support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Library/Application Support/gwnative")
}

/// The directory the loopback origin serves, and the one `patch::sync` fills.
///
/// Development runs straight out of the source tree. A packaged build does
/// *not* serve out of `Contents/Resources/web`, tempting as that is: the patch
/// client writes `Gw.jspi.wasm` into this directory, and writing into a bundle
/// invalidates its code signature — the same signature the keychain matches the
/// saved login against, so the cost of getting this wrong is an account that
/// silently stops appearing. The bundle's copy is a seed for a writable root
/// instead, refreshed on every launch so an upgraded app ships an upgraded
/// shell.
fn web_root() -> PathBuf {
    if let Ok(dir) = std::env::var("GWNATIVE_WEB_ROOT") {
        return PathBuf::from(dir);
    }
    let exe = std::env::current_exe().expect("current_exe");
    let seed = exe
        .parent()
        .and_then(Path::parent)
        .map(|contents| contents.join("Resources/web"))
        .filter(|seed| seed.is_dir());
    let Some(seed) = seed else {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    };
    let live = support_dir().join("web");
    if let Err(e) = seed_web(&seed, &live) {
        // Reported rather than fatal, and still the root we return: a partial
        // seed leaves the missing file to be noticed by whatever needed it,
        // whereas falling back to the bundle would put the patch sync inside
        // it, which is the one outcome this function exists to prevent.
        eprintln!("[gwnative] could not lay out {}: {e}", live.display());
    }
    live
}

/// Copy the bundle's shell files over the live web root.
///
/// Only what the bundle carries: the client artifacts sit in the same directory
/// once fetched and must survive. Contents are compared rather than timestamps,
/// which a copy does not preserve — these are a few tens of kilobytes, so the
/// comparison costs less than being wrong about it would.
fn seed_web(seed: &Path, live: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(live)?;
    for entry in std::fs::read_dir(seed)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let fresh = std::fs::read(entry.path())?;
        let installed = live.join(entry.file_name());
        if std::fs::read(&installed).is_ok_and(|current| current == fresh) {
            continue;
        }
        std::fs::write(&installed, &fresh)?;
    }
    Ok(())
}

fn main() {
    let root = web_root();
    let command = std::env::args().nth(1);
    let force_sync = command.as_deref() == Some("sync");
    // `serve` runs the origin without a window, so the snapshot range path can
    // be exercised from curl or a test.
    let headless = command.as_deref() == Some("serve");

    let missing = patch::missing_artifacts(&root);
    if (force_sync || !missing.is_empty())
        && let Err(e) = sync(&root, &missing)
    {
        // A stale-but-complete web root still boots, so a failed refresh is
        // only fatal when the client is not on disk at all.
        eprintln!("[gwnative] patch sync failed: {e}");
        if !missing.is_empty() {
            std::process::exit(1);
        }
    }
    if force_sync {
        return;
    }

    // Gw.snapshot is 4.2 GB and a session touches a fraction of it, so it is
    // served as a virtual ranged file rather than downloaded. Without a store
    // the shell still opens; only the game data is unavailable.
    let snapshot = match open_snapshot() {
        Ok(store) => {
            eprintln!(
                "[gwnative] snapshot: {:.1} GB in {} KiB chunks, on demand",
                store.snapshot_size() as f64 / 1e9,
                store.chunk_size() / 1024
            );
            // Pull what the last boot needed while the window is still being
            // built. By the time the client asks, the chunks that gate the
            // first frame are already local.
            store.warm_boot();
            Some(store)
        }
        Err(e) => {
            eprintln!("[gwnative] snapshot unavailable: {e}");
            None
        }
    };

    let token = session_token();
    let loopback = server::spawn(root.clone(), snapshot, token.clone()).expect("bind loopback");
    let url = format!("http://{}/index.html", loopback.addr);
    eprintln!("[gwnative] serving {} at {}", root.display(), url);

    if headless {
        println!("{}", loopback.addr);
        loop {
            std::thread::park();
        }
    }

    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    app.setMainMenu(Some(&make_menu(mtm)));

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 800.0));
    let webview = make_webview(mtm, frame, &url, &token);
    let window = make_window(mtm, frame, &webview);

    watch_keyboard_layout(&webview);

    window.makeKeyAndOrderFront(None);
    app.activate();
    app.run();
}

thread_local! {
    /// The live web view, for the handful of host events that have to reach the
    /// page outside a request. Main-thread-only by WebKit's rules, which is
    /// exactly what a thread-local on the main thread enforces.
    static PAGE: std::cell::RefCell<Option<Retained<WKWebView>>> =
        const { std::cell::RefCell::new(None) };
}

/// Keep `window.__gwnativeLayout` current when the player switches input source.
///
/// The injected copy is correct at document start and stale the moment somebody
/// presses ⌃Space. Switching layout mid-session is ordinary on a Mac — this
/// user runs French AZERTY — and a stale table would restate an Option-held key
/// as the character some *other* layout puts on it, which is worse than not
/// restating it at all.
fn watch_keyboard_layout(webview: &WKWebView) {
    PAGE.with(|page| *page.borrow_mut() = Some(webview.retain()));
    layout::watch(|json| {
        let script = format!(
            "window.__gwnativeLayout = {json}; \
             window.dispatchEvent(new Event('gw:layout-changed'));"
        );
        PAGE.with(|page| {
            if let Some(webview) = page.borrow().as_ref() {
                // SAFETY: delivered on the main thread — see `layout::watch`,
                // which documents why that holds.
                unsafe {
                    webview
                        .evaluateJavaScript_completionHandler(&NSString::from_str(&script), None);
                }
            }
        });
    });
}

fn open_snapshot() -> error::Result<Arc<chunks::ChunkStore>> {
    let client = patch::Client::from_env()?;
    let manifest = client.fetch_manifest()?;
    let store = chunks::ChunkStore::open(client, manifest, chunks::default_cache_dir())?;
    Ok(Arc::new(store))
}

fn sync(root: &Path, missing: &[&'static str]) -> error::Result<()> {
    if missing.is_empty() {
        eprintln!("[gwnative] refreshing client artifacts");
    } else {
        eprintln!(
            "[gwnative] fetching client artifacts: {}",
            missing.join(", ")
        );
    }
    let fetched = patch::sync(root)?;
    for (name, bytes) in fetched {
        eprintln!("[gwnative]   {name} ({bytes} bytes)");
    }
    Ok(())
}

/// A fresh random secret per launch, shared with the page and nothing else.
///
/// From the kernel, not a seeded generator: this authorises reading the saved
/// password, so it must not be reproducible by anything that knows when the
/// process started.
fn session_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn getrandom(buffer: &mut [u8]) {
    // SAFETY: `buffer` is a live slice and its length is passed alongside it.
    // `getentropy` fills exactly that many bytes and cannot fail for a length
    // of 256 or under, which is the only way it is called here.
    let status = unsafe { libc_getentropy(buffer.as_mut_ptr(), buffer.len()) };
    assert_eq!(status, 0, "getentropy failed");
}

unsafe extern "C" {
    #[link_name = "getentropy"]
    fn libc_getentropy(buffer: *mut u8, length: usize) -> i32;
}

fn make_webview(
    mtm: MainThreadMarker,
    frame: NSRect,
    url: &str,
    token: &str,
) -> Retained<WKWebView> {
    let config = unsafe { WKWebViewConfiguration::new(mtm) };

    // Hand the page its token out of band. Serving it over the loopback origin
    // instead would put it where any local process could simply ask for it,
    // which is the exposure the token exists to close.
    //
    // The keyboard layout rides the same channel for a different reason: it has
    // to be in place before the page can see a keydown, and a fetch at boot
    // would not be. See `layout` for what the page does with it.
    unsafe {
        let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
            WKUserScript::alloc(mtm),
            &NSString::from_str(&format!(
                "window.__gwnativeToken = {};\nwindow.__gwnativeLayout = {};",
                serde_json::Value::from(token),
                layout::as_json()
            )),
            WKUserScriptInjectionTime::AtDocumentStart,
            true,
        );
        config.userContentController().addUserScript(&script);
    }

    let webview =
        unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config) };

    // The stock WKWebView agent string has no Version/Safari token, and
    // Emscripten glue is known to branch on it.
    unsafe {
        webview.setCustomUserAgent(Some(&NSString::from_str(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
             AppleWebKit/605.1.15 (KHTML, like Gecko) Version/27.0 Safari/605.1.15",
        )));
    }

    let nsurl = NSURL::URLWithString(&NSString::from_str(url)).expect("url");
    let request = NSURLRequest::requestWithURL(&nsurl);
    unsafe { webview.loadRequest(&request) };

    webview
}

/// The application menu bar.
///
/// It is not decoration. A ⌘-key is delivered as a key equivalent, and what
/// turns ⌘V into the `paste:` action is an Edit menu item claiming it — with no
/// main menu, pasting an account name into the login field does nothing, and so
/// does ⌘Q. WKWebView already implements every one of these actions; the menu
/// only supplies the route to them.
fn make_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
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
            &item(mtm, "Hide Guild Wars", sel!(hide:), "h", None),
            &item(
                mtm,
                "Hide Others",
                sel!(hideOtherApplications:),
                "h",
                Some(command | NSEventModifierFlags::Option),
            ),
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

    menu
}

fn make_window(mtm: MainThreadMarker, frame: NSRect, webview: &WKWebView) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    window.setTitle(&NSString::from_str("Guild Wars"));
    window.center();
    window.setContentView(Some(webview));
    // Key events go to the first responder; mouse events are hit-tested and do
    // not. A window that never hands the web view first responder therefore
    // looks alive to the trackpad and deaf to the keyboard, which is exactly
    // what the game does when its canvas never sees a keydown.
    window.setInitialFirstResponder(Some(webview));
    window.makeFirstResponder(Some(webview));
    window
}
