//! The host capabilities the page can call, all of them named under `__`.
//!
//! Name resolution, an outbound socket bridge, the keychain, the settings, the
//! download control, the residency map, the diagnostics channel. None of it is
//! page content: it is the page asking the host to do something only the host
//! can do.
//!
//! Loopback is host-wide — any process running as this user can reach the
//! port — so the token is the whole of what separates the page from everything
//! else on the machine. It is checked once, at the top of [`serve`], rather
//! than on each route: a route added below inherits the gate by being under
//! `__`, and the wire test in [`super`] is what keeps that true. `Gw.snapshot`
//! and the static files stay open, because the vendored client requests those
//! itself and cannot be taught to carry a token.

use std::net::TcpStream;
use std::sync::Arc;

use super::{Context, Flow, tracing};
use crate::chunks::ChunkStore;
use crate::http::{Request, json, no_content, respond, text, token_matches};
use crate::{app, cache, diagnostics, disk, dock, keychain, net, relaunch, steam, ws};

/// Room to leave behind after a full download.
///
/// A volume with nothing left is not merely full: swap, the WebKit caches and
/// every atomic rename in this process all want space, and the first thing to
/// break would be the game rather than the download that caused it. 2 GB is
/// generous next to a 4.2 GB snapshot and cheap next to the alternative.
const DISK_HEADROOM: u64 = 2 * 1024 * 1024 * 1024;

/// Answer a `__` route.
///
/// `None` means this request is not one of ours and the caller should go on to
/// serve it as content. Every `__` name below answers here instead, including
/// the four that need a game image and have none — see [`no_snapshot`] for what
/// falling through cost them.
pub(super) fn serve(
    request: &Request,
    stream: &mut TcpStream,
    context: &Context,
) -> std::io::Result<Option<Flow>> {
    if !request.path.starts_with("__") {
        return Ok(None);
    }
    let flow = Flow::after(request);

    if !token_matches(&context.token, request.offered_token().as_deref()) {
        note!(
            "[loopback] refused an untokened {} /{}",
            request.method,
            request.path
        );
        text(stream, 403, "forbidden")?;
        return Ok(Some(flow));
    }

    // Every arm answers and falls out to `flow`, bar the two that decide the
    // connection's fate themselves.
    match request.path.as_str() {
        // Everything the page logs, including everything the client logs
        // through it. Two destinations and both are wanted: the terminal is
        // where a developer running `cargo run` reads it, and the diagnostics
        // file is the only one of the two a player has — without it, every
        // warning printed before a failure was lost to the one person who
        // could have sent it on.
        "__report" if request.method == "POST" => {
            let batch = String::from_utf8_lossy(&request.body);
            note!("[report] {batch}");
            context.recorder.page(&batch);
            no_content(stream)?;
        }
        "__dns" => dns(request, stream)?,
        "__credentials" => credentials(request, stream)?,
        "__steam" => steam_session(request, stream)?,
        "__settings" => settings(request, stream, context)?,
        "__socket" => return socket(request, stream, flow).map(Some),
        "__diag" => diag(request, stream, context)?,
        "__resident" => match &context.snapshot {
            Some(store) => resident(stream, store)?,
            None => no_snapshot(request, stream)?,
        },
        "__warm" => match &context.snapshot {
            Some(store) => warm(request, stream, store)?,
            None => no_snapshot(request, stream)?,
        },
        "__prefetch" => match &context.snapshot {
            Some(store) => prefetch(request, stream, store)?,
            None => no_snapshot(request, stream)?,
        },
        // The harness says the first frame is up. Two things follow from that.
        // Everything the store read before now is what booting costs, so that
        // is the list worth warming next time — and the client on disk has just
        // proved it runs here, which is the only evidence a freshly synced
        // build can offer and the thing the generation record is waiting for.
        // Deliberately one route: a second one would be a second chance to
        // disagree about what "it booted" means.
        "__booted" => {
            if let Some(store) = &context.snapshot {
                store.seal_boot_list();
            }
            context.generations.prove();
            no_content(stream)?;
        }
        // The client has exited cleanly and there is nothing left on screen but
        // its last frame. Answered before quitting rather than after, because
        // `terminate:` runs the flush and the reply would otherwise race it.
        "__quit" if request.method == "POST" => {
            no_content(stream)?;
            app::request_quit();
            return Ok(Some(Flow::Close));
        }
        // Ask the *next* launch to start from an empty cache. Deliberately not
        // a delete: see `cache::request_clear` for why a running store cannot
        // have the directory removed from under it. The caller relaunches.
        //
        // The directory comes from the open store rather than from
        // `cache::default_cache_dir`, so what gets cleared is what this launch
        // is actually reading — and a launch with no store has nothing it could
        // honestly promise to clear.
        "__data" if request.method == "DELETE" => match &context.snapshot {
            Some(store) => match cache::request_clear(store.cache_dir()) {
                Ok(()) => {
                    note!("[chunks] the game data will be cleared at the next launch");
                    no_content(stream)?;
                }
                Err(e) => {
                    note!("[chunks] the clear could not be armed: {e}");
                    text(stream, 500, &e.to_string())?;
                }
            },
            None => no_snapshot(request, stream)?,
        },
        // The same quit, with something to come back to. Deliberately two
        // routes and not one with a flag: quitting is unconditional and this
        // one is not, and a caller that asked to come back and did not needs to
        // be told so rather than left looking at a closing app.
        "__relaunch" if request.method == "POST" => match relaunch::start() {
            Ok(()) => {
                no_content(stream)?;
                app::request_quit();
                return Ok(Some(Flow::Close));
            }
            Err(reason) => {
                note!("[relaunch] {reason}");
                text(stream, 500, &reason)?;
            }
        },
        // An unknown `__` name, or a known one asked with a method it does not
        // answer. Said plainly rather than left to fall through to the static
        // file server, which would refuse it with a bare 403 and no hint that
        // the name was the problem.
        _ => {
            note!(
                "[loopback] no route for {} /{}",
                request.method,
                request.path
            );
            text(stream, 404, "no such host route")?;
        }
    }
    Ok(Some(flow))
}

