//! Everything the page asks for as a document: the snapshot, the client's own
//! web requests, and the files on disk.
//!
//! None of it is gated. The vendored client issues these itself and cannot be
//! taught to carry a token, so what protects them instead is that they cannot
//! reach anything: `resolve` refuses any path that escapes the web root, and
//! `proxy` forwards only to a closed table of hosts.

use std::io::Write;
use std::net::TcpStream;

use super::{Context, Flow, tracing};
use crate::chunks::ChunkStore;
use crate::http::{
    BODY_BUFFER, Request, etag, mime, parse_range, resolve, respond, respond_head,
    respond_not_modified, respond_proxy, respond_static, respond_streaming, text,
};
use crate::patch::SNAPSHOT;
use crate::proxy;
use crate::wasm::{COMPANION_KERNEL, COMPANION_KERNEL_PATH};

/// Largest span served from one request. The harness asks for far less; this
/// only stops a stray `Range: bytes=0-` from walking the whole image in one go.
///
/// The body is streamed, so the 8 MiB cap no longer bounds memory, but it still
/// bounds how long one request occupies a connection and a fetch slot. A 32 MiB
/// request is 128 chunks, and a demand read arriving behind it waits for all of
/// them.
const MAX_RANGE_BYTES: u64 = 8 * 1024 * 1024;

/// Answer anything that is not a host capability.
pub(super) fn serve(
    request: &Request,
    stream: &mut TcpStream,
    context: &Context,
) -> std::io::Result<Flow> {
    let flow = Flow::after(request);

    if request.path == SNAPSHOT
        && let Some(store) = &context.snapshot
    {
        // Worth timing rather than merely counting: a range that has to fetch
        // the chunks it covers blocks the client for as long as it takes, and
        // it is the one host-side latency the player feels directly. Record the
        // total rather than a gauge — a gauge keeps only the last value written,
        // so it would report whichever range happened to finish last instead of
        // anything about the session. Divided by the request count it is the
        // mean, and the mean hides the stalls, so keep the worst as well.
        let began = std::time::Instant::now();
        // A range that fails part way through has already put bytes on the wire
        // under a Content-Length it can no longer honour. There is no way back
        // to a clean message boundary, so the connection ends here.
        let intact = snapshot(stream, request, store)?;
        let ms = began.elapsed().as_secs_f64() * 1000.0;
        let metrics = &context.recorder.metrics;
        metrics.count("gw.range.requests", 1.0);
        metrics.count("gw.range.ms.total", ms);
        metrics.peak("gw.range.ms.max", ms);
        return Ok(if intact { flow } else { Flow::Close });
    }

    // The client's own web requests, which it addressed to this origin because
    // its glue rewrote them. See `proxy` for why, and why the table is closed.
    let (route, tail) = match request.path.split_once('/') {
        Some((route, tail)) => (route, tail),
        None => (request.path.as_str(), ""),
    };
    if proxy::host(route).is_some() {
        return forward(request, route, tail, stream, flow);
    }

    // A request for the document itself is the page starting over — the Reload
    // item, or a failed boot taking its own escape hatch. The class the fetch
    // threads run at follows what is on screen, and what is on screen is about
    // to be the launcher again.
    // Bare `/` and the explicit name are the same document — `resolve` treats
    // them alike, so this has to as well.
    if (request.path.is_empty() || request.path == "index.html")
        && let Some(store) = &context.snapshot
    {
        store.back_to_waiting();
    }

    // The companion, which is in the binary rather than the web root — see
    // `crate::wasm::COMPANION_KERNEL`. Answered here rather than in
    // [`static_file`] because there is no file: `resolve` would refuse it and
    // the page would be told a name it correctly asked for is forbidden.
    if request.path == COMPANION_KERNEL_PATH {
        if tracing() {
            note!(
                "[loopback] 200 /{COMPANION_KERNEL_PATH} ({} bytes, embedded)",
                COMPANION_KERNEL.len()
            );
        }
        // No validator: it is six kilobytes fetched once per launch, and the
        // only honest one would be a hash of bytes that are already in memory.
        respond_static(stream, "application/wasm", COMPANION_KERNEL, None)?;
        return Ok(flow);
    }

    static_file(request, stream, context)?;
    Ok(flow)
}

/// Relay one of the client's own requests to the host it was addressed to.
///
/// `route` is the first path segment and `tail` everything after the slash that
/// ended it, both borrowed from the request — upstream reads the body rather
/// than taking it, so nothing here has to own anything.
fn forward(
    request: &Request,
    route: &str,
    tail: &str,
    stream: &mut TcpStream,
    flow: Flow,
) -> std::io::Result<Flow> {
    if request.body.len() > proxy::MAX_BODY {
        text(stream, 413, "request body too large")?;
        return Ok(flow);
    }
    // Upstream wants the tail rooted; the caller's split left the slash behind.
    let tail = format!("/{tail}");
    match proxy::forward(
        route,
        &tail,
        &request.query,
        &request.method,
        &request.headers,
        &request.body,
    ) {
        Ok(reply) => {
            note!(
                "[proxy] {} /{route}{tail} -> {}",
                request.method,
                reply.status
            );
            respond_proxy(stream, &reply)?;
            // `respond_proxy` relays upstream's headers, and upstream's framing
            // is not ours to reinterpret, so this hop ends here.
            Ok(Flow::Close)
        }
        Err(e) => {
            note!("[proxy] {} /{route}{tail}: {e}", request.method);
            text(stream, 502, "proxy error")?;
            Ok(flow)
        }
    }
}

