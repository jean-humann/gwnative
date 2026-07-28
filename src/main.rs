//! Native macOS host for the Guild Wars WebAssembly client.
//!
//! ArenaNet ships `Gw.jspi.js` alongside `Gw.jspi.wasm` and regenerates both on
//! every patch, so their JavaScript has to run as-is — in an engine with JSPI
//! and WebGL. On macOS 27 that is WKWebView. Everything outside that realm
//! (patching, chunk storage, sockets, credentials, windowing) is Rust.

// Out of alphabetical order on purpose: `macro_rules!` is in scope only for
// what follows it, and `note!` is used by nearly every module below.
#[macro_use]
mod log;

mod alert;
mod app;
mod cache;
mod chunks;
mod commands;
mod diagnostics;
mod disk;
mod dock;
mod error;
mod generation;
mod http;
mod instance;
mod keychain;
mod layout;
mod manifest;
mod menu;
mod net;
mod notify;
mod patch;
mod paths;
mod proxy;
mod qos;
mod relaunch;
mod renderer;
#[cfg(test)]
mod scratch;
mod server;
mod settings;
mod sockets;
mod transport;
mod wasm;
mod webview;
mod window;
mod ws;

use std::path::Path;
use std::sync::Arc;

use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
    NSRunningApplication,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

/// Launch, in the order the parts depend on each other.
///
/// Each step below is a phase with its own reason for coming where it does, and
/// those reasons are on the functions rather than here. What this reads as is
/// the order itself: one instance, then a client worth running, then the data it
/// reads, then an origin to serve both from, and only then a window.
fn main() {
    let command = std::env::args().nth(1);
    let force_sync = command.as_deref() == Some("sync");
    // `serve` runs the origin without a window, so the snapshot range path can
    // be exercised from curl or a test.
    let headless = command.as_deref() == Some("serve");
    // The two commands above are the runs with a terminal attached. Everything
    // that would otherwise put a message on screen asks this first.
    let windowed = !headless && !force_sync;

    // Held for as long as the process lives; the kernel takes it back if the
    // process does not.
    let _instance = hold_the_only_instance(windowed);

    // Before the client can ask for the login, so the reason it will not get one
    // is on screen ahead of the dialog rather than after it.
    keychain::check_identity();

    let root = paths::web_root();
    let generations = install_client(&root, force_sync, windowed);
    if force_sync {
        return;
    }
    // Nothing was downloaded, so nothing was recorded — and a client with no
    // record is only ever checked for existence. Hash it once here and every
    // later launch gets a real check. No-op after the first time.
    generations.adopt(&root, &patch::artifacts());

    let snapshot = open_and_warm_snapshot();

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
    //
    // The outcome is carried to the page as well as to the log. A player who
    // clicks Save in the client's template window and watches nothing happen is
    // owed a sentence about why, and the log is not where they will look for
    // it; `settings-panel.js` is what turns this into that sentence.
    let (derived_wasm, template_save) =
        match wasm::prepare(&root.join("Gw.jspi.wasm"), &paths::derived_dir()) {
            Ok(Some(path)) => (Some(path), "ready"),
            Ok(None) => {
                note!("[gwnative] template save: unavailable, this client build is not certified");
                (None, "uncertified")
            }
            Err(reason) => {
                note!("[gwnative] template save unavailable: {reason}");
                (None, "failed")
            }
        };

    // Read before the window exists: the render scale the client is handed and
    // the gesture translation the page installs are both settled before the
    // first frame, so asking the page to fetch them later would mean booting
    // once at the wrong scale and correcting it in front of the player.
    let settings = Arc::new(settings::Store::open(
        paths::support_dir().join("settings.json"),
    ));

    let token = session_token();
    let loopback = match server::spawn(
        root.clone(),
        snapshot,
        recorder,
        derived_wasm,
        settings,
        generations,
        token.clone(),
    ) {
        Ok(loopback) => loopback,
        // Nothing downstream has an answer to this: the client is a page, and
        // without an origin to serve it from there is no client. `force_sync`
        // has already returned by here, so the only run with a terminal left is
        // the headless one.
        Err(e) => alert::fatal(
            !headless,
            "Guild Wars could not start",
            &format!("The local address the game is served from could not be opened.\n\n{e}"),
        ),
    };
    note!(
        "[gwnative] serving {} at http://{}/index.html",
        root.display(),
        loopback.addr
    );
    // The windowed app keeps its token to itself — it reaches the page over the
    // injection channel and nowhere else. But every measurement worth taking
    // lives behind that gate on `__diag`, and a benchmark that cannot read it
    // is a benchmark of nothing. So: on request, and only on request.
    if std::env::var_os("GWNATIVE_PRINT_TOKEN").is_some() {
        note!("[gwnative] session token {token}");
    }

    if headless {
        park_headless(&loopback, &token);
    }
    run_windowed(&loopback, &token, template_save);
}

