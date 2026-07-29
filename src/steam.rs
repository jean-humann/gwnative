//! Steam account authentication for Steam-purchased Guild Wars accounts.
//!
//! The Reforged client already knows how to redeem a Steam OAuth2 access token;
//! the host contract it probes is `login.hasProvider("Steam")` followed by
//! `login.getAuthToken("Steam", { silent })`. This module supplies the native
//! half of that contract:
//!
//! - a silent request may only replay an unexpired token from macOS Keychain;
//! - an explicit request opens one isolated, non-persistent `WKWebView`;
//! - the view is confined to Steam- and Valve-owned top-level origins;
//! - the Guild Wars redirect is intercepted before it loads, and only after its
//!   OAuth `state` matches;
//! - sign-out cancels an in-flight window before deleting the token.
//!
//! The OAuth configuration below is the Steam provider configuration bundled
//! with the official Guild Wars Reforged client. GWoNmac first documented how
//! that provider maps onto the web client's host seam; this is an independent
//! AppKit/WebKit implementation of the same protocol.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSURL, NSURLComponents, NSURLQueryItem, NSURLRequest, NSUUID,
};
use objc2_web_kit::{
    WKFrameInfo, WKMediaCaptureType, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy, WKOpenPanelParameters,
    WKPermissionDecision, WKSecurityOrigin, WKUIDelegate, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore, WKWindowFeatures,
};
use serde::{Deserialize, Serialize};

use crate::{app, keychain};

const CLIENT_ID: &str = "CE9BDCEC";
const AUTHORIZE_URL: &str = "https://steamcommunity.com/oauth/login";
const REDIRECT_URL: &str = "https://www.guildwars.com/app/live/auth";
const ALLOWED_HOST_SUFFIXES: [&str; 4] = [
    "steamcommunity.com",
    "steampowered.com",
    "steamstatic.com",
    "valvesoftware.com",
];

const KEYCHAIN_SERVICE: &str = "gwnative Steam (Guild Wars)";
const KEYCHAIN_ACCOUNT: &str = "oauth";
const MAX_TOKEN_LENGTH: usize = 4096;
const TOKEN_LIFETIME_MS: u64 = 365 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Session {
    token: String,
    /// Unix epoch milliseconds. `None` means the service has not supplied one,
    /// not that the token expired at the epoch.
    expiry: Option<u64>,
}

#[derive(Deserialize)]
pub(crate) struct Request {
    /// Only an explicit `false` is interactive; malformed requests are rejected
    /// by the route before they reach this type.
    pub silent: bool,
}

#[derive(Deserialize)]
pub(crate) struct Storeback {
    pub token: String,
    pub expiry: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct Answer {
    token: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn valid(session: &Session) -> bool {
    !session.token.is_empty()
        && session.token.len() <= MAX_TOKEN_LENGTH
        && session.expiry.is_none_or(|expiry| expiry > now_ms())
}

fn load() -> Option<Session> {
    let raw = keychain::load_secret(
        KEYCHAIN_SERVICE,
        KEYCHAIN_ACCOUNT,
        "the saved Steam session",
    )?;
    match serde_json::from_slice::<Session>(&raw) {
        Ok(session) if valid(&session) => Some(session),
        Ok(_) => {
            note!("[steam] the saved session expired or was invalid; discarding it");
            let _ =
                keychain::clear_secret(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, "saved Steam session");
            None
        }
        Err(e) => {
            note!("[steam] the saved session is unreadable ({e}); discarding it");
            let _ =
                keychain::clear_secret(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, "saved Steam session");
            None
        }
    }
}

fn save(session: &Session) -> Result<(), String> {
    if session.token.is_empty() || session.token.len() > MAX_TOKEN_LENGTH {
        return Err("the Steam token has an invalid length".into());
    }
    let encoded = serde_json::to_vec(session).map_err(|e| e.to_string())?;
    keychain::store_secret(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &encoded)
}

fn clear_saved() -> Result<(), String> {
    keychain::clear_secret(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, "saved Steam session")
}

struct Pending {
    generation: u64,
    waiters: Vec<Sender<Option<String>>>,
}

#[derive(Default)]
struct Coordinator {
    generation: u64,
    pending: Option<Pending>,
}

fn coordinator() -> &'static Mutex<Coordinator> {
    static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();
    COORDINATOR.get_or_init(|| Mutex::new(Coordinator::default()))
}

fn lock_coordinator() -> std::sync::MutexGuard<'static, Coordinator> {
    match coordinator().lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            coordinator().clear_poison();
            poisoned.into_inner()
        }
    }
}