/// The answer for a route that speaks for the game image on a launch that has
/// none.
///
/// A launch reaches this before the manifest is fetched, and stays here if the
/// fetch failed. Both routes used to answer it by declining to handle the
/// request at all, which left the static file server to refuse a `__` path with
/// a bare 403 — the exact outcome the fall-through arm above exists to prevent,
/// and one that reads to a caller as "you are not allowed to ask" rather than
/// "there is nothing here to ask about".
fn no_snapshot(request: &Request, stream: &mut TcpStream) -> std::io::Result<()> {
    note!(
        "[loopback] {} /{} has no game image to answer for",
        request.method,
        request.path
    );
    text(stream, 404, "no game image on this launch")
}

/// The game asks for an address before it dials. Answering here keeps name
/// resolution on the host, where the public-unicast policy lives.
fn dns(request: &Request, stream: &mut TcpStream) -> std::io::Result<()> {
    let name = request.param("name").unwrap_or_default();
    match net::resolve(&name) {
        Ok(address) => {
            if tracing() {
                note!("[dns] {name} -> {address}");
            }
            text(stream, 200, &address.to_string())
        }
        Err(e) => {
            note!("[dns] {name}: {e}");
            text(stream, 502, &e.to_string())
        }
    }
}

/// Saved login, gated with every other `__` route — which is what makes the
/// keychain's own access control mean anything on a host-wide port.
fn credentials(request: &Request, stream: &mut TcpStream) -> std::io::Result<()> {
    match request.method.as_str() {
        "GET" => match keychain::load() {
            Some(credentials) => {
                let body = serde_json::to_vec(&credentials).unwrap_or_default();
                note!("[credentials] read from the keychain");
                json(stream, 200, &body)
            }
            // Not an error: a first launch has nothing saved, and the client
            // treats "none" as "ask the player".
            None => {
                note!("[credentials] nothing saved yet");
                text(stream, 404, "no stored credentials")
            }
        },
        "PUT" => {
            let stored = serde_json::from_slice(&request.body)
                .map_err(|e| e.to_string())
                .and_then(|c: keychain::Credentials| keychain::store(&c));
            match stored {
                Ok(()) => {
                    note!("[credentials] saved to the keychain");
                    no_content(stream)
                }
                Err(e) => {
                    note!("[credentials] not saved: {e}");
                    text(stream, 400, &e)
                }
            }
        }
        "DELETE" => match keychain::clear() {
            Ok(()) => {
                note!("[credentials] cleared");
                no_content(stream)
            }
            Err(e) => {
                note!("[credentials] not cleared: {e}");
                text(stream, 500, &e)
            }
        },
        _ => not_allowed(stream, "GET, PUT, DELETE"),
    }
}