/// Take the single-instance lock, or hand the running app the foreground.
///
/// Before [`paths::web_root`], which seeds files a second instance may be
/// reading. A second launch of a windowed app should look like asking for the
/// one that is already open, not like nothing happening; raising it by pid
/// rather than bundle id works in development too, where there is no bundle to
/// identify.
fn hold_the_only_instance(windowed: bool) -> instance::Instance {
    let lock_path = paths::support_dir().join("gwnative.lock");
    // A relaunch is started by the app it replaces, so for a moment there
    // really are two — and this is the one that has to wait for the other.
    let patience = if relaunch::is_successor() {
        relaunch::PATIENCE
    } else {
        std::time::Duration::ZERO
    };
    match instance::acquire(&lock_path, patience) {
        Ok(held) => held,
        Err(reason) => {
            note!("[gwnative] {reason}");
            if windowed
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
    }
}

/// Make the web root hold a client this build can run, and return the record
/// that says so.
///
/// The rollback comes before anything reads the root: a client installed last
/// launch that never reported a first frame is one this build cannot run, and
/// the set it replaced is still stashed. See `generation` for why presence was
/// never enough on its own.
fn install_client(root: &Path, force_sync: bool, windowed: bool) -> Arc<generation::Store> {
    let generations = Arc::new(generation::Store::open(
        paths::support_dir().join("generations"),
    ));
    if let Some(refused) = generations.roll_back(root) {
        note!(
            "[gwnative] client build {refused} never reached a first frame; \
             restored the one before it"
        );
    }

    let missing = generations.unsound(root, &patch::artifacts());
    if (force_sync || !missing.is_empty())
        && let Err(e) = sync(root, &missing, &generations)
    {
        // A stale-but-complete web root still boots, so a failed refresh is
        // only fatal when the client is not on disk at all.
        if missing.is_empty() {
            note!("[gwnative] patch sync failed: {e}");
        } else {
            alert::fatal(
                windowed,
                "Guild Wars could not be installed",
                &format!(
                    "The client files could not be downloaded, and there is no \
                     complete copy on this Mac to fall back to. Check the network \
                     connection and open Guild Wars again.\n\n{e}"
                ),
            );
        }
    }
    generations
}

/// Open the snapshot store and set it reading before anything asks it for bytes.
///
/// Gw.snapshot is 4.2 GB and a session touches a fraction of it, so it is served
/// as a virtual ranged file rather than downloaded. Without a store the shell
/// still opens; only the game data is unavailable.
fn open_and_warm_snapshot() -> Option<Arc<chunks::ChunkStore>> {
    match open_snapshot() {
        Ok(store) => {
            note!(
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
            note!("[gwnative] snapshot unavailable: {e}");
            None
        }
    }
}

/// Serve until killed, with the address and token on stdout.
///
/// One line, because every route worth exercising is behind the gate and there
/// is otherwise no way past it from outside the page. Only ever printed here: in
/// the app the token reaches the page over the injection channel and nowhere
/// else. Written the same forgiving way as every line on stderr — see `log` — as
/// a harness that has already left is not worth aborting over.
fn park_headless(loopback: &server::Loopback, token: &str) -> ! {
    {
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout().lock(), "{} {token}", loopback.addr);
    }
    loop {
        std::thread::park();
    }
}

/// Build the window and hand the thread to AppKit. Returns once the app has
/// terminated.
fn run_windowed(loopback: &server::Loopback, token: &str, template_save: &str) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // The frame the web view is created at does not matter: `window::open`
    // resizes the window to the remembered one before it is ever shown, and the
    // content view follows.
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 800.0));
    let webview = webview::make(
        mtm,
        frame,
        &format!("http://{}/index.html", loopback.addr),
        token,
        &loopback.settings.get(),
        template_save,
    );
    let window = window::open(mtm, &webview, paths::support_dir().join("window.json"));

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
    // The last thing before the thread stops being ours. `app::request_quit`
    // reads this to know a `terminate:` will be heard.
    app::about_to_run();
    app.run();

    // `run` returns after `applicationWillTerminate`, so `window` has already
    // written itself once. This catches the exits that do not post it.
    window::flush();
}

