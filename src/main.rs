//! Native macOS host for the Guild Wars WebAssembly client.
//!
//! ArenaNet ships `Gw.jspi.js` alongside `Gw.jspi.wasm` and regenerates both on
//! every patch, so their JavaScript has to run as-is — in an engine with JSPI
//! and WebGL. On macOS 27 that is WKWebView. Everything outside that realm
//! (patching, chunk storage, sockets, credentials, windowing) is Rust.

mod app;
mod cache;
mod chunks;
mod commands;
mod diagnostics;
mod disk;
mod error;
mod generation;
mod http;
mod instance;
mod keychain;
mod layout;
mod manifest;
mod menu;
mod net;
mod patch;
mod proxy;
mod qos;
mod renderer;
#[cfg(test)]
mod scratch;
mod server;
mod settings;
mod sockets;
mod transport;
mod wasm;
mod window;
mod ws;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
    NSRunningApplication,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest};
use objc2_web_kit::{WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration};

/// `~/Library/Application Support/gwnative`, the one place this app writes.
///
/// The chunk cache is already a directory inside it — see
/// [`cache::default_cache_dir`], which explains why it is here rather than in
/// `~/Library/Caches`.
fn support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Library/Application Support/gwnative")
}

/// Where derived clients live. Owned outright by the transform, which empties it
/// whenever it cannot serve the input — an entry is ~8.2 MB, so keeping one per
/// build the machine has ever seen adds up quickly.
///
/// Deliberately not inside the web root: that directory is what the loopback
/// origin serves, and the derived module is reachable only through the one path
/// the server maps to it.
fn derived_dir() -> PathBuf {
    support_dir().join("derived")
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
    let command = std::env::args().nth(1);
    let force_sync = command.as_deref() == Some("sync");
    // `serve` runs the origin without a window, so the snapshot range path can
    // be exercised from curl or a test.
    let headless = command.as_deref() == Some("serve");

    // Before `web_root`, which seeds files a second instance may be reading.
    // Held for as long as the process lives; the kernel takes it back if the
    // process does not.
    let lock_path = support_dir().join("gwnative.lock");
    let _instance = match instance::acquire(&lock_path) {
        Ok(held) => held,
        Err(reason) => {
            eprintln!("[gwnative] {reason}");
            // A second launch of a windowed app should look like asking for the
            // one that is already open, not like nothing happening. Raising it
            // by pid rather than bundle id works in development too, where
            // there is no bundle to identify.
            if !headless
                && !force_sync
                && let Some(pid) = instance::holder(&lock_path)
                && let Some(mtm) = MainThreadMarker::new()
            {
                let _ = mtm;
                if let Some(running) =
                    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                {
                    running.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
                }
            }
            std::process::exit(1);
        }
    };

    // Before the client can ask for the login, so the reason it will not get one
    // is on screen ahead of the dialog rather than after it.
    keychain::check_identity();

    let root = web_root();

    // Before anything reads the web root: a client installed last launch that
    // never reported a first frame is one this build cannot run, and the set it
    // replaced is still stashed. See `generation` for why presence was never
    // enough on its own.
    let generations = Arc::new(generation::Store::open(support_dir().join("generations")));
    if let Some(refused) = generations.roll_back(&root) {
        eprintln!(
            "[gwnative] client build {refused} never reached a first frame; \
             restored the one before it"
        );
    }

    let missing = generations.unsound(&root, &patch::artifacts());
    if (force_sync || !missing.is_empty())
        && let Err(e) = sync(&root, &missing, &generations)
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
    // Nothing was downloaded, so nothing was recorded — and a client with no
    // record is only ever checked for existence. Hash it once here and every
    // later launch gets a real check. No-op after the first time.
    generations.adopt(&root, &patch::artifacts());

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
            // And on the launch that has no list to replay — the first one —
            // stay a little ahead of wherever the client is reading instead.
            store.start_readahead();
            Some(store)
        }
        Err(e) => {
            eprintln!("[gwnative] snapshot unavailable: {e}");
            None
        }
    };

    // Started before the window so that whatever the shell costs to build is
    // in the record too.
    let recorder = diagnostics::Recorder::open(diagnostics::default_log_dir());
    diagnostics::spawn_sampler(Arc::clone(&recorder), {
        let snapshot = snapshot.clone();
        move || match &snapshot {
            Some(store) => {
                let (cache, net, coalesced) = store.stats();
                serde_json::json!({"fromCache": cache, "fetched": net, "coalesced": coalesced})
            }
            None => serde_json::Value::Null,
        }
    });

    // Derive the client that can save a template, if this is a build we have
    // certified. A failure here is never fatal: the untransformed module still
    // plays, it just cannot save, list or delete a build — which is where the
    // client started. See `wasm` for what the derived module changes.
    let derived_wasm = match wasm::prepare(&root.join("Gw.jspi.wasm"), &derived_dir()) {
        Ok(Some(path)) => Some(path),
        Ok(None) => {
            println!("[gwnative] template save: unavailable, this client build is not certified");
            None
        }
        Err(reason) => {
            eprintln!("[gwnative] template save unavailable: {reason}");
            None
        }
    };

    // Read before the window exists: the render scale the client is handed and
    // the gesture translation the page installs are both settled before the
    // first frame, so asking the page to fetch them later would mean booting
    // once at the wrong scale and correcting it in front of the player.
    let settings = Arc::new(settings::Store::open(support_dir().join("settings.json")));

    let token = session_token();
    let loopback = server::spawn(
        root.clone(),
        snapshot,
        recorder,
        derived_wasm,
        settings,
        generations,
        token.clone(),
    )
    .expect("bind loopback");
    let url = format!("http://{}/index.html", loopback.addr);
    eprintln!("[gwnative] serving {} at {}", root.display(), url);
    // The windowed app keeps its token to itself — it reaches the page over the
    // injection channel and nowhere else. But every measurement worth taking
    // lives behind that gate on `__diag`, and a benchmark that cannot read it
    // is a benchmark of nothing. So: on request, and only on request.
    if std::env::var_os("GWNATIVE_PRINT_TOKEN").is_some() {
        eprintln!("[gwnative] session token {token}");
    }

    if headless {
        // Address and session token on one line, because every route worth
        // exercising is behind the gate and there is otherwise no way to get
        // past it from outside the page. Only ever printed here: in the app the
        // token reaches the page over the injection channel and nowhere else.
        println!("{} {token}", loopback.addr);
        loop {
            std::thread::park();
        }
    }

    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // The frame the web view is created at does not matter: `window::open`
    // resizes the window to the remembered one before it is ever shown, and the
    // content view follows.
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 800.0));
    let webview = make_webview(mtm, frame, &url, &token, &loopback.settings.get());
    let window = window::open(mtm, &webview, support_dir().join("window.json"));

    // After the window, not before: two of the menu's items are requests to the
    // page, and one moves the window. The menu only has to exist before `run`.
    app.setMainMenu(Some(&menu::install(
        mtm,
        &webview,
        loopback.settings.clone(),
        diagnostics::default_log_dir(),
    )));

    commands::attach(&webview);
    // After the load has been asked for, which is fine: the delegate is
    // consulted when the navigation is decided, not when it is requested.
    renderer::guard(mtm, &webview, &format!("http://{}", loopback.addr));
    // Before `run`, because the first thing it decides — whether closing the
    // window quits — can be asked the moment the window appears.
    app::own_lifecycle(mtm, &webview);

    window.makeKeyAndOrderFront(None);
    app.activate();
    app.run();

    // `run` returns after `applicationWillTerminate`, so `window` has already
    // written itself once. This catches the exits that do not post it.
    window::flush();
}

