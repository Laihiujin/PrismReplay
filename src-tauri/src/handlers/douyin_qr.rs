//! Douyin QR login driven through a hidden app webview.
//!
//! Douyin signs its passport requests inside the page runtime, so calling those endpoints directly
//! is rejected. Instead this opens the real creator-center login page in a hidden webview, shows the
//! page's own QR code in the app, and reads the session cookies once the user has scanned.
//!
//! The webview shares the built-in browser's persistent profile. A throwaway profile looks like a
//! new device on every login, which sends Douyin into SMS verification and risk control. It also
//! means a session already logged in there is imported without a scan.
//!
//! The window is revealed only when Douyin asks for a verification that has to be done by hand.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::Mutex;
use url::Url;

use super::account::{
    build_webview_cookie_result, collect_douyin_webview_cookies, PlatformQrInfo, PlatformQrStatus,
};

pub const KEY_PREFIX: &str = "webview:";

const LOGIN_URL: &str = "https://creator.douyin.com/creator-micro/login?enter_from=qr";
const LIVE_URL: &str = "https://live.douyin.com/";
const BRIDGE_PARAM: &str = "prism_qr_probe";

const QR_LOAD_TIMEOUT: Duration = Duration::from_secs(25);
const SESSION_TTL: Duration = Duration::from_secs(300);
/// Long enough to confirm on the phone; beyond it Douyin is likely showing something we can't detect.
const SCANNED_REVEAL_AFTER: Duration = Duration::from_secs(30);
const LIVE_SYNC_MIN: Duration = Duration::from_secs(3);
const LIVE_SYNC_MAX: Duration = Duration::from_secs(10);

/// Silences the live page visited after login so the hidden window never plays audio.
const MUTE_SCRIPT: &str = r#"
(function () {
  HTMLMediaElement.prototype.play = function () {
    this.muted = true;
    return Promise.resolve();
  };
  setInterval(function () {
    document.querySelectorAll('video, audio').forEach(function (el) {
      el.muted = true;
      el.pause();
    });
  }, 500);
})();
"#;

/// `eval` cannot return a value, so the probe leaves its result in the URL fragment, which the app
/// can read back through `WebviewWindow::url`. `replaceState` neither reloads nor routes the page.
const PROBE_SCRIPT: &str = r#"
(function (nonce, param) {
  var text = document.body ? document.body.innerText : '';
  var img = document.querySelector('img[aria-label="二维码"], #animate_qrcode_container img');
  var visible = img && img.naturalWidth > 0 && img.getClientRects().length > 0;
  var result = {
    nonce: nonce,
    qr: visible && img.src.indexOf('data:image') === 0 ? img.src : '',
    scanned: /扫码成功|扫描成功|已扫码|已扫描|请在.{0,8}确认/.test(text),
    expired: /二维码.{0,4}(失效|过期)/.test(text),
    verify: /接收短信验证码|身份验证|发送短信验证/.test(text) ||
      /security\/verify|captcha/.test(location.pathname)
  };
  history.replaceState(
    history.state,
    '',
    location.pathname + location.search + '#' + param + '=' + encodeURIComponent(JSON.stringify(result))
  );
})
"#;

#[derive(Debug, Default, PartialEq, Deserialize)]
struct Probe {
    nonce: u64,
    qr: String,
    scanned: bool,
    expired: bool,
    verify: bool,
}

struct Session {
    key: String,
    label: String,
    started: Instant,
    qr: String,
    scanned_at: Option<Instant>,
    logged_in_at: Option<Instant>,
    revealed: bool,
}

static SESSION: Mutex<Option<Session>> = Mutex::const_new(None);
static NEXT_NONCE: AtomicU64 = AtomicU64::new(1);

fn probe_from_url(url: &Url, nonce: u64) -> Option<Probe> {
    let fragment = url.fragment()?;
    let (_, raw) = url::form_urlencoded::parse(fragment.as_bytes())
        .find(|(name, _)| name == BRIDGE_PARAM)?;
    let probe: Probe = serde_json::from_str(&raw).ok()?;
    (probe.nonce == nonce).then_some(probe)
}

