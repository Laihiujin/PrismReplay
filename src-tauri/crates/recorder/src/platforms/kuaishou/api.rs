use super::response::{LiveStream, LiveStreamResponse, UserFollowCountResponse, UserFollowLive};
use crate::account::Account;
use crate::core::stream_info::{
    CdnNode, Codec, Format, PlatformStreamInfo, PlatformType, Quality, StreamVariant,
};
use crate::errors::RecorderError;
use aes::Aes128;
use base64::{Engine as _, engine::general_purpose};
use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use chrono::Utc;
use rand::Rng;
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use uuid::Uuid;

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const WEB_RATE_LIMIT_COOLDOWN_SECS: i64 = 90;
const WEB_RATE_LIMIT_RETRY_SECS: u64 = 20;
const WEB_MIN_REQUEST_GAP_MS: u64 = 1200;
const WEB_MIN_REQUEST_GAP_JITTER_MS: u64 = 800;
const FOLLOW_INFO_CACHE_TTL_SECS: i64 = 18;
const FOLLOW_INFO_MISS_TTL_SECS: i64 = 6;
const KWW_PROBE_CACHE_TTL_SECS: i64 = 3600;
const KWW_PROBE_MISS_TTL_SECS: i64 = 300;
const GDFP_PREFLIGHT_TTL_SECS: i64 = 300;
const KUAISHOU_DEFAULT_KPN: &str = "GAME_ZONE";
const KUAISHOU_DEFAULT_KPF: &str = "PC_WEB";
const KUAISHOU_DEFAULT_SHARE_METHOD: &str = "card";
const KUAISHOU_DEFAULT_CLIENT_TYPE: &str = "WEB_OUTSIDE_SHARE_H5";
const KUAISHOU_DEFAULT_SUB_BIZ: &str = "LIVE_SHARE_H5";
const KUAISHOU_DEFAULT_PRODUCT_NAME: &str = "PCLive";
const KUAISHOU_WEBWEAPON_FP_KEY: &str = "K8wm5PvY9nX7qJc2";
const KUAISHOU_WEBWEAPON_SIGN_KEY: &str = "H4tL6rNd3vB9xM5k";
const KUAISHOU_GDFP_APPKEY: &str = "10001001";
const KUAISHOU_GDFP_SECKEY: &str = "f2fff381c551a8dcdb765e316f3d44ac";
const KUAISHOU_GDFP_BUSS_TYPE_GAME_LIVE: &str = "gameLive";
const KUAISHOU_GDFP_SDK_VERSION: &str = "1.4.0";
const KUAISHOU_GDFP_PLATFORM: i64 = 3;
const KUAISHOU_GDFP_SCREEN_WIDTH: i64 = 1440;
const KUAISHOU_GDFP_SCREEN_HEIGHT: i64 = 900;
const KUAISHOU_GDFP_DEVICE_MEMORY_GB: i64 = 8;
const KUAISHOU_GDFP_CHROME_BUILD: &str = "20030107";
const KUAISHOU_GDFP_BROWSER_VENDOR: &str = "Google Inc.";
const KUAISHOU_GDFP_PRODUCT_SUBTYPE: &str = "ks";
const KUAISHOU_GDFP_PLUGIN_LIST: &str = "[{\"name\":\"PDF Viewer\",\"filename\":\"internal-pdf-viewer\",\"description\":\"Portable Document Format\"},{\"name\":\"Chrome PDF Viewer\",\"filename\":\"internal-pdf-viewer\",\"description\":\"Portable Document Format\"},{\"name\":\"Chromium PDF Viewer\",\"filename\":\"internal-pdf-viewer\",\"description\":\"Portable Document Format\"},{\"name\":\"Microsoft Edge PDF Viewer\",\"filename\":\"internal-pdf-viewer\",\"description\":\"Portable Document Format\"},{\"name\":\"WebKit built-in PDF\",\"filename\":\"internal-pdf-viewer\",\"description\":\"Portable Document Format\"}]";
const KUAISHOU_GDFP_MIME_LIST: &str = "[{\"type\":\"application/pdf\",\"description\":\"Portable Document Format\"},{\"type\":\"text/pdf\",\"description\":\"Portable Document Format\"}]";

type Aes128CbcEncryptor = cbc::Encryptor<Aes128>;

static WEB_COOLDOWN_UNTIL: AtomicI64 = AtomicI64::new(0);
static WEB_ROOM_COOLDOWN_UNTIL: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
static WEB_LAST_REQUEST_TS_MS: AtomicI64 = AtomicI64::new(0);
static WEB_REQUEST_GATE: OnceLock<Mutex<()>> = OnceLock::new();
static FOLLOW_INFO_CACHE: OnceLock<Mutex<HashMap<String, FollowInfoCacheEntry>>> = OnceLock::new();
static KWW_PROBE_CACHE: OnceLock<Mutex<HashMap<String, KwwProbeCacheEntry>>> = OnceLock::new();
static GDFP_PREFLIGHT_CACHE: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
static GDFP_FAILURE_CACHE: OnceLock<Mutex<HashMap<String, (i64, String)>>> = OnceLock::new();

#[derive(Debug, Clone)]
struct KuaishouRequestContext {
    principal_id: String,
    cookie_header: String,
    did: String,
    didv: String,
    kpn: String,
    kpf: String,
    user_id: Option<String>,
    raw_kww: Option<String>,
    kww: Option<String>,
}

impl KuaishouRequestContext {
    fn from_account(account: &Account, principal_id: &str) -> Self {
        let cookie_header = build_kuaishou_runtime_cookie(&account.cookies, principal_id);
        let did = get_cookie_value_ci(&cookie_header, "did")
            .or_else(|| get_cookie_value_ci(&cookie_header, "_did"))
            .unwrap_or_else(gen_web_did);
        let didv = get_cookie_value_ci(&cookie_header, "didv")
            .unwrap_or_else(|| Utc::now().timestamp_millis().to_string());
        let kpn = get_cookie_value_ci(&cookie_header, "kpn")
            .unwrap_or_else(|| KUAISHOU_DEFAULT_KPN.to_string());
        let kpf = get_cookie_value_ci(&cookie_header, "kpf")
            .unwrap_or_else(|| KUAISHOU_DEFAULT_KPF.to_string());
        let user_id = get_cookie_value_ci(&cookie_header, "userId")
            .or_else(|| get_cookie_value_ci(&cookie_header, "userIdStr"))
            .or_else(|| get_cookie_value_ci(&cookie_header, "user_id"));
        let raw_kww = resolve_kuaishou_raw_kww(&cookie_header);
        let kww = raw_kww
            .as_deref()
            .and_then(normalize_kuaishou_kww_header_value);

        Self {
            principal_id: principal_id.to_string(),
            cookie_header,
            did,
            didv,
            kpn,
            kpf,
            user_id,
            raw_kww,
            kww,
        }
    }

    fn web_referer(&self) -> String {
        format!("https://live.kuaishou.com/u/{}", self.principal_id)
    }

    fn mobile_referer(&self) -> String {
        let mut params = vec![
            format!("shareMethod={}", KUAISHOU_DEFAULT_SHARE_METHOD),
            format!("clientType={}", KUAISHOU_DEFAULT_CLIENT_TYPE),
            format!("kpn={}", self.kpn),
            format!("kpf={}", self.kpf),
            format!("subBiz={}", KUAISHOU_DEFAULT_SUB_BIZ),
            format!("principalId={}", self.principal_id),
            format!("did={}", self.did),
            format!("didv={}", self.didv),
        ];
        if let Some(user_id) = self.user_id.as_deref() {
            if !user_id.trim().is_empty() {
                params.push(format!("userId={user_id}"));
            }
        }
        format!("https://www.kuaishou.com/u/{}?{}", self.principal_id, params.join("&"))
    }

    fn by_user_url(&self) -> String {
        format!(
            "https://livev.m.chenzhongtech.com/rest/k/live/byUser?kpn={}&captchaToken=",
            self.kpn
        )
    }

    fn by_user_payload(&self) -> Value {
        let mut payload = json!({
            "source": 5,
            "eid": self.principal_id,
            "principalId": self.principal_id,
            "shareMethod": KUAISHOU_DEFAULT_SHARE_METHOD,
            "clientType": KUAISHOU_DEFAULT_CLIENT_TYPE,
            "kpn": self.kpn,
            "kpf": self.kpf,
            "subBiz": KUAISHOU_DEFAULT_SUB_BIZ,
            "did": self.did,
            "didv": self.didv,
            "clientid": "3",
            "webid": self.did,
        });
        if let Some(map) = payload.as_object_mut() {
            if let Some(user_id) = self.user_id.as_deref() {
                if !user_id.trim().is_empty() {
                    map.insert("userId".to_string(), Value::String(user_id.to_string()));
                }
            }
            for key in ["kwscode", "kwfv1", "kwssectoken"] {
                if let Some(value) = get_cookie_value_ci(&self.cookie_header, key) {
                    map.insert(key.to_string(), Value::String(value));
                }
            }
            if let Some(raw_kww) = self.raw_kww.as_deref().filter(|value| !value.trim().is_empty()) {
                map.insert("kww".to_string(), Value::String(raw_kww.to_string()));
            }
        }
        payload
    }

    fn apply_cookie(&self, headers: &mut reqwest::header::HeaderMap) {
        if !self.cookie_header.is_empty() {
            headers.insert("Cookie", self.cookie_header.parse().unwrap());
        }
    }

    fn apply_kww(&self, headers: &mut reqwest::header::HeaderMap) {
        if let Some(kww) = self.kww.as_deref() {
            if let Ok(value) = kww.parse() {
                headers.insert("kww", value);
            }
        }
    }
}

fn read_env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn read_env_bool(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .and_then(|raw| parse_env_bool(&raw))
}

fn use_public_page_fallback(account: &Account) -> bool {
    let _ = account;
    read_env_bool("LIVE_KUAISHOU_ENABLE_PUBLIC_PAGE_FALLBACK").unwrap_or(false)
}

fn use_follow_list_fallback(account: &Account) -> bool {
    let _ = account;
    read_env_bool("LIVE_KUAISHOU_ENABLE_FOLLOW_LIST_FALLBACK").unwrap_or(false)
}

fn use_room_page_kww_probe() -> bool {
    read_env_bool("LIVE_KUAISHOU_ENABLE_KWW_PROBE").unwrap_or(false)
}

fn use_gdfp_prefetch() -> bool {
    read_env_bool("LIVE_KUAISHOU_ENABLE_GDFP_PREFLIGHT").unwrap_or(true)
}

fn web_cooldown_secs() -> i64 {
    read_env_u64(
        "LIVE_KUAISHOU_WEB_COOLDOWN_SECS",
        WEB_RATE_LIMIT_COOLDOWN_SECS as u64,
    ) as i64
}

fn web_retry_secs() -> u64 {
    read_env_u64("LIVE_KUAISHOU_WEB_RETRY_SECS", WEB_RATE_LIMIT_RETRY_SECS)
}

fn web_min_request_gap_ms() -> u64 {
    read_env_u64("LIVE_KUAISHOU_WEB_MIN_GAP_MS", WEB_MIN_REQUEST_GAP_MS)
}

fn web_request_jitter_ms() -> u64 {
    read_env_u64(
        "LIVE_KUAISHOU_WEB_MIN_GAP_JITTER_MS",
        WEB_MIN_REQUEST_GAP_JITTER_MS,
    )
}

fn is_rate_limit_message(message: &str) -> bool {
    let trimmed = message.trim();
    !trimmed.is_empty()
        && (trimmed.contains("\u{64cd}\u{4f5c}\u{592a}\u{5feb}")
            || trimmed.contains("\u{8bf7}\u{7a0d}\u{5fae}\u{4f11}\u{606f}\u{4e00}\u{4e0b}")
            || trimmed.contains("\u{4f11}\u{606f}\u{4e00}\u{4e0b}")
            || trimmed.to_ascii_lowercase().contains("too fast")
            || trimmed.contains("\u{8bf7}\u{6c42}\u{8fc7}\u{5feb}")
            || trimmed.contains("\u{8bbf}\u{95ee}\u{8fc7}\u{4e8e}\u{9891}\u{7e41}")
            || trimmed.contains("\u{8bbf}\u{95ee}\u{9891}\u{7e41}")
            || trimmed.contains("\u{8bbf}\u{95ee}\u{592a}\u{5feb}")
            || trimmed.contains("\u{8bf7}\u{6c42}\u{9891}\u{7e41}")
            || trimmed.contains("\u{8bf7}\u{7a0d}\u{540e}\u{518d}\u{8bd5}")
            || trimmed.contains("\u{8bf7}\u{7a0d}\u{5019}\u{518d}\u{8bd5}")
            || trimmed.contains("\u{7a0d}\u{5019}\u{518d}\u{8bd5}")
            || trimmed.contains("\u{7a0d}\u{540e}\u{518d}\u{8bd5}")
            || trimmed.contains("\u{8bf7}\u{6c42}\u{8fc7}\u{4e8e}\u{9891}\u{7e41}"))
}

fn is_captcha_message(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.contains("\u{6ed1}\u{5757}")
        || trimmed.contains("\u{9a8c}\u{8bc1}")
        || trimmed.contains("\u{4eba}\u{673a}")
        || trimmed.contains("\u{5b89}\u{5168}\u{9a8c}\u{8bc1}")
        || trimmed.contains("\u{884c}\u{4e3a}\u{9a8c}\u{8bc1}")
        || trimmed.to_ascii_lowercase().contains("captcha")
}
fn is_room_disabled_message(message: &str) -> bool {
    let trimmed = message.trim();
    !trimmed.is_empty() && trimmed.contains("\u{672a}\u{542f}\u{7528}")
}

