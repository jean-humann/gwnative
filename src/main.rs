//! Native macOS host for the Guild Wars WebAssembly client.
//!
//! ArenaNet ships `Gw.jspi.js` alongside `Gw.jspi.wasm` and regenerates both on
//! every patch, so their JavaScript has to run as-is — in an engine with JSPI
//! and WebGL. On macOS 27 that is WKWebView. Everything outside that realm
//! (patching, chunk storage, sockets, credentials, windowing) is Rust.

mod chunks;
mod error;
mod manifest;
mod net;
mod patch;
mod server;
mod sockets;
mod ws;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};

fn web_root() -> PathBuf {
    // Development runs from the source tree; a packaged build reads from
    // Contents/Resources/web.
    if let Ok(dir) = std::env::var("GWNATIVE_WEB_ROOT") {
        return PathBuf::from(dir);
    }
    let exe = std::env::current_exe().expect("current_exe");
    let bundled = exe
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("Resources/web"));
    match bundled {
        Some(p) if p.is_dir() => p,
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web"),
    }
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
            Some(store)
        }
        Err(e) => {
            eprintln!("[gwnative] snapshot unavailable: {e}");
            None
        }
    };

    let loopback = server::spawn(root.clone(), snapshot).expect("bind loopback");
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

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 800.0));
    let webview = make_webview(mtm, frame, &url);
    let window = make_window(mtm, frame, &webview);

    window.makeKeyAndOrderFront(None);
    app.activate();
    app.run();
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

fn make_webview(mtm: MainThreadMarker, frame: NSRect, url: &str) -> Retained<WKWebView> {
    let config = unsafe { WKWebViewConfiguration::new(mtm) };

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
    window
}