fn open_snapshot() -> error::Result<Arc<chunks::ChunkStore>> {
    let client = patch::Client::from_env();
    let (manifest, source) = client.manifest(&paths::support_dir())?;
    if source == patch::Source::Disk {
        revalidate_manifest();
    }
    let store = chunks::ChunkStore::open(client, manifest, cache::default_cache_dir())?;
    Ok(Arc::new(store))
}

/// Check the cached manifest against the service, behind the launch.
///
/// Its own client and its own thread, because the point is that nothing waits
/// for it: the store already has the manifest it will run on, and this only
/// decides what the *next* launch opens. A background thread that cannot answer
/// keeps a socket busy until the patch client's request timeout and then goes
/// away, which is the correct amount of fuss to make about a check nobody is
/// waiting on.
fn revalidate_manifest() {
    std::thread::spawn(|| {
        qos::set(qos::Class::Utility);
        let client = patch::Client::from_env();
        match client.revalidate(&paths::support_dir()) {
            // Says what happened, not what it wishes had: the new manifest is
            // stored and the next launch opens on it, but nothing here re-syncs
            // the client artifacts. See [`patch::Client::revalidate`].
            Ok(true) => note!("[gwnative] the service has published a new manifest; stored"),
            Ok(false) => {}
            // Not an error the player has anything to do about: the app is
            // running the client it already had, which is what it would have
            // done anyway.
            Err(e) => note!("[gwnative] could not check for a new client build: {e}"),
        }
    });
}

/// Fetch the client, unless the only thing on offer is a build that has already
/// failed here.
///
/// The rejection check is the reason this fetches the manifest itself rather
/// than letting `patch::sync_with` do it: the identity of the build being
/// offered has to be known while declining it still costs nothing.
fn sync(
    root: &Path,
    unsound: &[&'static str],
    generations: &generation::Store,
) -> error::Result<()> {
    if unsound.is_empty() {
        note!("[gwnative] refreshing client artifacts");
    } else {
        note!(
            "[gwnative] fetching client artifacts: {}",
            unsound.join(", ")
        );
    }
    let client = patch::Client::from_env();
    let manifest = client.fetch_manifest()?;
    let names = patch::artifacts();
    let offered = generation::identify(&manifest, &names)?;

    if generations.rejected(&offered) {
        if unsound.is_empty() {
            note!(
                "[gwnative] the service still offers client build {offered}, which never reached \
                 a first frame here; keeping the one on disk"
            );
            return Ok(());
        }
        // The alternative to a build that did not work is no client at all, so
        // it gets another try — loudly, because if it fails the same way the
        // line above is the one that explains why nothing changed.
        note!(
            "[gwnative] client build {offered} never reached a first frame here, but the client \
             on disk is incomplete, so there is nothing else to run"
        );
    }

    generations.stash(root, &names);
    let fetched = patch::sync_with(&client, &manifest, root)?;
    for (name, bytes) in fetched {
        note!("[gwnative]   {name} ({bytes} bytes)");
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
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    getrandom(&mut bytes);
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
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
