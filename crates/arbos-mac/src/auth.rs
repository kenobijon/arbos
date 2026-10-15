//! "Sign in with GitHub": the browser leg of the relay's OAuth flow.
//!
//! The relay owns GitHub (`/auth/github/start` → GitHub → `/auth/github/callback`)
//! and hands back a one-time grant code on our redirect URI. This module only
//! gets that code to the app:
//!
//! - macOS: `ASWebAuthenticationSession` with the `arbos://auth/callback`
//!   scheme (registered in Info.plist); the session's completion block
//!   delivers the callback URL on the main thread.
//! - elsewhere (Linux CI): a loopback listener on `http://127.0.0.1:<port>/cb`
//!   and the system browser (`ARBOS_BROWSER` or `xdg-open`).
//!
//! The grant code is exchanged by the relay source (`POST /auth/token`).

use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use futures::FutureExt as _;
use gpui::App;
use gpui_tokio::Tokio;

/// How long the user gets to finish in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What the browser leg produced.
#[derive(Debug)]
pub enum SignIn {
    /// `?code=` from the redirect: exchange it at `POST /auth/token`.
    Grant(String),
    /// The relay runs `ARBOS_AUTH_MODE=paircode` (409 `auth_mode`): show the
    /// pairing-code entry instead.
    PairCodeMode,
}

/// Start the browser leg; `done` runs on the main thread with the result.
pub fn begin(
    relay_http: String,
    device_name: String,
    cx: &mut App,
    done: impl FnOnce(Result<SignIn>, &mut App) + 'static,
) {
    let redirect = platform::redirect();
    let start_url = format!(
        "{}/auth/github/start?redirect={}&device_name={}",
        relay_http.trim_end_matches('/'),
        url_encode(&redirect.uri),
        url_encode(&device_name)
    );
    let probe = Tokio::spawn(cx, probe_auth_mode(start_url.clone()));
    cx.spawn(async move |cx| {
        let outcome = match probe.await {
            Ok(Ok(Probe::Browser)) => platform::run(start_url, redirect, cx).await,
            Ok(Ok(Probe::PairCode)) => Ok(SignIn::PairCodeMode),
            Ok(Err(err)) => Err(err),
            Err(err) => Err(anyhow!("sign-in task: {err}")),
        };
        cx.update(|cx| done(outcome, cx));
    })
    .detach();
}

enum Probe {
    Browser,
    PairCode,
}

/// One request without following redirects: 302 means GitHub is wired up,
/// 409 `auth_mode` means the relay wants a pairing code.
async fn probe_auth_mode(start_url: String) -> Result<Probe> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .context("http client")?;
    let resp = client
        .get(&start_url)
        .send()
        .await
        .context("reach the relay")?;
    let status = resp.status();
    if status.is_redirection() {
        return Ok(Probe::Browser);
    }
    let body = resp.text().await.unwrap_or_default();
    if status.as_u16() == 409 || status.as_u16() == 410 {
        return Ok(Probe::PairCode);
    }
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or(body);
    Err(anyhow!("relay answered {status}: {message}"))
}

/// `code=` / `error=` from a callback URL of either shape.
fn parse_callback(url: &str) -> Result<String> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut error = None;
    let mut message = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = url_decode(v);
        match k {
            "code" => code = Some(v),
            "error" => error = Some(v),
            "message" => message = Some(v),
            _ => {}
        }
    }
    if let Some(error) = error {
        let message = message.unwrap_or_default();
        return Err(anyhow!(
            "{}",
            match error.as_str() {
                "oauth_denied" => "GitHub sign-in was cancelled.".to_string(),
                "invite_required" =>
                    "This relay needs an invite. Ask for one and try again.".to_string(),
                _ if !message.is_empty() => message,
                other => format!("sign-in failed ({other})"),
            }
        ));
    }
    code.filter(|c| !c.is_empty())
        .ok_or_else(|| anyhow!("the callback carried no code"))
}