/// A page that is mid-navigation cannot answer, which callers treat as "nothing to report yet".
async fn probe(window: &WebviewWindow) -> Option<Probe> {
    let nonce = NEXT_NONCE.fetch_add(1, Ordering::Relaxed);
    let script = format!("{PROBE_SCRIPT}({nonce}, '{BRIDGE_PARAM}');");
    window.eval(script).ok()?;
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(probe) = window.url().ok().and_then(|url| probe_from_url(&url, nonce)) {
            return Some(probe);
        }
    }
    None
}

fn is_login_url(url: &Url) -> bool {
    url.path().to_ascii_lowercase().contains("login")
}

fn close_session(app: &AppHandle, session: Session) {
    if let Some(window) = app.get_webview_window(&session.label) {
        let _ = window.close();
    }
}

fn status(code: u8, message: &str) -> PlatformQrStatus {
    PlatformQrStatus {
        code,
        message: Some(message.to_string()),
        ..Default::default()
    }
}

fn reveal(window: &WebviewWindow, session: &mut Session) {
    if !session.revealed {
        session.revealed = true;
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub async fn start(app: &AppHandle) -> Result<PlatformQrInfo, String> {
    let mut slot = SESSION.lock().await;
    if let Some(previous) = slot.take() {
        close_session(app, previous);
    }

    let id = uuid::Uuid::new_v4().simple().to_string();
    let label = format!("douyin-qr-{id}");
    let key = format!("{KEY_PREFIX}{id}");
    let login_url = Url::parse(LOGIN_URL).map_err(|e| format!("Invalid login URL: {e}"))?;
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(login_url))
        .title("抖音 登录验证")
        .inner_size(1100.0, 800.0)
        .visible(false)
        .initialization_script(MUTE_SCRIPT)
        .build()
        .map_err(|e| format!("打开内置浏览器失败: {e}"))?;

    let deadline = Instant::now() + QR_LOAD_TIMEOUT;
    let qr = loop {
        if let Some(probe) = probe(&window).await {
            if !probe.qr.is_empty() {
                break Some(probe.qr);
            }
        }
        // No QR page is coming if the profile is logged in; `poll` imports the session.
        let (_, _, has_login) = collect_douyin_webview_cookies(&window)?;
        if has_login && window.url().is_ok_and(|url| !is_login_url(&url)) {
            break None;
        }
        if Instant::now() >= deadline {
            let _ = window.close();
            return Err("抖音登录页加载超时，请检查网络后刷新二维码，或改用手动 Cookie".to_string());
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    };

    *slot = Some(Session {
        key: key.clone(),
        label,
        started: Instant::now(),
        qr: qr.clone().unwrap_or_default(),
        scanned_at: None,
        logged_in_at: None,
        revealed: false,
    });
    drop(slot);

    // Polling stops when the modal goes away without telling us; don't leave the page running.
    let app_for_expiry = app.clone();
    let key_for_expiry = key.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SESSION_TTL + Duration::from_secs(5)).await;
        let mut slot = SESSION.lock().await;
        if slot.as_ref().is_some_and(|s| s.key == key_for_expiry) {
            if let Some(session) = slot.take() {
                close_session(&app_for_expiry, session);
            }
        }
    });

    Ok(PlatformQrInfo {
        oauth_key: key,
        url: None,
        image: qr,
    })
}