/// Resolve the token the client asked for.
///
/// A silent request never opens UI. Explicit callers join the one interactive
/// attempt already in flight, so double-clicking the button cannot create two
/// bearer tokens or two Steam windows.
pub(crate) fn resolve(silent: bool) -> Option<Answer> {
    if silent {
        let _coordinator = lock_coordinator();
        let session = load()?;
        note!("[steam] vended the saved session");
        return Some(Answer {
            token: session.token,
        });
    }

    let (sender, receiver) = mpsc::channel();
    {
        let mut coordinator = lock_coordinator();
        if let Some(pending) = &mut coordinator.pending {
            pending.waiters.push(sender);
        } else {
            // Reaching the login screen after a replay means the server may
            // have revoked a locally unexpired token. An explicit click is
            // intent to acquire a fresh one, not to replay the same credential
            // for up to a year.
            if let Err(e) = clear_saved() {
                note!("[steam] could not discard the previous session: {e}");
            }
            coordinator.generation = coordinator.generation.wrapping_add(1);
            let generation = coordinator.generation;
            coordinator.pending = Some(Pending {
                generation,
                waiters: vec![sender],
            });
            begin(generation);
        }
    }

    let token = receiver.recv().ok().flatten()?;
    note!("[steam] acquired a new session");
    Some(Answer { token })
}

/// Refresh the saved expiry only when the client hands back the token already
/// held. It must never replace a working credential with a different value.
pub(crate) fn storeback(value: Storeback) -> Result<(), String> {
    let _coordinator = lock_coordinator();
    let Some(mut session) = load() else {
        return Ok(());
    };
    if value.token.is_empty() || value.token != session.token {
        return Ok(());
    }
    let now = now_ms();
    if value.expiry.is_some_and(|expiry| expiry <= now) {
        return Ok(());
    }
    if value.expiry.is_none() && session.expiry.is_some() {
        return Ok(());
    }
    session.expiry = value
        .expiry
        .map(|expiry| expiry.min(now.saturating_add(TOKEN_LIFETIME_MS)));
    save(&session)
}

/// Make sign-out final: first detach and refuse every older waiter, then close
/// its UI, then remove the persisted token.
pub(crate) fn clear() -> Result<(), String> {
    let (pending, cleared) = {
        let mut coordinator = lock_coordinator();
        coordinator.generation = coordinator.generation.wrapping_add(1);
        let pending = coordinator.pending.take();
        let cleared = clear_saved();
        (pending, cleared)
    };
    if let Some(pending) = pending {
        for waiter in pending.waiters {
            let _ = waiter.send(None);
        }
        cancel(pending.generation);
    }
    cleared
}

fn begin(generation: u64) {
    let request = Box::new(generation);
    // SAFETY: `begin_on_main` is the only consumer of this box, and
    // `app::to_main` invokes it exactly once.
    unsafe { app::to_main(Box::into_raw(request).cast(), begin_on_main) };
}

extern "C" fn begin_on_main(context: *mut c_void) {
    // SAFETY: `begin` created exactly this box for this callback.
    let generation = unsafe { *Box::from_raw(context.cast::<u64>()) };
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    if !is_current(generation) {
        return;
    }
    if let Err(e) = open(mtm, generation) {
        note!("[steam] the sign-in window could not open: {e}");
        complete(generation, None);
    }
}