/// A file from the web root, revalidated rather than re-sent when the page
/// already holds it.
fn static_file(
    request: &Request,
    stream: &mut TcpStream,
    context: &Context,
) -> std::io::Result<()> {
    // The derived client answers to the base module's own name, so the page
    // asks for one thing and the glue's `locateFile` needs no special case.
    // A certified module that cannot instantiate is retried against ArenaNet's
    // exact bytes. The query is issued only by the injected harness and changes
    // which local file answers, never the path being resolved.
    let original = request.param("gwnative-original").as_deref() == Some("1");
    let derived = (!original)
        .then(|| context.derived_wasm.get(&request.path).cloned())
        .flatten();
    let Some(file) = derived.or_else(|| resolve(&context.root, &request.path)) else {
        note!("[loopback] 403 /{}", request.path);
        return text(stream, 403, "forbidden");
    };

    let tag = std::fs::metadata(&file).ok().as_ref().and_then(etag);
    // Answer the validator before reading the file, not after: skipping the
    // 8.2 MB read is half of what the validator is for.
    if let Some(tag) = &tag
        && request.if_none_match.as_deref() == Some(tag.as_str())
    {
        if tracing() {
            note!("[loopback] 304 /{}", request.path);
        }
        return respond_not_modified(stream, tag);
    }
    match std::fs::read(&file) {
        Ok(body) => {
            if tracing() {
                note!("[loopback] 200 /{} ({} bytes)", request.path, body.len());
            }
            respond_static(stream, mime(&file), &body, tag.as_deref())
        }
        Err(_) => {
            note!("[loopback] 404 /{}", request.path);
            text(stream, 404, "not found")
        }
    }
}

/// Answer a snapshot range, streaming the body rather than assembling it.
///
/// Returns whether the connection is still usable: `false` means the head was
/// already on the wire when the body failed, so the caller must close.
fn snapshot(
    stream: &mut TcpStream,
    request: &Request,
    store: &ChunkStore,
) -> std::io::Result<bool> {
    let total = store.snapshot_size();

    // A HEAD is how the client learns the image size without pulling a byte.
    if request.method == "HEAD" {
        respond_head(stream, total, &[("Accept-Ranges", "bytes".into())])?;
        return Ok(true);
    }

    // Refusing an unranged GET is deliberate: answering it means streaming
    // 4.2 GB, which is never what the client wants.
    let Some((start, end)) = request.range.as_deref().and_then(|r| parse_range(r, total)) else {
        return unsatisfiable(stream, total, "snapshot requires a byte range");
    };

    let length = (end - start + 1).min(MAX_RANGE_BYTES);
    // Arithmetic, not I/O — which is the point. The old shape read the whole
    // span into a Vec purely to learn how long it was, so a 32 MiB range meant
    // 32 MiB resident in the host before a single byte reached the socket.
    let produced = store.readable(start, length);
    if produced == 0 {
        return unsatisfiable(stream, total, "range starts past the end of the snapshot");
    }

    // Chunks arrive as they are read, so the first one can be on the wire while
    // the last is still being fetched — and the peak allocation is one chunk
    // rather than the whole range.
    let last = start + produced - 1;
    respond_streaming(
        stream,
        206,
        "application/octet-stream",
        produced,
        &[
            ("Content-Range", format!("bytes {start}-{last}/{total}")),
            ("Accept-Ranges", "bytes".into()),
        ],
    )?;

    let mut body = std::io::BufWriter::with_capacity(BODY_BUFFER, &mut *stream);
    if let Err(e) = store.read_into(start, length, &mut body) {
        note!("[loopback] /{SNAPSHOT} {start}+{length} failed mid-body: {e}");
        return Ok(false);
    }
    body.flush()?;
    drop(body);
    stream.flush()?;

    if tracing() {
        note!("[loopback] 206 /{SNAPSHOT} bytes {start}-{last}/{total} ({produced} bytes)");
    }
    Ok(true)
}

/// A 416 carrying the length, which is what tells the client what it should
/// have asked for. Nothing has been written yet, so the connection survives.
fn unsatisfiable(stream: &mut TcpStream, total: u64, why: &str) -> std::io::Result<bool> {
    respond(
        stream,
        416,
        "text/plain",
        why.as_bytes(),
        &[("Content-Range", format!("bytes */{total}"))],
    )?;
    Ok(true)
}
