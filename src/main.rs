//! Native macOS host for the Guild Wars WebAssembly client.
//!
//! ArenaNet publishes matching JSPI and Asyncify JavaScript/WebAssembly pairs.
//! Their generated JavaScript has to run as-is in WebKit; this app probes its
//! own WKWebView and chooses the JSPI pair only when suspend/resume works,
//! otherwise it uses the official Asyncify pair. Everything outside that realm
//! (patching, chunk storage, sockets, credentials, windowing) is Rust.

// Out of alphabetical order on purpose: `macro_rules!` is in scope only for
// what follows it, and `note!` is used by nearly every module below.
#[macro_use]
mod log;

mod activation_cover;
mod alert;
mod app;
mod cache;
mod chunks;
mod cli;
mod commands;
mod diagnostics;
mod disk;
mod dock;
mod error;
mod game_api;
mod generation;
mod generation_state;
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
mod profile;
mod proxy;
mod qos;
mod relaunch;
mod release;
mod renderer;
mod report;
#[cfg(test)]
mod scratch;
mod server;
mod settings;
mod shell;
mod sockets;
mod transport;
mod updater;
mod wasm;
mod webview;
mod window;
mod ws;

use std::path::{Path, PathBuf};
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
    // Before anything is opened, downloaded or locked: a question about this
    // executable deserves an answer and not a launch.
    let invocation = match cli::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(exit) => {
            let out: &mut dyn std::io::Write = if exit.failed {
                &mut std::io::stderr()
            } else {
                &mut std::io::stdout()
            };
            let _ = writeln!(out, "{}", exit.message);
            std::process::exit(i32::from(exit.failed) * 2);
        }
    };
    for notice in &invocation.notices {
        note!(
            "[gwnative] {}: {} ({:?})",
            notice.option,
            notice.message,
            notice.kind
        );
    }
    if invocation.verbose {
        server::enable_tracing();
        sockets::enable_tracing();
    }
    let command = invocation.command;
    if command == cli::Command::Certify {
        let root = invocation.web_root.clone().unwrap_or_else(paths::web_root);
        match wasm::certificate_candidate(&root) {
            Ok(candidate) => println!("{candidate}"),
            Err(reason) => {
                eprintln!("{reason}");
                std::process::exit(1);
            }
        }
        return;
    }
    let base_support = paths::base_support_dir();
    if command == cli::Command::Profiles {
        let profiles = match profile::list(&base_support) {
            Ok(profiles) => profiles,
            Err(reason) => {
                note!("[gwnative] {reason}");
                std::process::exit(2);
            }
        };
        for profile in profiles {
            println!(
                "{}\t{}\t{}\t{}",
                profile.id,
                profile.display_name,
                profile.color,
                profile.port()
            );
        }
        return;
    }
    let profile = match profile::select(&base_support, invocation.profile.as_deref()) {
        Ok(profile) => profile,
        Err(reason) => {
            note!("[gwnative] {reason}");
            std::process::exit(2);
        }
    };
    let paths = paths::Layout::new(&invocation, &profile);
    let client_sync = command == cli::Command::Sync;
    let maintenance = matches!(command, cli::Command::Sync | cli::Command::Repair);
    let headless = command == cli::Command::Serve;
    // The two commands above are the runs with a terminal attached. Everything
    // that would otherwise put a message on screen asks this first.
    let windowed = !headless && !maintenance;

    // Held for as long as the process lives; the kernel takes it back if the
    // process does not.
    let _instance = hold_the_only_instance(windowed, &paths, invocation.new_instance);

    // Profiles share the content-addressed game-data cache even though their
    // client manifests are isolated. Hold this before any active manifest can
    // be promoted, and keep it for the store's lifetime.
    let cache_lease = cache::prepare(paths.cache_dir()).unwrap_or_else(|reason| {
        alert::fatal(
            windowed,
            "Guild Wars game data is busy",
            &format!("The shared game-data cache could not be locked safely.\n\n{reason}"),
        )
    });
    let client = patch::Client::from_env().with_offline(invocation.offline);
    let protected_chunks = client.cached_profile_chunk_names(&base_support);
    cache::finish_maintenance(&cache_lease, paths.cache_dir(), &protected_chunks).unwrap_or_else(
        |reason| {
            alert::fatal(
                windowed,
                "Guild Wars game data could not be maintained",
                &reason.to_string(),
            )
        },
    );

    // Before the client can ask for the login, so the reason it will not get one
    // is on screen ahead of the dialog rather than after it.
    keychain::check_identity();

    let root = paths.web_root().to_owned();
    // One client and one manifest, for everything below that needs either.
    let manifest = load_manifest(
        &client,
        client_sync,
        paths.support_dir(),
        invocation.offline,
    );
    let revalidate = manifest
        .as_ref()
        .is_ok_and(|(_, source)| !matches!(source, patch::Source::Service))
        && !invocation.offline
        && !invocation.no_update;

    // Repair is a game-data operation. In particular, `repair --no-update`
    // must not install a pending client generation merely because the service
    // offered one after the active snapshot manifest was promoted.
    let generations = if command == cli::Command::Repair {
        Arc::new(generation::Store::open(
            paths.support_dir().join("generations"),
        ))
    } else {
        install_client(
            &root,
            &client,
            manifest
                .as_ref()
                .map(|(manifest, source)| (manifest, *source)),
            client_sync,
            windowed,
            paths.support_dir(),
        )
    };
    if !invocation.needs_snapshot() {
        return;
    }
    // Do this only after installation. A pending offer may be the manifest
    // being installed above; refreshing that file concurrently could otherwise
    // promote a newer manifest over the artifacts fetched from the older one.
    if revalidate {
        revalidate_manifest(paths.support_dir().to_owned());
    }
    // Snapshot chunks must come from the manifest promoted with the client that
    // is actually on disk. `manifest` may be a newer pending offer whose client
    // download failed, or one that rollback just refused.
    let active_manifest = client.active_manifest(paths.support_dir());
    let snapshot = match active_manifest {
        Ok(manifest) => open_and_warm_snapshot(
            client,
            manifest,
            paths.cache_dir(),
            cache_lease,
            invocation.no_prefetch || maintenance,
        ),
        // Without a manifest there is no chunk list, so there is no snapshot —
        // the same outcome as failing to open one, reported the same way.
        Err(e) => {
            note!("[gwnative] snapshot unavailable: {e}");
            None
        }
    };
    if let Some(image) = invocation.image_path.as_deref() {
        let Some(store) = &snapshot else {
            note!("[gwnative] a local image cannot be imported without a cached manifest");
            std::process::exit(1);
        };
        if let Err(reason) = store.import_image(image) {
            note!(
                "[gwnative] local game image {} was refused: {reason}",
                image.display()
            );
            std::process::exit(1);
        }
    }
    if matches!(command, cli::Command::Sync | cli::Command::Repair) {
        verify_and_download_snapshot(snapshot, invocation.jobs);
        return;
    }

    // Every browser launch selects one immutable, inventory-verified shell.
    // This is after instance exclusion and maintenance, but before the server
    // or WebKit can observe a path from the revision.
    let shell_root = paths.prepare_shell().unwrap_or_else(|error| {
        alert::fatal(
            windowed,
            "Guild Wars could not prepare its browser shell",
            &format!(
                "No complete reviewed shell revision could be selected, so the browser was not \
                 started with a partial update.\n\n{error}"
            ),
        )
    });

    // Started before the window so that whatever the shell costs to build is
    // in the record too.
    let recorder = diagnostics::Recorder::open(paths.support_dir().join("diagnostics"));
    // First line in the file, so a log sent on by a player says which Mac and
    // which build it came from without anybody having to write back and ask.
    recorder.session();
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

    // Read before the window exists: the render scale the client is handed and
    // the gesture translation the page installs are both settled before the
    // first frame, so asking the page to fetch them later would mean booting
    // once at the wrong scale and correcting it in front of the player. Which
    // client module to derive is settled here too — see below.
    let settings_path = paths.support_dir().join("settings.json");
    if invocation.legacy.reset_preferences {
        match settings::reset(&settings_path) {
            Ok(true) => note!(
                "[gwnative] local preferences reset; previous values kept at {}",
                settings_path.with_extension("json.reset").display()
            ),
            Ok(false) => {}
            Err(error) => note!("[gwnative] local preferences could not be reset: {error}"),
        }
    }
    let profile_settings = Arc::new(settings::Store::open(settings_path));
    let legacy_update_settings = if profile.is_default() {
        profile_settings.get()
    } else {
        settings::Store::open(base_support.join("settings.json")).get()
    };
    // Application updates replace one bundle, not one profile. Sparkle's own
    // preferences are app-global too, so a small global record owns update
    // intent and cadence. The old default settings file seeds it once.
    let update_settings = Arc::new(settings::UpdateStore::open(
        base_support.join("updates.json"),
        &legacy_update_settings,
    ));
    let settings = Arc::new(settings::ScopedStore::new(
        profile_settings,
        update_settings,
    ));

    // Derive the client that can save a template, if this is a build we have
    // certified, and layer optional enhancements on top when the player has
    // asked for one. A failure here is never fatal: the untransformed module
    // still plays, it just cannot save, list or delete a build — which is where
    // the client started. See `wasm` for what each derived module changes.
    //
    // The outcome is carried to the page as well as to the log. A player who
    // clicks Save in the client's template window and watches nothing happen is
    // owed a sentence about why, and the log is not where they will look for
    // it; `settings-panel.js` is what turns this into that sentence.
    let enhance = settings.get().enhancements_enabled();
    let wasm::Prepared {
        derived: derived_wasm,
        module,
    } = match wasm::prepare(
        &root,
        paths.derived_dir(),
        &paths::certificate_dir(),
        enhance,
        &generations,
    ) {
        Ok(prepared) => prepared,
        Err(reason) => {
            note!("[gwnative] client certification unavailable: {reason}");
            wasm::failed(enhance)
        }
    };
    module.logs();

    let tokens = server::CapabilityTokens {
        browser: session_token(),
        game_reader: session_token(),
        game_publisher: session_token(),
    };
    let loopback = match server::spawn(server::Config {
        root: root.clone(),
        shell_root,
        snapshot,
        recorder,
        derived_wasm,
        settings,
        generations: Arc::clone(&generations),
        tokens: tokens.clone(),
        port: paths.port(),
        credential_account: profile.keychain_account(),
    }) {
        Ok(loopback) => loopback,
        // Nothing downstream has an answer to this: the client is a page, and
        // without an origin to serve it from there is no client. Maintenance
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
        note!("[gwnative] session token {}", tokens.browser);
    }
    // External state consumers receive no credential, settings, diagnostics,
    // process-control, or publication authority.
    if std::env::var_os("GWNATIVE_PRINT_GAME_TOKEN").is_some() {
        note!("[gwnative] game API read token {}", tokens.game_reader);
    }

    if headless {
        park_headless(&loopback, &tokens.browser);
    }
    run_windowed(
        &loopback,
        &tokens,
        &module,
        &invocation,
        paths.support_dir(),
        profile.website_data_store_id(),
        (root, generations),
    );
}