fn room_cooldown_key(room_key: &str) -> Option<String> {
    let key = resolve_principal_id(room_key);
    let trimmed = key.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

async fn set_web_cooldown(account: &Account, room_key: &str, reason: &str) {
    let cooldown_secs = web_cooldown_secs();
    let until = Utc::now().timestamp() + cooldown_secs;
    let now = Utc::now().timestamp();

    if let Some(key) = room_cooldown_key(room_key) {
        let map = WEB_ROOM_COOLDOWN_UNTIL.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = map.lock().await;
        guard.retain(|_, value| *value > now);
        guard.insert(key.clone(), until);
        log::info!(
            "[Kuaishou] Web room cooldown set room={} ({}s): {}",
            key,
            cooldown_secs,
            reason
        );
    }

    if account.is_guest() {
        return;
    }

    if should_use_global_web_cooldown(account) {
        WEB_COOLDOWN_UNTIL.store(until, Ordering::Relaxed);
        log::info!(
            "[Kuaishou] Web global cooldown set ({}s): {}",
            cooldown_secs,
            reason
        );
    }
}

async fn web_api_allowed(account: &Account, room_key: &str) -> bool {
    let now = Utc::now().timestamp();
    if let Some(key) = room_cooldown_key(room_key) {
        let map = WEB_ROOM_COOLDOWN_UNTIL.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = map.lock().await;
        match guard.get(&key).copied() {
            Some(until) if now < until => false,
            Some(_) => {
                guard.remove(&key);
                true
            }
            None => true,
        }
    } else if account.is_guest() {
        true
    } else if should_use_global_web_cooldown(account) {
        now >= WEB_COOLDOWN_UNTIL.load(Ordering::Relaxed)
    } else {
        true
    }
}

fn should_use_global_web_cooldown(account: &Account) -> bool {
    if account.is_guest() {
        return false;
    }
    read_env_bool("LIVE_KUAISHOU_LOGIN_GLOBAL_COOLDOWN").unwrap_or(false)
}

fn web_cooldown_error() -> RecorderError {
    RecorderError::ApiError {
        error: format!(
            "快手接口冷却中，请约 {} 秒后重试",
            web_retry_secs()
        ),
    }
}

async fn wait_for_web_request_slot(scene: &str) {
    let gate = WEB_REQUEST_GATE.get_or_init(|| Mutex::new(()));
    let _guard = gate.lock().await;

    let min_gap_ms = web_min_request_gap_ms();
    let jitter_max_ms = web_request_jitter_ms();
    let jitter_ms = if jitter_max_ms == 0 {
        0
    } else {
        rand::random_range(0..=jitter_max_ms)
    };
    let required_gap_ms = min_gap_ms.saturating_add(jitter_ms);

    if required_gap_ms > 0 {
        let now_ms = Utc::now().timestamp_millis();
        let last_ms = WEB_LAST_REQUEST_TS_MS.load(Ordering::Relaxed);
        if last_ms > 0 {
            let elapsed_ms = now_ms.saturating_sub(last_ms);
            let wait_ms = (required_gap_ms as i64).saturating_sub(elapsed_ms);
            if wait_ms > 0 {
                log::debug!(
                    "[Kuaishou] {} request throttled for {}ms (gap={}ms, jitter={}ms)",
                    scene,
                    wait_ms,
                    min_gap_ms,
                    jitter_ms
                );
                tokio::time::sleep(Duration::from_millis(wait_ms as u64)).await;
            }
        }
    }

    WEB_LAST_REQUEST_TS_MS.store(Utc::now().timestamp_millis(), Ordering::Relaxed);
}

fn gdfp_prefetch_cache_key(ctx: &KuaishouRequestContext, buss_type: &str) -> String {
    format!(
        "{}:{}:{}",
        ctx.principal_id.trim().to_ascii_lowercase(),
        ctx.did.trim().to_ascii_lowercase(),
        buss_type.trim().to_ascii_lowercase()
    )
}

async fn gdfp_prefetch_allowed(cache_key: &str) -> bool {
    let now = Utc::now().timestamp();
    let cache = GDFP_PREFLIGHT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    match guard.get(cache_key).copied() {
        Some(until) if now < until => false,
        Some(_) => {
            guard.remove(cache_key);
            true
        }
        None => true,
    }
}

async fn mark_gdfp_prefetch(cache_key: String) {
    let until = Utc::now().timestamp() + GDFP_PREFLIGHT_TTL_SECS;
    let cache = GDFP_PREFLIGHT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    guard.insert(cache_key, until);
}

fn gdfp_failure_key(principal_id: &str) -> Option<String> {
    room_cooldown_key(principal_id)
}

async fn set_gdfp_failure(principal_id: &str, reason: impl Into<String>) {
    let Some(key) = gdfp_failure_key(principal_id) else {
        return;
    };
    let cache = GDFP_FAILURE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    guard.insert(key, (Utc::now().timestamp(), reason.into()));
}

async fn clear_gdfp_failure(principal_id: &str) {
    let Some(key) = gdfp_failure_key(principal_id) else {
        return;
    };
    let cache = GDFP_FAILURE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    guard.remove(&key);
}

async fn recent_gdfp_failure(principal_id: &str) -> Option<String> {
    let Some(key) = gdfp_failure_key(principal_id) else {
        return None;
    };
    let cache = GDFP_FAILURE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    let now = Utc::now().timestamp();
    match guard.get(&key).cloned() {
        Some((ts, reason)) if now.saturating_sub(ts) <= GDFP_PREFLIGHT_TTL_SECS => Some(reason),
        Some(_) => {
            guard.remove(&key);
            None
        }
        None => None,
    }
}

async fn captcha_error_with_context(principal_id: &str) -> RecorderError {
    if let Some(reason) = recent_gdfp_failure(principal_id).await {
        RecorderError::ApiError {
            error: format!(
                "Please complete captcha verification (gdfp preflight unreachable: {reason})"
            ),
        }
    } else {
        RecorderError::ApiError {
            error: "Please complete captcha verification".to_string(),
        }
    }
}

fn build_gdfp_sign(timestamp_secs: i64) -> String {
    let raw = format!(
        "{}{}{}",
        KUAISHOU_GDFP_APPKEY, KUAISHOU_GDFP_SECKEY, timestamp_secs
    );
    format!("{:x}", md5::compute(raw.as_bytes()))
}

fn encode_gdfp_payload(value: &Value) -> Result<String, RecorderError> {
    serde_json::to_vec(value)
        .map(|bytes| general_purpose::STANDARD.encode(bytes))
        .map_err(|e| RecorderError::ApiError {
            error: format!("Failed to encode gdfp payload: {e}"),
        })
}

fn build_gdfp_sdk_init_payload(ctx: &KuaishouRequestContext) -> Result<Value, RecorderError> {
    let payload = json!({
        "device_id": ctx.did,
        "sdkver": KUAISHOU_GDFP_SDK_VERSION,
        "pver": "0.0.0",
        "hp": KUAISHOU_GDFP_BUSS_TYPE_GAME_LIVE,
        "platform": KUAISHOU_GDFP_PLATFORM,
    });
    Ok(json!({
        "data": encode_gdfp_payload(&payload)?,
        "flag": 2,
    }))
}

fn gdfp_profile_from_user_agent() -> (&'static str, &'static str, &'static str, &'static str) {
    let ua = USER_AGENT.to_ascii_lowercase();
    if ua.contains("windows") {
        (
            "10.0",
            "Windows",
            "Win32",
            "ANGLE (Intel, Intel(R) UHD Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)",
        )
    } else if ua.contains("macintosh") || ua.contains("mac os") {
        (
            "10.15.7",
            "Mac OS",
            "MacIntel",
            "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)",
        )
    } else {
        (
            "6.6.0",
            "Linux",
            "Linux x86_64",
            "ANGLE (Google, Vulkan 1.3.0 SwiftShader Device (Subzero), SwiftShader driver)",
        )
    }
}

fn build_gdfp_cookie_snapshot(ctx: &KuaishouRequestContext) -> Value {
    let mut cookie_map = parse_cookie_map(&ctx.cookie_header);
    cookie_map
        .entry("did".to_string())
        .or_insert_with(|| ctx.did.clone());
    cookie_map
        .entry("didv".to_string())
        .or_insert_with(|| ctx.didv.clone());
    cookie_map
        .entry("kpn".to_string())
        .or_insert_with(|| ctx.kpn.clone());
    cookie_map
        .entry("kpf".to_string())
        .or_insert_with(|| ctx.kpf.clone());
    if let Some(user_id) = ctx.user_id.as_deref().filter(|value| !value.trim().is_empty()) {
        cookie_map
            .entry("userId".to_string())
            .or_insert_with(|| user_id.to_string());
    }
    if let Some(raw_kww) = ctx.raw_kww.as_deref().filter(|value| !value.trim().is_empty()) {
        cookie_map
            .entry("kww".to_string())
            .or_insert_with(|| raw_kww.to_string());
    }
    let mut json_map = Map::new();
    for (key, value) in cookie_map {
        if !value.trim().is_empty() {
            json_map.insert(key, Value::String(value));
        }
    }
    Value::Object(json_map)
}

fn build_gdfp_module_browser_payload(
    ctx: &KuaishouRequestContext,
    room_url: &str,
    root_url: &str,
    now_ms: i64,
) -> Value {
    let (os_version, os_name, navigator_platform, renderer) = gdfp_profile_from_user_agent();
    let init_time = now_ms.saturating_sub(1200);
    let create_start = init_time.saturating_sub(800);
    let module_uuid = Uuid::new_v4().to_string();
    let script_urls = vec![
        "https://static.yximgs.com/udata/pkg/ks-track-platform-new/weblogger/3.9.49/async/gzipper.min.js",
        "https://p2-game.kskwai.com/udata/pkg/KS-GAME-WEB/pc-live-next/js/vendors.f39e14126038dfed8050.js",
        "https://p2-game.kskwai.com/udata/pkg/KS-GAME-WEB/pc-live-next/js/purecommon.f39e14126038dfed8050.js",
        "https://p2-game.kskwai.com/udata/pkg/KS-GAME-WEB/pc-live-next/js/common.f39e14126038dfed8050.js",
        "https://p2-game.kskwai.com/udata/pkg/KS-GAME-WEB/pc-live-next/js/app.f39e14126038dfed8050.js",
    ];
    let ua = USER_AGENT;
    let vendor_renderer = format!("{} ({os_name})", KUAISHOU_GDFP_BROWSER_VENDOR);
    let webgl_vendor = "WebKit";
    let webgl_version = "WebGL 1.0 (OpenGL ES 2.0 Chromium)";
    let webgl_glsl = "WebGL GLSL ES 1.0 (OpenGL ES GLSL ES 1.0 Chromium)";
    let cookie_snapshot = build_gdfp_cookie_snapshot(ctx);
    let plugin_hash = format!("{:x}", md5::compute(KUAISHOU_GDFP_PLUGIN_LIST.as_bytes()));
    let mime_hash = format!("{:x}", md5::compute(KUAISHOU_GDFP_MIME_LIST.as_bytes()));
    let ua_hash = format!("{:x}", md5::compute(ua.as_bytes()));
    let room_hash = format!("{:x}", md5::compute(room_url.as_bytes()));
    let renderer_hash = format!("{:x}", md5::compute(renderer.as_bytes()));
    let mut module = Map::new();
    module.insert("1".to_string(), Value::String(String::new()));
    module.insert("2".to_string(), Value::String(room_url.to_string()));
    module.insert("4".to_string(), Value::String(root_url.to_string()));
    module.insert("5".to_string(), Value::String(module_uuid));
    module.insert("6".to_string(), Value::String("快手直播".to_string()));
    module.insert("7".to_string(), cookie_snapshot);
    module.insert(
        "8".to_string(),
        Value::String(format!(
            "{}x{}",
            KUAISHOU_GDFP_SCREEN_WIDTH, KUAISHOU_GDFP_SCREEN_HEIGHT
        )),
    );
    module.insert("9".to_string(), Value::from(KUAISHOU_GDFP_SCREEN_HEIGHT));
    module.insert("10".to_string(), Value::from(KUAISHOU_GDFP_SCREEN_WIDTH));
    module.insert("11".to_string(), Value::String(ua.to_string()));
    module.insert("12".to_string(), Value::String(os_version.to_string()));
    module.insert("13".to_string(), Value::String("zh-CN".to_string()));
    module.insert("14".to_string(), Value::String(os_name.to_string()));
    module.insert(
        "16".to_string(),
        Value::String(ctx.user_id.clone().unwrap_or_default()),
    );
    module.insert("17".to_string(), Value::String(ctx.did.clone()));
    module.insert(
        "21".to_string(),
        json!({
            "type": "PAGE_ENTER",
            "timestamp": init_time,
            "initTime": create_start,
        }),
    );
    module.insert("24".to_string(), Value::String("99".to_string()));
    module.insert("25".to_string(), Value::String("99".to_string()));
    module.insert("26".to_string(), Value::String("0".to_string()));
    module.insert("28".to_string(), Value::String(room_hash));
    module.insert("29".to_string(), Value::String("/p/z/s".to_string()));
    module.insert(
        "30".to_string(),
        Value::String(KUAISHOU_GDFP_PRODUCT_SUBTYPE.to_string()),
    );
    module.insert("31".to_string(), Value::from(1));
    module.insert("32".to_string(), Value::String("0.00".to_string()));
    module.insert("33".to_string(), Value::String("Mozilla".to_string()));
    module.insert("34".to_string(), Value::String("Netscape".to_string()));
    module.insert("35".to_string(), Value::String(ua.to_string()));
    module.insert(
        "36".to_string(),
        Value::String(navigator_platform.to_string()),
    );
    module.insert("37".to_string(), Value::String("[\"zh-CN\"]".to_string()));
    module.insert(
        "41".to_string(),
        Value::String(KUAISHOU_GDFP_CHROME_BUILD.to_string()),
    );
    module.insert(
        "42".to_string(),
        Value::String(KUAISHOU_GDFP_BROWSER_VENDOR.to_string()),
    );
    module.insert("43".to_string(), Value::String(String::new()));
    module.insert("44".to_string(), Value::from(KUAISHOU_GDFP_DEVICE_MEMORY_GB));
    module.insert("50".to_string(), Value::from(0));
    module.insert("51".to_string(), Value::String(String::new()));
    module.insert("52".to_string(), Value::from(1));
    module.insert("53".to_string(), Value::String("Gecko".to_string()));
    module.insert("54".to_string(), Value::Bool(true));
    module.insert("55".to_string(), Value::from(1));
    module.insert("56".to_string(), Value::String(ua.to_string()));
    module.insert("57".to_string(), Value::String("zh-CN".to_string()));
    module.insert(
        "58".to_string(),
        Value::String(KUAISHOU_GDFP_PLUGIN_LIST.to_string()),
    );
    module.insert(
        "59".to_string(),
        Value::String(KUAISHOU_GDFP_MIME_LIST.to_string()),
    );
    module.insert("61".to_string(), Value::String(vendor_renderer));
    module.insert("62".to_string(), Value::String(renderer.to_string()));
    module.insert("63".to_string(), Value::String("1".to_string()));
    module.insert("64".to_string(), Value::String(webgl_vendor.to_string()));
    module.insert("65".to_string(), Value::String(webgl_version.to_string()));
    module.insert("66".to_string(), Value::String(webgl_glsl.to_string()));
    module.insert("67".to_string(), Value::from(0));
    module.insert("68".to_string(), Value::String(plugin_hash.clone()));
    module.insert("69".to_string(), Value::String(mime_hash.clone()));
    module.insert("70".to_string(), Value::String(ua_hash));
    module.insert("75".to_string(), Value::String("0".to_string()));
    module.insert("76".to_string(), Value::String("true".to_string()));
    module.insert("77".to_string(), json!(script_urls));
    module.insert("80".to_string(), Value::String("0".to_string()));
    module.insert("81".to_string(), Value::String("0".to_string()));
    module.insert(
        "82".to_string(),
        json!({
            "pc": KUAISHOU_GDFP_PLUGIN_LIST.len(),
            "ph": plugin_hash,
            "mf": KUAISHOU_GDFP_MIME_LIST.len(),
            "mh": mime_hash,
            "gr": renderer.len(),
            "gh": renderer_hash,
        }),
    );
    module.insert(
        "87".to_string(),
        json!({
            "w": KUAISHOU_GDFP_SCREEN_WIDTH,
            "h": KUAISHOU_GDFP_SCREEN_HEIGHT,
            "c": 24,
            "p": 24,
        }),
    );
    module.insert(
        "88".to_string(),
        Value::String(format!(
            "{:x}",
            md5::compute(ctx.principal_id.as_bytes())
        )),
    );
    module.insert(
        "89".to_string(),
        json!({
            "cs": create_start,
            "ce": create_start.saturating_add(120),
            "as": init_time.saturating_sub(80),
            "ae": init_time.saturating_sub(40),
            "cae": init_time.saturating_sub(20),
            "gs": init_time,
            "ge": init_time.saturating_add(320),
            "ls": now_ms,
        }),
    );
    module.insert("90".to_string(), Value::String(String::new()));
    Value::Object(module)
}

fn build_gdfp_pzs_payload(ctx: &KuaishouRequestContext) -> Result<Value, RecorderError> {
    let now_ms = Utc::now().timestamp_millis();
    let room_url = ctx.web_referer();
    let root_url = "https://live.kuaishou.com";
    let payload = json!({
        "1": ctx.did,
        "3": KUAISHOU_GDFP_APPKEY,
        "6": KUAISHOU_GDFP_BUSS_TYPE_GAME_LIVE,
        "7": 10,
        "9": now_ms,
        "10": "",
        "12": KUAISHOU_GDFP_APPKEY,
        "13": "",
        "14": KUAISHOU_GDFP_SDK_VERSION,
        "module_section": [build_gdfp_module_browser_payload(ctx, &room_url, root_url, now_ms)]
    });
    Ok(json!({
        "flag": 2,
        "data": encode_gdfp_payload(&payload)?,
    }))
}

async fn send_gdfp_request(
    client: &Client,
    path: &str,
    extra_query: Option<&str>,
    ctx: &KuaishouRequestContext,
    payload: &Value,
) -> Result<(), RecorderError> {
    let timestamp_secs = Utc::now().timestamp();
    let sign = build_gdfp_sign(timestamp_secs);
    let mut url = format!(
        "https://gdfp.gifshow.com{path}?appkey={}&seckey={}&bussType={}&timestamp={}&sign={}",
        KUAISHOU_GDFP_APPKEY,
        KUAISHOU_GDFP_SECKEY,
        KUAISHOU_GDFP_BUSS_TYPE_GAME_LIVE,
        timestamp_secs,
        sign
    );
    if let Some(extra) = extra_query.map(str::trim).filter(|value| !value.is_empty()) {
        url.push('&');
        url.push_str(extra);
    }
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());
    headers.insert("Accept", "*/*".parse().unwrap());
    headers.insert("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap());
    headers.insert(
        "Content-Type",
        "text/plain;charset=UTF-8".parse().unwrap(),
    );
    headers.insert("Referer", ctx.web_referer().parse().unwrap());
    headers.insert("Origin", "https://live.kuaishou.com".parse().unwrap());
    headers.insert("Sec-Fetch-Dest", "empty".parse().unwrap());
    headers.insert("Sec-Fetch-Mode", "cors".parse().unwrap());
    headers.insert("Sec-Fetch-Site", "cross-site".parse().unwrap());

    wait_for_web_request_slot("gdfp").await;
    let response = client
        .post(url)
        .headers(headers)
        .body(serde_json::to_string(payload).map_err(|e| RecorderError::ApiError {
            error: format!("Failed to serialize gdfp payload: {e}"),
        })?)
        .send()
        .await?;

    let status = response.status();
    let set_cookie = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join(" | ");
    let body_preview = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(240)
        .collect::<String>();

    log::debug!(
        "[Kuaishou] gdfp {} status={} room={} set_cookie={} body_preview={}",
        path,
        status,
        ctx.principal_id,
        set_cookie,
        body_preview
    );
    Ok(())
}

async fn warm_kuaishou_gdfp(client: &Client, ctx: &KuaishouRequestContext) {
    if !use_gdfp_prefetch() {
        return;
    }
    let cache_key = gdfp_prefetch_cache_key(ctx, KUAISHOU_GDFP_BUSS_TYPE_GAME_LIVE);
    if !gdfp_prefetch_allowed(&cache_key).await {
        return;
    }

    let sdk_payload = match build_gdfp_sdk_init_payload(ctx) {
        Ok(payload) => payload,
        Err(error) => {
            log::debug!("[Kuaishou] Skip gdfp sdk_init build: {error}");
            return;
        }
    };
    let pzs_payload = match build_gdfp_pzs_payload(ctx) {
        Ok(payload) => payload,
        Err(error) => {
            log::debug!("[Kuaishou] Skip gdfp p/z/s build: {error}");
            return;
        }
    };

    if let Err(error) =
        send_gdfp_request(client, "/s/u/v", Some("type=SDK_INIT"), ctx, &sdk_payload).await
    {
        set_gdfp_failure(
            &ctx.principal_id,
            format!("sdk_init failed: {}", error.to_string().replace('\n', " ")),
        )
        .await;
        log::debug!("[Kuaishou] gdfp sdk_init failed: {error}");
        return;
    }
    if let Err(error) = send_gdfp_request(client, "/p/z/s", None, ctx, &pzs_payload).await {
        set_gdfp_failure(
            &ctx.principal_id,
            format!("pzs failed: {}", error.to_string().replace('\n', " ")),
        )
        .await;
        log::debug!("[Kuaishou] gdfp p/z/s failed: {error}");
        return;
    }

    mark_gdfp_prefetch(cache_key).await;
    clear_gdfp_failure(&ctx.principal_id).await;
}

pub fn is_rate_limited_error(error: &RecorderError) -> bool {
    match error {
        RecorderError::ApiError { error } => is_rate_limit_message(error),
        _ => false,
    }
}

pub fn is_captcha_error(error: &RecorderError) -> bool {
    match error {
        RecorderError::ApiError { error } => is_captcha_message(error),
        _ => false,
    }
}

pub fn is_room_disabled_error(error: &RecorderError) -> bool {
    match error {
        RecorderError::ApiError { error } => is_room_disabled_message(error),
        _ => false,
    }
}

fn decode_json_string(raw: &str) -> Option<String> {
    serde_json::from_str::<String>(&format!("\"{raw}\""))
        .ok()
        .or_else(|| {
            let decoded = raw
                .replace("\\u002F", "/")
                .replace("\\u0026", "&")
                .replace("\\u003D", "=");
            if decoded == raw {
                None
            } else {
                Some(decoded)
            }
        })
}

fn extract_rate_limit_message_from_body(body: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        let keys = ["error_msg", "errorMsg", "errMsg", "message", "msg"];
        for key in keys {
            if let Some(msg) = value.get(key).and_then(|v| v.as_str()) {
                if is_rate_limit_message(msg) {
                    return Some(msg.to_string());
                }
            }
        }
    }

    if is_rate_limit_message(body) {
        return Some(body.trim().to_string());
    }

    None
}

fn restore_stream_url_escapes(raw: &str) -> String {
    raw.replace("\\u002F", "/")
        .replace("\\u0026", "&")
        .replace("\\u003D", "=")
        .replace("\\/", "/")
        .replace("&amp;", "&")
}

fn is_kuaishou_live_stream_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("pull.yximgs.com")
        || lower.contains("/gifshow/")
        || lower.contains("hwsecret=")
        || lower.contains("srcstrm=")
}

