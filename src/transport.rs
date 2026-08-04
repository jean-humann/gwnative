//! Outbound HTTPS, through the operating system's stack.
//!
//! Every request here goes out over one shared `NSURLSession`, which is the
//! same CFNetwork machinery Safari uses: TLS with ALPN, and HTTP/2 when the
//! server offers it. That last part is the reason this module exists. The patch
//! CDN speaks HTTP/2, and on HTTP/2 every concurrent fetch is a stream on a
//! single warm connection — one TCP handshake, one TLS handshake, one congestion
//! window that stays hot for the whole download.
//!
//! The ureq client this replaces spoke HTTP/1.1, where a connection carries
//! one request at a time, and kept three idle connections per host against
//! eight concurrent fetches — so of the fetches that arrived together, most
//! found the pool empty and opened a connection of their own: a TCP
//! handshake, a TLS handshake and a cold congestion window, to move one
//! 256 KiB chunk. ureq's maintainer has ruled HTTP/2 out of scope (ureq#219),
//! so the fix is a different stack, and the one the OS ships costs nothing to
//! depend on. Same blank install, same machine: the boot set that took the
//! whole of a five-minute window to download over ureq arrives through this
//! module in well under a minute.
//!
//! Callers hand over a method, URL, headers and an optional body, and block
//! until the whole response is in memory. That contract is deliberate: it is
//! exactly what the ureq call sites already assumed, so the fetch pool in
//! `chunks` and the forwarder in `proxy` keep their shape and only the wire
//! underneath them changes.

use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, define_class, msg_send};
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSObject, NSObjectProtocol, NSString,
    NSURL, NSURLRequest, NSURLRequestCachePolicy, NSURLResponse, NSURLSession,
    NSURLSessionConfiguration, NSURLSessionDelegate, NSURLSessionTask, NSURLSessionTaskDelegate,
};

/// A whole response, buffered.
pub struct Response {
    pub status: u16,
    /// Names lower-cased. One entry per name: CFNetwork's header dictionary
    /// merges repeated headers into a single comma-joined value, so a response
    /// carrying several `Set-Cookie` lines arrives here as one. None of the
    /// current callers splits one back apart — the proxy discards cookie state
    /// and the patch client only reads `content-length` — but a caller that ever
    /// needs the individual values will not find them here.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `NoRedirect` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "GWNativeNoRedirect"]
    struct NoRedirect;

    unsafe impl NSObjectProtocol for NoRedirect {}

    unsafe impl NSURLSessionDelegate for NoRedirect {}

    /// Without a delegate, `NSURLSession` follows redirects on its own — the
    /// 3xx is swallowed and the request is replayed wherever `Location`
    /// pointed, headers and all. Both callers treat a redirect as a stop
    /// signal: the patch client because an endpoint that suddenly wants to
    /// send it elsewhere is wrong in a way worth surfacing, and the proxy
    /// because vetting `Location` against its allowlist *is* its job, which it
    /// cannot do from a response it never sees. Answering the callback with
    /// nil declines the redirect and delivers the 3xx itself as the response.
    unsafe impl NSURLSessionTaskDelegate for NoRedirect {
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        fn refuse_redirect(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse,
            _request: &NSURLRequest,
            completion: &DynBlock<dyn Fn(*mut NSURLRequest)>,
        ) {
            completion.call((std::ptr::null_mut(),));
        }
    }
);