fn open_snapshot() -> error::Result<Arc<chunks::ChunkStore>> {
    let client = patch::Client::from_env()?;
    let manifest = client.fetch_manifest()?;
    let store = chunks::ChunkStore::open(client, manifest, cache::default_cache_dir())?;
    Ok(Arc::new(store))
}

/// Fetch the client, unless the only thing on offer is a build that has already
/// failed here.
///
/// Returns whether anything was written. The rejection check is the reason this
/// fetches the manifest itself rather than letting `patch::sync_with` do it: the
/// identity of the build being offered has to be known while declining it still
/// costs nothing.
fn sync(
    root: &Path,
    unsound: &[&'static str],
    generations: &generation::Store,
) -> error::Result<()> {
    if unsound.is_empty() {
        eprintln!("[gwnative] refreshing client artifacts");
    } else {
        eprintln!(
            "[gwnative] fetching client artifacts: {}",
            unsound.join(", ")
        );
    }
    let client = patch::Client::from_env()?;
    let manifest = client.fetch_manifest()?;
    let names = patch::artifacts();
    let offered = generation::identify(&manifest, &names)?;

    if generations.rejected(&offered) {
        if unsound.is_empty() {
            eprintln!(
                "[gwnative] the service still offers client build {offered}, which never reached \
                 a first frame here; keeping the one on disk"
            );
            return Ok(());
        }
        // The alternative to a build that did not work is no client at all, so
        // it gets another try — loudly, because if it fails the same way the
        // line above is the one that explains why nothing changed.
        eprintln!(
            "[gwnative] client build {offered} never reached a first frame here, but the client \
             on disk is incomplete, so there is nothing else to run"
        );
    }

    generations.stash(root, &names);
    let fetched = patch::sync_with(&client, &manifest, root)?;
    for (name, bytes) in fetched {
        eprintln!("[gwnative]   {name} ({bytes} bytes)");
    }
    generations.record(&offered, root, &names);
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
    settings: &settings::Settings,
) -> Retained<WKWebView> {
    let config = unsafe { WKWebViewConfiguration::new(mtm) };

    // Hand the page its token out of band. Serving it over the loopback origin
    // instead would put it where any local process could simply ask for it,
    // which is the exposure the token exists to close.
    //
    // The keyboard layout rides the same channel for a different reason: it has
    // to be in place before the page can see a keydown, and a fetch at boot
    // would not be. See `layout` for what the page does with it.
    //
    // Settings ride it for that same reason. The render scale is read by the
    // client's first call into the graphics host and the touch mode decides
    // which listeners `input.js` installs, so both are needed before anything
    // the page could await. `PUT /__settings` is what changes them afterwards;
    // this is only the value they start at.
    unsafe {
        let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
            WKUserScript::alloc(mtm),
            &NSString::from_str(&format!(
                "window.__gwnativeToken = {};\nwindow.__gwnativeLayout = {};\n\
                 window.__gwnativeBridgeMarkers = {};\nwindow.__gwnativeSettings = {};",
                serde_json::Value::from(token),
                layout::as_json(),
                wasm::markers_json(),
                serde_json::to_string(settings).unwrap_or_else(|_| "{}".to_owned()),
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