fn extract_direct_stream_urls_from_html(html_str: &str, cookie: &str) -> Vec<StreamInfo> {
    static DIRECT_HTTP_RE: OnceLock<Regex> = OnceLock::new();
    static DIRECT_RTMP_RE: OnceLock<Regex> = OnceLock::new();

    let normalized = restore_stream_url_escapes(html_str);
    let http_re = DIRECT_HTTP_RE.get_or_init(|| {
        Regex::new(r#"https?://[^\s"'<>\\]+(?:\.m3u8|\.flv)(?:\?[^\s"'<>\\]*)?"#).unwrap()
    });
    let rtmp_re =
        DIRECT_RTMP_RE.get_or_init(|| Regex::new(r#"rtmps?://[^\s"'<>\\]+"#).unwrap());

    let mut urls: Vec<StreamInfo> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for mat in http_re.find_iter(&normalized) {
        let candidate = mat.as_str();
        if !is_kuaishou_live_stream_url(candidate) {
            continue;
        }
        push_stream_info_unique(
            &mut urls,
            &mut seen,
            candidate,
            "Direct".to_string(),
            None,
            cookie,
        );
    }

    for mat in rtmp_re.find_iter(&normalized) {
        let candidate = mat.as_str();
        if !is_kuaishou_live_stream_url(candidate) {
            continue;
        }
        push_stream_info_unique(
            &mut urls,
            &mut seen,
            candidate,
            "Direct".to_string(),
            None,
            cookie,
        );
    }

    urls.sort_by(|a, b| a.url.cmp(&b.url));
    urls
}

fn extract_hls_play_url(html_str: &str) -> Option<String> {
    let regex = Regex::new(r#""hlsPlayUrl"\s*:\s*"([^"]+)""#).ok()?;
    let raw = regex.captures(html_str)?.get(1)?.as_str();
    let decoded = decode_json_string(raw)?;
    if decoded.contains(".m3u8") {
        Some(decoded)
    } else {
        None
    }
}

fn extract_initial_state(html_str: &str) -> Option<String> {
    let patterns = [
        r#"(?s)window\.__INITIAL_STATE__\s*=\s*(\{.*?\});\s*\(function"#,
        r#"(?s)window\.__INITIAL_STATE__\s*=\s*(\{.*?\})\s*;\s*</script>"#,
        r#"(?s)window\['__INITIAL_STATE__'\]\s*=\s*(\{.*?\})\s*;\s*</script>"#,
        r#"(?s)window\.__INITIAL_STATE__\s*=\s*(\{.*?\})\s*;\s*window\.__"#,
        r#"(?s)__INITIAL_STATE__\s*=\s*(\{.*?\})\s*;\s*\(function"#,
        r#"(?s)__INITIAL_STATE__\s*=\s*(\{.*?\})\s*;\s*<"#,
    ];

    for (i, pattern) in patterns.iter().enumerate() {
        if let Ok(regex) = Regex::new(pattern) {
            if let Some(captures) = regex.captures(html_str) {
                if let Some(value) = captures.get(1) {
                    let json_str = value.as_str().trim();
                    log::debug!(
                        "[Kuaishou] Extracted __INITIAL_STATE__ using pattern #{}",
                        i
                    );
                    return Some(json_str.to_string());
                }
            }
        }
    }

    log::warn!(
        "[Kuaishou] Failed to extract __INITIAL_STATE__ with any pattern, HTML size: {} bytes",
        html_str.len()
    );
    // Log a snippet of the HTML for debugging (first 500 chars)
    let snippet_len = html_str.len().min(500);
    if snippet_len > 0 {
        log::debug!("[Kuaishou] HTML snippet: {}...", &html_str[..snippet_len]);
    }

    None
}

fn clean_json_state(raw: &str) -> String {
    let mut trimmed = raw.trim().trim_end_matches(';').trim().to_string();
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            trimmed = trimmed[start..=end].to_string();
        }
    }
    if trimmed.contains("undefined") {
        trimmed = trimmed.replace("undefined", "null");
    }
    trimmed
}

fn extract_metadata_from_html(html_str: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut title = None;
    let mut cover = None;
    let mut avatar = None;

    // Try finding title
    let title_patterns = [
        r#"<title>(.*?)</title>"#,
        r#""caption"\s*:\s*"([^"]+)""#,
        r#""title"\s*:\s*"([^"]+)""#,
    ];
    for p in title_patterns {
        if let Some(m) = Regex::new(p)
            .ok()
            .and_then(|re| re.captures(html_str))
            .and_then(|c| c.get(1))
        {
            let t = m
                .as_str()
                .replace(" - \u{5feb}\u{624b}\u{76f4}\u{64ad}", "")
                .trim()
                .to_string();
            if !t.is_empty()
                && !t.contains("\u{9519}\u{8bef}\u{4ee3}\u{7801}")
                && !t.contains("Error Code")
            {
                title = Some(t);
                break;
            }
        }
    }

    // Try finding avatar
    let avatar_patterns = [
        r#""headUrl"\s*:\s*"([^"]+)""#,
        r#""avatar"\s*:\s*"([^"]+)""#,
        r#""avatarUrl"\s*:\s*"([^"]+)""#,
    ];
    for p in avatar_patterns {
        if let Some(m) = Regex::new(p)
            .ok()
            .and_then(|re| re.captures(html_str))
            .and_then(|c| c.get(1))
        {
            if let Some(decoded) = decode_json_string(m.as_str()) {
                avatar = Some(normalize_image_url(&decoded));
                break;
            }
        }
    }

    // Try finding cover
    let cover_patterns = [
        r#""poster"\s*:\s*"([^"]+)""#,
        r#""coverUrl"\s*:\s*"([^"]+)""#,
        r#""cover"\s*:\s*"([^"]+)""#,
        r#""snapshot"\s*:\s*"([^"]+)""#,
    ];
    for p in cover_patterns {
        if let Some(m) = Regex::new(p)
            .ok()
            .and_then(|re| re.captures(html_str))
            .and_then(|c| c.get(1))
        {
            if let Some(decoded) = decode_json_string(m.as_str()) {
                cover = Some(normalize_image_url(&decoded));
                break;
            }
        }
    }

    (title, cover, avatar)
}

fn score_live_stream_response(response: &LiveStreamResponse) -> i64 {
    let mut score = 0;

    if let Some(stream) = response.live_stream.as_ref() {
        score += 10;
        if stream.play_urls.is_some() {
            score += 1_000;
        }
        if stream
            .caption
            .as_deref()
            .map(str::trim)
            .is_some_and(|v| !v.is_empty())
        {
            score += 50;
        }
        if stream
            .cover_url
            .as_deref()
            .map(str::trim)
            .is_some_and(|v| !v.is_empty())
        {
            score += 20;
        }
    }

    if response
        .author
        .as_ref()
        .is_some_and(|a| !a.id.trim().is_empty() || !a.name.trim().is_empty())
    {
        score += 25;
    }

    if response
        .config
        .as_ref()
        .and_then(|cfg| cfg.caption.as_deref())
        .map(str::trim)
        .is_some_and(|v| !v.is_empty())
    {
        score += 10;
    }

    if response.error_type.is_some() {
        score -= 500;
    }

    score
}

fn find_live_stream_response(value: &Value) -> Option<LiveStreamResponse> {
    fn visit(value: &Value, best: &mut Option<(i64, LiveStreamResponse)>) {
        match value {
            Value::Object(map) => {
                if map.contains_key("liveStream") || map.contains_key("live_stream") {
                    let mut cloned = map.clone();
                    if !cloned.contains_key("liveStream") {
                        if let Some(v) = cloned.remove("live_stream") {
                            cloned.insert("liveStream".to_string(), v);
                        }
                    }
                    if let Some(Value::String(text)) = cloned.get("liveStream") {
                        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                            cloned.insert("liveStream".to_string(), parsed);
                        }
                    }
                    if let Ok(response) =
                        serde_json::from_value::<LiveStreamResponse>(Value::Object(cloned))
                    {
                        if response.live_stream.is_some()
                            || response.author.is_some()
                            || response.error_type.is_some()
                        {
                            let score = score_live_stream_response(&response);
                            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                                *best = Some((score, response));
                            }
                        }
                    }
                }

                if map.contains_key("playUrls")
                    || map.contains_key("hlsPlayUrl")
                    || map.contains_key("multiResolutionPlayUrls")
                    || map.contains_key("multiResolutionHlsPlayUrls")
                {
                    let mut direct_stream = map.clone();
                    if let Some(Value::String(text)) = direct_stream.get("playUrls") {
                        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                            direct_stream.insert("playUrls".to_string(), parsed);
                        }
                    }
                    if let Some(Value::String(text)) = direct_stream.get("author") {
                        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                            direct_stream.insert("author".to_string(), parsed);
                        }
                    }
                    if let Ok(live_stream) =
                        serde_json::from_value::<LiveStream>(Value::Object(direct_stream.clone()))
                    {
                        let response = LiveStreamResponse {
                            live_stream: Some(live_stream),
                            author: direct_stream
                                .get("author")
                                .cloned()
                                .and_then(|v| serde_json::from_value(v).ok()),
                            config: direct_stream
                                .get("config")
                                .cloned()
                                .and_then(|v| serde_json::from_value(v).ok()),
                            error_type: direct_stream
                                .get("errorType")
                                .or_else(|| direct_stream.get("error_type"))
                                .cloned()
                                .and_then(|v| serde_json::from_value(v).ok()),
                        };
                        let score = score_live_stream_response(&response);
                        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                            *best = Some((score, response));
                        }
                    }
                }

                for child in map.values() {
                    visit(child, best);
                }
            }
            Value::Array(values) => {
                for child in values {
                    visit(child, best);
                }
            }
            Value::String(text) => {
                if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                    visit(&parsed, best);
                }
            }
            _ => {}
        }
    }

    let mut best: Option<(i64, LiveStreamResponse)> = None;
    visit(value, &mut best);
    best.map(|(_, response)| response)
}

fn parse_live_stream_response(json_str: &str) -> Result<LiveStreamResponse, RecorderError> {
    let livestream_regex = Regex::new(r#"(?s)(\{"liveStream".*?),"gameInfo"#).map_err(|e| {
        RecorderError::ApiError {
            error: format!("Failed to create regex: {}", e),
        }
    })?;

    if let Some(cap) = livestream_regex
        .captures(json_str)
        .and_then(|cap| cap.get(1))
    {
        let full_json = format!("{}}}", cap.as_str());
        if let Ok(response) = serde_json::from_str::<LiveStreamResponse>(&full_json) {
            return Ok(response);
        }
    }

    let state: Value = serde_json::from_str(json_str).map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse JSON: {}", e),
    })?;

    find_live_stream_response(&state).ok_or(RecorderError::ApiError {
        error: "Failed to extract liveStream data".to_string(),
    })
}

fn parse_room_info_from_initial_state(
    json_str: &str,
    principal_id: &str,
    cookies: &str,
) -> Result<Option<RoomInfo>, RecorderError> {
    let state: Value = serde_json::from_str(json_str)
        .or_else(|_| serde_json::from_str(&clean_json_state(json_str)))
        .map_err(|e| RecorderError::ApiError {
            error: format!("Failed to parse __INITIAL_STATE__: {}", e),
        })?;

    if let Some(play_item) = state
        .get("liveroom")
        .and_then(|v| v.get("playList"))
        .and_then(|v| v.as_array())
        .and_then(|list| list.first())
    {
        if let Some(room) = parse_room_info_from_livedetail_value(play_item, principal_id, cookies)? {
            if room.live_status || !room.streams.is_empty() {
                return Ok(Some(room));
            }
        }
    }

    parse_room_info_from_livedetail_value(&state, principal_id, cookies)
}

fn normalize_cookie_header(cookies: &str) -> String {
    cookies
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

fn get_cookie_value_ci(cookies: &str, key: &str) -> Option<String> {
    let target = key.to_ascii_lowercase();
    for part in cookies.split(';').map(str::trim) {
        if let Some((k, v)) = part.split_once('=') {
            if k.trim().to_ascii_lowercase() == target {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn append_cookie_pair(mut header: String, key: &str, value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return header;
    }
    if !header.is_empty() {
        header.push_str("; ");
    }
    header.push_str(key);
    header.push('=');
    header.push_str(value);
    header
}

fn merge_cookie_kv(map: &mut HashMap<String, String>, key: &str, value: &str) {
    let key = key.trim();
    if key.is_empty() {
        return;
    }
    map.insert(key.to_string(), value.trim().to_string());
}

fn parse_cookie_map(cookies: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in normalize_cookie_header(cookies).split(';').map(str::trim) {
        if let Some((name, value)) = part.split_once('=') {
            merge_cookie_kv(&mut map, name, value);
        }
    }
    map
}

fn merge_set_cookie_headers(
    map: &mut HashMap<String, String>,
    headers: &reqwest::header::HeaderMap,
) {
    for cookie_header in headers.get_all(reqwest::header::SET_COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
            if let Some(cookie_part) = cookie_str.split(';').next() {
                if let Some((name, value)) = cookie_part.split_once('=') {
                    merge_cookie_kv(map, name, value);
                }
            }
        }
    }
}

fn format_cookie_map(map: &HashMap<String, String>) -> String {
    let mut pairs: Vec<String> = map.iter().map(|(k, v)| format!("{k}={v}")).collect();
    pairs.sort();
    pairs.join("; ")
}

fn ensure_kuaishou_request_cookie(cookies: &str) -> String {
    let normalized = normalize_cookie_header(cookies);
    let filtered = filter_kuaishou_cookie_header(&normalized);
    let ensured = if filtered.is_empty() {
        let did = gen_web_did();
        let didv = Utc::now().timestamp_millis();
        format!("did={did}; didv={didv}")
    } else {
        ensure_kuaishou_base_cookies(&filtered)
    };

    ensured
}

pub(crate) fn normalize_record_cookie(cookies: &str) -> String {
    let mut normalized = normalize_cookie_header(cookies);
    if normalized.is_empty() {
        let did = gen_web_did();
        let didv = Utc::now().timestamp_millis();
        return format!("did={did}; didv={didv}");
    }

    let did_existing = get_cookie_value_ci(&normalized, "did")
        .or_else(|| get_cookie_value_ci(&normalized, "_did"));
    if did_existing.is_none() {
        normalized = append_cookie_pair(normalized, "did", &gen_web_did());
    }

    if get_cookie_value_ci(&normalized, "didv").is_none() {
        let didv = Utc::now().timestamp_millis().to_string();
        normalized = append_cookie_pair(normalized, "didv", &didv);
    }

    normalized
}

fn ensure_kuaishou_base_cookies(cookies: &str) -> String {
    let mut normalized = filter_kuaishou_cookie_header(&normalize_cookie_header(cookies));
    if normalized.is_empty() {
        return normalized;
    }

    let did_existing = get_cookie_value_ci(&normalized, "did");
    if did_existing.is_none() {
        let fallback_did = get_cookie_value_ci(&normalized, "_did");
        let did_value = fallback_did.unwrap_or_else(gen_web_did);
        normalized = append_cookie_pair(normalized, "did", &did_value);
    }

    if get_cookie_value_ci(&normalized, "didv").is_none() {
        let didv = Utc::now().timestamp_millis().to_string();
        normalized = append_cookie_pair(normalized, "didv", &didv);
    }

    normalized
}

fn filter_kuaishou_cookie_header(cookies: &str) -> String {
    let mut kept = Vec::new();
    for part in cookies.split(';').map(str::trim) {
        if part.is_empty() {
            continue;
        }
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if key.is_empty() || val.is_empty() {
            continue;
        }
        kept.push(format!("{key}={val}"));
    }
    kept.join("; ")
}

fn extract_user_id_from_url(url: &str) -> String {
    if let Some(query) = url.split('?').nth(1) {
        for pair in query.split('&') {
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (key.trim(), value.trim()),
                None => continue,
            };
            if value.is_empty() {
                continue;
            }
            if key.eq_ignore_ascii_case("principalId")
                || key.eq_ignore_ascii_case("userId")
                || key.eq_ignore_ascii_case("user_id")
            {
                return value.to_string();
            }
        }
    }

    let fragment = url.split('#').nth(1).map(str::trim).unwrap_or("");
    let url_no_fragment = url.split('#').next().unwrap_or(url);
    let url_no_query = url_no_fragment.split('?').next().unwrap_or(url_no_fragment);
    let trimmed = url_no_query.trim_end_matches('/');

    if let Some(pos) = trimmed.find("/u/") {
        let tail = &trimmed[(pos + 3)..];
        let candidate = tail.split('/').next().unwrap_or(tail).trim();
        if !candidate.is_empty() && !candidate.eq_ignore_ascii_case("kuaishou") {
            return candidate.to_string();
        }
    }
    if let Some(pos) = trimmed.find("/profile/") {
        let tail = &trimmed[(pos + 9)..];
        let candidate = tail.split('/').next().unwrap_or(tail).trim();
        if !candidate.is_empty() && !candidate.eq_ignore_ascii_case("kuaishou") {
            return candidate.to_string();
        }
    }

    if trimmed.contains("kuaishou.com") {
        if let Some(last) = trimmed.rsplit('/').next() {
            let candidate = last.trim();
            if !candidate.is_empty() && !candidate.eq_ignore_ascii_case("kuaishou") {
                return candidate.to_string();
            }
        }
    }

    if !fragment.is_empty()
        && !fragment.contains('/')
        && !fragment.contains('?')
        && !fragment.contains('&')
        && !fragment.contains('=')
    {
        return fragment.to_string();
    }

    String::new()
}

fn build_web_candidate_urls(url: &str) -> Vec<String> {
    let mut urls = Vec::new();
    if !url.is_empty() {
        urls.push(url.to_string());
    }

    if url.contains("live.kuaishou.com") {
        let www_url = url.replace("live.kuaishou.com", "www.kuaishou.com");
        if !urls.contains(&www_url) {
            urls.push(www_url);
        }
    } else if url.contains("www.kuaishou.com") {
        let live_url = url.replace("www.kuaishou.com", "live.kuaishou.com");
        if !urls.contains(&live_url) {
            urls.push(live_url);
        }
    }

    let eid = extract_user_id_from_url(url);
    if !eid.is_empty() {
        let candidates = [
            format!("https://live.kuaishou.com/u/{eid}"),
            format!("https://www.kuaishou.com/u/{eid}"),
            format!("https://www.kuaishou.com/live/u/{eid}"),
        ];
        for candidate in candidates {
            if !urls.contains(&candidate) {
                urls.push(candidate);
            }
        }
    }

    urls
}

async fn fetch_web_html(
    client: &Client,
    account: &Account,
    url: &str,
) -> Result<(String, reqwest::Url), RecorderError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());
    headers.insert(
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
            .parse()
            .unwrap(),
    );
    headers.insert(
        "Accept-Language",
        "zh-CN,zh;q=0.8,zh-TW;q=0.7,zh-HK;q=0.5,en-US;q=0.3,en;q=0.2"
            .parse()
            .unwrap(),
    );

    let mut cookie_header = String::new();
    if !account.cookies.is_empty() {
        let cookie = normalize_cookie_header(&account.cookies);
        if !cookie.is_empty() {
            cookie_header = cookie;
            headers.insert("Cookie", cookie_header.parse().unwrap());
        }
    }

    for attempt in 0..2 {
        if !web_api_allowed(account, url).await {
            let retry_secs = web_retry_secs();
            log::info!(
                "[Kuaishou] Web rate limited, retrying after {}s (attempt {})",
                retry_secs,
                attempt + 1
            );
            let retry_jitter = rand::random_range(0..=2);
            tokio::time::sleep(Duration::from_secs(retry_secs + retry_jitter)).await;
        }

        // Homepage prewarm is only needed for guest mode by default.
        if should_homepage_prewarm(account) {
            let mut pre_headers = headers.clone();
            pre_headers.insert("Referer", "https://live.kuaishou.com/".parse().unwrap());
            wait_for_web_request_slot("homepage_prewarm").await;
            match client
                .get("https://live.kuaishou.com/")
                .headers(pre_headers)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    let mut prewarm_map: HashMap<String, String> = HashMap::new();
                    for cookie_header in resp.headers().get_all("set-cookie") {
                        if let Ok(cookie_str) = cookie_header.to_str() {
                            if let Some(pair) = cookie_str.split(';').next() {
                                if let Some((name, value)) = pair.split_once('=') {
                                    let name = name.trim();
                                    let value = value.trim();
                                    if !name.is_empty() {
                                        prewarm_map.insert(name.to_string(), value.to_string());
                                    }
                                }
                            }
                        }
                    }
                    let body = resp.text().await.unwrap_or_default();
                    if !status.is_success() {
                        log::warn!("[Kuaishou] Homepage prewarm status: {}", status);
                    }
                    if let Some(msg) = extract_rate_limit_message_from_body(&body) {
                        set_web_cooldown(account, url, &msg).await;
                    }
                    if !prewarm_map.is_empty() {
                        let mut merged: HashMap<String, String> = HashMap::new();
                        for part in cookie_header.split(';').map(str::trim) {
                            if let Some((k, v)) = part.split_once('=') {
                                let key = k.trim();
                                if !key.is_empty() {
                                    merged.insert(key.to_string(), v.trim().to_string());
                                }
                            }
                        }
                        for (k, v) in prewarm_map {
                            merged.entry(k).or_insert(v);
                        }
                        let mut pairs: Vec<String> = merged
                            .into_iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect();
                        pairs.sort();
                        cookie_header = pairs.join("; ");
                    }
                }
                Err(err) => {
                    log::debug!("[Kuaishou] Homepage prewarm failed: {}", err);
                }
            }
        } else {
            log::debug!("[Kuaishou] Skip homepage prewarm for login account");
        }

        let mut last_error: Option<RecorderError> = None;
        let mut last_small: Option<(String, reqwest::Url)> = None;

        for candidate in build_web_candidate_urls(url) {
            let referer = if candidate.contains("www.kuaishou.com") {
                "https://www.kuaishou.com/"
            } else {
                "https://live.kuaishou.com/"
            };
            let mut req_headers = headers.clone();
            if !cookie_header.is_empty() {
                req_headers.insert("Cookie", cookie_header.parse().unwrap());
            }
            req_headers.insert("Referer", referer.parse().unwrap());
            wait_for_web_request_slot("room_page").await;

            let response = client.get(&candidate).headers(req_headers).send().await?;
            let status = response.status();
            let final_url = response.url().clone();
            let html_str = response.text().await?;

            if !status.is_success() {
                log::warn!(
                    "[Kuaishou] Web response status: {}, url: {}",
                    status,
                    final_url
                );
                if let Some(msg) = extract_rate_limit_message_from_body(&html_str) {
                    set_web_cooldown(account, url, &msg).await;
                    last_error = Some(RecorderError::ApiError { error: msg });
                    continue;
                }
                let snippet_len = html_str.len().min(200);
                if snippet_len > 0 {
                    log::debug!(
                        "[Kuaishou] Web error snippet: {}...",
                        &html_str[..snippet_len]
                    );
                }
                last_error = Some(RecorderError::ApiError {
                    error: format!("Kuaishou web status: {}", status),
                });
                continue;
            }

            if html_str.len() <= 256 {
                log::warn!(
                    "[Kuaishou] Web response small ({} bytes), url: {}",
                    html_str.len(),
                    final_url
                );
                if let Some(msg) = extract_rate_limit_message_from_body(&html_str) {
                    set_web_cooldown(account, url, &msg).await;
                    last_error = Some(RecorderError::ApiError { error: msg });
                    continue;
                }
                let snippet_len = html_str.len().min(200);
                if snippet_len > 0 {
                    log::debug!(
                        "[Kuaishou] Web small snippet: {}...",
                        &html_str[..snippet_len]
                    );
                }
                last_small = Some((html_str, final_url));
                continue;
            }

            return Ok((html_str, final_url));
        }

        if let Some((html, final_url)) = last_small {
            return Ok((html, final_url));
        }

        let err = last_error.unwrap_or_else(|| RecorderError::ApiError {
            error: "Failed to fetch Kuaishou web page".to_string(),
        });

        if let RecorderError::ApiError { error } = &err {
            if is_rate_limit_message(error) && attempt == 0 {
                let retry_secs = web_retry_secs();
                log::info!(
                    "[Kuaishou] Web rate limited, retrying after {}s",
                    retry_secs
                );
                let retry_jitter = rand::random_range(0..=2);
                tokio::time::sleep(Duration::from_secs(retry_secs + retry_jitter)).await;
                continue;
            }
        }
        return Err(err);
    }

    Err(RecorderError::ApiError {
        error: "???????????".to_string(),
    })
}