fn cancel(generation: u64) {
    let request = Box::new(generation);
    // SAFETY: `cancel_on_main` is the only consumer of this box.
    unsafe { app::to_main(Box::into_raw(request).cast(), cancel_on_main) };
}

extern "C" fn cancel_on_main(context: *mut c_void) {
    // SAFETY: `cancel` created exactly this box for this callback.
    let generation = unsafe { *Box::from_raw(context.cast::<u64>()) };
    settle_window(generation, None, true);
}

fn is_current(generation: u64) -> bool {
    lock_coordinator()
        .pending
        .as_ref()
        .is_some_and(|pending| pending.generation == generation)
}

/// Complete one OAuth generation. Persistence and waiter replies happen away
/// from AppKit's main thread; a slow keychain must not freeze the sign-in sheet.
fn complete(generation: u64, token: Option<String>) {
    std::thread::spawn(move || {
        let token = token.filter(|token| !token.is_empty() && token.len() <= MAX_TOKEN_LENGTH);
        let pending = {
            let mut coordinator = lock_coordinator();
            let current = coordinator
                .pending
                .as_ref()
                .is_some_and(|pending| pending.generation == generation);
            if !current {
                return;
            }
            if let Some(token) = &token {
                let session = Session {
                    token: token.clone(),
                    expiry: Some(now_ms().saturating_add(TOKEN_LIFETIME_MS)),
                };
                if let Err(e) = save(&session) {
                    // The freshly acquired token still authenticates this
                    // session; only once-per-machine persistence was lost.
                    note!("[steam] the new session could not be saved: {e}");
                }
            }
            coordinator.pending.take()
        };
        let Some(pending) = pending else { return };
        for waiter in pending.waiters {
            let _ = waiter.send(token.clone());
        }
    });
}

#[derive(Debug, PartialEq, Eq)]
enum Redirect {
    Unrelated,
    Rejected,
    Token(String),
}