/// The Steam OAuth session, separate from the ArenaNet email/password item.
///
/// POST is used for both reads because an interactive request can open UI and
/// therefore is not a cacheable retrieval. The route never logs or reflects a
/// token except in its token-gated JSON response.
fn steam_session(request: &Request, stream: &mut TcpStream) -> std::io::Result<()> {
    match request.method.as_str() {
        "POST" => {
            let asked = serde_json::from_slice::<steam::Request>(&request.body);
            match asked {
                Ok(asked) => match steam::resolve(asked.silent) {
                    Some(answer) => {
                        let body = serde_json::to_vec(&answer).unwrap_or_default();
                        json(stream, 200, &body)
                    }
                    None => text(stream, 404, "no Steam session available"),
                },
                Err(e) => text(stream, 400, &format!("invalid Steam session request: {e}")),
            }
        }
        "PUT" => {
            let stored = serde_json::from_slice(&request.body)
                .map_err(|e| e.to_string())
                .and_then(|value: steam::Storeback| steam::storeback(value));
            match stored {
                Ok(()) => no_content(stream),
                Err(e) => {
                    note!("[steam] the session expiry could not be refreshed: {e}");
                    text(stream, 400, &e)
                }
            }
        }
        "DELETE" => match steam::clear() {
            Ok(()) => {
                note!("[steam] the saved session was cleared");
                no_content(stream)
            }
            Err(e) => {
                note!("[steam] the saved session could not be cleared: {e}");
                text(stream, 500, &e)
            }
        },
        _ => not_allowed(stream, "POST, PUT, DELETE"),
    }
}

/// What the player chose. GET is the authoritative read — the page is handed a
/// copy at document start, but a settings window opened an hour later must not
/// show what was true at launch. PUT takes a patch and answers with the whole,
/// so the page never has to guess what its change merged into.
fn settings(request: &Request, stream: &mut TcpStream, context: &Context) -> std::io::Result<()> {
    match request.method.as_str() {
        "GET" => {
            let body = serde_json::to_vec(&context.settings.get()).unwrap_or_default();
            json(stream, 200, &body)
        }
        "PUT" => {
            let applied = serde_json::from_slice(&request.body)
                .map_err(|e| e.to_string())
                .and_then(|raw| context.settings.apply(&raw));
            match applied {
                Ok(settings) => {
                    // The updater keeps the two update switches itself, so a
                    // patch that moved either has to reach it. Sent for every
                    // accepted patch rather than only those two: the comparison
                    // that decides whether anything actually changed has to
                    // happen on the main thread anyway, where the properties
                    // can be read. A build with no updater drops it.
                    crate::updater::follow(
                        settings.auto_check_updates,
                        settings.auto_install_updates,
                    );
                    let body = serde_json::to_vec(&settings).unwrap_or_default();
                    json(stream, 200, &body)
                }
                // A refused patch is a bug in the page, not a player error, so
                // it is said out loud rather than only answered with 400.
                Err(e) => {
                    note!("[settings] refused a patch: {e}");
                    text(stream, 400, &e)
                }
            }
        }
        _ => not_allowed(stream, "GET, PUT"),
    }
}

/// The upgrade to a game socket. The only route that answers with something
/// other than a response: past here the connection has stopped being HTTP.
fn socket(request: &Request, stream: &mut TcpStream, flow: Flow) -> std::io::Result<Flow> {
    if !request.wants_websocket() {
        respond(
            stream,
            426,
            "text/plain",
            b"__socket is a websocket endpoint",
            &[("Upgrade", "websocket".into())],
        )?;
        return Ok(flow);
    }
    let destination = request.param("to").unwrap_or_default();
    ws::accept(stream, request.websocket_key.as_deref().unwrap_or(""))?;
    Ok(Flow::Bridge(destination))
}