fn normalize_image_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("//") {
        format!("https:{}", trimmed)
    } else {
        trimmed.to_string()
    }
}

#[derive(Clone, Debug)]
struct FollowLiveInfo {
    principal_id: Option<String>,
    caption: Option<String>,
    cover_url: Option<String>,
    user_name: Option<String>,
    user_id: Option<String>,
    user_avatar: Option<String>,
    live: Option<bool>,
    streams: Vec<StreamInfo>,
}

#[derive(Clone, Debug)]
struct FollowInfoCacheEntry {
    ts: i64,
    info: Option<FollowLiveInfo>,
}

#[derive(Clone, Debug)]
struct KwwProbeCacheEntry {
    ts: i64,
    kww: Option<String>,
}

fn kww_probe_cache_ttl_secs() -> i64 {
    read_env_u64(
        "LIVE_KUAISHOU_KWW_PROBE_CACHE_TTL_SECS",
        KWW_PROBE_CACHE_TTL_SECS as u64,
    ) as i64
}

fn kww_probe_miss_ttl_secs() -> i64 {
    read_env_u64(
        "LIVE_KUAISHOU_KWW_PROBE_MISS_TTL_SECS",
        KWW_PROBE_MISS_TTL_SECS as u64,
    ) as i64
}

async fn get_cached_kww_probe(room_id: &str) -> Option<Option<String>> {
    let cache = KWW_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    let now = Utc::now().timestamp();
    let Some(entry) = guard.get(room_id).cloned() else {
        return None;
    };

    let ttl = if entry.kww.is_some() {
        kww_probe_cache_ttl_secs()
    } else {
        kww_probe_miss_ttl_secs()
    };
    if now.saturating_sub(entry.ts) > ttl {
        guard.remove(room_id);
        return None;
    }
    Some(entry.kww)
}

async fn set_cached_kww_probe(room_id: String, kww: Option<String>) {
    let cache = KWW_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    guard.insert(
        room_id,
        KwwProbeCacheEntry {
            ts: Utc::now().timestamp(),
            kww,
        },
    );
}

fn normalize_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn follow_match_target_ids(room_id: &str, author_id: &str, author_name: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut push = |value: String| {
        let normalized = normalize_id(&value);
        if normalized.is_empty() {
            return;
        }
        if !targets.iter().any(|item| item == &normalized) {
            targets.push(normalized);
        }
    };

    for raw in [room_id, author_id, author_name] {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        push(trimmed.to_string());
        let extracted = extract_user_id_from_url(trimmed);
        if !extracted.is_empty() {
            push(extracted);
        }
        let resolved = resolve_principal_id(trimmed);
        if !resolved.is_empty() {
            push(resolved);
        }
    }

    targets
}

fn follow_item_matches(item: &UserFollowLive, room_id: &str, author_id: &str, author_name: &str) -> bool {
    let targets = follow_match_target_ids(room_id, author_id, author_name);
    if targets.is_empty() {
        return false;
    }

    let principal_id = item
        .user
        .as_ref()
        .and_then(|user| user.principal_id.as_deref())
        .map(normalize_id)
        .unwrap_or_default();
    let user_id = item
        .user
        .as_ref()
        .and_then(|user| user.user_id.as_deref())
        .map(normalize_id)
        .unwrap_or_default();
    let user_name = item
        .user
        .as_ref()
        .and_then(|user| user.user_name.as_deref())
        .map(normalize_id)
        .unwrap_or_default();
    let live_stream_id = item
        .live_stream_id
        .as_deref()
        .map(normalize_id)
        .unwrap_or_default();

    targets.iter().any(|target| {
        (!principal_id.is_empty() && target == &principal_id)
            || (!user_id.is_empty() && target == &user_id)
            || (!user_name.is_empty() && target == &user_name)
            || (!live_stream_id.is_empty() && target == &live_stream_id)
    })
}

fn should_homepage_prewarm(account: &Account) -> bool {
    if let Some(enabled) = read_env_bool("LIVE_KUAISHOU_HOMEPAGE_PREWARM") {
        return enabled;
    }
    account.is_guest()
}

fn follow_info_cache_ttl_secs() -> i64 {
    read_env_u64(
        "LIVE_KUAISHOU_FOLLOW_CACHE_TTL_SECS",
        FOLLOW_INFO_CACHE_TTL_SECS as u64,
    ) as i64
}

fn follow_info_miss_ttl_secs() -> i64 {
    read_env_u64(
        "LIVE_KUAISHOU_FOLLOW_MISS_TTL_SECS",
        FOLLOW_INFO_MISS_TTL_SECS as u64,
    ) as i64
}

fn follow_info_cache_key(room_id: &str, author_id: &str, author_name: &str) -> Option<String> {
    if !room_id.trim().is_empty() {
        return Some(format!("room:{}", normalize_id(room_id)));
    }
    if !author_id.trim().is_empty() {
        return Some(format!("author:{}", normalize_id(author_id)));
    }
    if !author_name.trim().is_empty() {
        return Some(format!("name:{}", normalize_id(author_name)));
    }
    None
}

async fn get_cached_follow_info(key: &str) -> Option<Option<FollowLiveInfo>> {
    let cache = FOLLOW_INFO_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    let now = Utc::now().timestamp();
    if let Some(entry) = guard.get(key) {
        let ttl_secs = if entry.info.is_some() {
            follow_info_cache_ttl_secs()
        } else {
            follow_info_miss_ttl_secs()
        };
        if now.saturating_sub(entry.ts) <= ttl_secs {
            return Some(entry.info.clone());
        }
    }
    guard.remove(key);
    None
}

async fn set_cached_follow_info(key: String, info: Option<FollowLiveInfo>) {
    let cache = FOLLOW_INFO_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    guard.insert(
        key,
        FollowInfoCacheEntry {
            ts: Utc::now().timestamp(),
            info,
        },
    );
}

async fn fetch_follow_live_info_cached(
    client: &Client,
    account: &Account,
    room_id: &str,
    author_id: &str,
    author_name: &str,
) -> Option<FollowLiveInfo> {
    let cache_key = follow_info_cache_key(room_id, author_id, author_name);
    if let Some(key) = cache_key.as_deref() {
        if let Some(cached) = get_cached_follow_info(key).await {
            return cached;
        }
    }

    // Cache only follow-list results. Livedetail fallback is handled by outer
    // room-info flow; mixing fallback here can poison follow cache for too long.
    let fetched = fetch_follow_live_info(client, account, room_id, author_id, author_name).await;

    if let Some(key) = cache_key {
        set_cached_follow_info(key, fetched.clone()).await;
    }

    fetched
}

