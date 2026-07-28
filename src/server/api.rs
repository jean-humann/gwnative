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
use crate::{app, diagnostics, disk, dock, keychain, net, ws};

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
/// serve it as content — which is also how the three routes that need a
/// snapshot behave when there is none, so a build with no image installed
/// refuses them exactly as it refuses any other name it does not know.
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
        // Diagnostic channel for the bring-up harness. Real host calls will go
        // over WKScriptMessageHandlerWithReply; this only exists so a page can
        // report results without a UI.
        "__report" if request.method == "POST" => {
            note!("[report] {}", String::from_utf8_lossy(&request.body));
            no_content(stream)?;
        }
        "__dns" => dns(request, stream)?,
        "__credentials" => credentials(request, stream)?,
        "__settings" => settings(request, stream, context)?,
        "__socket" => return socket(request, stream, flow).map(Some),
        "__diag" => diag(request, stream, context)?,
        "__resident" => match &context.snapshot {
            Some(store) => resident(stream, store)?,
            None => return Ok(None),
        },
        "__warm" => match &context.snapshot {
            Some(store) => warm(request, stream, store)?,
            None => return Ok(None),
        },
        "__prefetch" => match &context.snapshot {
            Some(store) => prefetch(request, stream, store)?,
            None => return Ok(None),
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
        "DELETE" => {
            keychain::clear();
            note!("[credentials] cleared");
            no_content(stream)
        }
        _ => not_allowed(stream, "GET, PUT, DELETE"),
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
    let body = serde_json::json!({
        "footprintMiB": usage.footprint as f64 / 1048576.0,
        "cpuSeconds": usage.cpu().as_secs_f64(),
        "chunks": { "fromCache": from_cache, "fetched": fetched, "coalesced": coalesced },
        "retries": { "attempts": retried, "sleptMs": slept_ms },
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
    let free = disk::available(store.cache_dir());

    if request.method == "POST" {
        if request.query == "stop" {
            store.stop_full_download();
        } else if let Some(free) = free.filter(|free| *free < outstanding + DISK_HEADROOM) {
            // Refused rather than started-and-abandoned: a sweep that runs
            // until the volume fills takes the rest of the machine down with
            // it, and the client streams perfectly well without it.
            note!(
                "[prefetch] refused: {:.1} GB free, {:.1} GB needed",
                free as f64 / 1e9,
                (outstanding + DISK_HEADROOM) as f64 / 1e9
            );
            let body = format!(
                r#"{{"error":"not enough room","free":{free},"needed":{}}}"#,
                outstanding + DISK_HEADROOM
            );
            return json(stream, 507, body.as_bytes());
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
    let cached = store.resident_count();
    let total = store.chunk_count();
    let chunk_size = store.chunk_size();
    // `null` rather than a guess when the volume will not say: the page treats
    // not knowing as no reason to stop, which is the same thing it does when
    // this whole route is missing.
    let free = free.map_or("null".to_owned(), |free| free.to_string());
    // Two numbers, because they answer different questions: `outstanding` is
    // what the download would write and belongs in the prose, `needed` is what
    // the volume must have and is what the refusal above compares.
    let needed = outstanding + DISK_HEADROOM;
    let body = format!(
        r#"{{"cached":{cached},"total":{total},"fetched":{fetched},"running":{running},"chunkSize":{chunk_size},"outstanding":{outstanding},"needed":{needed},"free":{free}}}"#
    );
    json(stream, 200, body.as_bytes())
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