impl NoRedirect {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `NSObject`'s designated initializer, on a freshly allocated
        // instance whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}

/// The one session everything shares. Sharing is not a nicety: HTTP/2
/// multiplexing happens per connection and connections are pooled per
/// session, so a session per caller would be back to paying handshakes.
/// `NSURLSession` is documented thread-safe and marked `Send + Sync`.
fn session() -> &'static NSURLSession {
    static SESSION: OnceLock<Retained<NSURLSession>> = OnceLock::new();
    SESSION.get_or_init(|| {
        let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        // No response cache: the patch client keeps its own content-addressed
        // store, and the proxy speaks for origins whose caching policy is not
        // ours to apply. Ephemeral already keeps everything off disk; this
        // keeps it out of memory too.
        config.setURLCache(None);
        config.setRequestCachePolicy(NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData);
        // No cookie jar either. ureq neither stored nor replayed cookies, and
        // a transport that silently attaches state to requests would change
        // what the proxy forwards. Cookies stay the page's business.
        config.setHTTPShouldSetCookies(false);
        config.setHTTPCookieStorage(None);
        let delegate = NoRedirect::new();
        // SAFETY: a nil queue asks the session to create its own serial
        // queue for delegate and completion-handler calls, which is the
        // documented default. The session retains the delegate.
        unsafe {
            NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
                &config,
                Some(ProtocolObject::from_ref(&*delegate)),
                None,
            )
        }
    })
}

/// Send one request and block until the whole response has arrived, for at
/// most `timeout`. Redirects are refused (the 3xx comes back as the
/// response); statuses are answers, not errors. The error string is transport
/// detail — DNS, TLS, timeout — in CFNetwork's words.
pub fn fetch(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<Response, String> {
    let target = NSURL::URLWithString(&NSString::from_str(url))
        .ok_or_else(|| format!("not a URL: {url}"))?;
    // Built fresh on the calling thread each time: NSMutableURLRequest is not
    // thread-safe, and the task copies it on creation anyway.
    let request = NSMutableURLRequest::requestWithURL(&target);
    request.setHTTPMethod(&NSString::from_str(method));
    // The request-level timeout is an idle timer — it fires when the
    // connection stalls, not when the transfer is merely slow. The total
    // budget is enforced at the receive below.
    request.setTimeoutInterval(timeout.as_secs_f64());
    for (name, value) in headers {
        request.setValue_forHTTPHeaderField(
            Some(&NSString::from_str(value)),
            &NSString::from_str(name),
        );
    }
    if let Some(body) = body {
        request.setHTTPBody(Some(&NSData::with_bytes(body)));
    }

    let (tx, rx) = mpsc::channel::<Result<Response, String>>();
    let handler = RcBlock::new(
        move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
            // Everything is copied into plain Rust before it leaves the block:
            // NSData is not Send, and this closure runs on the session's one
            // serial queue, where anything slower than a copy would stall
            // every other in-flight completion. The pointers are nullable —
            // data and response on failure, error on success.
            let outcome = (|| {
                if let Some(error) = unsafe { error.as_ref() } {
                    return Err(error.localizedDescription().to_string());
                }
                let response = unsafe { response.as_ref() }
                    .and_then(|r| r.downcast_ref::<NSHTTPURLResponse>())
                    .ok_or("no HTTP response")?;
                let status = u16::try_from(response.statusCode())
                    .map_err(|_| "implausible status code".to_owned())?;
                let fields = response.allHeaderFields();
                let mut headers = Vec::with_capacity(fields.count());
                for key in fields.allKeys().iter() {
                    let (Some(name), Some(value)) = (
                        key.downcast_ref::<NSString>(),
                        fields
                            .objectForKey(&*key)
                            .and_then(|v| v.downcast::<NSString>().ok()),
                    ) else {
                        continue;
                    };
                    headers.push((name.to_string().to_ascii_lowercase(), value.to_string()));
                }
                let body = unsafe { data.as_ref() }
                    .map(|d| d.to_vec())
                    .unwrap_or_default();
                Ok(Response {
                    status,
                    headers,
                    body,
                })
            })();
            let _ = tx.send(outcome);
        },
    );
    // SAFETY: the completion handler must be sendable; the closure above
    // captures only the channel sender, which is Send.
    let task = unsafe { session().dataTaskWithRequest_completionHandler(&request, &handler) };
    task.resume();

    match rx.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(_) => {
            // The handler still fires after a cancel; its send lands in a
            // dropped receiver and is discarded.
            task.cancel();
            Err(format!("timed out after {}s: {url}", timeout.as_secs()))
        }
    }
}