fn extract_kuaishou_kww(cookies: &str) -> Option<String> {
    for part in cookies.split(';').map(str::trim) {
        if let Some((key, value)) = part.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            if key == "kww" || key == "kwfv1" {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn get_kuaishou_kww_override() -> Option<String> {
    for key in ["LIVE_KUAISHOU_KWW", "KUAISHOU_KWW"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    if let Some(overrides) = crate::reverse_generate::qr_login::fetch_kuaishou_overrides() {
        if let Some(value) = overrides.get("kww").or_else(|| overrides.get("kwfv1")) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn resolve_kuaishou_kww(cookies: &str) -> Option<String> {
    resolve_kuaishou_raw_kww(cookies).and_then(|value| normalize_kuaishou_kww_header_value(&value))
}

fn extract_kuaishou_kww_from_html(html: &str) -> Option<String> {
    let patterns = [
        r#"(?i)"kww"\s*:\s*"([^"]+)""#,
        r#"(?i)"kwfv1"\s*:\s*"([^"]+)""#,
        r#"(?i)\bkww\s*=\s*"([^"]+)""#,
        r#"(?i)\bkwfv1\s*=\s*"([^"]+)""#,
    ];
    for pattern in patterns {
        let Ok(re) = Regex::new(pattern) else {
            continue;
        };
        if let Some(m) = re.captures(html).and_then(|caps| caps.get(1)) {
            let value = m.as_str().trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn normalize_kuaishou_kww_header_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.contains("###ssr") {
        Some(trimmed.to_string())
    } else {
        Some(format!("{trimmed}###ssrc"))
    }
}

fn resolve_kuaishou_raw_kww(cookies: &str) -> Option<String> {
    extract_kuaishou_kww(cookies).or_else(get_kuaishou_kww_override)
}

fn encode_uri_component_like_js(input: &str) -> String {
    const UNESCAPED: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'();,/?:@&=+$,#";
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        if UNESCAPED.contains(byte) {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", byte));
        }
    }
    out
}

fn random_base62(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        let idx = rng.random_range(0..CHARSET.len());
        out.push(CHARSET[idx] as char);
    }
    out
}

fn aes_128_cbc_encrypt_base64(plain: &str, secret: &str) -> Option<String> {
    let key = secret.as_bytes();
    if key.len() != 16 {
        return None;
    }
    let mut buffer = plain.as_bytes().to_vec();
    let message_len = buffer.len();
    let block_size = 16;
    let pad_len = block_size - (message_len % block_size);
    buffer.resize(message_len + pad_len, 0);
    let cipher = Aes128CbcEncryptor::new_from_slices(key, key).ok()?;
    let encrypted = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, message_len)
        .ok()?;
    Some(general_purpose::STANDARD.encode(encrypted))
}

fn build_kuaishou_webweapon_fingerprint(encrypted: &str, suffix: char) -> Option<String> {
    if encrypted.len() < 6 {
        return None;
    }
    Some(format!(
        "K{}W{}{}{}",
        &encrypted[..4],
        &encrypted[4..encrypted.len() - 2],
        suffix,
        &encrypted[encrypted.len() - 2..]
    ))
}

fn generate_kuaishou_webweapon_values(
    room_url: &str,
    did: &str,
    product_name: &str,
) -> Option<(String, String, String)> {
    let now_ms = Utc::now().timestamp_millis();
    let room_ref = encode_uri_component_like_js(room_url);
    let fingerprint_seed = random_base62(8);
    let fingerprint_plain =
        format!("{room_ref}|{did}|{product_name}|{now_ms}|{fingerprint_seed}");
    let encrypted_fp = aes_128_cbc_encrypt_base64(&fingerprint_plain, KUAISHOU_WEBWEAPON_FP_KEY)?;
    let kwfv1 = build_kuaishou_webweapon_fingerprint(&encrypted_fp, 'F')?;

    let sec_token = random_base62(64);
    let kwscode_plain = format!(
        "{room_ref}|{did}|{product_name}|{now_ms}|{}",
        &sec_token[..8]
    );
    let encrypted_sign =
        aes_128_cbc_encrypt_base64(&kwscode_plain, KUAISHOU_WEBWEAPON_SIGN_KEY)?;
    let kwscode = build_kuaishou_webweapon_fingerprint(&encrypted_sign, 'S')?;

    Some((kwfv1, sec_token, kwscode))
}

pub(crate) fn build_kuaishou_runtime_cookie(cookies: &str, principal_id: &str) -> String {
    let normalized = normalize_record_cookie(cookies);
    let mut map = parse_cookie_map(&normalized);

    let did = map
        .get("did")
        .cloned()
        .or_else(|| map.get("_did").cloned())
        .unwrap_or_else(gen_web_did);
    map.entry("did".to_string()).or_insert_with(|| did.clone());
    map.entry("didv".to_string())
        .or_insert_with(|| Utc::now().timestamp_millis().to_string());
    map.entry("clientid".to_string())
        .or_insert_with(|| "3".to_string());
    map.entry("webid".to_string())
        .or_insert_with(|| did.clone());
    map.entry("kpn".to_string())
        .or_insert_with(|| KUAISHOU_DEFAULT_KPN.to_string());
    map.entry("kpf".to_string())
        .or_insert_with(|| KUAISHOU_DEFAULT_KPF.to_string());
    map.entry("kwpsecproductname".to_string())
        .or_insert_with(|| KUAISHOU_DEFAULT_PRODUCT_NAME.to_string());

    let has_kwfv1 = map.contains_key("kwfv1");
    let has_kwscode = map.contains_key("kwscode");
    let has_kwssectoken = map.contains_key("kwssectoken");
    if !has_kwfv1 || !has_kwscode || !has_kwssectoken {
        let room_url = format!("https://live.kuaishou.com/u/{principal_id}");
        let product_name = map
            .get("kwpsecproductname")
            .cloned()
            .unwrap_or_else(|| KUAISHOU_DEFAULT_PRODUCT_NAME.to_string());
        if let Some((kwfv1, kwssectoken, kwscode)) =
            generate_kuaishou_webweapon_values(&room_url, &did, &product_name)
        {
            map.entry("kwfv1".to_string()).or_insert(kwfv1);
            map.entry("kwssectoken".to_string())
                .or_insert(kwssectoken);
            map.entry("kwscode".to_string()).or_insert(kwscode);
        }
    }

    format_cookie_map(&map)
}

async fn fetch_follow_live_info(
    client: &Client,
    account: &Account,
    room_id: &str,
    author_id: &str,
    author_name: &str,
) -> Option<FollowLiveInfo> {
    if !web_api_allowed(account, room_id).await {
        return None;
    }
    let referer_principal = resolve_principal_id(room_id);
    let referer = if referer_principal.is_empty() {
        "https://live.kuaishou.com/".to_string()
    } else {
        format!("https://live.kuaishou.com/u/{referer_principal}")
    };

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().ok()?);
    headers.insert("Accept", "application/json, text/plain, */*".parse().ok()?);
    headers.insert("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8".parse().ok()?);
    headers.insert(
        "sec-ch-ua",
        "\"Not:A-Brand\";v=\"99\", \"Google Chrome\";v=\"145\", \"Chromium\";v=\"145\""
            .parse()
            .ok()?,
    );
    headers.insert("sec-ch-ua-mobile", "?0".parse().ok()?);
    headers.insert("sec-ch-ua-platform", "\"macOS\"".parse().ok()?);
    headers.insert("Sec-Fetch-Dest", "empty".parse().ok()?);
    headers.insert("Sec-Fetch-Mode", "cors".parse().ok()?);
    headers.insert("Sec-Fetch-Site", "same-origin".parse().ok()?);
    headers.insert("Referer", referer.parse().ok()?);
    headers.insert("Origin", "https://live.kuaishou.com".parse().ok()?);

    let cookie_header = ensure_kuaishou_request_cookie(&account.cookies);
    if !cookie_header.is_empty() {
        headers.insert("Cookie", cookie_header.parse().ok()?);
    }
    if let Some(kww) = resolve_kuaishou_kww(&cookie_header) {
        if let Ok(value) = kww.parse() {
            headers.insert("kww", value);
        }
    }

    wait_for_web_request_slot("follow_live_info").await;
    let response = client
        .get("https://live.kuaishou.com/live_api/baseuser/userFollowCount")
        .headers(headers)
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        log::debug!("[Kuaishou] userFollowCount status: {}", response.status());
        return None;
    }

    let body = response.text().await.ok()?;
    if let Some(msg) = extract_rate_limit_message_from_body(&body) {
        set_web_cooldown(account, room_id, &msg).await;
        return None;
    }
    let data: UserFollowCountResponse = serde_json::from_str(&body).ok()?;
    let follow_list = data.data?.follow;
    let stream_cookie = normalize_record_cookie(&account.cookies);

    for item in follow_list {
        if !follow_item_matches(&item, room_id, author_id, author_name) {
            continue;
        }
        let streams = parse_stream_infos_from_follow_item(&item, &stream_cookie);
        let user = item.user.as_ref();
        return Some(FollowLiveInfo {
            principal_id: user.and_then(|v| v.principal_id.clone()),
            caption: item.caption,
            cover_url: item.cover_url.or(item.rt_cover_url),
            user_name: user.and_then(|v| v.user_name.clone()),
            user_id: user.and_then(|v| v.user_id.clone()),
            user_avatar: user.and_then(|v| v.head_url.clone()),
            live: user.and_then(|v| v.live),
            streams,
        });
    }

    None
}

async fn fetch_livedetail_info(
    client: &Client,
    account: &Account,
    room_id: &str,
) -> Option<FollowLiveInfo> {
    let principal_id = resolve_principal_id(room_id);
    let value = fetch_livedetail_value(client, account, &principal_id)
        .await
        .ok()
        .flatten()?;
    let principal_hint = find_string_value(&value, &["principalId", "principal_id", "eid"])
        .and_then(|v| normalize_principal_candidate(&v))
        .unwrap_or_else(|| principal_id.clone());

    let caption = find_string_value(&value, &["caption", "title", "liveTitle", "streamTitle"]);
    let cover_url = find_image_url(&value, &["coverUrl", "cover", "poster", "snapshot"]);
    let user_info = find_user_info(&value);

    Some(FollowLiveInfo {
        principal_id: Some(principal_hint),
        caption,
        cover_url,
        user_name: user_info.as_ref().map(|u| u.user_name.clone()),
        user_id: user_info.as_ref().map(|u| u.user_id.clone()),
        user_avatar: user_info.as_ref().map(|u| u.user_avatar.clone()),
        live: None,
        streams: Vec::new(),
    })
}

async fn get_room_info_via_public_page(
    client: &Client,
    account: &Account,
    principal_id: &str,
) -> Result<Option<RoomInfo>, RecorderError> {
    if principal_id.trim().is_empty() {
        return Ok(None);
    }
    if !web_api_allowed(account, principal_id).await {
        return Ok(None);
    }

    let principal = resolve_principal_id(principal_id);
    let url = format!("https://live.kuaishou.com/u/{principal}");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());
    headers.insert(
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
            .parse()
            .unwrap(),
    );
    headers.insert("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap());
    headers.insert("Referer", "https://live.kuaishou.com/".parse().unwrap());
    headers.insert("Origin", "https://live.kuaishou.com".parse().unwrap());
    headers.insert("Sec-Fetch-Dest", "document".parse().unwrap());
    headers.insert("Sec-Fetch-Mode", "navigate".parse().unwrap());
    headers.insert("Sec-Fetch-Site", "none".parse().unwrap());
    headers.insert("Sec-Fetch-User", "?1".parse().unwrap());
    headers.insert("Upgrade-Insecure-Requests", "1".parse().unwrap());

    wait_for_web_request_slot("room_page_public").await;
    let response = client.get(&url).headers(headers).send().await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let html_str = response.text().await?;

    let rate_limit_msg = extract_rate_limit_message_from_body(&html_str);
    let stream_cookie = normalize_record_cookie(&account.cookies);
    let page_streams = extract_direct_stream_urls_from_html(&html_str, &stream_cookie);

    if let Some(json_str) = extract_initial_state(&html_str) {
        match parse_room_info_from_initial_state(&json_str, &principal, &stream_cookie) {
            Ok(Some(room)) => {
                let mut room = room;
                if !page_streams.is_empty() {
                    let mut seen: HashSet<String> =
                        room.streams.iter().map(|s| s.url.clone()).collect();
                    for stream in page_streams.iter().cloned() {
                        if seen.insert(stream.url.clone()) {
                            room.streams.push(stream);
                        }
                    }
                    if !room.streams.is_empty() {
                        sort_stream_infos(&mut room.streams);
                    }
                }
                if !room.streams.is_empty() {
                    log::debug!(
                        "[Kuaishou] public page fallback resolved streams: principalId={}, streams={}",
                        principal,
                        room.streams.len()
                    );
                }
                return Ok(Some(room));
            }
            Ok(None) => {}
            Err(err) => {
                if let RecorderError::ApiError { error } = &err {
                    if is_rate_limit_message(error) || is_captcha_message(error) {
                        set_web_cooldown(account, principal_id, error).await;
                    }
                }
                return Err(err);
            }
        }
    }

    if !page_streams.is_empty() {
        let (title, cover, avatar) = extract_metadata_from_html(&html_str);
        let user_name = title
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Kuaishou Live".to_string());
        let user_avatar = avatar.unwrap_or_default();
        let room_cover_url = cover.unwrap_or_else(|| user_avatar.clone());
        log::debug!(
            "[Kuaishou] public page fallback resolved direct streams: principalId={}, streams={}",
            principal,
            page_streams.len()
        );
        return Ok(Some(RoomInfo {
            live_status: true,
            room_title: user_name.clone(),
            room_cover_url,
            user_id: principal.clone(),
            user_name,
            user_avatar,
            streams: page_streams,
        }));
    }

    if let Some(hls_url) = extract_hls_play_url(&html_str) {
        let (title, cover, avatar) = extract_metadata_from_html(&html_str);
        let user_name = title
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Kuaishou Live".to_string());
        let user_avatar = avatar.unwrap_or_default();
        let room_cover_url = cover.unwrap_or_else(|| user_avatar.clone());
        log::debug!(
            "[Kuaishou] public page fallback resolved hlsPlayUrl directly: principalId={}",
            principal
        );
        return Ok(Some(RoomInfo {
            live_status: true,
            room_title: user_name.clone(),
            room_cover_url,
            user_id: principal.clone(),
            user_name,
            user_avatar,
            streams: vec![StreamInfo {
                url: hls_url,
                quality: "Blue".to_string(),
                bitrate: None,
                cookie: Some(stream_cookie),
            }],
        }));
    }

    if let Some(msg) = rate_limit_msg {
        set_web_cooldown(account, principal_id, &msg).await;
        return Err(RecorderError::ApiError { error: msg });
    }

    Ok(None)
}

fn resolve_principal_id(input: &str) -> String {
    let extracted = extract_user_id_from_url(input);
    if !extracted.is_empty() {
        return extracted;
    }
    let trimmed = input.trim();
    if let Some((prefix, suffix)) = trimmed.split_once('#') {
        if prefix.trim().eq_ignore_ascii_case("kuaishou") && !suffix.trim().is_empty() {
            return suffix.trim().to_string();
        }
    }
    trimmed
        .trim_start_matches('@')
        .trim_end_matches('/')
        .to_string()
}

fn normalize_principal_candidate(value: &str) -> Option<String> {
    let normalized = value.trim().trim_matches('/').trim_start_matches('@').to_string();
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("://")
        || normalized.contains('/')
        || normalized.contains('?')
        || normalized.contains('&')
        || normalized.contains('=')
        || normalized.eq_ignore_ascii_case("kuaishou")
    {
        return None;
    }
    Some(normalized)
}

fn principal_id_candidates(input: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |value: String| {
        let Some(normalized) = normalize_principal_candidate(&value) else {
            return;
        };
        if !candidates.iter().any(|item| item.eq_ignore_ascii_case(&normalized)) {
            candidates.push(normalized);
        }
    };

    let trimmed = input.trim().to_string();
    if let Some((prefix, suffix)) = trimmed.split_once('#') {
        if prefix.trim().eq_ignore_ascii_case("kuaishou") && !suffix.trim().is_empty() {
            push(suffix.trim().to_string());
        }
    }

    let extracted = extract_user_id_from_url(&trimmed);
    if !extracted.is_empty() {
        push(extracted);
    }
    push(resolve_principal_id(&trimmed));
    candidates
}

fn room_info_score(room: &RoomInfo) -> i64 {
    let mut score = 0;
    if room.live_status {
        score += 8;
    }
    if !room.streams.is_empty() {
        score += 16;
    }
    if !room.user_id.trim().is_empty() {
        score += 4;
    }
    if !room.user_name.trim().is_empty() {
        score += 2;
    }
    if !room.room_title.trim().is_empty() {
        score += 1;
    }
    score
}

fn log_room_info_source(source: &str, principal_id: &str, room: &RoomInfo) {
    log::info!(
        "[Kuaishou] stream source={} principalId={} live_status={} streams={} user_id={} title={}",
        source,
        principal_id,
        room.live_status,
        room.streams.len(),
        room.user_id,
        room.room_title
    );
}

fn log_stream_urls_source(source: &str, principal_id: &str, streams: &[StreamInfo]) {
    let first_url = streams
        .first()
        .map(|stream| stream.url.as_str())
        .unwrap_or_default();
    log::info!(
        "[Kuaishou] stream source={} principalId={} stream_count={} first_url={}",
        source,
        principal_id,
        streams.len(),
        first_url
    );
}

fn push_attempt_trace(traces: &mut Vec<String>, principal_id: &str, stage: &str, detail: String) {
    traces.push(format!("{principal_id}:{stage}={detail}"));
}

fn find_bool_value(value: &Value, keys: &[&str]) -> Option<bool> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key) {
                    match found {
                        Value::Bool(v) => return Some(*v),
                        Value::Number(v) => {
                            if let Some(n) = v.as_i64() {
                                return Some(n != 0);
                            }
                        }
                        Value::String(v) => {
                            let lower = v.trim().to_ascii_lowercase();
                            if matches!(lower.as_str(), "1" | "true" | "yes" | "on" | "live") {
                                return Some(true);
                            }
                            if matches!(lower.as_str(), "0" | "false" | "no" | "off") {
                                return Some(false);
                            }
                        }
                        _ => {}
                    }
                }
            }
            for child in map.values() {
                if let Some(found) = find_bool_value(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(values) => {
            for child in values {
                if let Some(found) = find_bool_value(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn build_livedetail_query_params(
    cookie: &str,
    principal_id: &str,
    kww_override: Option<&str>,
) -> Vec<(String, String)> {
    let did = get_cookie_value_ci(cookie, "did")
        .or_else(|| get_cookie_value_ci(cookie, "_did"))
        .unwrap_or_else(gen_web_did);
    let kpn = get_cookie_value_ci(cookie, "kpn").unwrap_or_else(|| "GAME_ZONE".to_string());
    let kpf = get_cookie_value_ci(cookie, "kpf").unwrap_or_else(|| "PC_WEB".to_string());

    let mut params = vec![
        ("principalId".to_string(), principal_id.to_string()),
        ("caver".to_string(), "2".to_string()),
        ("did".to_string(), did),
        ("kpn".to_string(), kpn),
        ("kpf".to_string(), kpf),
    ];

    for key in ["clientid", "webid", "kwscode", "kwfv1", "kwssectoken", "kww"] {
        if let Some(v) = get_cookie_value_ci(cookie, key) {
            params.push((key.to_string(), v));
        }
    }
    if params
        .iter()
        .all(|(k, v)| !k.eq_ignore_ascii_case("kww") || v.trim().is_empty())
    {
        if let Some(v) = kww_override {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                params.push(("kww".to_string(), trimmed.to_string()));
            }
        }
    }
    params
}

fn build_sorted_query_string(params: &[(String, String)]) -> String {
    let mut pairs: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    pairs.sort();
    pairs.join("&")
}

async fn fetch_livedetail_value(
    client: &Client,
    account: &Account,
    principal_id: &str,
) -> Result<Option<Value>, RecorderError> {
    if principal_id.trim().is_empty() {
        return Ok(None);
    }
    if !web_api_allowed(account, principal_id).await {
        return Ok(None);
    }

    let ctx = KuaishouRequestContext::from_account(account, principal_id);
    warm_kuaishou_gdfp(client, &ctx).await;
    let cookie_header = ctx.cookie_header.clone();

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());
    headers.insert("Accept", "application/json, text/plain, */*".parse().unwrap());
    headers.insert("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap());
    headers.insert(
        "sec-ch-ua",
        "\"Not:A-Brand\";v=\"99\", \"Google Chrome\";v=\"145\", \"Chromium\";v=\"145\""
            .parse()
            .unwrap(),
    );
    headers.insert("sec-ch-ua-mobile", "?0".parse().unwrap());
    headers.insert("sec-ch-ua-platform", "\"macOS\"".parse().unwrap());
    headers.insert("Sec-Fetch-Dest", "empty".parse().unwrap());
    headers.insert("Sec-Fetch-Mode", "cors".parse().unwrap());
    headers.insert("Sec-Fetch-Site", "same-origin".parse().unwrap());
    headers.insert("Referer", ctx.web_referer().parse().unwrap());
    headers.insert("Origin", "https://live.kuaishou.com".parse().unwrap());

    ctx.apply_cookie(&mut headers);
    let mut resolved_kww = ctx.kww.clone();
    if resolved_kww.is_none() && use_room_page_kww_probe() {
        let probe_room = resolve_principal_id(principal_id);
        if let Some(cached) = get_cached_kww_probe(&probe_room).await {
            resolved_kww = cached;
        } else {
            let probe_url = format!("https://live.kuaishou.com/u/{probe_room}");
            if let Ok((html, _)) = fetch_web_html(client, account, &probe_url).await {
                resolved_kww = extract_kuaishou_kww_from_html(&html);
                if resolved_kww.is_some() {
                    log::info!("[Kuaishou] Extracted kww from room page HTML");
                }
            }
            set_cached_kww_probe(probe_room, resolved_kww.clone()).await;
        }
    }
    if let Some(kww) = resolved_kww.as_deref() {
        if let Ok(value) = kww.parse() {
            headers.insert("kww", value);
        }
    }

    let mut best_value: Option<Value> = None;
    let mut best_score: i64 = i64::MIN;

    for principal in principal_id_candidates(principal_id) {
        let params_full =
            build_livedetail_query_params(&cookie_header, &principal, resolved_kww.as_deref());
        let params_basic = vec![("principalId".to_string(), principal.clone())];
        let param_candidates = [params_basic, params_full];

        for params in param_candidates {
            let did = params
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("did"))
                .map(|(_, v)| v.clone())
                .unwrap_or_else(gen_web_did);
            let signer = crate::reverse_generate::kuaishou_sign::KuaishouSign::new(&did);
            let sign_inputs = [
                format!("principalId={principal}"),
                build_sorted_query_string(&params),
            ];

            // Try unsigned first, then signed variants.
            for attempt in 0..=sign_inputs.len() {
                let mut req = client
                    .get("https://live.kuaishou.com/live_api/liveroom/livedetail")
                    .query(&params)
                    .headers(headers.clone());
                let mut sign_note = "none".to_string();

                if attempt > 0 {
                    let sign_input = &sign_inputs[attempt - 1];
                    if !sign_input.trim().is_empty() {
                        let sign = signer.generate_sign(sign_input);
                        req = req.query(&[("sign", sign.as_str())]);
                        sign_note = sign_input.clone();
                    }
                }

                wait_for_web_request_slot("live_detail").await;
                let response = req.send().await?;
                let status = response.status();
                let body = response.text().await?;

                if let Some(msg) = extract_rate_limit_message_from_body(&body) {
                    set_web_cooldown(account, principal_id, &msg).await;
                    return Err(RecorderError::ApiError { error: msg });
                }

                log::debug!(
                    "[Kuaishou] livedetail attempt status={}, sign_input={}, principalId={}",
                    status,
                    sign_note,
                    principal
                );

                if !status.is_success() {
                    continue;
                }

                if is_captcha_message(&body) || body.to_ascii_lowercase().contains("captcha") {
                    return Err(captcha_error_with_context(principal_id).await);
                }

                let Ok(value) = serde_json::from_str::<Value>(&body) else {
                    continue;
                };

                if let Ok(Some(room)) =
                    parse_room_info_from_livedetail_value(&value, &principal, &cookie_header)
                {
                    if room.live_status && !room.streams.is_empty() {
                        return Ok(Some(value));
                    }
                }

                let score = score_livedetail_value(&value, &principal, &cookie_header);
                if score > best_score {
                    best_score = score;
                    best_value = Some(value);
                }
            }
        }
    }

    Ok(best_value)
}

const KUAISHOU_MGRAPHQL_URL: &str = "https://live.kuaishou.com/m_graphql";
const KUAISHOU_MOBILE_USER_AGENT: &str =
    "ios/7.830 (ios 17.0; ; iPhone 15 (A2846/A3089/A3090/A3092))";
const KUAISHOU_WEBLIVEDETAIL_QUERY: &str = r#"
query LiveDetail($principalId: String) {
  webLiveDetail(principalId: $principalId) {
    author {
      id
      name
      headUrl
    }
    liveStream {
      caption
      coverUrl
      playUrls {
        h264 {
          adaptationSet {
            representation {
              url
              name
              qualityType
              bitrate
            }
          }
        }
        h265 {
          adaptationSet {
            representation {
              url
              name
              qualityType
              bitrate
            }
          }
        }
      }
    }
  }
}
"#;

async fn fetch_mgraphql_live_detail_value(
    client: &Client,
    account: &Account,
    principal_id: &str,
) -> Result<Option<Value>, RecorderError> {
    if principal_id.trim().is_empty() {
        return Ok(None);
    }
    if !web_api_allowed(account, principal_id).await {
        return Ok(None);
    }

    let ctx = KuaishouRequestContext::from_account(account, principal_id);
    warm_kuaishou_gdfp(client, &ctx).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());
    headers.insert("Accept", "application/json, text/plain, */*".parse().unwrap());
    headers.insert("Content-Type", "application/json".parse().unwrap());
    headers.insert("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap());
    headers.insert("Origin", "https://live.kuaishou.com".parse().unwrap());
    headers.insert("Referer", ctx.web_referer().parse().unwrap());
    headers.insert("Sec-Fetch-Dest", "empty".parse().unwrap());
    headers.insert("Sec-Fetch-Mode", "cors".parse().unwrap());
    headers.insert("Sec-Fetch-Site", "same-origin".parse().unwrap());
    ctx.apply_cookie(&mut headers);
    ctx.apply_kww(&mut headers);

    let payload = json!({
        "operationName": "LiveDetail",
        "variables": { "principalId": principal_id },
        "query": KUAISHOU_WEBLIVEDETAIL_QUERY,
    });

    wait_for_web_request_slot("m_graphql").await;
    let response = client
        .post(KUAISHOU_MGRAPHQL_URL)
        .headers(headers)
        .json(&payload)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if let Some(msg) = extract_rate_limit_message_from_body(&body) {
        set_web_cooldown(account, principal_id, &msg).await;
        return Err(RecorderError::ApiError { error: msg });
    }
    if !status.is_success() {
        log::debug!(
            "[Kuaishou] m_graphql status={}, principalId={}, body_preview={}",
            status,
            principal_id,
            preview_log_body(&body)
        );
        return Ok(None);
    }
    if is_captcha_message(&body) || body.to_ascii_lowercase().contains("captcha") {
        return Err(captcha_error_with_context(principal_id).await);
    }

    log::debug!(
        "[Kuaishou] m_graphql status={}, principalId={}, body_preview={}",
        status,
        principal_id,
        preview_log_body(&body)
    );

    let value = serde_json::from_str::<Value>(&body).map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse m_graphql response: {e}"),
    })?;
    log_mgraphql_value(principal_id, &value);
    Ok(Some(value))
}

async fn fetch_by_user_live_value(
    client: &Client,
    account: &Account,
    principal_id: &str,
) -> Result<Option<Value>, RecorderError> {
    if principal_id.trim().is_empty() {
        return Ok(None);
    }
    if !web_api_allowed(account, principal_id).await {
        return Ok(None);
    }

    let ctx = KuaishouRequestContext::from_account(account, principal_id);
    warm_kuaishou_gdfp(client, &ctx).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", KUAISHOU_MOBILE_USER_AGENT.parse().unwrap());
    headers.insert(
        "Accept-Language",
        "zh-CN,zh;q=0.8,zh-TW;q=0.7,zh-HK;q=0.5,en-US;q=0.3,en;q=0.2"
            .parse()
            .unwrap(),
    );
    headers.insert("Referer", ctx.mobile_referer().parse().unwrap());
    headers.insert("Content-Type", "application/json".parse().unwrap());
    ctx.apply_cookie(&mut headers);
    ctx.apply_kww(&mut headers);

    let payload = ctx.by_user_payload();

    wait_for_web_request_slot("by_user").await;
    let response = client
        .post(ctx.by_user_url())
        .headers(headers)
        .json(&payload)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    log::debug!(
        "[Kuaishou] byUser status={}, principalId={}, body_preview={}",
        status,
        principal_id,
        preview_log_body(&body)
    );

    if !status.is_success() {
        return Ok(None);
    }

    let value = serde_json::from_str::<Value>(&body).map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse byUser response: {e}"),
    })?;

    let result_code = value.get("result").and_then(|v| v.as_i64());
    if let Some(result) = result_code {
        if result != 1 {
            let error = value
                .get("error_msg")
                .or_else(|| value.get("errorMsg"))
                .or_else(|| value.get("message"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Kuaishou byUser returned result={result}"));
            if is_rate_limit_message(&error) || is_captcha_message(&error) {
                set_web_cooldown(account, principal_id, &error).await;
            }
            return Err(RecorderError::ApiError { error });
        }
    }

    Ok(Some(value))
}

fn preview_log_body(body: &str) -> String {
    const MAX_CHARS: usize = 160;
    let mut preview = body.replace('\n', " ").replace('\r', " ");
    preview.truncate(preview.char_indices().nth(MAX_CHARS).map(|(idx, _)| idx).unwrap_or(preview.len()));
    preview
}

fn log_mgraphql_value(principal_id: &str, value: &Value) {
    let errors = value
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    let detail = value.get("data").and_then(|data| data.get("webLiveDetail"));
    let live_stream = detail.and_then(|detail| detail.get("liveStream"));
    let author = detail.and_then(|detail| detail.get("author"));

    log::debug!(
        "[Kuaishou] m_graphql parsed principalId={} errors={} detail_type={} live_stream_type={} author_type={} live_stream_summary={} author_summary={}",
        principal_id,
        errors,
        value_type_label(detail),
        value_type_label(live_stream),
        value_type_label(author),
        summarize_mgraphql_live_stream(live_stream),
        summarize_author(author)
    );

    if errors > 0 {
        log::debug!(
            "[Kuaishou] m_graphql errors principalId={} errors_preview={}",
            principal_id,
            preview_log_body(&value.get("errors").unwrap_or(&Value::Null).to_string())
        );
    }
}

fn value_type_label(value: Option<&Value>) -> &'static str {
    match value {
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
        None => "missing",
    }
}

fn summarize_author(author: Option<&Value>) -> String {
    match author {
        Some(Value::Object(map)) => {
            let keys = map.keys().take(8).cloned().collect::<Vec<_>>().join(",");
            let id = map
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let name = map
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            format!("keys=[{keys}] id={id} name={name}")
        }
        Some(Value::String(text)) => format!("string_len={}", text.len()),
        Some(other) => other.to_string(),
        None => "missing".to_string(),
    }
}

fn summarize_mgraphql_live_stream(live_stream: Option<&Value>) -> String {
    match live_stream {
        Some(Value::Object(map)) => {
            let keys = map.keys().take(12).cloned().collect::<Vec<_>>().join(",");
            let caption = map
                .get("caption")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let has_play_urls = map.get("playUrls").is_some();
            let has_hls = map.get("hlsPlayUrl").is_some();
            let has_multi = map.get("multiResolutionPlayUrls").is_some();
            format!(
                "keys=[{keys}] caption={caption} playUrls={has_play_urls} hlsPlayUrl={has_hls} multiResolutionPlayUrls={has_multi}"
            )
        }
        Some(Value::String(text)) => {
            let parsed = serde_json::from_str::<Value>(text).ok();
            match parsed.as_ref() {
                Some(Value::Object(map)) => {
                    let keys = map.keys().take(12).cloned().collect::<Vec<_>>().join(",");
                    format!("string_json keys=[{keys}] len={}", text.len())
                }
                Some(other) => format!("string_json type={} len={}", value_type_label(Some(other)), text.len()),
                None => format!("string_len={}", text.len()),
            }
        }
        Some(Value::Array(items)) => format!("array_len={}", items.len()),
        Some(other) => other.to_string(),
        None => "missing".to_string(),
    }
}

fn score_livedetail_value(value: &Value, principal_id: &str, cookies: &str) -> i64 {
    let mut score = 0;

    if find_live_stream_response(value).is_some() {
        score += 1;
    }

    if let Ok(Some(room)) = parse_room_info_from_livedetail_value(value, principal_id, cookies) {
        if room.live_status {
            score += 1_000;
        }
        if !room.streams.is_empty() {
            score += 100_000 + room.streams.len() as i64 * 10;
        }
        if !is_placeholder_user_name(&room.user_name) {
            score += 100;
        }
        if !room.room_title.trim().is_empty() && room.room_title != room.user_name {
            score += 50;
        }
    }

    score
}
fn parse_room_info_from_livedetail_value(
    value: &Value,
    principal_id: &str,
    cookies: &str,
) -> Result<Option<RoomInfo>, RecorderError> {
    let live_data = find_live_stream_response(value);
    if let Some(error) = live_data
        .as_ref()
        .and_then(|data| data.error_type.as_ref())
    {
        return Err(RecorderError::ApiError {
            error: format!("{}: {}", error.title, error.content),
        });
    }

    let fallback_hls = find_string_value(value, &["hlsPlayUrl", "hls_play_url"])
        .filter(|url| url.contains(".m3u8"))
        .map(|url| StreamInfo {
            url,
            quality: "Blue".to_string(),
            bitrate: None,
            cookie: Some(cookies.to_string()),
        });

    let live_stream = live_data.as_ref().and_then(|data| data.live_stream.clone());
    let mut streams = if let Some(stream) = live_stream.clone() {
        parse_stream_infos_from_live_stream(stream, fallback_hls.clone(), cookies)
            .unwrap_or_else(|_| fallback_hls.clone().into_iter().collect())
    } else {
        fallback_hls.clone().into_iter().collect()
    };

    // livedetail schema varies (array playUrls / multiResolutionPlayUrls),
    // recursively parse follow-like nodes as fallback.
    let parsed_from_value = parse_stream_infos_from_livedetail_value(value, cookies);
    if !parsed_from_value.is_empty() {
        let mut seen = streams
            .iter()
            .map(|stream| stream.url.clone())
            .collect::<std::collections::HashSet<_>>();
        for stream in parsed_from_value {
            if seen.insert(stream.url.clone()) {
                streams.push(stream);
            }
        }
        sort_stream_infos(&mut streams);
    }

    let author = live_data
        .as_ref()
        .and_then(|data| data.author.clone())
        .unwrap_or_default();
    let user_info = find_user_info(value);

    let user_name = if !author.name.trim().is_empty() {
        author.name.trim().to_string()
    } else {
        user_info
            .as_ref()
            .map(|u| u.user_name.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Kuaishou Live".to_string())
    };
    let user_id = if !author.id.trim().is_empty() {
        author.id.trim().to_string()
    } else {
        user_info
            .as_ref()
            .map(|u| u.user_id.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| principal_id.to_string())
    };
    let user_avatar = author
        .head_url
        .as_ref()
        .map(|url| normalize_image_url(url))
        .filter(|url| !url.is_empty())
        .or_else(|| {
            user_info
                .as_ref()
                .map(|u| normalize_image_url(&u.user_avatar))
                .filter(|url| !url.is_empty())
        })
        .unwrap_or_default();

    let title = live_stream
        .as_ref()
        .and_then(|stream| stream.caption.clone())
        .or_else(|| {
            live_data
                .as_ref()
                .and_then(|data| data.config.as_ref().and_then(|c| c.caption.clone()))
        })
        .or_else(|| find_string_value(value, &["caption", "title", "liveTitle", "streamTitle"]))
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| user_name.clone());

    let room_cover_url = live_stream
        .as_ref()
        .and_then(|stream| stream.cover_url.clone())
        .map(|url| normalize_image_url(&url))
        .filter(|url| !url.is_empty())
        .or_else(|| find_image_url(value, &["coverUrl", "cover", "poster", "snapshot"]))
        .unwrap_or_else(|| user_avatar.clone());

    let live_status = if !streams.is_empty() {
        true
    } else {
        find_bool_value(
            value,
            &["living", "isLiving", "liveStatus", "live_status", "isLive"],
        )
        .unwrap_or(false)
    };

    if streams.is_empty() && title.trim().is_empty() && user_id.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(RoomInfo {
        live_status,
        room_title: title,
        room_cover_url,
        user_id,
        user_name,
        user_avatar,
        streams,
    }))
}