pub async fn poll(app: &AppHandle, key: &str) -> Result<PlatformQrStatus, String> {
    let mut slot = SESSION.lock().await;
    let Some(session) = slot.as_mut().filter(|s| s.key == key) else {
        return Ok(status(2, "二维码已失效，请点击刷新二维码"));
    };
    let Some(window) = app.get_webview_window(&session.label) else {
        *slot = None;
        return Ok(status(2, "登录窗口已关闭，请点击刷新二维码"));
    };
    if session.started.elapsed() >= SESSION_TTL {
        if let Some(session) = slot.take() {
            close_session(app, session);
        }
        return Ok(status(2, "二维码已过期，请点击刷新二维码"));
    }

    let (cookie_header, cookie_list, has_login) = collect_douyin_webview_cookies(&window)?;

    if let Some(logged_in_at) = session.logged_in_at {
        let elapsed = logged_in_at.elapsed();
        let has_visitor_cookie = cookie_header.split("; ").any(|c| c.starts_with("ttwid="));
        if elapsed < LIVE_SYNC_MAX && (elapsed < LIVE_SYNC_MIN || !has_visitor_cookie) {
            return Ok(status(1, "登录成功，正在同步账号"));
        }
        let finished = build_webview_cookie_result("douyin", cookie_header, cookie_list).await;
        if let Some(session) = slot.take() {
            close_session(app, session);
        }
        let result = finished?;
        return Ok(PlatformQrStatus {
            code: 0,
            cookies: result.cookies,
            extra: Some(result.extra),
            ..Default::default()
        });
    }

    let url = window.url().map_err(|e| format!("读取登录页地址失败: {e}"))?;
    if has_login && !is_login_url(&url) {
        // The recorder talks to the live site, so collect the cookies that site sets too.
        session.logged_in_at = Some(Instant::now());
        let _ = window.eval(format!("location.replace('{LIVE_URL}');"));
        return Ok(status(1, "登录成功，正在同步账号"));
    }

    let Some(probe) = probe(&window).await else {
        return Ok(status(1, "new"));
    };
    if probe.verify {
        reveal(&window, session);
        return Ok(status(1, "抖音需要身份验证，请在弹出的窗口中完成"));
    }
    if probe.expired {
        if let Some(session) = slot.take() {
            close_session(app, session);
        }
        return Ok(status(2, "二维码已过期，请点击刷新二维码"));
    }
    if probe.scanned {
        let scanned_at = *session.scanned_at.get_or_insert_with(Instant::now);
        if scanned_at.elapsed() >= SCANNED_REVEAL_AFTER {
            reveal(&window, session);
            return Ok(status(1, "如手机上已确认，请在弹出的窗口中继续"));
        }
        return Ok(status(1, "已扫码，请在手机上确认登录"));
    }

    let mut waiting = status(1, "new");
    if !probe.qr.is_empty() && probe.qr != session.qr {
        // The page rotated its code, so the one on screen no longer works.
        session.qr = probe.qr.clone();
        waiting.image = Some(probe.qr);
    }
    Ok(waiting)
}

pub async fn cancel(app: &AppHandle) {
    if let Some(session) = SESSION.lock().await.take() {
        close_session(app, session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge_url(json: &str) -> Url {
        let encoded = urlencoding::encode(json);
        Url::parse(&format!(
            "https://creator.douyin.com/creator-micro/login?enter_from=qr#{BRIDGE_PARAM}={encoded}"
        ))
        .unwrap()
    }

    #[test]
    fn reads_probe_with_a_data_url_intact() {
        let qr = "data:image/png;base64,iVBOR+w/0KGgo=";
        let json = format!(
            r#"{{"nonce":7,"qr":"{qr}","scanned":true,"expired":false,"verify":false}}"#
        );
        let probe = probe_from_url(&bridge_url(&json), 7).unwrap();
        assert_eq!(probe.qr, qr);
        assert!(probe.scanned);
    }

    #[test]
    fn ignores_a_stale_or_missing_probe() {
        let json = r#"{"nonce":7,"qr":"","scanned":false,"expired":false,"verify":false}"#;
        assert_eq!(probe_from_url(&bridge_url(json), 8), None);
        let plain = Url::parse("https://creator.douyin.com/creator-micro/home").unwrap();
        assert_eq!(probe_from_url(&plain, 7), None);
    }

    #[test]
    fn tells_login_pages_from_the_logged_in_site() {
        let login = Url::parse(LOGIN_URL).unwrap();
        assert!(is_login_url(&login));
        let home = Url::parse("https://creator.douyin.com/creator-micro/home").unwrap();
        assert!(!is_login_url(&home));
    }
}