pub(crate) fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Redirect {
    pub uri: String,
    #[cfg(not(target_os = "macos"))]
    listener: Option<std::net::TcpListener>,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use gpui::AsyncApp;

    pub fn redirect() -> Redirect {
        Redirect {
            uri: "arbos://auth/callback".into(),
        }
    }

    pub async fn run(start_url: String, _redirect: Redirect, cx: &mut AsyncApp) -> Result<SignIn> {
        let (tx, rx) = futures::channel::oneshot::channel::<Result<String>>();
        cx.update(|_| session::start(&start_url, tx))?;
        let url = futures::select! {
            r = rx.fuse() => r.map_err(|_| anyhow!("sign-in window closed"))??,
            _ = cx.background_executor().timer(SIGN_IN_TIMEOUT).fuse() => {
                return Err(anyhow!("sign-in timed out"));
            }
        };
        parse_callback(&url).map(SignIn::Grant)
    }

    /// `ASWebAuthenticationSession` + the presentation anchor it insists on.
    mod session {
        use std::cell::RefCell;
        use std::rc::Rc;

        use anyhow::{anyhow, Result};
        use block2::RcBlock;
        use objc2::rc::Retained;
        use objc2::runtime::ProtocolObject;
        use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
        use objc2_app_kit::{NSApplication, NSWindow};
        use objc2_authentication_services::{
            ASPresentationAnchor, ASWebAuthenticationPresentationContextProviding,
            ASWebAuthenticationSession,
        };
        use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString, NSURL};

        define_class!(
            // SAFETY: NSObject has no subclassing requirements and the type
            // does not implement Drop.
            #[unsafe(super(NSObject))]
            #[thread_kind = MainThreadOnly]
            #[name = "ArbosAuthAnchor"]
            struct Anchor;

            unsafe impl NSObjectProtocol for Anchor {}

            unsafe impl ASWebAuthenticationPresentationContextProviding for Anchor {
                #[unsafe(method_id(presentationAnchorForWebAuthenticationSession:))]
                unsafe fn anchor(
                    &self,
                    _session: &ASWebAuthenticationSession,
                ) -> Retained<ASPresentationAnchor> {
                    let app = NSApplication::sharedApplication(self.mtm());
                    let window = app
                        .keyWindow()
                        .or_else(|| app.mainWindow())
                        .unwrap_or_else(|| NSWindow::new(self.mtm()));
                    // ASPresentationAnchor is NSWindow on macOS; the binding
                    // spells it NSObject, so walk up the class chain.
                    Retained::into_super(Retained::into_super(window))
                }
            }
        );

        impl Anchor {
            fn new(mtm: MainThreadMarker) -> Retained<Self> {
                let this = Self::alloc(mtm).set_ivars(());
                // SAFETY: plain NSObject init on a freshly allocated instance.
                unsafe { msg_send![super(this), init] }
            }
        }

        thread_local! {
            // Keep the session and its anchor alive until the block fires.
            static LIVE: RefCell<Option<(Retained<ASWebAuthenticationSession>, Retained<Anchor>)>> =
                const { RefCell::new(None) };
        }

        pub fn start(start_url: &str, tx: futures::channel::oneshot::Sender<Result<String>>) {
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = tx.send(Err(anyhow!("sign-in must start on the main thread")));
                return;
            };
            let tx = Rc::new(RefCell::new(Some(tx)));
            let block_tx = tx.clone();
            let handler = RcBlock::new(move |url: *mut NSURL, error: *mut NSError| {
                let result = if !url.is_null() {
                    // SAFETY: non-null NSURL* handed to the completion block.
                    let url = unsafe { &*url };
                    url.absoluteString()
                        .map(|s| s.to_string())
                        .ok_or_else(|| anyhow!("callback URL was empty"))
                } else if !error.is_null() {
                    // SAFETY: non-null NSError* handed to the completion block.
                    let error = unsafe { &*error };
                    let code = error.code();
                    // ASWebAuthenticationSessionErrorCodeCanceledLogin = 1.
                    if code == 1 {
                        Err(anyhow!("GitHub sign-in was cancelled."))
                    } else {
                        Err(anyhow!("{}", error.localizedDescription()))
                    }
                } else {
                    Err(anyhow!("sign-in ended without a result"))
                };
                if let Some(tx) = block_tx.borrow_mut().take() {
                    let _ = tx.send(result);
                }
                LIVE.with(|live| live.borrow_mut().take());
            });
            let Some(url) = NSURL::URLWithString(&NSString::from_str(start_url)) else {
                return;
            };
            let anchor = Anchor::new(mtm);
            // SAFETY: plain init with a valid URL, scheme and handler block.
            // The scheme-based initializer is deprecated in favour of
            // `initWithURL:callback:completionHandler:` (macOS 14.4); the app
            // targets 14.0, so the older one stays.
            #[allow(deprecated)]
            let session = unsafe {
                ASWebAuthenticationSession::initWithURL_callbackURLScheme_completionHandler(
                    ASWebAuthenticationSession::alloc(),
                    &url,
                    Some(&NSString::from_str("arbos")),
                    &*handler as *const _ as *mut _,
                )
            };
            // SAFETY: the anchor outlives the session (both live in LIVE).
            unsafe {
                session.setPresentationContextProvider(Some(ProtocolObject::from_ref(&*anchor)));
                session.setPrefersEphemeralWebBrowserSession(false);
            }
            // SAFETY: documented to be called once on the main thread.
            let started = unsafe { session.start() };
            if started {
                LIVE.with(|live| *live.borrow_mut() = Some((session, anchor)));
            } else if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Err(anyhow!("the sign-in window could not be opened")));
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    use super::*;
    use gpui::AsyncApp;

    pub fn redirect() -> Redirect {
        let listener = TcpListener::bind("127.0.0.1:0").ok();
        let port = listener
            .as_ref()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
            .unwrap_or(0);
        Redirect {
            uri: format!("http://127.0.0.1:{port}/cb"),
            listener,
        }
    }

    pub async fn run(start_url: String, redirect: Redirect, cx: &mut AsyncApp) -> Result<SignIn> {
        let listener = redirect
            .listener
            .ok_or_else(|| anyhow!("no loopback port for the sign-in callback"))?;
        let wait = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || accept_callback(listener))
                .await
                .map_err(|e| anyhow!("{e}"))?
        });
        open_browser(&start_url)?;
        let url = futures::select! {
            r = wait.fuse() => r?,
            _ = cx.background_executor().timer(SIGN_IN_TIMEOUT).fuse() => {
                return Err(anyhow!("sign-in timed out"));
            }
        };
        parse_callback(&url).map(SignIn::Grant)
    }

    fn open_browser(url: &str) -> Result<()> {
        let browser = std::env::var("ARBOS_BROWSER").unwrap_or_else(|_| "xdg-open".into());
        std::process::Command::new(&browser)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("open the browser with {browser}"))?;
        Ok(())
    }

    /// One request on the loopback listener: the redirect from the relay.
    fn accept_callback(listener: TcpListener) -> Result<String> {
        listener.set_nonblocking(false)?;
        loop {
            let (mut stream, _) = listener.accept()?;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let target = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            if !target.starts_with("/cb") {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                continue;
            }
            let body = "<!doctype html><title>Arbos</title><body style=\"font-family:system-ui;padding:40px\"><p>Signed in. You can close this tab and go back to Arbos.</p></body>";
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            );
            return Ok(format!("http://127.0.0.1{target}"));
        }
    }
}