async fn get_room_info_via_livedetail(
    client: &Client,
    account: &Account,
    principal_id: &str,
) -> Result<Option<RoomInfo>, RecorderError> {
    let Some(value) = fetch_livedetail_value(client, account, principal_id).await? else {
        return Ok(None);
    };
    parse_room_info_from_livedetail_value(&value, principal_id, &account.cookies)
}

async fn get_room_info_via_mgraphql(
    client: &Client,
    account: &Account,
    principal_id: &str,
) -> Result<Option<RoomInfo>, RecorderError> {
    let Some(value) = fetch_mgraphql_live_detail_value(client, account, principal_id).await? else {
        return Ok(None);
    };
    for (idx, candidate) in extract_mgraphql_candidate_values(&value).into_iter().enumerate() {
        log::debug!(
            "[Kuaishou] m_graphql candidate principalId={} idx={} type={} summary={}",
            principal_id,
            idx,
            value_type_label(Some(&candidate)),
            summarize_mgraphql_candidate(&candidate)
        );
        if let Some(room) =
            parse_room_info_from_livedetail_value(&candidate, principal_id, &account.cookies)?
        {
            if room.live_status || !room.streams.is_empty() {
                return Ok(Some(room));
            }
        }
    }
    parse_room_info_from_livedetail_value(&value, principal_id, &account.cookies)
}

async fn get_room_info_via_by_user(
    client: &Client,
    account: &Account,
    principal_id: &str,
) -> Result<Option<RoomInfo>, RecorderError> {
    let Some(value) = fetch_by_user_live_value(client, account, principal_id).await? else {
        return Ok(None);
    };
    parse_room_info_from_livedetail_value(&value, principal_id, &account.cookies)
}

fn summarize_mgraphql_candidate(candidate: &Value) -> String {
    match candidate {
        Value::Object(map) => {
            let keys = map.keys().take(12).cloned().collect::<Vec<_>>().join(",");
            let has_author = map.get("author").is_some();
            let has_live_stream = map.get("liveStream").is_some();
            let has_play_urls = map.get("playUrls").is_some();
            let has_hls = map.get("hlsPlayUrl").is_some();
            let has_multi = map.get("multiResolutionPlayUrls").is_some();
            format!(
                "keys=[{keys}] author={has_author} liveStream={has_live_stream} playUrls={has_play_urls} hlsPlayUrl={has_hls} multiResolutionPlayUrls={has_multi}"
            )
        }
        Value::String(text) => format!("string_len={}", text.len()),
        Value::Array(items) => format!("array_len={}", items.len()),
        _ => candidate.to_string(),
    }
}

fn extract_mgraphql_candidate_values(value: &Value) -> Vec<Value> {
    let mut candidates = Vec::new();

    let mut push_unique = |candidate: Value| {
        if !candidates.iter().any(|existing| existing == &candidate) {
            candidates.push(candidate);
        }
    };

    push_unique(value.clone());

    if let Some(data) = value.get("data") {
        push_unique(data.clone());
        if let Some(detail) = data.get("webLiveDetail") {
            push_unique(detail.clone());
            if let Some(author) = detail.get("author") {
                let wrapped = json!({
                    "author": author,
                    "liveStream": detail.get("liveStream").cloned().unwrap_or(Value::Null),
                });
                push_unique(wrapped);
            }
            if let Some(stream) = detail.get("liveStream") {
                push_unique(stream.clone());
                if let Value::String(text) = stream {
                    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                        push_unique(parsed.clone());
                        if let Some(author) = detail.get("author") {
                            push_unique(json!({
                                "author": author,
                                "liveStream": parsed,
                            }));
                        }
                    }
                }
            }
        }
    }

    candidates
}

fn extract_image_url(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Object(map) => {
            if let Some(url) = map.get("url").and_then(|v| v.as_str()) {
                Some(url.to_string())
            } else if let Some(list) = map
                .get("urlList")
                .or_else(|| map.get("url_list"))
                .or_else(|| map.get("urls"))
                .and_then(|v| v.as_array())
            {
                list.first().and_then(|v| v.as_str()).map(|s| s.to_string())
            } else {
                None
            }
        }
        Value::Array(list) => list.first().and_then(|v| v.as_str()).map(|s| s.to_string()),
        _ => None,
    }
}

fn find_image_url(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key) {
                    if let Some(url) = extract_image_url(value) {
                        return Some(normalize_image_url(&url));
                    }
                }
            }

            for (key, value) in map {
                if key.to_ascii_lowercase().contains("cover")
                    || key.to_ascii_lowercase().contains("avatar")
                {
                    if let Some(url) = extract_image_url(value) {
                        return Some(normalize_image_url(&url));
                    }
                }
            }

            for child in map.values() {
                if let Some(url) = find_image_url(child, keys) {
                    return Some(url);
                }
            }

            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(url) = find_image_url(value, keys) {
                    return Some(url);
                }
            }
            None
        }
        _ => None,
    }
}

fn quality_rank(label: &str) -> i64 {
    let lower = label.trim().to_ascii_lowercase();

    if lower.contains("4k") || lower.contains("2160") || lower.contains("uhd") {
        return 20000;
    }
    if lower.contains("2k") || lower.contains("1440") || lower.contains("qhd") {
        return 15000;
    }

    if lower.contains("original")
        || lower.contains("source")
        || lower.contains("\u{539f}\u{753b}")
    {
        return 30000;
    }

    if lower.contains("\u{84dd}\u{5149}") || lower.contains("blue") {
        if lower.contains("8m") {
            return 8500;
        }
        if lower.contains("4m") {
            return 4500;
        }
        return 4200;
    }

    if lower.contains("1080") || lower.contains("fhd") {
        return 4000;
    }
    if lower.contains("\u{8d85}\u{6e05}") {
        return 2500;
    }
    if lower.contains("720") || lower.contains("hd") {
        return 2000;
    }
    if lower.contains("\u{9ad8}\u{6e05}") {
        return 1500;
    }
    if lower.contains("540") {
        return 1200;
    }
    if lower.contains("480") || lower.contains("sd") || lower.contains("\u{6807}\u{6e05}") {
        return 1000;
    }
    if lower.contains("360") || lower.contains("ld") || lower.contains("\u{6d41}\u{7545}") {
        return 600;
    }

    let mut digits = String::new();
    for ch in lower.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            break;
        }
    }
    if let Ok(val) = digits.parse::<i64>() {
        if val > 100 {
            return val;
        }
    }

    0
}
/// QR code information for login
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QrInfo {
    pub qr_login_token: String,
    pub qr_login_signature: String,
    pub image_data: String,
    pub qr_cookie: String,
}

#[derive(Clone, Debug)]
pub struct RoomInfo {
    pub live_status: bool,
    pub room_title: String,
    pub room_cover_url: String,
    pub user_id: String,
    pub user_name: String,
    pub user_avatar: String,
    pub streams: Vec<StreamInfo>,
}

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub url: String,
    pub quality: String,
    pub bitrate: Option<i64>,
    pub cookie: Option<String>,
}

impl PlatformStreamInfo for StreamInfo {
    fn primary_variant(&self) -> Result<StreamVariant, RecorderError> {
        let quality = match self.quality.to_ascii_lowercase().as_str() {
            "4k" | "blueray4k" => Quality::BluRay4K,
            "blueray" | "bluray" | "origin" | "original" => Quality::Origin,
            "uhd" => Quality::UltraHD,
            "hd" => Quality::HD,
            "sd" => Quality::SD,
            "smooth" | "ld" => Quality::Smooth,
            _ => Quality::Origin,
        };
        let format = if self.url.starts_with("rtmp://") || self.url.starts_with("rtmps://") {
            Format::RTMP
        } else if self.url.contains(".flv") {
            Format::FLV
        } else {
            Format::HLS
        };

        Ok(StreamVariant {
            url: self.url.clone(),
            format,
            codec: Codec::AVC,
            quality,
            bitrate: self.bitrate.map(|b| b as u64),
        })
    }

    fn all_variants(&self) -> Vec<StreamVariant> {
        self.primary_variant().into_iter().collect()
    }

    fn expires_at(&self) -> Option<i64> {
        None
    }

    fn cdn_nodes(&self) -> Vec<CdnNode> {
        Vec::new()
    }

    fn platform(&self) -> PlatformType {
        PlatformType::Kuaishou
    }
}

fn normalize_stream_url(url: &str) -> Option<String> {
    let mut normalized = url.trim().to_string();
    if normalized.is_empty() {
        return None;
    }

    // Some Kuaishou responses occasionally return `sidc=xxxtsc=origin` without '&'.
    if normalized.contains("sidc=") && normalized.contains("tsc=") && !normalized.contains("&tsc=")
    {
        static SIDC_TSC_FIX_RE: OnceLock<Regex> = OnceLock::new();
        let re = SIDC_TSC_FIX_RE.get_or_init(|| Regex::new(r"(sidc=[^&#]+)tsc=").unwrap());
        normalized = re.replace(&normalized, "$1&tsc=").to_string();
    }

    while normalized.contains("&&") {
        normalized = normalized.replace("&&", "&");
    }
    Some(normalized)
}

fn sort_stream_infos(streams: &mut [StreamInfo]) {
    streams.sort_by(|a, b| {
        let a_m3u8 = a.url.contains(".m3u8");
        let b_m3u8 = b.url.contains(".m3u8");
        b_m3u8
            .cmp(&a_m3u8)
            .then_with(|| b.bitrate.unwrap_or(0).cmp(&a.bitrate.unwrap_or(0)))
            .then_with(|| quality_rank(&b.quality).cmp(&quality_rank(&a.quality)))
    });
}

fn push_stream_info_unique(
    urls: &mut Vec<StreamInfo>,
    seen: &mut std::collections::HashSet<String>,
    raw_url: &str,
    quality: String,
    bitrate: Option<i64>,
    cookie: &str,
) {
    let Some(url) = normalize_stream_url(raw_url) else {
        return;
    };
    if !seen.insert(url.clone()) {
        return;
    }
    urls.push(StreamInfo {
        url,
        quality,
        bitrate,
        cookie: Some(cookie.to_string()),
    });
}

fn parse_stream_infos_from_follow_item(item: &UserFollowLive, cookie: &str) -> Vec<StreamInfo> {
    let mut urls: Vec<StreamInfo> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(hls) = item.hls_play_url.as_ref() {
        push_stream_info_unique(
            &mut urls,
            &mut seen,
            hls,
            "Blue".to_string(),
            None,
            cookie,
        );
    }

    for level in &item.multi_resolution_play_urls {
        let quality = level
            .name
            .as_deref()
            .or(level.short_name.as_deref())
            .unwrap_or_default()
            .trim()
            .to_string();
        for play in &level.urls {
            push_stream_info_unique(
                &mut urls,
                &mut seen,
                &play.url,
                quality.clone(),
                play.bitrate,
                cookie,
            );
        }
    }

    for play in &item.play_urls {
        let quality = play
            .bitrate
            .map(|b| format!("{b}kbps"))
            .unwrap_or_else(|| "Unknown".to_string());
        push_stream_info_unique(
            &mut urls,
            &mut seen,
            &play.url,
            quality,
            play.bitrate,
            cookie,
        );
    }

    sort_stream_infos(&mut urls);
    urls
}

fn collect_stream_infos_from_livedetail_value(
    value: &Value,
    cookie: &str,
    urls: &mut Vec<StreamInfo>,
    seen: &mut std::collections::HashSet<String>,
) {
    match value {
        Value::Object(map) => {
            if map.contains_key("hlsPlayUrl")
                || map.contains_key("playUrls")
                || map.contains_key("multiResolutionPlayUrls")
                || map.contains_key("multiResolutionHlsPlayUrls")
            {
                if let Ok(item) = serde_json::from_value::<UserFollowLive>(Value::Object(map.clone()))
                {
                    for stream in parse_stream_infos_from_follow_item(&item, cookie) {
                        if seen.insert(stream.url.clone()) {
                            urls.push(stream);
                        }
                    }
                } else if let Some(hls) = map.get("hlsPlayUrl").and_then(|v| v.as_str()) {
                    push_stream_info_unique(
                        urls,
                        seen,
                        hls,
                        "Blue".to_string(),
                        None,
                        cookie,
                    );
                }
            }

            if let Some(levels) = map
                .get("multiResolutionHlsPlayUrls")
                .and_then(|v| v.as_array())
            {
                for level in levels {
                    let quality = level
                        .get("name")
                        .or_else(|| level.get("shortName"))
                        .or_else(|| level.get("level"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Blue")
                        .trim()
                        .to_string();
                    if let Some(entries) = level.get("urls").and_then(|v| v.as_array()) {
                        for entry in entries {
                            if let Some(raw_url) = entry.as_str() {
                                push_stream_info_unique(
                                    urls,
                                    seen,
                                    raw_url,
                                    quality.clone(),
                                    None,
                                    cookie,
                                );
                            } else if let Some(raw_url) =
                                entry.get("url").and_then(|v| v.as_str())
                            {
                                let bitrate = entry.get("bitrate").and_then(|v| v.as_i64());
                                push_stream_info_unique(
                                    urls,
                                    seen,
                                    raw_url,
                                    quality.clone(),
                                    bitrate,
                                    cookie,
                                );
                            }
                        }
                    }
                }
            }
            for child in map.values() {
                collect_stream_infos_from_livedetail_value(child, cookie, urls, seen);
            }
        }
        Value::Array(list) => {
            for child in list {
                collect_stream_infos_from_livedetail_value(child, cookie, urls, seen);
            }
        }
        Value::String(text) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                collect_stream_infos_from_livedetail_value(&parsed, cookie, urls, seen);
            }
        }
        _ => {}
    }
}