/// Which chunks are already on disk, one bit each. The launcher draws from this
/// rather than from a count, because a bitmap also says where the gaps are.
fn resident(stream: &mut TcpStream, store: &ChunkStore) -> std::io::Result<()> {
    let bits = store.resident_bitmap();
    respond(
        stream,
        200,
        "application/octet-stream",
        &bits,
        &[("X-Chunk-Size", store.chunk_size().to_string())],
    )
}

/// "Have this range ready", answered with nothing.
///
/// The client warms what it is about to read, and the page used to do that by
/// requesting the range and discarding the response. That put the bytes on the
/// loopback socket and then through `arrayBuffer()` — 256 KiB of garbage per
/// chunk, at boot faster than the collector kept up with, which is most of why
/// the renderer's footprint peaked near 1.7 GB on a launch that settles at 400
/// MB. Warming is the same work on the host with no body attached.
fn warm(request: &Request, stream: &mut TcpStream, store: &ChunkStore) -> std::io::Result<()> {
    let number = |name: &str| request.param(name).and_then(|v| v.parse::<u64>().ok());
    let (Some(offset), Some(bytes)) = (number("offset"), number("bytes")) else {
        return text(stream, 400, "__warm needs offset and bytes");
    };
    match store.warm(offset, bytes) {
        Ok(()) => no_content(stream),
        Err(e) => {
            note!("[warm] {offset}+{bytes}: {e}");
            text(stream, 502, &e.to_string())
        }
    }
}

/// The page's own metrics. POST folds a batch in, GET reads back everything the
/// host has — its own sampler's figures included, so one fetch answers "what is
/// this process doing" from inside the page.
fn diag(request: &Request, stream: &mut TcpStream, context: &Context) -> std::io::Result<()> {
    if request.method == "POST" {
        match serde_json::from_slice(&request.body) {
            Ok(body) => diagnostics::absorb(&context.recorder.metrics, &body),
            Err(e) => note!("[diag] ignoring a malformed batch: {e}"),
        }
        // Nothing back. The page posts a batch every second and never reads the
        // reply, so answering with the full snapshot meant serializing every
        // metric the host holds once a second and handing the page a response
        // body it would never drain — one more each second, all of them
        // growing, for the life of the session.
        return no_content(stream);
    }
    let usage = diagnostics::usage().unwrap_or_default();
    // The chunk store's own tally rides along rather than living on a route of
    // its own. It used to be `__stats`, which nothing ever called: a second
    // endpoint to remember, answering a question this one is already the place
    // to ask.
    let (from_cache, fetched, coalesced) = context
        .snapshot
        .as_ref()
        .map_or((0, 0, 0), |store| store.stats());
    // Beside them because they explain them: a slow session with no retries was
    // queueing, and the same session with seconds on the clock here was waiting
    // for a network that had already failed.
    let (retried, slept_ms) = context
        .snapshot
        .as_ref()
        .map_or((0, 0), |store| store.retries());
    // The one field here the page reads back for the player rather than for a
    // log: when the client reports a fatal read, this is the only account of
    // why it happened that anyone has. See `ChunkStore::last_failure`.
    let last_failure = context
        .snapshot
        .as_ref()
        .and_then(|store| store.last_failure());
    let body = serde_json::json!({
        "footprintMiB": usage.footprint as f64 / 1048576.0,
        "cpuSeconds": usage.cpu().as_secs_f64(),
        "chunks": { "fromCache": from_cache, "fetched": fetched, "coalesced": coalesced },
        "retries": { "attempts": retried, "sleptMs": slept_ms },
        "lastFetchFailure": last_failure,
        "metrics": context.recorder.metrics.snapshot(),
    });
    json(stream, 200, body.to_string().as_bytes())
}