fn verify_and_download_snapshot(snapshot: Option<Arc<chunks::ChunkStore>>, jobs: Option<usize>) {
    let Some(snapshot) = snapshot else {
        note!("[gwnative] game-data verification could not start without a cached manifest");
        std::process::exit(1);
    };
    if !snapshot.start_verify() {
        note!("[gwnative] a game-data check is already running");
        return;
    }
    let mut reported = u64::MAX;
    loop {
        let (checked, total, running, discarded, failed) = snapshot.verify_progress();
        let percent = checked
            .saturating_mul(100)
            .checked_div(total)
            .unwrap_or(100);
        if percent != reported {
            reported = percent;
            note!("[gwnative] checking cached game data: {checked}/{total} ({percent}%)");
        }
        if !running {
            note!(
                "[gwnative] game-data check complete: {checked}/{total}, \
                 {discarded} corrupt chunk(s) discarded for safe refetch, \
                 {failed} cache failure(s)"
            );
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    download_snapshot(Some(snapshot), jobs);
}

fn download_snapshot(snapshot: Option<Arc<chunks::ChunkStore>>, jobs: Option<usize>) {
    let Some(snapshot) = snapshot else {
        note!("[gwnative] the full game image is unavailable without a cached manifest");
        std::process::exit(1);
    };
    let total = snapshot.chunk_count();
    let resident = snapshot.resident_count();
    let verify_failures = snapshot.verify_progress().4;
    if resident == total && verify_failures == 0 {
        note!("[gwnative] full game image is already present");
        return;
    }

    let workers = jobs.unwrap_or(32);
    match snapshot.start_full_download_with_workers(workers) {
        Ok(true) => {}
        Ok(false) => note!("[gwnative] a full-image download is already running"),
        Err(reason) => {
            note!("[gwnative] full image refused: {reason}");
            std::process::exit(1);
        }
    }
    let mut reported = u64::MAX;
    loop {
        let (done, sweep_total, running) = snapshot.prefetch_progress();
        let percent = done
            .saturating_mul(100)
            .checked_div(sweep_total)
            .unwrap_or(100);
        if percent != reported {
            reported = percent;
            note!("[gwnative] downloading full game image: {done}/{sweep_total} ({percent}%)");
        }
        if !running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    let resident = snapshot.resident_count();
    if resident != total || verify_failures > 0 {
        let reason = snapshot
            .last_failure()
            .unwrap_or_else(|| "one or more chunks could not be cached".into());
        note!("[gwnative] full game image is incomplete: {resident}/{total} chunks ({reason})");
        std::process::exit(1);
    }
    note!("[gwnative] full game image verified: {resident}/{total} chunks");
}

/// Take the single-instance lock, or hand the running app the foreground.
///
/// Before [`paths::web_root`], which seeds files a second instance may be
/// reading. A second launch of a windowed app should look like asking for the
/// one that is already open, not like nothing happening; raising it by pid
/// rather than bundle id works in development too, where there is no bundle to
/// identify.
fn hold_the_only_instance(
    windowed: bool,
    paths: &paths::Layout,
    new_instance: bool,
) -> (instance::Instance, Option<instance::Instance>) {
    let profile_path = paths.support_dir().join("gwnative.lock");
    if new_instance {
        return (acquire_instance(windowed, &profile_path), None);
    }

    let global_path = paths::base_support_dir().join("gwnative.lock");
    if profile_path == global_path {
        return (acquire_instance(windowed, &global_path), None);
    }
    let global = acquire_instance(windowed, &global_path);
    let profile = acquire_instance(windowed, &profile_path);
    (profile, Some(global))
}

fn acquire_instance(windowed: bool, lock_path: &Path) -> instance::Instance {
    // A relaunch is started by the app it replaces, so for a moment there
    // really are two — and this is the one that has to wait for the other.
    let patience = if relaunch::is_successor() {
        relaunch::PATIENCE
    } else {
        std::time::Duration::ZERO
    };
    match instance::acquire(lock_path, patience) {
        Ok(held) => held,
        Err(reason) => {
            note!("[gwnative] {reason}");
            if windowed
                && let Some(pid) = instance::holder(lock_path)
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

/// The manifest this launch runs on, and the decision about how old it may be.
///
/// One manifest for the whole launch. Two things need one — [`install_client`],
/// to know which build the service is offering, and the chunk store, for the
/// snapshot's chunk list — and each used to fetch its own, so a launch that
/// synced asked for the same 1.2 MB document twice.
///
/// A launch takes the copy on disk and checks it against the service afterwards,
/// off the path to the window; see [`patch::Client::manifest`]. The `sync`
/// command does not, because it is an explicit request for whatever is on offer
/// now.
fn load_manifest(
    client: &patch::Client,
    client_sync: bool,
    support_dir: &Path,
    offline: bool,
) -> error::Result<(manifest::Manifest, patch::Source)> {
    let dir = support_dir;
    if offline {
        return client
            .cached_manifest(dir)
            .map(|manifest| (manifest, patch::Source::Active));
    }
    if client_sync {
        return client
            .fetch_manifest(dir)
            .map(|manifest| (manifest, patch::Source::Service));
    }
    client.manifest(dir)
}

/// Make the web root hold a client this build can run, and return the record
/// that says so.
///
/// Recovery comes before anything reads the root. A failed optional transform
/// is disabled while ArenaNet's exact generation stays installed; only a failed
/// unmodified attempt can restore its stashed predecessor. See `generation` for
/// why presence was never enough on its own.
///
/// Three things ask for a sync, and until the manifest was in hand here only two
/// could: the `sync` command, an artifact that is missing or has rotted, and the
/// service offering a build that is not the one installed. Without that last one
/// a published patch was picked up when a file *rotted* rather than when it
/// *shipped*, which is to say by accident.
fn install_client(
    root: &Path,
    client: &patch::Client,
    manifest: Result<(&manifest::Manifest, patch::Source), &error::Error>,
    client_sync: bool,
    windowed: bool,
    support_dir: &Path,
) -> Arc<generation::Store> {
    let generations = Arc::new(generation::Store::open(support_dir.join("generations")));
    match generations.recover(root) {
        generation::Recovery::None => {}
        generation::Recovery::InstallationRestored => note!(
            "[gwnative] restored the proven client and manifest after an interrupted installation"
        ),
        generation::Recovery::TransformDisabled { runtime, build } => note!(
            "[gwnative] {runtime} transform {}… did not reach a first frame; \
             retrying the same official client unmodified",
            &build[..12]
        ),
        generation::Recovery::RuntimeFailed(launch) => note!(
            "[gwnative] official {} runtime failed before proof; keeping both official runtimes",
            launch.runtime
        ),
    }

    // The build on offer, named before a byte of it is downloaded: `identify`
    // works from the sizes and chunk hashes the manifest already carries, and on
    // a warm launch that manifest came off the disk. So a launch with nothing to
    // do learns there is nothing to do without making a request.
    //
    // Both ways of not having one collapse to a string, because both are only
    // ever displayed: either there is no manifest, or there is one that does not
    // describe this client, and either way what this launch has is the client on
    // disk.
    let names = patch::artifacts();
    let missing = generations.unsound(root, &names);
    // A complete installation that predates the generation record is a valid
    // rollback target. Adopt it before deciding whether the service offers
    // something newer, so migration cannot replace the only playable copy
    // without preserving it first.
    if missing.is_empty() {
        generations.adopt(root, &names);
    }

    let plan = manifest
        .map_err(|e| e.to_string())
        .and_then(|(manifest, source)| {
            generation::identify(manifest, &names)
                .map(|offered| (manifest, source, offered))
                .map_err(|e| e.to_string())
        });

    let outdated = plan
        .as_ref()
        .is_ok_and(|(_, _, offered)| generations.stale(offered));

    // A manifest can change while all five client artifacts stay byte-for-byte
    // identical: snapshot chunks and their metadata have their own release
    // cadence. It is also possible to lose only the active manifest cache while
    // the recorded official client remains sound. In both cases the fetched
    // pending manifest names the exact installed artifact generation, so
    // promote it without re-downloading or unproving the client.
    if let Ok((_, source, offered)) = &plan
        && should_activate_pending_manifest(client_sync, missing.is_empty(), *source, outdated)
    {
        let failure = match client.activate_manifest(support_dir) {
            Ok(true) => {
                if !generations.refresh_manifest(offered) {
                    note!(
                        "[gwnative] activated snapshot metadata but could not update its \
                         generation record; the next launch will retry"
                    );
                }
                note!(
                    "[gwnative] activated updated snapshot metadata for client generation {offered}"
                );
                return generations;
            }
            Ok(false) => "the offered manifest had no pending cache entry".to_owned(),
            Err(error) => error.to_string(),
        };
        // A valid active manifest for these exact client artifacts still
        // describes a playable snapshot, so an update that could not be
        // promoted is deferred. If there is no matching active copy,
        // continuing would start the shell with an incoherent game image.
        let active_matches = client
            .active_manifest(support_dir)
            .and_then(|active| generation::identify(&active, &names))
            .is_ok_and(|active| active == *offered);
        if active_matches {
            note!(
                "[gwnative] could not activate updated snapshot metadata; \
                 keeping the active manifest: {failure}"
            );
            return generations;
        }
        alert::fatal(
            windowed,
            "Guild Wars could not be installed",
            &format!(
                "The client files are complete, but their matching game-data manifest \
                 could not be restored, so Guild Wars will not start with an unknown \
                 snapshot.\n\n{failure}"
            ),
        );
    }
    if missing.is_empty()
        && !outdated
        && let Ok((_, patch::Source::Active, offered)) = &plan
        && !generations.refresh_manifest(offered)
    {
        note!(
            "[gwnative] could not reconcile the active manifest with client generation {offered}"
        );
    }
    if !(client_sync || !missing.is_empty() || outdated) {
        return generations;
    }

    let failure = match &plan {
        Ok((manifest, _, offered)) => ClientSync {
            root,
            generations: &generations,
            client,
            manifest,
            offered,
            support_dir,
        }
        .run(&missing, client_sync)
        .err()
        .map(|e| e.to_string()),
        Err(e) => Some(e.clone()),
    };
    if let Some(detail) = failure {
        // Recheck after the failed transaction. Promotion and restoration both
        // touch live paths; the entry-state `missing` answer is not proof that
        // the client is still complete now. A stale but verified root can boot.
        // A partially restored root must stop here instead of handing mixed
        // runtime pairs to the page.
        let now_unsound = generations.unsound(root, &names);
        if now_unsound.is_empty() {
            note!("[gwnative] patch sync failed: {detail}");
        } else {
            alert::fatal(
                windowed,
                "Guild Wars could not be installed",
                &format!(
                    "The client files could not be downloaded or restored as one \
                     verified set, so Guild Wars will not start with mixed client \
                     files. Check the network connection and open Guild Wars \
                     again.\n\nAffected: {}\n\n{detail}",
                    now_unsound.join(", ")
                ),
            );
        }
    }
    generations
}

fn should_activate_pending_manifest(
    force_sync: bool,
    artifacts_sound: bool,
    source: patch::Source,
    client_is_stale: bool,
) -> bool {
    !force_sync && artifacts_sound && source != patch::Source::Active && !client_is_stale
}

/// Open the snapshot store and set it reading before anything asks it for bytes.
///
/// Gw.snapshot is 4.2 GB and a session touches a fraction of it, so it is served
/// as a virtual ranged file rather than downloaded. Without a store the shell
/// still opens; only the game data is unavailable.
fn open_and_warm_snapshot(
    client: patch::Client,
    manifest: manifest::Manifest,
    cache_dir: &Path,
    cache_lease: cache::Lease,
    no_prefetch: bool,
) -> Option<Arc<chunks::ChunkStore>> {
    let cache_dir = cache_dir.to_owned();
    match chunks::ChunkStore::open(client, manifest, cache_dir, cache_lease).map(Arc::new) {
        Ok(store) => {
            note!(
                "[gwnative] snapshot: {:.1} GB in {} KiB chunks, on demand",
                store.snapshot_size() as f64 / 1e9,
                store.chunk_size() / 1024
            );
            // Pull what the last boot needed while the window is still being
            // built. By the time the client asks, the chunks that gate the
            // first frame are already local.
            if !no_prefetch {
                store.warm_boot();
            }
            // And on the launch that has no list to replay — the first one —
            // stay a little ahead of wherever the client is reading instead.
            if !no_prefetch {
                store.start_readahead();
            }
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
fn run_windowed(
    loopback: &server::Loopback,
    tokens: &server::CapabilityTokens,
    module: &wasm::Module,
    invocation: &cli::Invocation,
    support_dir: &Path,
    website_data_store_id: Option<&str>,
    recovery: (PathBuf, Arc<generation::Store>),
) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // Before the window: the Dock tile is created with the application, and an
    // icon set afterwards is one the player can watch change.
    dock::set_icon(mtm);

    // Before the web view, because the page is handed the settings it starts
    // with and the updater is allowed to change two of them: on the first
    // launch after Sparkle shipped, the profile's opt-in is what seeds it, and
    // afterwards the updater's own answer is what the panel has to show.
    let automatic_updates_allowed = invocation.automatic_updates_allowed();
    if automatic_updates_allowed {
        updater::start(mtm, &loopback.settings);
    } else {
        note!("[gwnative] automatic application update checks disabled for this launch");
    }

    // The frame the web view is created at does not matter: `window::open`
    // resizes the window to the remembered one before it is ever shown, and the
    // content view follows.
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 800.0));
    let url = format!("http://{}/index.html", loopback.addr);
    let launch_nonce = session_token();
    let webview = webview::make(
        mtm,
        frame,
        webview::Origin {
            url: &url,
            token: &tokens.browser,
            game_publisher_token: &tokens.game_publisher,
            launch_nonce: &launch_nonce,
            website_data_store_id,
        },
        &loopback.settings.get(),
        module,
        invocation,
    );
    let window = window::open(
        mtm,
        &webview,
        support_dir.join("window.json"),
        invocation.legacy.window_mode,
    );
    activation_cover::install(&webview, &window, loopback.recorder.clone());

    // After the window, not before: two of the menu's items are requests to the
    // page, and one moves the window. The menu only has to exist before `run`.
    app.setMainMenu(Some(&menu::install(
        mtm,
        &webview,
        loopback.settings.clone(),
        loopback.recorder.clone(),
    )));

    commands::attach(&webview);
    // After the load has been asked for, which is fine: the delegate is
    // consulted when the navigation is decided, not when it is requested.
    let (root, generations) = recovery;
    renderer::guard(
        mtm,
        &webview,
        &format!("http://{}", loopback.addr),
        root,
        generations,
    );
    // Before `run`, because the first thing it decides — whether closing the
    // window quits — can be asked the moment the window appears.
    app::own_lifecycle(mtm, &webview);

    // After the menu, because the answer is shown through the same alert its
    // item uses, and off this thread — the request takes up to five seconds and
    // the page is loading. A no-op unless the player asked to be told; see
    // [`release::due`].
    if automatic_updates_allowed {
        menu::check_for_updates_at_launch(&loopback.settings);
    }

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

/// Check the cached manifest against the service, behind the launch.
///
/// Its own client and its own thread, because the point is that nothing waits
/// for it: the store already has the manifest it will run on, and this only
/// decides what the *next* launch opens. A background thread that cannot answer
/// keeps a socket busy until the patch client's request timeout and then goes
/// away, which is the correct amount of fuss to make about a check nobody is
/// waiting on.
fn revalidate_manifest(support_dir: std::path::PathBuf) {
    std::thread::spawn(move || {
        qos::set(qos::Class::Utility);
        let client = patch::Client::from_env();
        match client.revalidate(&support_dir) {
            // The next launch opens on this pending offer, sees it names a build
            // that is not the one on disk, and installs the matching artifact
            // set before promoting the manifest. See [`patch::Client::revalidate`].
            Ok(true) => note!(
                "[gwnative] the service has published a new client generation; \
                 it will be installed at the next launch"
            ),
            Ok(false) => {}
            // Not an error the player has anything to do about: the app is
            // running the client it already had, which is what it would have
            // done anyway.
            Err(e) => note!("[gwnative] could not check for a new client generation: {e}"),
        }
    });
}

/// Fetch the client, unless the only thing on offer is a build that has already
/// failed here.
///
/// The manifest and the id of the build it offers both belong to the caller:
/// naming that build is how the caller decided this was worth calling, and the
/// rejection check below needs the same name while declining still costs
/// nothing. Nothing here fetches a manifest — see [`load_manifest`].
struct ClientSync<'a> {
    root: &'a Path,
    generations: &'a generation::Store,
    client: &'a patch::Client,
    manifest: &'a manifest::Manifest,
    offered: &'a str,
    support_dir: &'a Path,
}

impl ClientSync<'_> {
    fn run(self, unsound: &[&'static str], force: bool) -> error::Result<()> {
        let Self {
            root,
            generations,
            client,
            manifest,
            offered,
            support_dir,
        } = self;
        if unsound.is_empty() {
            note!("[gwnative] installing client generation {offered}");
        } else {
            note!(
                "[gwnative] fetching client artifacts: {}",
                unsound.join(", ")
            );
        }
        let names = patch::artifacts();

        if generations.rejected(offered) && !force {
            if unsound.is_empty() {
                note!(
                    "[gwnative] the service still offers client generation {offered}, which never reached \
                 a first frame here; keeping the one on disk"
                );
                return Ok(());
            }
            // The alternative to a build that did not work is no client at all, so
            // it gets another try — loudly, because if it fails the same way the
            // line above is the one that explains why nothing changed.
            note!(
                "[gwnative] client generation {offered} never reached a first frame here, but the client \
             on disk is incomplete, so there is nothing else to run"
            );
        }

        if !generations.stash(root, &names) && unsound.is_empty() {
            return Err(std::io::Error::other(
                "the working client could not be preserved before replacement",
            )
            .into());
        }
        let fetched = match patch::sync_with(client, manifest, root) {
            Ok(fetched) => fetched,
            Err(error) => {
                // Promotion has its own best-effort restore, but a failure in that
                // restore is exactly when the durable, verified generation stash
                // matters. Keep its record until the whole pair is back in place.
                if matches!(
                    generations.recover(root),
                    generation::Recovery::InstallationRestored
                ) {
                    note!("[gwnative] restored the proven client after sync failed");
                }
                return Err(error);
            }
        };
        if let Err(error) = client.activate_manifest(support_dir) {
            if matches!(
                generations.recover(root),
                generation::Recovery::InstallationRestored
            ) {
                note!("[gwnative] restored the proven client after manifest activation failed");
            }
            return Err(error);
        }
        for (name, bytes) in fetched {
            note!("[gwnative]   {name} ({bytes} bytes)");
        }
        if !generations.record(offered, root, &names) {
            let _ = generations.recover(root);
            return Err(std::io::Error::other(
                "the installed client could not be recorded durably",
            )
            .into());
        }
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_client_artifacts_activate_pending_snapshot_metadata() {
        assert!(should_activate_pending_manifest(
            false,
            true,
            patch::Source::Pending,
            false,
        ));
        assert!(should_activate_pending_manifest(
            false,
            true,
            patch::Source::Service,
            false,
        ));
    }

    #[test]
    fn manifest_activation_never_replaces_client_installation_work() {
        assert!(!should_activate_pending_manifest(
            true,
            true,
            patch::Source::Pending,
            false,
        ));
        assert!(!should_activate_pending_manifest(
            false,
            false,
            patch::Source::Pending,
            false,
        ));
        assert!(!should_activate_pending_manifest(
            false,
            true,
            patch::Source::Pending,
            true,
        ));
        assert!(!should_activate_pending_manifest(
            false,
            true,
            patch::Source::Active,
            false,
        ));
    }
}