fn parse_stream_infos_from_livedetail_value(value: &Value, cookie: &str) -> Vec<StreamInfo> {
    let mut urls = Vec::new();
    let mut seen = std::collections::HashSet::new();
    collect_stream_infos_from_livedetail_value(value, cookie, &mut urls, &mut seen);
    sort_stream_infos(&mut urls);
    urls
}

fn parse_stream_infos_from_live_stream(
    live_stream: LiveStream,
    fallback_hls: Option<StreamInfo>,
    cookie: &str,
) -> Result<Vec<StreamInfo>, RecorderError> {
    let mut urls = Vec::new();
    let mut all_representations = Vec::new();

    if let Some(play_urls) = live_stream.play_urls {
        if let Some(h264) = play_urls.h264 {
            if let Some(set) = h264.adaptation_set {
                all_representations.extend(set.representation);
            }
        }

        if let Some(h265) = play_urls.h265 {
            if let Some(set) = h265.adaptation_set {
                all_representations.extend(set.representation);
            }
        }
    }

    if all_representations.is_empty() {
        if let Some(fallback) = fallback_hls {
            return Ok(vec![fallback]);
        }
        return Err(RecorderError::ApiError {
            error: "No usable stream representations found".to_string(),
        });
    }

    all_representations.sort_by(|a, b| b.bitrate.unwrap_or(0).cmp(&a.bitrate.unwrap_or(0)));

    let mut seen_urls = std::collections::HashSet::new();
    urls.extend(all_representations.into_iter().filter_map(|rep| {
        let normalized = normalize_stream_url(&rep.url)?;
        if seen_urls.contains(&normalized) {
            return None;
        }
        seen_urls.insert(normalized.clone());
        Some(StreamInfo {
            url: normalized,
            quality: rep.name.or(rep.quality_type).unwrap_or_default(),
            bitrate: rep.bitrate,
            cookie: Some(cookie.to_string()),
        })
    }));

    if !urls.iter().any(|stream| stream.url.contains(".m3u8")) {
        if let Some(fallback) = fallback_hls.clone() {
            urls.insert(0, fallback);
        }
    }

    sort_stream_infos(&mut urls);

    if !urls.iter().any(|stream| stream.url.contains(".m3u8")) {
        if let Some(flv_url) = urls
            .iter()
            .find(|stream| stream.url.contains(".flv"))
            .map(|stream| stream.url.clone())
        {
            let guessed_hls = flv_url.replacen(".flv", ".m3u8", 1);
            if guessed_hls != flv_url {
                log::info!("[Kuaishou] No m3u8 found, guessing HLS from FLV URL");
                if let Some(url) = normalize_stream_url(&guessed_hls) {
                    urls.insert(
                        0,
                        StreamInfo {
                            url,
                            quality: "Blue".to_string(),
                            bitrate: None,
                            cookie: Some(cookie.to_string()),
                        },
                    );
                }
            }
        }
    }

    Ok(urls)
}

/// QR code status for polling
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QrStatus {
    pub code: u8,
    pub cookies: String,
    pub message: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub user_avatar: Option<String>,
}

fn gen_web_did() -> String {
    let mut rng = rand::rng();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        hex.push_str(&format!("{:02x}", byte));
    }
    format!("web_{hex}")
}

fn extract_qr_message(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            let keys = [
                "error_msg",
                "errorMsg",
                "errMsg",
                "message",
                "msg",
                "prompt",
            ];
            for key in keys {
                if let Some(Value::String(msg)) = map.get(key) {
                    let trimmed = msg.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
            for child in map.values() {
                if let Some(msg) = extract_qr_message(child) {
                    return Some(msg);
                }
            }
            None
        }
        Value::Array(values) => {
            for child in values {
                if let Some(msg) = extract_qr_message(child) {
                    return Some(msg);
                }
            }
            None
        }
        _ => None,
    }
}

fn has_captcha_signal(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                let key_lower = key.to_ascii_lowercase();
                if key_lower.contains("captcha")
                    || key_lower.contains("verify")
                    || key_lower.contains("risk")
                    || key_lower.contains("slider")
                {
                    return true;
                }
                if let Value::String(text) = val {
                    let lower = text.to_ascii_lowercase();
                    if lower.contains("captcha")
                        || lower.contains("verify")
                        || lower.contains("slider")
                        || text.contains("\u{6ed1}\u{5757}")
                    {
                        return true;
                    }
                }
            }
            for child in map.values() {
                if has_captcha_signal(child) {
                    return true;
                }
            }
            false
        }
        Value::Array(values) => values.iter().any(has_captcha_signal),
        _ => false,
    }
}

fn extract_qr_scan_user(value: &Value) -> (Option<String>, Option<String>, Option<String>) {
    let user = value.get("user");
    let user_id = user
        .and_then(|v| v.get("user_id").or_else(|| v.get("userId")))
        .and_then(|v| {
            v.as_i64()
                .map(|n| n.to_string())
                .or_else(|| v.as_str().map(|s| s.to_string()))
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let user_name = user
        .and_then(|v| {
            v.get("user_name")
                .or_else(|| v.get("userName"))
                .or_else(|| v.get("name"))
        })
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let user_avatar = user
        .and_then(|v| {
            v.get("headurl")
                .or_else(|| v.get("headUrl"))
                .or_else(|| v.get("avatar"))
        })
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    (user_id, user_name, user_avatar)
}

fn room_info_from_follow_info(candidate: &str, info: FollowLiveInfo) -> RoomInfo {
    let user_name = info
        .user_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "Kuaishou Live".to_string());
    let user_id = info
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| {
            info.principal_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| candidate.to_string());
    let user_avatar = info.user_avatar.unwrap_or_default();
    let room_title = info
        .caption
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| user_name.clone());
    let room_cover_url = info
        .cover_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| user_avatar.clone());
    let live_status = !info.streams.is_empty() || info.live.unwrap_or(false);

    RoomInfo {
        live_status,
        room_title,
        room_cover_url,
        user_id,
        user_name,
        user_avatar,
        streams: info.streams,
    }
}

fn is_placeholder_user_name(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.is_empty() || lower == "kuaishou live" || lower == "kuaishou"
}

fn push_follow_lookup_candidate(
    lookups: &mut Vec<(String, String, String)>,
    seen: &mut HashSet<String>,
    room_id: &str,
    author_id: &str,
    author_name: &str,
) {
    let room = room_id.trim();
    if room.is_empty() {
        return;
    }
    let aid = if author_id.trim().is_empty() {
        room
    } else {
        author_id.trim()
    };
    let aname = if author_name.trim().is_empty() {
        aid
    } else {
        author_name.trim()
    };
    let key = format!(
        "{}|{}|{}",
        normalize_id(room),
        normalize_id(aid),
        normalize_id(aname)
    );
    if seen.insert(key) {
        lookups.push((room.to_string(), aid.to_string(), aname.to_string()));
    }
}

/// Get room information from Kuaishou reverse APIs only.
pub async fn get_room_info(
    client: &Client,
    account: &Account,
    url: &str,
) -> Result<RoomInfo, RecorderError> {
    let account_obj = ensure_guest_cookie(account);
    let account = &account_obj;
    if !web_api_allowed(account, url).await {
        return Err(web_cooldown_error());
    }
    let mut best_room: Option<RoomInfo> = None;
    let mut best_score = i64::MIN;
    let mut last_error: Option<RecorderError> = None;
    let mut attempt_traces: Vec<String> = Vec::new();
    let candidates = principal_id_candidates(url);
    let mut follow_lookups: Vec<(String, String, String)> = Vec::new();
    let mut follow_lookup_seen: HashSet<String> = HashSet::new();

    for principal_id in &candidates {
        push_follow_lookup_candidate(
            &mut follow_lookups,
            &mut follow_lookup_seen,
            principal_id,
            principal_id,
            principal_id,
        );
        match get_room_info_via_by_user(client, account, principal_id).await {
            Ok(Some(room)) => {
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "by_user",
                    format!(
                        "ok/live={}/streams={}/title={}",
                        room.live_status,
                        room.streams.len(),
                        room.room_title
                    ),
                );
                push_follow_lookup_candidate(
                    &mut follow_lookups,
                    &mut follow_lookup_seen,
                    principal_id,
                    &room.user_id,
                    &room.user_name,
                );
                if !room.user_id.trim().is_empty() {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        &room.user_id,
                        &room.user_id,
                        &room.user_name,
                    );
                }
                if !is_placeholder_user_name(&room.user_name) {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        principal_id,
                        principal_id,
                        &room.user_name,
                    );
                }
                if room.live_status && !room.streams.is_empty() {
                    log_room_info_source("by_user", principal_id, &room);
                    return Ok(room);
                }
                let score = room_info_score(&room);
                if score > best_score {
                    best_score = score;
                    best_room = Some(room);
                }
            }
            Ok(None) => push_attempt_trace(
                &mut attempt_traces,
                principal_id,
                "by_user",
                "none".to_string(),
            ),
            Err(err) => {
                if is_rate_limited_error(&err) || is_captcha_error(&err) {
                    return Err(err);
                }
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "by_user",
                    format!("err={}", err),
                );
                last_error = Some(err);
            }
        }

        match get_room_info_via_livedetail(client, account, &principal_id).await {
            Ok(Some(room)) => {
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "livedetail",
                    format!(
                        "ok/live={}/streams={}/title={}",
                        room.live_status,
                        room.streams.len(),
                        room.room_title
                    ),
                );
                push_follow_lookup_candidate(
                    &mut follow_lookups,
                    &mut follow_lookup_seen,
                    principal_id,
                    &room.user_id,
                    &room.user_name,
                );
                if !room.user_id.trim().is_empty() {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        &room.user_id,
                        &room.user_id,
                        &room.user_name,
                    );
                }
                if !is_placeholder_user_name(&room.user_name) {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        principal_id,
                        principal_id,
                        &room.user_name,
                    );
                }
                if room.live_status && !room.streams.is_empty() {
                    log_room_info_source("livedetail", principal_id, &room);
                    return Ok(room);
                }
                let score = room_info_score(&room);
                if score > best_score {
                    best_score = score;
                    best_room = Some(room);
                }
            }
            Ok(None) => push_attempt_trace(
                &mut attempt_traces,
                principal_id,
                "livedetail",
                "none".to_string(),
            ),
            Err(err) => {
                if is_rate_limited_error(&err) || is_captcha_error(&err) {
                    return Err(err);
                }
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "livedetail",
                    format!("err={}", err),
                );
                last_error = Some(err);
            }
        }

        match get_room_info_via_mgraphql(client, account, principal_id).await {
            Ok(Some(room)) => {
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "m_graphql",
                    format!(
                        "ok/live={}/streams={}/title={}",
                        room.live_status,
                        room.streams.len(),
                        room.room_title
                    ),
                );
                push_follow_lookup_candidate(
                    &mut follow_lookups,
                    &mut follow_lookup_seen,
                    principal_id,
                    &room.user_id,
                    &room.user_name,
                );
                if !room.user_id.trim().is_empty() {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        &room.user_id,
                        &room.user_id,
                        &room.user_name,
                    );
                }
                if !is_placeholder_user_name(&room.user_name) {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        principal_id,
                        principal_id,
                        &room.user_name,
                    );
                }
                if room.live_status && !room.streams.is_empty() {
                    log_room_info_source("m_graphql", principal_id, &room);
                    return Ok(room);
                }
                let score = room_info_score(&room);
                if score > best_score {
                    best_score = score;
                    best_room = Some(room);
                }
            }
            Ok(None) => push_attempt_trace(
                &mut attempt_traces,
                principal_id,
                "m_graphql",
                "none".to_string(),
            ),
            Err(err) => {
                if is_rate_limited_error(&err) || is_captcha_error(&err) {
                    return Err(err);
                }
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "m_graphql",
                    format!("err={}", err),
                );
                last_error = Some(err);
            }
        }
    }

    if use_follow_list_fallback(account) {
        for (lookup_room, lookup_author_id, lookup_author_name) in follow_lookups {
            if let Some(info) = fetch_follow_live_info_cached(
                client,
                account,
                &lookup_room,
                &lookup_author_id,
                &lookup_author_name,
            )
            .await
            {
                let room = room_info_from_follow_info(&lookup_room, info);
                push_attempt_trace(
                    &mut attempt_traces,
                    &lookup_room,
                    "follow_list",
                    format!(
                        "ok/live={}/streams={}/title={}",
                        room.live_status,
                        room.streams.len(),
                        room.room_title
                    ),
                );
                if room.live_status && !room.streams.is_empty() {
                    log_room_info_source("follow_list_fallback", &lookup_room, &room);
                    return Ok(room);
                }
                let score = room_info_score(&room);
                if score > best_score {
                    best_score = score;
                    best_room = Some(room);
                }
            }
        }
    }

    if use_public_page_fallback(account) {
        // Public page fallback: anonymous page can expose stream URLs even when
        // livedetail returns masked payloads (e.g. result=2 + undefined URL).
        for principal_id in &candidates {
            match get_room_info_via_public_page(client, account, principal_id).await {
                Ok(Some(room)) => {
                    push_attempt_trace(
                        &mut attempt_traces,
                        principal_id,
                        "public_page",
                        format!(
                            "ok/live={}/streams={}/title={}",
                            room.live_status,
                            room.streams.len(),
                            room.room_title
                        ),
                    );
                    if room.live_status && !room.streams.is_empty() {
                        log_room_info_source("public_page_fallback", principal_id, &room);
                        return Ok(room);
                    }
                    let score = room_info_score(&room);
                    if score > best_score {
                        best_score = score;
                        best_room = Some(room);
                    }
                }
                Ok(None) => push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "public_page",
                    "none".to_string(),
                ),
                Err(err) => {
                    if is_rate_limited_error(&err) || is_captcha_error(&err) {
                        return Err(err);
                    }
                    push_attempt_trace(
                        &mut attempt_traces,
                        principal_id,
                        "public_page",
                        format!("err={}", err),
                    );
                    last_error = Some(err);
                }
            }
        }
    }

    if let Some(room) = best_room {
        if !attempt_traces.is_empty() {
            log::info!(
                "[Kuaishou] best_effort trace url={} {}",
                url,
                attempt_traces.join(" | ")
            );
        }
        log_room_info_source("best_effort", url, &room);
        return Ok(room);
    }
    if let Some(err) = last_error {
        if !attempt_traces.is_empty() {
            log::info!(
                "[Kuaishou] room_info failed trace url={} {}",
                url,
                attempt_traces.join(" | ")
            );
        }
        return Err(err);
    }
    Err(RecorderError::ApiError {
        error: "Failed to fetch Kuaishou room info via reverse APIs".to_string(),
    })
}
/// Get stream URLs from Kuaishou reverse APIs only.
pub async fn get_stream_urls(
    client: &Client,
    account: &Account,
    url: &str,
) -> Result<Vec<StreamInfo>, RecorderError> {
    let account_obj = ensure_guest_cookie(account);
    let account = &account_obj;
    if !web_api_allowed(account, url).await {
        return Err(web_cooldown_error());
    }
    let mut last_error: Option<RecorderError> = None;
    let mut attempt_traces: Vec<String> = Vec::new();
    let candidates = principal_id_candidates(url);
    let mut follow_lookups: Vec<(String, String, String)> = Vec::new();
    let mut follow_lookup_seen: HashSet<String> = HashSet::new();

    for principal_id in &candidates {
        push_follow_lookup_candidate(
            &mut follow_lookups,
            &mut follow_lookup_seen,
            principal_id,
            principal_id,
            principal_id,
        );
        match get_room_info_via_by_user(client, account, principal_id).await {
            Ok(Some(room)) if !room.streams.is_empty() => {
                log_stream_urls_source("by_user", principal_id, &room.streams);
                return Ok(room.streams);
            }
            Ok(Some(room)) => {
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "by_user",
                    format!("empty_streams/title={}", room.room_title),
                );
                push_follow_lookup_candidate(
                    &mut follow_lookups,
                    &mut follow_lookup_seen,
                    principal_id,
                    &room.user_id,
                    &room.user_name,
                );
                if !room.user_id.trim().is_empty() {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        &room.user_id,
                        &room.user_id,
                        &room.user_name,
                    );
                }
                if !is_placeholder_user_name(&room.user_name) {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        principal_id,
                        principal_id,
                        &room.user_name,
                    );
                }
                last_error = Some(RecorderError::ApiError {
                    error: "Kuaishou byUser returned empty stream list".to_string(),
                });
            }
            Ok(None) => push_attempt_trace(
                &mut attempt_traces,
                principal_id,
                "by_user",
                "none".to_string(),
            ),
            Err(err) => {
                if is_rate_limited_error(&err) || is_captcha_error(&err) {
                    return Err(err);
                }
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "by_user",
                    format!("err={}", err),
                );
                last_error = Some(err);
            }
        }

        match get_room_info_via_livedetail(client, account, &principal_id).await {
            Ok(Some(room)) if !room.streams.is_empty() => {
                log_stream_urls_source("livedetail", principal_id, &room.streams);
                return Ok(room.streams);
            }
            Ok(Some(room)) => {
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "livedetail",
                    format!("empty_streams/title={}", room.room_title),
                );
                push_follow_lookup_candidate(
                    &mut follow_lookups,
                    &mut follow_lookup_seen,
                    principal_id,
                    &room.user_id,
                    &room.user_name,
                );
                if !room.user_id.trim().is_empty() {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        &room.user_id,
                        &room.user_id,
                        &room.user_name,
                    );
                }
                if !is_placeholder_user_name(&room.user_name) {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        principal_id,
                        principal_id,
                        &room.user_name,
                    );
                }
                last_error = Some(RecorderError::ApiError {
                    error: "Kuaishou livedetail returned empty stream list".to_string(),
                });
            }
            Ok(None) => push_attempt_trace(
                &mut attempt_traces,
                principal_id,
                "livedetail",
                "none".to_string(),
            ),
            Err(err) => {
                if is_rate_limited_error(&err) || is_captcha_error(&err) {
                    return Err(err);
                }
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "livedetail",
                    format!("err={}", err),
                );
                last_error = Some(err);
            }
        }

        match get_room_info_via_mgraphql(client, account, principal_id).await {
            Ok(Some(room)) if !room.streams.is_empty() => {
                log_stream_urls_source("m_graphql", principal_id, &room.streams);
                return Ok(room.streams);
            }
            Ok(Some(room)) => {
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "m_graphql",
                    format!("empty_streams/title={}", room.room_title),
                );
                push_follow_lookup_candidate(
                    &mut follow_lookups,
                    &mut follow_lookup_seen,
                    principal_id,
                    &room.user_id,
                    &room.user_name,
                );
                if !room.user_id.trim().is_empty() {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        &room.user_id,
                        &room.user_id,
                        &room.user_name,
                    );
                }
                if !is_placeholder_user_name(&room.user_name) {
                    push_follow_lookup_candidate(
                        &mut follow_lookups,
                        &mut follow_lookup_seen,
                        principal_id,
                        principal_id,
                        &room.user_name,
                    );
                }
                last_error = Some(RecorderError::ApiError {
                    error: "Kuaishou m_graphql returned empty stream list".to_string(),
                });
            }
            Ok(None) => push_attempt_trace(
                &mut attempt_traces,
                principal_id,
                "m_graphql",
                "none".to_string(),
            ),
            Err(err) => {
                if is_rate_limited_error(&err) || is_captcha_error(&err) {
                    return Err(err);
                }
                push_attempt_trace(
                    &mut attempt_traces,
                    principal_id,
                    "m_graphql",
                    format!("err={}", err),
                );
                last_error = Some(err);
            }
        }
    }

    if use_follow_list_fallback(account) {
        for (lookup_room, lookup_author_id, lookup_author_name) in follow_lookups {
            if let Some(info) = fetch_follow_live_info_cached(
                client,
                account,
                &lookup_room,
                &lookup_author_id,
                &lookup_author_name,
            )
            .await
            {
                if !info.streams.is_empty() {
                    log_stream_urls_source("follow_list_fallback", &lookup_room, &info.streams);
                    return Ok(info.streams);
                }
            }
        }
    }

    if use_public_page_fallback(account) {
        for principal_id in &candidates {
            match get_room_info_via_public_page(client, account, principal_id).await {
                Ok(Some(room)) if !room.streams.is_empty() => {
                    log_stream_urls_source("public_page_fallback", principal_id, &room.streams);
                    return Ok(room.streams);
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(err) => {
                    if is_rate_limited_error(&err) || is_captcha_error(&err) {
                        return Err(err);
                    }
                    last_error = Some(err);
                }
            }
        }
    }

    if let Some(err) = last_error {
        if !attempt_traces.is_empty() {
            log::info!(
                "[Kuaishou] stream_urls failed trace url={} {}",
                url,
                attempt_traces.join(" | ")
            );
        }
        return Err(err);
    }
    Err(RecorderError::ApiError {
        error: "Failed to fetch Kuaishou stream list via reverse APIs".to_string(),
    })
}