/// Full download: POST starts or stops the background sweep, GET polls it. The
/// launcher offers this as the alternative to streaming on demand.
fn prefetch(
    request: &Request,
    stream: &mut TcpStream,
    store: &Arc<ChunkStore>,
) -> std::io::Result<()> {
    // What a full download would still have to write, and what the volume says
    // it could take. Both are reported on every poll so the page can show the
    // price before the player agrees to it — asking for 4.2 GB and then filling
    // the disk is the failure this exists to avoid.
    let outstanding = (store.chunk_count() - store.resident_count()) as u64 * store.chunk_size();
    // Two numbers, because they answer different questions: `outstanding` is
    // what the download would write and belongs in the prose, `needed` is what
    // the volume must have and is what the refusal below compares. Taken once,
    // so the figure the refusal reports and the figure it decided on cannot
    // come to be written differently.
    let needed = outstanding + DISK_HEADROOM;
    let free = disk::available(store.cache_dir());

    if request.method == "POST" {
        if request.query == "stop" {
            store.stop_full_download();
        } else if request.query == "verify" {
            // Ahead of the disk check, which asks what a download would still
            // have to write: this one writes nothing, and can only ever free
            // space by discarding what fails. Refusing it for want of room
            // would refuse the check on exactly the full volume where an
            // interrupted write is likeliest to have left damage.
            store.start_verify();
        } else if let Some(free) = free.filter(|free| *free < needed) {
            // Refused rather than started-and-abandoned: a sweep that runs
            // until the volume fills takes the rest of the machine down with
            // it, and the client streams perfectly well without it.
            note!(
                "[prefetch] refused: {:.1} GB free, {:.1} GB needed",
                free as f64 / 1e9,
                needed as f64 / 1e9
            );
            let body = serde_json::json!({
                "error": "not enough room",
                "free": free,
                "needed": needed,
            });
            return json(stream, 507, body.to_string().as_bytes());
        } else if store.start_full_download() {
            // Only on the start that actually started something: the icon
            // follows the sweep, and a second POST while one is running is
            // answered by the same progress the first one is already drawing.
            dock::follow(store);
        }
    }
    // `cached` is residency, not sweep progress: the launcher's question is how
    // much of the game is already paid for, and a sweep's own counter restarts
    // at zero every time one does. `fetched` is kept beside it because it is
    // the only number that moves when a sweep is re-walking ground it already
    // has, which is what "running but not advancing" looks like from the page.
    let (fetched, _, running) = store.prefetch_progress();
    // Reported on every poll rather than from a route of its own, so the page
    // draws the check and the sweep from one timer. They never run at once —
    // the check is what a full launch does before deciding whether a sweep is
    // needed — but the page should not have to know that to render.
    let (checked, verify_total, verifying, discarded) = store.verify_progress();
    let cached = store.resident_count();
    let total = store.chunk_count();
    let chunk_size = store.chunk_size();
    // Built by the encoder rather than spelled out, like [`diag`] above it and
    // every other JSON body in this crate. Nothing in here is a string today,
    // so writing the braces by hand was not wrong — it was one field away from
    // being wrong, and a second hand-rolled encoder is how the first one gets
    // copied into a third place that does carry a string.
    let body = serde_json::json!({
        "cached": cached,
        "total": total,
        "fetched": fetched,
        "running": running,
        "chunkSize": chunk_size,
        "outstanding": outstanding,
        "needed": needed,
        "verifying": verifying,
        "verified": checked,
        // Distinct chunks, not indices — equal to `total` on a snapshot that
        // repeats nothing, which today's does, and smaller on one that does
        // not. Sent as its own field either way so the page never has to guess
        // which case it is in.
        "verifyTotal": verify_total,
        "discarded": discarded,
        // `null` rather than a guess when the volume will not say: the page
        // treats not knowing as no reason to stop, which is the same thing it
        // does when this whole route is missing. `None` encodes as `null` with
        // nothing here having to say so.
        "free": free,
    });
    json(stream, 200, body.to_string().as_bytes())
}

/// The right method exists; this was not it. `Allow` is the only part a client
/// can act on, so it and the prose are written from the same string.
fn not_allowed(stream: &mut TcpStream, allow: &str) -> std::io::Result<()> {
    respond(
        stream,
        405,
        "text/plain",
        format!("use {allow}").as_bytes(),
        &[("Allow", allow.to_owned())],
    )
}