struct Parts {
    scheme: String,
    host: String,
    port: Option<i64>,
    user: Option<String>,
    password: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

fn parts(raw: &str) -> Option<Parts> {
    let components = NSURLComponents::componentsWithString_encodingInvalidCharacters(
        &NSString::from_str(raw),
        false,
    )?;
    Some(Parts {
        scheme: components.scheme()?.to_string().to_lowercase(),
        host: components.host()?.to_string().to_lowercase(),
        port: components.port().map(|port| port.longLongValue()),
        user: components.user().map(|value| value.to_string()),
        password: components.password().map(|value| value.to_string()),
        path: components
            .path()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        query: components
            .percentEncodedQuery()
            .map(|value| value.to_string()),
        fragment: components
            .percentEncodedFragment()
            .map(|value| value.to_string()),
    })
}

fn is_allowed(raw: &str) -> bool {
    if raw == "about:blank" {
        return true;
    }
    let Some(url) = parts(raw) else { return false };
    if url.user.is_some() || url.password.is_some() || url.scheme != "https" {
        return false;
    }
    if url.port.is_some_and(|port| port != 443) {
        return false;
    }
    ALLOWED_HOST_SUFFIXES
        .iter()
        .any(|suffix| url.host == *suffix || url.host.ends_with(&format!(".{suffix}")))
}

fn query_value(raw: Option<&str>, name: &str) -> Option<String> {
    let query = raw?;
    let components = NSURLComponents::new();
    components.setPercentEncodedQuery(Some(&NSString::from_str(query)));
    components.queryItems()?.iter().find_map(|item| {
        (item.name().to_string() == name).then(|| item.value().map(|value| value.to_string()))?
    })
}

fn inspect_redirect(raw: &str, expected_state: &str) -> Redirect {
    let Some(url) = parts(raw) else {
        return Redirect::Unrelated;
    };
    let Some(target) = parts(REDIRECT_URL) else {
        return Redirect::Unrelated;
    };
    if url.user.is_some()
        || url.password.is_some()
        || url.scheme != "https"
        || target.scheme != "https"
        || url.host != target.host
        || url.port != target.port
        || url.path != target.path
    {
        return Redirect::Unrelated;
    }

    let fragment_has_carrier = ["access_token", "token", "state"]
        .iter()
        .any(|name| query_value(url.fragment.as_deref(), name).is_some());
    let carrier = if fragment_has_carrier {
        url.fragment.as_deref()
    } else {
        url.query.as_deref()
    };
    if query_value(carrier, "state").as_deref() != Some(expected_state) {
        return Redirect::Rejected;
    }
    let token = query_value(carrier, "access_token")
        .or_else(|| query_value(carrier, "token"))
        .filter(|token| !token.is_empty() && token.len() <= MAX_TOKEN_LENGTH);
    token.map_or(Redirect::Rejected, Redirect::Token)
}

fn authorization_url(state: &str) -> Option<String> {
    let components = NSURLComponents::componentsWithString(&NSString::from_str(AUTHORIZE_URL))?;
    let items = NSArray::from_retained_slice(&[
        NSURLQueryItem::queryItemWithName_value(
            &NSString::from_str("response_type"),
            Some(&NSString::from_str("token")),
        ),
        NSURLQueryItem::queryItemWithName_value(
            &NSString::from_str("client_id"),
            Some(&NSString::from_str(CLIENT_ID)),
        ),
        NSURLQueryItem::queryItemWithName_value(
            &NSString::from_str("redirect_uri"),
            Some(&NSString::from_str(REDIRECT_URL)),
        ),
        NSURLQueryItem::queryItemWithName_value(
            &NSString::from_str("state"),
            Some(&NSString::from_str(state)),
        ),
    ]);
    components.setQueryItems(Some(&items));
    components.string().map(|value| value.to_string())
}

pub struct Ivars {
    generation: u64,
    state: String,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Guard` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Guard;

    unsafe impl NSObjectProtocol for Guard {}

    unsafe impl WKNavigationDelegate for Guard {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _webview: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // A successful redirect closes the last strong owner of this weak
            // delegate. Keep the receiver alive until this callback returns.
            let _keepalive = self.retain();
            let is_main = unsafe {
                action
                    .targetFrame()
                    .is_some_and(|frame| frame.isMainFrame())
            };
            // Subframes and resources do not change the address the player is
            // trusting. New-window navigations have no target frame and are
            // refused; the UI delegate below refuses to create them too.
            if !is_main {
                handler.call((if unsafe { action.targetFrame().is_some() } {
                    WKNavigationActionPolicy::Allow
                } else {
                    WKNavigationActionPolicy::Cancel
                },));
                return;
            }
            if unsafe { action.shouldPerformDownload() } {
                handler.call((WKNavigationActionPolicy::Cancel,));
                return;
            }

            let url = unsafe { action.request().URL() }
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string());
            let policy = match url.as_deref() {
                Some(url) => match inspect_redirect(url, &self.ivars().state) {
                    Redirect::Token(token) => {
                        // WebKit must receive its decision while the view and
                        // its navigation are still alive.
                        handler.call((WKNavigationActionPolicy::Cancel,));
                        settle_window(self.ivars().generation, Some(token), true);
                        return;
                    }
                    Redirect::Rejected => {
                        note!("[steam] refused an OAuth redirect with invalid state or token");
                        handler.call((WKNavigationActionPolicy::Cancel,));
                        settle_window(self.ivars().generation, None, true);
                        return;
                    }
                    Redirect::Unrelated if is_allowed(url) => WKNavigationActionPolicy::Allow,
                    Redirect::Unrelated => {
                        note!("[steam] refused a top-level navigation outside Steam or Valve");
                        WKNavigationActionPolicy::Cancel
                    }
                },
                None => WKNavigationActionPolicy::Cancel,
            };
            handler.call((policy,));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _webview: &WKWebView,
            response: &WKNavigationResponse,
            handler: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            let displayable = unsafe { response.canShowMIMEType() };
            handler.call((if displayable {
                WKNavigationResponsePolicy::Allow
            } else {
                WKNavigationResponsePolicy::Cancel
            },));
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _webview: &WKWebView) {
            let _keepalive = self.retain();
            note!("[steam] the sign-in web process stopped");
            settle_window(self.ivars().generation, None, true);
        }
    }

    unsafe impl WKUIDelegate for Guard {
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn refuse_popup(
            &self,
            _webview: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            _action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> *mut WKWebView {
            std::ptr::null_mut()
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media_permission(
            &self,
            _webview: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            _kind: WKMediaCaptureType,
            handler: &DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            handler.call((WKPermissionDecision::Deny,));
        }

        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn refuse_file_panel(
            &self,
            _webview: &WKWebView,
            _parameters: &WKOpenPanelParameters,
            _frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn(*mut NSArray<NSURL>)>,
        ) {
            handler.call((std::ptr::null_mut(),));
        }

        #[unsafe(method(webView:requestDeviceOrientationAndMotionPermissionForOrigin:initiatedByFrame:decisionHandler:))]
        fn motion_permission(
            &self,
            _webview: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            handler.call((WKPermissionDecision::Deny,));
        }
    }

    unsafe impl NSWindowDelegate for Guard {
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _window: &NSWindow) -> bool {
            let _keepalive = self.retain();
            settle_window(self.ivars().generation, None, false);
            true
        }
    }
);

impl Guard {
    fn new(mtm: MainThreadMarker, generation: u64, state: String) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { generation, state });
        // SAFETY: NSObject's designated initializer on a fresh allocation.
        unsafe { msg_send![super(this), init] }
    }
}

struct Active {
    generation: u64,
    window: Retained<NSWindow>,
    parent: Option<Retained<NSWindow>>,
    _webview: Retained<WKWebView>,
    _guard: Retained<Guard>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

fn open(mtm: MainThreadMarker, generation: u64) -> Result<(), &'static str> {
    let state = NSUUID::UUID().UUIDString().to_string();
    let url = authorization_url(&state).ok_or("the authorization URL is invalid")?;
    let url = NSURL::URLWithString(&NSString::from_str(&url))
        .ok_or("the authorization URL is invalid")?;

    let configuration = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe {
        configuration.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
        configuration
            .preferences()
            .setJavaScriptCanOpenWindowsAutomatically(false);
    }

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(520.0, 720.0));
    let webview = unsafe {
        WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &configuration)
    };
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("Sign in with Steam"));
    window.setContentView(Some(&webview));
    unsafe { window.setReleasedWhenClosed(false) };

    let guard = Guard::new(mtm, generation, state);
    unsafe {
        webview.setNavigationDelegate(Some(ProtocolObject::from_ref(&*guard)));
        webview.setUIDelegate(Some(ProtocolObject::from_ref(&*guard)));
    }
    window.setDelegate(Some(ProtocolObject::from_ref(&*guard)));

    let application = NSApplication::sharedApplication(mtm);
    let parent = application.mainWindow().or_else(|| application.keyWindow());
    ACTIVE.with(|active| {
        *active.borrow_mut() = Some(Active {
            generation,
            window: window.clone(),
            parent: parent.clone(),
            _webview: webview.clone(),
            _guard: guard,
        });
    });

    let request = NSURLRequest::requestWithURL(&url);
    unsafe { webview.loadRequest(&request) };

    if let Some(parent) = parent {
        parent.beginSheet_completionHandler(&window, None);
    } else {
        window.center();
        window.makeKeyAndOrderFront(None);
    }
    application.activate();
    Ok(())
}

fn settle_window(generation: u64, token: Option<String>, close: bool) {
    let active = ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_ref() {
            Some(active) if active.generation == generation => slot.take(),
            _ => None,
        }
    });
    // A programmatic close re-enters the window delegate below. By then the
    // active attempt has already been taken, so the second callback must not
    // race the real token with a cancellation.
    let Some(active) = active else { return };

    complete(generation, token);
    if close {
        if let Some(parent) = &active.parent {
            parent.endSheet(&active.window);
        }
        active.window.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_request_carries_the_official_provider_config_and_nonce() {
        let url = authorization_url("fresh-state").unwrap();
        assert!(url.starts_with(AUTHORIZE_URL));
        let parts = parts(&url).unwrap();
        assert_eq!(
            query_value(parts.query.as_deref(), "response_type").as_deref(),
            Some("token")
        );
        assert_eq!(
            query_value(parts.query.as_deref(), "client_id").as_deref(),
            Some(CLIENT_ID)
        );
        assert_eq!(
            query_value(parts.query.as_deref(), "redirect_uri").as_deref(),
            Some(REDIRECT_URL)
        );
        assert_eq!(
            query_value(parts.query.as_deref(), "state").as_deref(),
            Some("fresh-state")
        );
    }

    #[test]
    fn top_level_navigation_is_confined_to_https_steam_and_valve_hosts() {
        for allowed in [
            "https://steamcommunity.com/login",
            "https://store.steampowered.com/",
            "https://cdn.steamstatic.com/asset",
            "https://help.valvesoftware.com/",
            "https://login.steamcommunity.com/",
        ] {
            assert!(is_allowed(allowed), "{allowed}");
        }
        for denied in [
            "http://steamcommunity.com/login",
            "https://steamcommunity.com.evil.test/",
            "https://user@steamcommunity.com/",
            "https://steamcommunity.com:444/",
            "file:///tmp/token",
            "javascript:alert(1)",
            "https://www.guildwars.com/app/live/auth",
        ] {
            assert!(!is_allowed(denied), "{denied}");
        }
    }

    #[test]
    fn redirect_requires_the_exact_https_target_and_matching_state() {
        assert_eq!(
            inspect_redirect(
                "https://www.guildwars.com/app/live/auth#access_token=secret&state=nonce",
                "nonce",
            ),
            Redirect::Token("secret".into())
        );
        assert_eq!(
            inspect_redirect(
                "https://www.guildwars.com/app/live/auth?token=secret&state=nonce",
                "nonce",
            ),
            Redirect::Token("secret".into())
        );
        for rejected in [
            "https://www.guildwars.com/app/live/auth#access_token=secret&state=wrong",
            "https://www.guildwars.com/app/live/auth#state=nonce",
        ] {
            assert_eq!(inspect_redirect(rejected, "nonce"), Redirect::Rejected);
        }
        for unrelated in [
            "http://www.guildwars.com/app/live/auth#access_token=secret&state=nonce",
            "https://www.guildwars.com.evil.test/app/live/auth#access_token=secret&state=nonce",
            "https://www.guildwars.com/app/live/other#access_token=secret&state=nonce",
            "https://user@www.guildwars.com/app/live/auth#access_token=secret&state=nonce",
        ] {
            assert_eq!(inspect_redirect(unrelated, "nonce"), Redirect::Unrelated);
        }
    }

    #[test]
    fn session_validation_rejects_empty_oversized_and_expired_tokens() {
        assert!(!valid(&Session {
            token: String::new(),
            expiry: None,
        }));
        assert!(!valid(&Session {
            token: "x".repeat(MAX_TOKEN_LENGTH + 1),
            expiry: None,
        }));
        assert!(!valid(&Session {
            token: "token".into(),
            expiry: Some(1),
        }));
        assert!(valid(&Session {
            token: "token".into(),
            expiry: None,
        }));
    }
}