fn ensure_guest_cookie(account: &crate::account::Account) -> crate::account::Account {
    let mut next = account.clone();
    let normalized = normalize_cookie_header(&next.cookies);
    let ensured = ensure_kuaishou_base_cookies(&normalized);
    if ensured != next.cookies {
        next.cookies = ensured;
    } else if normalized != next.cookies {
        next.cookies = normalized;
    }
    next
}

pub async fn fetch_guest_state(client: &Client) -> Result<String, RecorderError> {
    log::info!("[Kuaishou] Fetching guest state from homepage...");
    let url = "https://live.kuaishou.com/";
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());

    // Attempt fetch
    let response = client.get(url).headers(headers).send().await?;

    let mut map = HashMap::new();
    for value in response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
    {
        if let Ok(raw) = value.to_str() {
            if let Some((pair, _)) = raw.split_once(';') {
                if let Some((name, val)) = pair.split_once('=') {
                    map.insert(name.trim().to_string(), val.trim().to_string());
                }
            }
        }
    }

    if let Some(did) = map.get("did") {
        log::info!("[Kuaishou] Found device_id: {}", did);
        return Ok(did.to_string());
    }

    Ok(String::new())
}

/// Get QR code for login
pub async fn get_qr(client: &Client) -> Result<QrInfo, RecorderError> {
    // Check overrides
    let overrides =
        crate::reverse_generate::qr_login::fetch_kuaishou_overrides().unwrap_or_default();

    let mut headers = reqwest::header::HeaderMap::new();
    let user_agent = overrides
        .get("user_agent")
        .filter(|v| !v.is_empty())
        .map(|v| v.as_str())
        .unwrap_or(USER_AGENT);

    headers.insert("User-Agent", user_agent.parse().unwrap());
    headers.insert(
        "Content-Type",
        "application/x-www-form-urlencoded".parse().unwrap(),
    );
    headers.insert("Referer", "https://live.kuaishou.com/".parse().unwrap());

    // Handle device_id (did)
    let mut did = overrides
        .get("device_id")
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .unwrap_or_default();

    if did.is_empty() {
        if let Ok(real_did) = fetch_guest_state(client).await {
            if !real_did.is_empty() {
                did = real_did;
                let _ = crate::reverse_generate::qr_login::update_kuaishou_config(
                    Some(&did),
                    None,
                    true,
                );
            }
        }
    }
    // Fallback to random if still empty
    if did.is_empty() {
        did = gen_web_did();
    }

    // Check custom cookie in overrides
    let mut qr_cookie = if let Some(cookie) = overrides.get("cookie").filter(|v| !v.is_empty()) {
        cookie.clone()
    } else {
        // Build cookie using did
        let didv = Utc::now().timestamp_millis();
        format!("did={did}; didv={didv}; kwpsecproductname=PCLive")
    };

    headers.insert("Cookie", qr_cookie.parse().unwrap());

    let response = client
        .post("https://id.kuaishou.com/rest/c/infra/ks/qr/start")
        .headers(headers)
        .body("sid=kuaishou.live.web&channelType=UNKNOWN&encryptHeaders=")
        .send()
        .await?;
    let response_headers = response.headers().clone();

    let data: serde_json::Value = response.json().await?;
    let mut cookie_map = parse_cookie_map(&qr_cookie);
    merge_set_cookie_headers(&mut cookie_map, &response_headers);
    qr_cookie = normalize_record_cookie(&format_cookie_map(&cookie_map));

    Ok(QrInfo {
        qr_login_token: data["qrLoginToken"]
            .as_str()
            .ok_or(RecorderError::InvalidValue)?
            .to_string(),
        qr_login_signature: data["qrLoginSignature"]
            .as_str()
            .ok_or(RecorderError::InvalidValue)?
            .to_string(),
        image_data: data["imageData"]
            .as_str()
            .ok_or(RecorderError::InvalidValue)?
            .to_string(),
        qr_cookie,
    })
}
pub fn get_kuaishou_cookie_item(cookies: &str, key: &str) -> Option<String> {
    for cookie in cookies.split(';').map(str::trim) {
        if let Some((name, value)) = cookie.split_once('=') {
            if name.trim() == key {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn find_string_value(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(Value::String(value)) = map.get(*key) {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
            for child in map.values() {
                if let Some(found) = find_string_value(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(values) => {
            for child in values {
                if let Some(found) = find_string_value(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|parsed| find_string_value(&parsed, keys)),
        _ => None,
    }
}

fn is_title_useful(title: &str, author_name: &str) -> bool {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return false;
    }
    if !author_name.trim().is_empty() && trimmed == author_name.trim() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("????") || lower.contains("error code") || lower.contains("????") {
        return false;
    }
    if lower.ends_with("'s live") || trimmed.ends_with("???") {
        return false;
    }
    true
}

fn get_string_field(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = map.get(*key) {
            match value {
                Value::String(value) if !value.is_empty() => return Some(value.clone()),
                Value::Number(value) => return Some(value.to_string()),
                _ => {}
            }
        }
    }
    None
}

fn user_info_from_map(map: &Map<String, Value>) -> Option<crate::UserInfo> {
    let user_id = get_string_field(map, &["user_id", "userId", "userIdStr", "uid"])
        .or_else(|| get_string_field(map, &["id"]));
    let user_name = get_string_field(map, &["user_name", "userName", "nickname", "nickName"])
        .or_else(|| get_string_field(map, &["name"]));

    let (Some(user_id), Some(user_name)) = (user_id, user_name) else {
        return None;
    };

    if user_id == "1" || is_placeholder_user_name(&user_name) {
        return None;
    }

    let user_avatar = get_string_field(
        map,
        &[
            "headurl",
            "headUrl",
            "avatar",
            "avatarUrl",
            "portrait",
            "profilePic",
        ],
    )
    .unwrap_or_default();

    Some(crate::UserInfo {
        user_id,
        user_name,
        user_avatar,
    })
}
fn find_user_info(value: &Value) -> Option<crate::UserInfo> {
    match value {
        Value::Object(map) => {
            if let Some(user_info) = user_info_from_map(map) {
                return Some(user_info);
            }
            for child in map.values() {
                if let Some(user_info) = find_user_info(child) {
                    return Some(user_info);
                }
            }
            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(user_info) = find_user_info(value) {
                    return Some(user_info);
                }
            }
            None
        }
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|parsed| find_user_info(&parsed)),
        _ => None,
    }
}

async fn fetch_baseuser_info(client: &Client, account: &Account) -> Option<crate::UserInfo> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().ok()?);
    headers.insert("Accept", "application/json, text/plain, */*".parse().ok()?);
    headers.insert("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8".parse().ok()?);
    headers.insert("Referer", "https://live.kuaishou.com/".parse().ok()?);
    headers.insert("Origin", "https://live.kuaishou.com".parse().ok()?);

    let cookie_header = ensure_kuaishou_request_cookie(&account.cookies);
    if !cookie_header.is_empty() {
        headers.insert("Cookie", cookie_header.parse().ok()?);
    }
    if let Some(kww) = resolve_kuaishou_kww(&cookie_header) {
        if let Ok(value) = kww.parse() {
            headers.insert("kww", value);
        }
    }

    let response = client
        .get("https://live.kuaishou.com/live_api/baseuser/userinfo")
        .headers(headers)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    let value: Value = serde_json::from_str(&body).ok()?;
    if let Some(info) = find_user_info(&value) {
        return Some(info);
    }
    value.get("data").and_then(find_user_info)
}

/// Get user information from cookies
pub async fn get_user_info(
    client: &Client,
    account: &Account,
) -> Result<crate::UserInfo, RecorderError> {
    let account = ensure_guest_cookie(account);
    let uid = get_cookie_value_ci(&account.cookies, "userId")
        .or_else(|| get_cookie_value_ci(&account.cookies, "user_id"))
        .or_else(|| get_cookie_value_ci(&account.cookies, "userIdStr"))
        .or_else(|| get_cookie_value_ci(&account.cookies, "uid"));

    if let Some(info) = fetch_baseuser_info(client, &account).await {
        return Ok(info);
    }

    let mut candidates = vec![
        "https://live.kuaishou.com/".to_string(),
        "https://www.kuaishou.com/".to_string(),
    ];

    if let Some(uid) = uid.as_ref() {
        candidates.push(format!("https://live.kuaishou.com/u/{uid}"));
        candidates.push(format!("https://www.kuaishou.com/u/{uid}"));
        candidates.push(format!("https://www.kuaishou.com/profile/{uid}"));
    }

    for url in candidates {
        let (html_str, _) = match fetch_web_html(client, &account, &url).await {
            Ok(result) => result,
            Err(_) => continue,
        };

        if let Some(json_str) = extract_initial_state(&html_str) {
            if let Ok(state) = serde_json::from_str::<Value>(&json_str) {
                if let Some(user_info) = find_user_info(&state) {
                    return Ok(user_info);
                }
            }

            #[derive(Deserialize)]
            struct KuaishouUser {
                #[serde(default, alias = "user_id", alias = "userId", alias = "id")]
                user_id: String,
                #[serde(default, alias = "user_name", alias = "userName", alias = "name")]
                user_name: String,
                #[serde(
                    default,
                    alias = "headurl",
                    alias = "headUrl",
                    alias = "avatar",
                    alias = "avatarUrl"
                )]
                head_url: String,
            }

            let user_regex = Regex::new(
                r#"(?s)"profile":\{"ownerCount".*?"user":(.*?),"currentWork"#,
            )
            .map_err(|e| RecorderError::ApiError {
                error: format!("Failed to create user regex: {}", e),
            })?;

            if let Some(user_str) = user_regex
                .captures(&json_str)
                .and_then(|cap| cap.get(1))
                .map(|m| m.as_str())
            {
                if let Ok(user) = serde_json::from_str::<KuaishouUser>(user_str) {
                    if !user.user_id.is_empty() || !user.user_name.is_empty() {
                        return Ok(crate::UserInfo {
                            user_id: user.user_id,
                            user_name: user.user_name,
                            user_avatar: user.head_url,
                        });
                    }
                }
            }
        }

        let (title, _cover, avatar) = extract_metadata_from_html(&html_str);
        if title.is_some() || avatar.is_some() {
            return Ok(crate::UserInfo {
                user_id: "".to_string(),
                user_name: title.unwrap_or_else(|| "Kuaishou".to_string()),
                user_avatar: avatar.unwrap_or_default(),
            });
        }
    }

    if let Some(uid) = uid {
        if let Some(info) = fetch_livedetail_info(client, &account, &uid).await {
            return Ok(crate::UserInfo {
                user_id: info.user_id.unwrap_or(uid),
                user_name: info.user_name.unwrap_or_else(|| "Kuaishou".to_string()),
                user_avatar: info.user_avatar.unwrap_or_default(),
            });
        }
    }

    Err(RecorderError::ApiError {
        error: "Failed to parse user info from page".to_string(),
    })
}

/// Poll QR code status and get cookies after successful login
pub async fn get_qr_status(
    client: &Client,
    qr_login_token: &str,
    qr_login_signature: &str,
    qr_cookie: Option<&str>,
) -> Result<QrStatus, RecorderError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());
    headers.insert(
        "Content-Type",
        "application/x-www-form-urlencoded".parse().unwrap(),
    );
    headers.insert("Referer", "https://live.kuaishou.com/".parse().unwrap());
    let mut cookies_map = qr_cookie.map(parse_cookie_map).unwrap_or_default();

    let payload = format!(
        "qrLoginToken={qr_login_token}&qrLoginSignature={qr_login_signature}&channelType=UNKNOWN&encryptHeaders=&sid=kuaishou.live.web"
    );

    let mut scan_headers = headers.clone();
    let cookie_header = format_cookie_map(&cookies_map);
    if !cookie_header.is_empty() {
        scan_headers.insert("Cookie", cookie_header.parse().unwrap());
    }

    // Step 1: Check scan status
    let scan_response = client
        .post("https://id.kuaishou.com/rest/c/infra/ks/qr/scanResult")
        .headers(scan_headers)
        .body(payload.clone())
        .send()
        .await?;
    let scan_resp_headers = scan_response.headers().clone();
    let scan_data: serde_json::Value = scan_response.json().await?;
    merge_set_cookie_headers(&mut cookies_map, &scan_resp_headers);
    log::warn!("[Kuaishou] QR scanResult: {}", scan_data);
    let (scan_user_id, scan_user_name, scan_user_avatar) = extract_qr_scan_user(&scan_data);

    // If not scanned yet, return pending status
    if scan_data["result"].as_u64().unwrap_or(1) != 1 {
        let message = extract_qr_message(&scan_data);
        return Ok(QrStatus {
            code: 1,
            cookies: normalize_record_cookie(&format_cookie_map(&cookies_map)),
            message,
            user_id: scan_user_id,
            user_name: scan_user_name,
            user_avatar: scan_user_avatar,
        });
    }

    let mut accept_headers = headers.clone();
    let cookie_header = format_cookie_map(&cookies_map);
    if !cookie_header.is_empty() {
        accept_headers.insert("Cookie", cookie_header.parse().unwrap());
    }

    // Step 2: Check accept status
    let accept_response = client
        .post("https://id.kuaishou.com/rest/c/infra/ks/qr/acceptResult")
        .headers(accept_headers)
        .body(payload)
        .send()
        .await?;
    let accept_resp_headers = accept_response.headers().clone();
    let accept_data: serde_json::Value = accept_response.json().await?;
    merge_set_cookie_headers(&mut cookies_map, &accept_resp_headers);
    log::warn!("[Kuaishou] QR acceptResult: {}", accept_data);

    // If not accepted yet, return pending status
    if accept_data["result"].as_u64().unwrap_or(1) != 1 {
        let message = extract_qr_message(&accept_data);
        return Ok(QrStatus {
            code: 2,
            cookies: normalize_record_cookie(&format_cookie_map(&cookies_map)),
            message,
            user_id: scan_user_id,
            user_name: scan_user_name,
            user_avatar: scan_user_avatar,
        });
    }

    // Step 3: Get qrToken and perform callback
    let qr_token = accept_data["qrToken"]
        .as_str()
        .ok_or(RecorderError::InvalidValue)?;

    let mut callback_headers = headers.clone();
    let cookie_header = format_cookie_map(&cookies_map);
    if !cookie_header.is_empty() {
        callback_headers.insert("Cookie", cookie_header.parse().unwrap());
    }

    let callback_response = client
        .post("https://id.kuaishou.com/pass/kuaishou/login/qr/callback")
        .headers(callback_headers)
        .body(format!("qrToken={qr_token}&sid=kuaishou.live.web"))
        .send()
        .await?;

    let callback_resp_headers = callback_response.headers().clone();
    let callback_json: serde_json::Value = callback_response.json().await.unwrap_or_default();
    merge_set_cookie_headers(&mut cookies_map, &callback_resp_headers);
    log::warn!("[Kuaishou] QR callback: {}", callback_json);
    let callback_message = extract_qr_message(&callback_json);

    if let Some(value) = callback_json
        .get("kuaishou.live.web_st")
        .and_then(|v| v.as_str())
    {
        if !value.is_empty() {
            merge_cookie_kv(&mut cookies_map, "kuaishou.live.web_st", value);
        }
    }
    if let Some(value) = callback_json
        .get("kuaishou.live.web_ph")
        .and_then(|v| v.as_str())
    {
        if !value.is_empty() {
            merge_cookie_kv(&mut cookies_map, "kuaishou.live.web_ph", value);
        }
    }
    if let Some(value) = callback_json
        .get("kuaishou.live.web.at")
        .and_then(|v| v.as_str())
    {
        if !value.is_empty() {
            merge_cookie_kv(&mut cookies_map, "kuaishou.live.web.at", value);
        }
    }
    if let Some(value) = callback_json.get("ssecurity").and_then(|v| v.as_str()) {
        if !value.is_empty() {
            merge_cookie_kv(&mut cookies_map, "ssecurity", value);
        }
    }
    if let Some(value) = callback_json.get("passToken").and_then(|v| v.as_str()) {
        if !value.is_empty() {
            merge_cookie_kv(&mut cookies_map, "passToken", value);
        }
    }
    if let Some(value) = callback_json.get("userId").and_then(|v| v.as_i64()) {
        merge_cookie_kv(&mut cookies_map, "userId", &value.to_string());
    }

    let cookies = normalize_record_cookie(&format_cookie_map(&cookies_map));

    if cookies.is_empty() {
        if let Some(message) = callback_message {
            return Ok(QrStatus {
                code: 2,
                cookies: String::new(),
                message: Some(message),
                user_id: scan_user_id,
                user_name: scan_user_name,
                user_avatar: scan_user_avatar,
            });
        }
        if has_captcha_signal(&callback_json) {
            return Ok(QrStatus {
                code: 2,
                cookies: String::new(),
                message: Some("\u{9700}\u{8981}\u{6ed1}\u{5757}\u{9a8c}\u{8bc1}".to_string()),
                user_id: scan_user_id,
                user_name: scan_user_name,
                user_avatar: scan_user_avatar,
            });
        }
        return Err(RecorderError::ApiError {
            error: "Failed to extract cookies from response".to_string(),
        });
    }

    Ok(QrStatus {
        code: 0,
        cookies,
        message: None,
        user_id: scan_user_id,
        user_name: scan_user_name,
        user_avatar: scan_user_avatar,
    })
}

/// Download file from URL to local path
pub async fn download_file(
    client: &Client,
    url: &str,
    path: &std::path::Path,
) -> Result<(), RecorderError> {
    if url.is_empty() {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| RecorderError::IoError(e))?;
        }
    }

    let response = client.get(url).send().await?;
    let bytes = response.bytes().await?;
    let mut file = tokio::fs::File::create(&path).await?;
    let mut content = std::io::Cursor::new(bytes);
    tokio::io::copy(&mut content, &mut file).await?;
    Ok(())
}
