use super::response::{RoomInfo as SigiRoomInfo, SigiStateResponse, StreamUrl as SigiStreamUrl};
use crate::account::Account;
use crate::core::stream_info::{
    CdnNode, Codec, Format, PlatformStreamInfo, PlatformType, Quality, StreamVariant,
};
use crate::errors::RecorderError;
use crate::reverse_generate::x_bogus::XBogus;
use crate::reverse_generate::x_gnarly;
use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use rand::Rng;
use regex::Regex;
use reqwest::header::{HeaderMap, LOCATION};
use reqwest::redirect::Policy;
use reqwest::{Client, Proxy, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use toml::Value as TomlValue;
use url::form_urlencoded::Serializer;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const FEED_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36";
const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const TIKTOK_COOLDOWN_SECS: i64 = 120;
const TIKTOK_MSSDK_URL: &str = "https://mssdk.tiktokw.us/web/report?msToken=1Ab-7YxR9lUHSem0PraI_XzdKmpHb6j50L8AaXLAd2aWTdoJCYLfX_67rVQFE4UwwHVHmyG_NfIipqrlLT3kCXps-5PYlNAqtdwEg7TrDyTAfCKyBrOLmhMUjB55oW8SPZ4_EkNxNFUdV7MquA==";
const TIKTOK_MSSDK_MAGIC: i64 = 538969122;
const TIKTOK_MSSDK_VERSION: i64 = 1;
const TIKTOK_MSSDK_DATA_TYPE: i64 = 8;
const TIKTOK_MSSDK_STR_DATA: &str = "3BvqYbNXLLOcZehvxZVbjpAu7vq82RoWmFSJHLFwzDwJIZevE0AeilQfP55LridxmdGGjknoksqIsLqlMHMif0IFK/Br7JWqxOHnYuMwVCnttFc0Y4MFvdVWM5FECiEulJC0Dc+eeVsNSrFnAc9K7fazqdglyJgGLSfXIJmgyCvvQ4pg0u5HBVVugLSWs242X42fjoWymaUCLZJQo6vi6WLyuV7l5IC3Mg+lelr5xBQD6Q7hBIFEw8zzxJ1n2DyA4xLbOHTQdKvEtsK7XzyWwjpRnojPTbBl69Zosnuru+lOBIl+tFu/+hCQ1m0jYZwTP4rVE75L3Du6+KZ5v/9TyFYjq7y3y9bGLP4d7yQueJbF90G1yrZ6htElrZ2vqZKDrIqBVbmOZr/nph12k2JKrITtN0R/pMsp0sJ4gesQnXxcD/pLOFAINHk7umgbe6LzJ7+TLUdGuO4M7xiEg/jCqhjgJX1izZ4NPoBDp35zRxj6Y6OrcstlTN/cv5sz663+Nco/mEwhGq2VwrL4gAIAPycndIsb48dPdtngmLqNDNN0ZyVRjgqVIDXXrxigXCkR9CH89Dlrrb7QQqWVgRXz9/k5ihEM43BR3sd3mMU/XgFLN1Aoxf6GzzdxP2QPBI75/ZoHoAmu54v8gTmA3ntCGlEF0zgaFGTdpkGdb+oZgyQM4pw1aAyxmFINXkpD3IKKoGev9kD9gTFnhiQMGCMemhZS7ZYdbuGu0Cb+lQKaL/QTt80FMyGmW8kzVy9xW/ja9BcdEJYRoaufuFRkBFG5ay8x4WHLR6hEapXqQial/cREbLL4sQytpjtmnndFqvT7xN5DhgsLY2Z7451MJhD6NJXKNrMafGZSbItzQWY=";
const TIKTOK_TTWID_CHECK_URL: &str = "https://www.tiktok.com/ttwid/check/";
const TIKTOK_TTWID_CHECK_DATA: &str = "{\"aid\":1988,\"service\":\"www.tiktok.com\",\"union\":false,\"unionHost\":\"\",\"needFid\":false,\"fid\":\"\",\"migrate_priority\":0}";
const TIKTOK_ROOM_INFO_URL: &str = "https://webcast.tiktok.com/webcast/room/info/";
const TIKTOK_ROOM_CHECK_ALIVE_URL: &str = "https://webcast.tiktok.com/webcast/room/check_alive/";
const TIKTOK_ROOM_ENTER_URL: &str = "https://webcast.tiktok.com/webcast/room/enter/";
const TIKTOK_IM_FETCH_URL: &str = "https://webcast.tiktok.com/webcast/im/fetch/";
const TIKTOK_FEED_URL_TEMPLATE: &str = "https://webcast.tiktok.com/webcast/feed/?aid=1988&app_language=zh-Hans&app_name=tiktok_web&browser_language=zh-CN&browser_name=Mozilla&browser_online=true&browser_platform=Win32&browser_version=5.0%20(Windows%20NT%2010.0;%20Win64;%20x64)%20AppleWebKit/537.36%20(KHTML,%20like%20Gecko)%20Chrome/138.0.0.0%20Safari/537.36&channel=tiktok_web&channel_id=86&cookie_enabled=true&cpu_number=32&data_collection_enabled=true&device_id={device_id}&device_platform=web_pc&device_type=web_h265&focus_state=true&from_page=&hidden_banner=true&history_len=9&is_fullscreen=false&is_non_personalized=0&is_page_visible=true&max_time=0&os=windows&priority_region=JP&referer={referer}&region=JP&req_from=pc_web_recommend_room&root_referer={root_referer}&screen_height=1440&screen_width=2560&tz_name=Asia/Shanghai&user_is_login=true&verifyFp={verify_fp}&webcast_language=zh-Hans&msToken={ms_token}&X-Bogus={x_bogus}&X-Gnarly={x_gnarly}";

static TIKTOK_COOLDOWN_UNTIL: AtomicI64 = AtomicI64::new(0);

fn tiktok_api_allowed() -> bool {
    let now = Utc::now().timestamp();
    if now >= TIKTOK_COOLDOWN_UNTIL.load(Ordering::Relaxed) {
        return true;
    }
    // Allow immediate retry when proxy is configured (useful after switching proxy).
    proxy_url_from_env().is_some()
}

fn set_tiktok_cooldown(reason: &str) {
    let until = Utc::now().timestamp() + TIKTOK_COOLDOWN_SECS;
    TIKTOK_COOLDOWN_UNTIL.store(until, Ordering::Relaxed);
    log::info!(
        "[TikTok] API cooldown set ({}s): {}",
        TIKTOK_COOLDOWN_SECS,
        reason
    );
}

#[derive(Clone, Debug)]
pub struct RoomInfo {
    pub live_status: bool,
    pub room_title: String,
    pub room_cover_url: String,
    pub user_id: String,
    pub user_name: String,
    pub user_avatar: String,
}

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub hls_url: Option<String>,
    pub rtmp_url: Option<String>,
    pub resolution: Option<String>,
}

impl PlatformStreamInfo for StreamInfo {
    fn primary_variant(&self) -> Result<StreamVariant, RecorderError> {
        if let Some(rtmp_url) = &self.rtmp_url {
            return Ok(StreamVariant {
                url: rtmp_url.clone(),
                format: Format::RTMP,
                codec: Codec::AVC,
                quality: Quality::Origin,
                bitrate: None,
            });
        }

        if let Some(hls_url) = &self.hls_url {
            return Ok(StreamVariant {
                url: hls_url.clone(),
                format: Format::HLS,
                codec: Codec::AVC,
                quality: Quality::Origin,
                bitrate: None,
            });
        }

        Err(RecorderError::NoStreamAvailable)
    }

    fn all_variants(&self) -> Vec<StreamVariant> {
        let mut variants = Vec::new();

        if let Some(rtmp_url) = &self.rtmp_url {
            variants.push(StreamVariant {
                url: rtmp_url.clone(),
                format: Format::RTMP,
                codec: Codec::AVC,
                quality: Quality::Origin,
                bitrate: None,
            });
        }

        if let Some(hls_url) = &self.hls_url {
            variants.push(StreamVariant {
                url: hls_url.clone(),
                format: Format::HLS,
                codec: Codec::AVC,
                quality: Quality::Origin,
                bitrate: None,
            });
        }

        variants
    }

    fn expires_at(&self) -> Option<i64> {
        None
    }

    fn cdn_nodes(&self) -> Vec<CdnNode> {
        Vec::new()
    }

    fn platform(&self) -> PlatformType {
        PlatformType::TikTok
    }
}

#[derive(Clone, Debug)]
pub struct DanmuFetchResult {
    pub messages: Vec<String>,
    pub next_cursor: Option<String>,
    pub fetch_interval_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TikTokProxyCheck {
    pub ok: bool,
    pub status: u16,
    pub waf: bool,
    pub proxy: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TikTokCookieCheck {
    pub ok: bool,
    pub user_id: String,
    pub user_name: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TikTokQrInfo {
    pub oauth_key: String,
    pub url: String,
    pub image: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TikTokQrStatus {
    pub code: u8,
    pub cookies: String,
    pub message: String,
}

fn normalize_proxy_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("socks5://")
        || trimmed.starts_with("socks5h://")
    {
        return Some(trimmed.to_string());
    }
    Some(format!("http://{trimmed}"))
}

pub fn proxy_url_from_env() -> Option<String> {
    for key in ["TIKTOK_PROXY_URL", "tiktok_proxy_url"] {
        if let Ok(value) = env::var(key) {
            if let Some(url) = normalize_proxy_url(&value) {
                return Some(url);
            }
        }
    }
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = env::var(key) {
            if let Some(url) = normalize_proxy_url(&value) {
                return Some(url);
            }
        }
    }
    None
}

pub fn build_proxy_client(proxy_url: &str) -> Result<Client, RecorderError> {
    let proxy = Proxy::all(proxy_url).map_err(|_| RecorderError::ApiError {
        error: "Invalid proxy URL".to_string(),
    })?;
    Client::builder()
        .proxy(proxy)
        .http1_only()
        .build()
        .map_err(|_| RecorderError::ApiError {
            error: "Failed to build proxy client".to_string(),
        })
}

fn extract_script_json(html_str: &str, script_id: &str) -> Option<String> {
    let pattern = format!(r#"(?s)<script[^>]*id=['"]{script_id}['"][^>]*>(.*?)</script>"#);
    let regex = Regex::new(&pattern).ok()?;
    regex
        .captures(html_str)
        .and_then(|cap| cap.get(1))
        .map(|value| value.as_str().to_string())
}

fn parse_first_json_value(raw: &str) -> Option<Value> {
    let start = raw.find('{').or_else(|| raw.find('['))?;
    let candidate = &raw[start..];
    let mut deserializer = serde_json::Deserializer::from_str(candidate);
    Value::deserialize(&mut deserializer).ok()
}

fn extract_json_after_marker(html_str: &str, marker: &str) -> Option<Value> {
    for (index, _) in html_str.match_indices(marker) {
        let slice = &html_str[index + marker.len()..];
        if let Some(value) = parse_first_json_value(slice) {
            return Some(value);
        }
    }
    None
}

fn is_waf_wait_page(html_str: &str) -> bool {
    let lower = html_str.to_ascii_lowercase();
    lower.contains("slardar-config")
        || lower.contains("slardarwaf")
        || lower.contains("_wafchallengeid")
        || lower.contains("please wait")
}

fn extract_state_value(html_str: &str) -> Option<Value> {
    let script_ids = [
        "SIGI_STATE",
        "__UNIVERSAL_DATA_FOR_REHYDRATION__",
        "__NEXT_DATA__",
    ];
    for script_id in script_ids {
        if let Some(json_str) = extract_script_json(html_str, script_id) {
            if let Some(parsed) = parse_first_json_value(&json_str) {
                return Some(parsed);
            }
        }
    }

    for marker in script_ids {
        if let Some(parsed) = extract_json_after_marker(html_str, marker) {
            return Some(parsed);
        }
    }

    // Try regex for window assignments
    let patterns = [
        r#"(?s)window\['SIGI_STATE'\]\s*=\s*(.*?);\s*window"#,
        r#"(?s)window\['SIGI_STATE'\]\s*=\s*(.*?);\s*</script>"#,
        r#"(?s)window\.SIGI_STATE\s*=\s*(.*?);\s*window"#,
        r#"(?s)window\.__UNIVERSAL_DATA_FOR_REHYDRATION__\s*=\s*(.*?);\s*window"#,
        r#"(?s)window\.__UNIVERSAL_DATA_FOR_REHYDRATION__\s*=\s*(.*?);\s*</script>"#,
    ];

    for pattern in patterns {
        if let Ok(regex) = Regex::new(pattern) {
            if let Some(cap) = regex.captures(html_str) {
                if let Some(json_str) = cap.get(1) {
                    if let Some(parsed) = parse_first_json_value(json_str.as_str()) {
                        return Some(parsed);
                    }
                }
            }
        }
    }

    None
}

fn decode_json_string(raw: &str) -> Option<String> {
    serde_json::from_str::<String>(&format!("\"{raw}\""))
        .ok()
        .or_else(|| {
            let decoded = raw
                .replace("\\u002F", "/")
                .replace("\\/", "/")
                .replace("\\u0026", "&")
                .replace("\\u003D", "=");
            if decoded == raw {
                None
            } else {
                Some(decoded)
            }
        })
}

fn extract_m3u8_from_html(html_str: &str) -> Option<String> {
    let regex = Regex::new(r#"(https?:\\?/\\?/[^"'\\s]+\\.m3u8[^"'\\s]*)"#).ok()?;
    let raw = regex.captures(html_str)?.get(1)?.as_str();
    let decoded = decode_json_string(raw).unwrap_or_else(|| raw.to_string());
    if decoded.contains(".m3u8") {
        Some(decoded)
    } else {
        None
    }
}

fn gen_device_id() -> String {
    let mut rng = rand::rng();
    (0..19)
        .map(|_| char::from(b'0' + rng.random_range(0..10)))
        .collect()
}

fn build_qr_params(
    token: Option<&str>,
    device_id: &str,
    verify_fp: &str,
    ms_token: &str,
) -> Vec<(String, String)> {
    let mut params = vec![
        ("next".to_string(), "https://www.tiktok.com".to_string()),
        ("multi_login".to_string(), "1".to_string()),
        ("did".to_string(), device_id.to_string()),
        ("locale".to_string(), "zh-Hans".to_string()),
        ("app_language".to_string(), "zh".to_string()),
        ("aid".to_string(), "1459".to_string()),
        ("account_sdk_source".to_string(), "web".to_string()),
        ("sdk_version".to_string(), "2.1.11-tiktokbeta.3".to_string()),
        ("language".to_string(), "zh-Hant".to_string()),
        ("verifyFp".to_string(), verify_fp.to_string()),
        ("target_aid".to_string(), "".to_string()),
        ("standalone_aid".to_string(), "".to_string()),
        ("msToken".to_string(), ms_token.to_string()),
    ];
    if let Some(token) = token {
        params.push(("token".to_string(), token.to_string()));
    }

    let shark_extra = serde_json::json!({
        "aid": 1459,
        "app_name": "Tik_Tok_Login",
        "channel": "tiktok_web",
        "device_platform": "web_pc",
        "device_id": device_id,
        "region": "TW",
        "priority_region": "",
        "os": "windows",
        "referer": "https://www.tiktok.com/",
        "cookie_enabled": true,
        "screen_width": 2560,
        "screen_height": 1440,
        "browser_language": "zh-CN",
        "browser_platform": "Win32",
        "browser_name": "Mozilla",
        "browser_version": "5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36",
        "browser_online": true,
        "verifyFp": verify_fp,
        "app_language": "zh-Hans",
        "webcast_language": "zh-Hans",
        "tz_name": "Asia/Shanghai",
        "is_page_visible": true,
        "focus_state": true,
        "is_fullscreen": false,
        "history_len": 2,
        "user_is_login": false,
        "data_collection_enabled": true
    });
    params.push(("shark_extra".to_string(), shark_extra.to_string()));

    params
}

fn build_query(params: &[(String, String)]) -> String {
    let mut serializer = Serializer::new(String::new());
    for (key, value) in params {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn collect_cookie_map(headers: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for value in headers.get_all(reqwest::header::SET_COOKIE).iter() {
        if let Ok(raw) = value.to_str() {
            if let Some((pair, _)) = raw.split_once(';') {
                if let Some((name, val)) = pair.split_once('=') {
                    map.insert(name.trim().to_string(), val.trim().to_string());
                }
            }
        }
    }
    map
}

fn merge_cookie_maps(target: &mut HashMap<String, String>, extra: HashMap<String, String>) {
    for (key, value) in extra {
        target.insert(key, value);
    }
}

fn build_cookie_string(map: &HashMap<String, String>) -> String {
    let mut pairs: Vec<String> = map
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    pairs.sort();
    pairs.join("; ")
}

fn has_login_cookie(map: &HashMap<String, String>) -> bool {
    map.keys().any(|key| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "sessionid" | "sessionid_ss" | "sid_tt" | "sid_guard" | "uid_tt" | "uid_tt_ss"
        )
    })
}

fn parse_cookie_header(header: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in header.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        if let Some((key, value)) = pair.split_once('=') {
            map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    map
}

fn merge_cookie_pair(map: &mut HashMap<String, String>, key: &str, value: &str) {
    let key = key.trim();
    if key.is_empty() {
        return;
    }
    map.insert(key.to_string(), value.trim().to_string());
}

fn is_cookie_attr_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "path"
            | "domain"
            | "expires"
            | "max-age"
            | "secure"
            | "httponly"
            | "samesite"
            | "priority"
            | "partitioned"
            | "comment"
            | "version"
    )
}

fn merge_cookie_header_string(map: &mut HashMap<String, String>, raw: &str) {
    for part in raw.split(';').map(str::trim) {
        if part.is_empty() {
            continue;
        }
        if let Some((name, value)) = part.split_once('=') {
            if is_cookie_attr_key(name.trim()) {
                continue;
            }
            merge_cookie_pair(map, name, value);
        }
    }
}

fn merge_set_cookie_line(map: &mut HashMap<String, String>, raw: &str) {
    if let Some(pair) = raw.split(';').next() {
        if let Some((name, value)) = pair.split_once('=') {
            merge_cookie_pair(map, name, value);
        }
    }
}

fn merge_cookie_map_from_url(map: &mut HashMap<String, String>, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() || (!trimmed.starts_with("http://") && !trimmed.starts_with("https://")) {
        return;
    }
    let Ok(url) = Url::parse(trimmed) else {
        return;
    };
    for (k, v) in url.query_pairs() {
        let key_lower = k.to_ascii_lowercase();
        if key_lower.contains("set_cookie") || key_lower.contains("set-cookie") {
            merge_set_cookie_line(map, &v);
        } else if key_lower.contains("cookie") {
            merge_cookie_header_string(map, &v);
        }
    }
    if let Some(fragment) = url.fragment() {
        for (k, v) in url::form_urlencoded::parse(fragment.as_bytes()) {
            let key_lower = k.to_ascii_lowercase();
            if key_lower.contains("set_cookie") || key_lower.contains("set-cookie") {
                merge_set_cookie_line(map, &v);
            } else if key_lower.contains("cookie") {
                merge_cookie_header_string(map, &v);
            }
        }
    }
}

fn merge_cookie_map_from_json_inner(
    map: &mut HashMap<String, String>,
    value: &Value,
    key_hint: Option<&str>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Object(obj) => {
            let hint_is_cookie = key_hint
                .map(|v| v.to_ascii_lowercase().contains("cookie"))
                .unwrap_or(false);
            let looks_cookie_obj = obj.contains_key("domain")
                || obj.contains_key("path")
                || obj.contains_key("expires")
                || obj.contains_key("httpOnly")
                || obj.contains_key("sameSite");
            if hint_is_cookie || looks_cookie_obj {
                if let (Some(name), Some(val)) = (
                    obj.get("name").and_then(Value::as_str),
                    obj.get("value").and_then(Value::as_str),
                ) {
                    merge_cookie_pair(map, name, val);
                }
            }
            if let (Some(name), Some(val)) = (
                obj.get("cookieName").and_then(Value::as_str),
                obj.get("cookieValue").and_then(Value::as_str),
            ) {
                merge_cookie_pair(map, name, val);
            }
            for (key, child) in obj {
                merge_cookie_map_from_json_inner(map, child, Some(key), depth + 1);
            }
        }
        Value::Array(items) => {
            for item in items {
                merge_cookie_map_from_json_inner(map, item, key_hint, depth + 1);
            }
        }
        Value::String(raw) => {
            let hint = key_hint.unwrap_or_default().to_ascii_lowercase();
            if hint.contains("set_cookie") || hint.contains("set-cookie") || hint == "setcookie" {
                merge_set_cookie_line(map, raw);
                return;
            }
            if hint.contains("cookie") {
                merge_cookie_header_string(map, raw);
                return;
            }
            if hint == "location"
                || hint.ends_with("url")
                || hint.ends_with("uri")
                || raw.starts_with("http://")
                || raw.starts_with("https://")
            {
                merge_cookie_map_from_url(map, raw);
            }
        }
        _ => {}
    }
}

fn merge_cookie_map_from_json(map: &mut HashMap<String, String>, value: &Value) {
    merge_cookie_map_from_json_inner(map, value, None, 0);
}

fn to_iso_8859_1_bytes(value: &str) -> Vec<u8> {
    value
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if code <= 0xFF {
                code as u8
            } else {
                b'?'
            }
        })
        .collect()
}

fn mssdk_url() -> String {
    env::var("TIKTOK_MSSDK_URL").unwrap_or_else(|_| TIKTOK_MSSDK_URL.to_string())
}

fn apply_tiktok_extra_headers(headers: &mut HeaderMap) {
    let mappings = [
        ("TIKTOK_X_MSSDK_INFO", "x-mssdk-info"),
        (
            "TIKTOK_TT_TICKET_GUARD_ITERATION_VERSION",
            "tt-ticket-guard-iteration-version",
        ),
        (
            "TIKTOK_TT_TICKET_GUARD_PUBLIC_KEY",
            "tt-ticket-guard-public-key",
        ),
        ("TIKTOK_TT_TICKET_GUARD_VERSION", "tt-ticket-guard-version"),
        (
            "TIKTOK_TT_TICKET_GUARD_WEB_VERSION",
            "tt-ticket-guard-web-version",
        ),
    ];
    if let Ok(value) = env::var("TIKTOK_TT_TICKET_GUARD_CLIENT_DATA") {
        let value = value.trim();
        if !value.is_empty() {
            let encoded = if value.starts_with('{') || value.starts_with('[') {
                let bytes = to_iso_8859_1_bytes(value);
                general_purpose::STANDARD.encode(bytes)
            } else {
                value.to_string()
            };
            if let Ok(parsed) = encoded.parse() {
                headers.insert("tt-ticket-guard-client-data", parsed);
            }
        }
    }
    for (env_key, header_name) in mappings {
        if let Ok(value) = env::var(env_key) {
            let value = value.trim();
            if !value.is_empty() {
                if let Ok(parsed) = value.parse() {
                    headers.insert(header_name, parsed);
                }
            }
        }
    }
}

fn apply_browser_headers(headers: &mut HeaderMap, mobile: bool) {
    let defaults = [
        (
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        ),
        ("accept-language", "en-US,en;q=0.9"),
        (
            "sec-ch-ua",
            "\"Chromium\";v=\"141\", \"Not?A_Brand\";v=\"8\", \"Google Chrome\";v=\"141\"",
        ),
        ("sec-ch-ua-mobile", if mobile { "?1" } else { "?0" }),
        ("sec-ch-ua-platform", "\"Windows\""),
        ("sec-fetch-dest", "document"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-site", "none"),
        ("sec-fetch-user", "?1"),
        ("upgrade-insecure-requests", "1"),
    ];
    for (key, value) in defaults {
        if let Ok(parsed) = value.parse() {
            headers.insert(key, parsed);
        }
    }
}

fn build_page_headers(url: &str, user_agent: &str, mobile: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("User-Agent", user_agent.parse().unwrap());
    if let Ok(parsed) = url.parse() {
        headers.insert("referer", parsed);
    }
    apply_browser_headers(&mut headers, mobile);
    headers
}

fn build_feed_headers(account: &Account) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", FEED_USER_AGENT.parse().unwrap());
    headers.insert("accept", "*/*".parse().unwrap());
    headers.insert(
        "accept-language",
        "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap(),
    );
    headers.insert("origin", "https://www.tiktok.com".parse().unwrap());
    headers.insert(
        "sec-ch-ua",
        "\"Not)A;Brand\";v=\"8\", \"Chromium\";v=\"138\", \"Google Chrome\";v=\"138\""
            .parse()
            .unwrap(),
    );
    headers.insert("sec-ch-ua-mobile", "?0".parse().unwrap());
    headers.insert("sec-ch-ua-platform", "\"Windows\"".parse().unwrap());
    headers.insert("sec-fetch-dest", "empty".parse().unwrap());
    headers.insert("sec-fetch-mode", "cors".parse().unwrap());
    headers.insert("sec-fetch-site", "same-site".parse().unwrap());
    headers.insert("priority", "u=1, i".parse().unwrap());
    headers.insert("referer", "https://www.tiktok.com/".parse().unwrap());
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }
    apply_tiktok_extra_headers(&mut headers);
    headers
}

fn build_im_headers(account: &Account) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let user_agent =
        env::var("TIKTOK_FEED_USER_AGENT").unwrap_or_else(|_| FEED_USER_AGENT.to_string());
    headers.insert("user-agent", user_agent.parse().unwrap());
    headers.insert("accept", "application/json".parse().unwrap());
    headers.insert(
        "accept-language",
        "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap(),
    );
    headers.insert(
        "sec-ch-ua",
        "\"Not)A;Brand\";v=\"8\", \"Chromium\";v=\"138\", \"Google Chrome\";v=\"138\""
            .parse()
            .unwrap(),
    );
    headers.insert("sec-ch-ua-mobile", "?0".parse().unwrap());
    headers.insert("sec-ch-ua-platform", "\"Windows\"".parse().unwrap());
    headers.insert("sec-fetch-dest", "empty".parse().unwrap());
    headers.insert("sec-fetch-mode", "cors".parse().unwrap());
    headers.insert("sec-fetch-site", "same-site".parse().unwrap());
    headers.insert("priority", "u=1, i".parse().unwrap());
    headers.insert("referer", "https://www.tiktok.com/".parse().unwrap());
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }
    apply_tiktok_extra_headers(&mut headers);
    headers
}

fn build_check_alive_headers(account: &Account, referer: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", FEED_USER_AGENT.parse().unwrap());
    headers.insert("accept", "*/*".parse().unwrap());
    headers.insert(
        "accept-language",
        "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap(),
    );
    headers.insert("origin", "https://www.tiktok.com".parse().unwrap());
    headers.insert(
        "sec-ch-ua",
        "\"Not)A;Brand\";v=\"8\", \"Chromium\";v=\"138\", \"Google Chrome\";v=\"138\""
            .parse()
            .unwrap(),
    );
    headers.insert("sec-ch-ua-mobile", "?0".parse().unwrap());
    headers.insert("sec-ch-ua-platform", "\"Windows\"".parse().unwrap());
    headers.insert("sec-fetch-dest", "empty".parse().unwrap());
    headers.insert("sec-fetch-mode", "cors".parse().unwrap());
    headers.insert("sec-fetch-site", "same-site".parse().unwrap());
    headers.insert("priority", "u=1, i".parse().unwrap());
    if let Ok(parsed) = referer.parse() {
        headers.insert("referer", parsed);
    }
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }
    apply_tiktok_extra_headers(&mut headers);
    headers
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn extract_chat_text(value: &Value) -> Option<String> {
    const TEXT_KEYS: [&str; 12] = [
        "content",
        "text",
        "message",
        "msg",
        "comment",
        "display_text",
        "displayText",
        "body",
        "content_text",
        "text_content",
        "prompt",
        "chat",
    ];
    const NESTED_KEYS: [&str; 9] = [
        "message", "payload", "data", "content", "text", "comment", "msg", "body", "extras",
    ];

    fn inner(value: &Value, depth: usize) -> Option<String> {
        if depth > 4 {
            return None;
        }
        match value {
            Value::String(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            Value::Number(value) => Some(value.to_string()),
            Value::Array(values) => {
                for item in values {
                    if let Some(text) = inner(item, depth + 1) {
                        return Some(text);
                    }
                }
                None
            }
            Value::Object(map) => {
                for key in TEXT_KEYS {
                    if let Some(value) = map.get(key) {
                        if let Some(text) = inner(value, depth + 1) {
                            return Some(text);
                        }
                    }
                }
                for key in NESTED_KEYS {
                    if let Some(value) = map.get(key) {
                        if let Some(text) = inner(value, depth + 1) {
                            return Some(text);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    inner(value, 0)
}

fn build_im_fetch_url(room_id: &str, cursor: Option<&str>, account: &Account) -> String {
    let device_id = env::var("TIKTOK_FEED_DEVICE_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(gen_device_id);
    let verify_fp = env::var("TIKTOK_FEED_VERIFY_FP")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "s_v_web_id"))
        .unwrap_or_default();
    let ms_token = env::var("TIKTOK_FEED_MS_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "msToken"))
        .unwrap_or_default();
    let mut params = vec![
        ("version_code".to_string(), "270000".to_string()),
        ("device_platform".to_string(), "web".to_string()),
        ("aid".to_string(), "1988".to_string()),
        ("app_name".to_string(), "tiktok_web".to_string()),
        ("app_language".to_string(), "zh-Hans".to_string()),
        ("browser_online".to_string(), "true".to_string()),
        ("browser_language".to_string(), "zh-CN".to_string()),
        ("browser_name".to_string(), "Mozilla".to_string()),
        ("browser_platform".to_string(), "Win32".to_string()),
        ("browser_version".to_string(), FEED_USER_AGENT.to_string()),
        ("screen_width".to_string(), "2560".to_string()),
        ("screen_height".to_string(), "1440".to_string()),
        ("tz_name".to_string(), "Asia/Shanghai".to_string()),
        ("ws_direct".to_string(), "1".to_string()),
        ("channel".to_string(), "tiktok_web".to_string()),
        ("cookie_enabled".to_string(), "true".to_string()),
        ("device_id".to_string(), device_id),
        ("is_page_visible".to_string(), "true".to_string()),
        ("priority_region".to_string(), "JP".to_string()),
        ("region".to_string(), "JP".to_string()),
        ("live_id".to_string(), "12".to_string()),
        ("client_enter".to_string(), "1".to_string()),
        ("room_id".to_string(), room_id.to_string()),
        ("identity".to_string(), "audience".to_string()),
        ("history_comment_count".to_string(), "6".to_string()),
        ("fetch_rule".to_string(), "1".to_string()),
        ("last_rtt".to_string(), "-1".to_string()),
        ("internal_ext".to_string(), "0".to_string()),
        ("sup_ws_ds_opt".to_string(), "1".to_string()),
        ("resp_content_type".to_string(), "json".to_string()),
        ("resp_type".to_string(), "json".to_string()),
        ("verifyFp".to_string(), verify_fp),
        ("msToken".to_string(), ms_token),
    ];
    if let Some(cursor) = cursor {
        params.push(("cursor".to_string(), cursor.to_string()));
    } else {
        params.push(("cursor".to_string(), "0".to_string()));
    }
    let query = build_query(&params);
    let mut url = format!("{TIKTOK_IM_FETCH_URL}?{query}");
    if let Some(x_gnarly) = resolve_x_gnarly(&query, "", FEED_USER_AGENT) {
        url.push_str("&X-Gnarly=");
        url.push_str(&x_gnarly);
    }
    if !url.contains("X-Bogus=") {
        let xbogus = XBogus::new(FEED_USER_AGENT).get_x_bogus(&url);
        url.push_str("&X-Bogus=");
        url.push_str(&xbogus);
    }
    url
}

pub async fn fetch_danmu_json(
    client: &Client,
    account: &Account,
    room_id: &str,
    cursor: Option<&str>,
) -> Result<DanmuFetchResult, RecorderError> {
    let url = build_im_fetch_url(room_id, cursor, account);
    let headers = build_im_headers(account);
    let response = client.get(url).headers(headers).send().await?;
    if !response.status().is_success() {
        return Err(RecorderError::ApiError {
            error: format!("TikTok danmu status: {}", response.status()),
        });
    }
    let json: Value = response.json().await.map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse TikTok danmu JSON: {e}"),
    })?;
    let status_code = json
        .get("status_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if status_code != 0 {
        return Err(RecorderError::ApiError {
            error: format!("TikTok danmu error status: {}", status_code),
        });
    }
    let mut texts = Vec::new();
    let data = json.get("data");
    if let Some(messages) = data
        .and_then(|value| value.get("messages"))
        .and_then(|value| value.as_array())
    {
        for message in messages {
            if let Some(text) = extract_chat_text(message) {
                texts.push(text);
            }
        }
    }
    let next_cursor = data
        .and_then(|value| value.get("cursor"))
        .and_then(value_to_string)
        .or_else(|| json.get("cursor").and_then(value_to_string));
    let fetch_interval_ms = data
        .and_then(|value| value.get("fetch_interval"))
        .and_then(|value| value.as_i64())
        .map(|value| value.max(0) as u64);
    Ok(DanmuFetchResult {
        messages: texts,
        next_cursor,
        fetch_interval_ms,
    })
}

fn build_account_info_headers(account: &Account, referer: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", FEED_USER_AGENT.parse().unwrap());
    headers.insert("accept", "*/*".parse().unwrap());
    headers.insert(
        "accept-language",
        "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap(),
    );
    headers.insert("origin", "https://www.tiktok.com".parse().unwrap());
    headers.insert(
        "sec-ch-ua",
        "\"Not)A;Brand\";v=\"8\", \"Chromium\";v=\"138\", \"Google Chrome\";v=\"138\""
            .parse()
            .unwrap(),
    );
    headers.insert("sec-ch-ua-mobile", "?0".parse().unwrap());
    headers.insert("sec-ch-ua-platform", "\"Windows\"".parse().unwrap());
    headers.insert("sec-fetch-dest", "empty".parse().unwrap());
    headers.insert("sec-fetch-mode", "cors".parse().unwrap());
    headers.insert("sec-fetch-site", "same-origin".parse().unwrap());
    headers.insert("priority", "u=1, i".parse().unwrap());
    if let Ok(parsed) = referer.parse() {
        headers.insert("referer", parsed);
    }
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }
    apply_tiktok_extra_headers(&mut headers);
    headers
}

fn build_room_enter_headers(account: &Account, referer: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", FEED_USER_AGENT.parse().unwrap());
    headers.insert("accept", "*/*".parse().unwrap());
    headers.insert(
        "accept-language",
        "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap(),
    );
    headers.insert("origin", "https://www.tiktok.com".parse().unwrap());
    headers.insert(
        "sec-ch-ua",
        "\"Not)A;Brand\";v=\"8\", \"Chromium\";v=\"138\", \"Google Chrome\";v=\"138\""
            .parse()
            .unwrap(),
    );
    headers.insert("sec-ch-ua-mobile", "?0".parse().unwrap());
    headers.insert("sec-ch-ua-platform", "\"Windows\"".parse().unwrap());
    headers.insert("sec-fetch-dest", "empty".parse().unwrap());
    headers.insert("sec-fetch-mode", "cors".parse().unwrap());
    headers.insert("sec-fetch-site", "same-site".parse().unwrap());
    headers.insert(
        "content-type",
        "application/x-www-form-urlencoded; charset=UTF-8"
            .parse()
            .unwrap(),
    );
    headers.insert("priority", "u=1, i".parse().unwrap());
    if let Ok(parsed) = referer.parse() {
        headers.insert("referer", parsed);
    }
    if let Some(token) = build_secsdk_csrf_token(account) {
        if let Ok(parsed) = token.parse() {
            headers.insert("x-secsdk-csrf-token", parsed);
        }
    }
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }
    apply_tiktok_extra_headers(&mut headers);
    headers
}

fn encode_url_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn cookie_value_from_account(account: &Account, key: &str) -> Option<String> {
    if account.cookies.is_empty() {
        return None;
    }
    parse_cookie_header(&account.cookies).get(key).cloned()
}

fn apply_feed_template(template: &str, referer: &str, account: &Account) -> String {
    let mut url = template.to_string();
    let root_referer = env::var("TIKTOK_FEED_ROOT_REFERER").unwrap_or_else(|_| referer.to_string());
    let ms_token = env::var("TIKTOK_FEED_MS_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "msToken"))
        .unwrap_or_default();
    let verify_fp = env::var("TIKTOK_FEED_VERIFY_FP")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "s_v_web_id"))
        .unwrap_or_default();
    let device_id = env::var("TIKTOK_FEED_DEVICE_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(gen_device_id);
    let replacements = [
        ("{referer}", encode_url_component(referer)),
        ("{root_referer}", encode_url_component(&root_referer)),
        ("{ms_token}", ms_token),
        ("{verify_fp}", verify_fp),
        ("{device_id}", device_id),
    ];
    for (key, value) in replacements {
        if url.contains(key) {
            url = url.replace(key, &value);
        }
    }
    url
}

fn feed_url_from_env(referer: &str, account: &Account) -> Option<String> {
    if let Ok(value) = env::var("TIKTOK_FEED_URL") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(apply_feed_template(trimmed, referer, account));
        }
    }
    if let Ok(value) = env::var("TIKTOK_FEED_URL_TEMPLATE") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(apply_feed_template(trimmed, referer, account));
        }
    }
    Some(apply_feed_template(
        TIKTOK_FEED_URL_TEMPLATE,
        referer,
        account,
    ))
}

fn strip_empty_query_params(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        return url.to_string();
    };
    let mut serializer = Serializer::new(String::new());
    for (key, value) in parsed.query_pairs() {
        let value = value.to_string();
        if value.is_empty() || value.contains('{') {
            continue;
        }
        serializer.append_pair(&key, &value);
    }
    let new_query = serializer.finish();
    if new_query.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.set_query(Some(&new_query));
    }
    parsed.to_string()
}

fn read_x_gnarly() -> Option<String> {
    if let Some(value) = read_tiktok_web_value("x_gnarly") {
        return Some(value);
    }
    let candidates = ["TIKTOK_X_GNARLY", "TIKTOK_FEED_X_GNARLY"];
    for key in candidates {
        if let Ok(value) = env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn resolve_x_gnarly(query: &str, body: &str, user_agent: &str) -> Option<String> {
    if let Some(value) = read_x_gnarly() {
        return Some(value);
    }
    if query.trim().is_empty() {
        return None;
    }
    let signing_ua = read_tiktok_web_value("user_agent").unwrap_or_else(|| user_agent.to_string());
    Some(x_gnarly::sign(query, body, &signing_ua))
}

fn reverse_generate_root() -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    let root = if cwd.file_name().and_then(|s| s.to_str()) == Some("src-tauri") {
        cwd.parent().unwrap_or(&cwd).to_path_buf()
    } else {
        cwd
    };
    Some(
        root.join("src-tauri")
            .join("crates")
            .join("recorder")
            .join("src")
            .join("ReverseGenerate"),
    )
}

fn read_tiktok_web_value(key: &str) -> Option<String> {
    let root = reverse_generate_root()?;
    let path = root.join("tiktok_web.toml");
    let raw = fs::read_to_string(path).ok()?;
    let parsed: TomlValue = raw.parse().ok()?;
    let table = parsed
        .get("tiktok_web")
        .and_then(|v| v.as_table())
        .or_else(|| parsed.as_table())?;
    let value = table.get(key)?.as_str()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn build_live_referer(handle: &str) -> String {
    if handle.is_empty() {
        "https://www.tiktok.com/".to_string()
    } else {
        format!("https://www.tiktok.com/@{handle}/live")
    }
}

fn build_profile_referer(handle: &str) -> String {
    if handle.is_empty() {
        "https://www.tiktok.com/".to_string()
    } else {
        format!("https://www.tiktok.com/@{handle}")
    }
}

fn extract_room_id_from_state(value: &Value) -> Option<String> {
    let room_id = serde_json::from_value::<SigiStateResponse>(value.clone())
        .ok()
        .and_then(|state| state.room_store)
        .and_then(|store| store.room_info)
        .and_then(|info| info.room_id);
    if let Some(room_id) = room_id {
        if room_id.chars().all(|c| c.is_ascii_digit()) {
            return Some(room_id);
        }
    }

    find_room_info(value)
        .and_then(|info| info.room_id)
        .filter(|room_id| room_id.chars().all(|c| c.is_ascii_digit()))
}

fn extract_room_id_from_check_alive(value: &Value) -> Option<String> {
    let mut fallback = None;
    let mut saw_status = false;
    let entries: Vec<Value> = match value.get("data") {
        Some(Value::Array(values)) => values.clone(),
        Some(Value::Object(map)) => vec![Value::Object(map.clone())],
        _ => Vec::new(),
    };
    for entry in entries {
        let map = match entry.as_object() {
            Some(map) => map,
            None => continue,
        };
        let room_id = get_string_field(map, &["room_id", "roomId", "id", "id_str"]);
        let Some(room_id) = room_id else {
            continue;
        };
        if fallback.is_none() {
            fallback = Some(room_id.clone());
        }

        let status = get_i64_field(map, &["status", "live_status", "liveStatus"]);
        if let Some(status) = status {
            saw_status = true;
            if status == 1 || status == 2 {
                return Some(room_id);
            }
            continue;
        }
        let alive = map
            .get("is_alive")
            .and_then(Value::as_i64)
            .map(|v| v == 1)
            .or_else(|| map.get("alive").and_then(Value::as_bool))
            .or_else(|| map.get("isLive").and_then(Value::as_bool));
        if alive.is_some() {
            saw_status = true;
        }
        if alive == Some(true) {
            return Some(room_id);
        }
    }

    if saw_status {
        return None;
    }
    fallback
}

fn collect_check_alive_room_ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let entries: Vec<Value> = match value.get("data") {
        Some(Value::Array(values)) => values.clone(),
        Some(Value::Object(map)) => vec![Value::Object(map.clone())],
        _ => Vec::new(),
    };
    for entry in entries {
        let map = match entry.as_object() {
            Some(map) => map,
            None => continue,
        };
        if let Some(room_id) = get_string_field(map, &["room_id", "roomId", "id", "id_str"]) {
            ids.push(room_id);
        }
    }
    ids
}

fn build_check_alive_url(referer: &str, room_ids: &[String]) -> Result<String, RecorderError> {
    if room_ids.is_empty() {
        return Err(RecorderError::ApiError {
            error: "TikTok check_alive missing room ids".to_string(),
        });
    }
    let device_id = env::var("TIKTOK_FEED_DEVICE_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(gen_device_id);
    let root_referer = env::var("TIKTOK_FEED_ROOT_REFERER").unwrap_or_else(|_| referer.to_string());
    let params = vec![
        ("aid".to_string(), "1988".to_string()),
        ("app_language".to_string(), "zh-Hans".to_string()),
        ("app_name".to_string(), "tiktok_web".to_string()),
        ("browser_language".to_string(), "zh-CN".to_string()),
        ("browser_name".to_string(), "Mozilla".to_string()),
        ("browser_online".to_string(), "true".to_string()),
        ("browser_platform".to_string(), "Win32".to_string()),
        ("browser_version".to_string(), FEED_USER_AGENT.to_string()),
        ("channel".to_string(), "tiktok_web".to_string()),
        ("cookie_enabled".to_string(), "true".to_string()),
        ("data_collection_enabled".to_string(), "true".to_string()),
        ("device_id".to_string(), device_id),
        ("device_platform".to_string(), "web_pc".to_string()),
        ("focus_state".to_string(), "false".to_string()),
        ("from_page".to_string(), "".to_string()),
        ("history_len".to_string(), "7".to_string()),
        ("is_fullscreen".to_string(), "false".to_string()),
        ("is_page_visible".to_string(), "true".to_string()),
        ("os".to_string(), "windows".to_string()),
        ("priority_region".to_string(), "JP".to_string()),
        ("referer".to_string(), referer.to_string()),
        ("region".to_string(), "JP".to_string()),
        ("room_ids".to_string(), room_ids.join(",")),
        ("root_referer".to_string(), root_referer),
    ];
    let query = build_query(&params);
    let mut url = format!("{TIKTOK_ROOM_CHECK_ALIVE_URL}?{query}");
    if let Some(x_gnarly) = resolve_x_gnarly(&query, "", FEED_USER_AGENT) {
        url.push_str("&X-Gnarly=");
        url.push_str(&x_gnarly);
    }
    if !url.contains("X-Bogus=") {
        let xbogus = XBogus::new(FEED_USER_AGENT).get_x_bogus(&url);
        url.push_str("&X-Bogus=");
        url.push_str(&xbogus);
    }
    Ok(strip_empty_query_params(&url))
}

fn build_room_enter_url(referer: &str, account: &Account, body: &str) -> String {
    let device_id = env::var("TIKTOK_FEED_DEVICE_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(gen_device_id);
    let root_referer = env::var("TIKTOK_FEED_ROOT_REFERER")
        .unwrap_or_else(|_| "https://www.google.com/".to_string());
    let verify_fp = env::var("TIKTOK_FEED_VERIFY_FP")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "s_v_web_id"))
        .unwrap_or_default();
    let ms_token = env::var("TIKTOK_FEED_MS_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "msToken"))
        .unwrap_or_default();
    let params = vec![
        ("aid".to_string(), "1988".to_string()),
        ("app_language".to_string(), "zh-Hans".to_string()),
        ("app_name".to_string(), "tiktok_web".to_string()),
        ("browser_language".to_string(), "zh-CN".to_string()),
        ("browser_name".to_string(), "Mozilla".to_string()),
        ("browser_online".to_string(), "true".to_string()),
        ("browser_platform".to_string(), "Win32".to_string()),
        ("browser_version".to_string(), FEED_USER_AGENT.to_string()),
        ("channel".to_string(), "tiktok_web".to_string()),
        ("cookie_enabled".to_string(), "true".to_string()),
        ("data_collection_enabled".to_string(), "true".to_string()),
        ("device_id".to_string(), device_id),
        ("device_platform".to_string(), "web_pc".to_string()),
        ("device_type".to_string(), "web_h265".to_string()),
        ("focus_state".to_string(), "true".to_string()),
        ("from_page".to_string(), "".to_string()),
        ("history_len".to_string(), "13".to_string()),
        ("is_fullscreen".to_string(), "false".to_string()),
        ("is_page_visible".to_string(), "true".to_string()),
        ("os".to_string(), "windows".to_string()),
        ("priority_region".to_string(), "JP".to_string()),
        ("referer".to_string(), referer.to_string()),
        ("region".to_string(), "JP".to_string()),
        ("root_referer".to_string(), root_referer),
        ("screen_height".to_string(), "1440".to_string()),
        ("screen_width".to_string(), "2560".to_string()),
        ("tz_name".to_string(), "Asia/Shanghai".to_string()),
        ("user_is_login".to_string(), "true".to_string()),
        ("verifyFp".to_string(), verify_fp),
        ("webcast_language".to_string(), "zh-Hans".to_string()),
        ("msToken".to_string(), ms_token),
    ];
    let query = build_query(&params);
    let mut url = format!("{TIKTOK_ROOM_ENTER_URL}?{query}");
    if let Some(x_gnarly) = resolve_x_gnarly(&query, body, FEED_USER_AGENT) {
        url.push_str("&X-Gnarly=");
        url.push_str(&x_gnarly);
    }
    let xbogus = XBogus::new(FEED_USER_AGENT).get_x_bogus(&url);
    url.push_str("&X-Bogus=");
    url.push_str(&xbogus);
    strip_empty_query_params(&url)
}

fn build_secsdk_csrf_token(account: &Account) -> Option<String> {
    let csrf_token = cookie_value_from_account(account, "csrfToken")?;
    let session_id = cookie_value_from_account(account, "csrf_session_id")?;
    if csrf_token.is_empty() || session_id.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(csrf_token.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    Some(format!("000100000001{digest},{session_id}"))
}

fn build_account_info_url(account: &Account, referer: &str) -> String {
    let device_id = env::var("TIKTOK_FEED_DEVICE_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(gen_device_id);
    let root_referer = env::var("TIKTOK_FEED_ROOT_REFERER")
        .unwrap_or_else(|_| "https://www.google.com/".to_string());
    let verify_fp = env::var("TIKTOK_FEED_VERIFY_FP")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "s_v_web_id"))
        .unwrap_or_default();
    let ms_token = env::var("TIKTOK_FEED_MS_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cookie_value_from_account(account, "msToken"))
        .unwrap_or_default();
    let params = vec![
        ("aid".to_string(), "1459".to_string()),
        ("app_language".to_string(), "zh".to_string()),
        ("app_name".to_string(), "tiktok_web".to_string()),
        ("browser_language".to_string(), "zh-CN".to_string()),
        ("browser_name".to_string(), "Mozilla".to_string()),
        ("browser_online".to_string(), "true".to_string()),
        ("browser_platform".to_string(), "Win32".to_string()),
        ("browser_version".to_string(), FEED_USER_AGENT.to_string()),
        ("channel".to_string(), "tiktok_web".to_string()),
        ("cookie_enabled".to_string(), "true".to_string()),
        ("data_collection_enabled".to_string(), "true".to_string()),
        ("device_id".to_string(), device_id),
        ("device_platform".to_string(), "web_pc".to_string()),
        ("focus_state".to_string(), "false".to_string()),
        ("from_page".to_string(), "".to_string()),
        ("history_len".to_string(), "7".to_string()),
        ("is_fullscreen".to_string(), "false".to_string()),
        ("is_page_visible".to_string(), "true".to_string()),
        ("locale".to_string(), "zh-Hans".to_string()),
        ("os".to_string(), "windows".to_string()),
        ("priority_region".to_string(), "JP".to_string()),
        ("referer".to_string(), referer.to_string()),
        ("region".to_string(), "JP".to_string()),
        ("root_referer".to_string(), root_referer),
        ("screen_height".to_string(), "1440".to_string()),
        ("screen_width".to_string(), "2560".to_string()),
        ("tz_name".to_string(), "Asia/Shanghai".to_string()),
        ("user_is_login".to_string(), "true".to_string()),
        ("verifyFp".to_string(), verify_fp),
        ("webcast_language".to_string(), "zh-Hans".to_string()),
        ("msToken".to_string(), ms_token),
    ];
    let mut url = format!(
        "https://www.tiktok.com/passport/web/account/info/?{}",
        build_query(&params)
    );
    if url.contains("X-Bogus=") {
        url = strip_empty_query_params(&url);
        return url;
    }
    let xbogus = XBogus::new(FEED_USER_AGENT).get_x_bogus(&url);
    url.push_str("&X-Bogus=");
    url.push_str(&xbogus);
    strip_empty_query_params(&url)
}

fn prepare_feed_url(mut url: String) -> Result<String, RecorderError> {
    let user_agent =
        env::var("TIKTOK_FEED_USER_AGENT").unwrap_or_else(|_| FEED_USER_AGENT.to_string());
    let query_for_sign = extract_sign_query(&url);
    let computed_x_gnarly = query_for_sign
        .as_deref()
        .and_then(|query| resolve_x_gnarly(query, "", &user_agent));
    let x_gnarly_value = read_x_gnarly().or(computed_x_gnarly);
    if url.contains("{x_gnarly}") {
        if let Some(x_gnarly) = x_gnarly_value.as_deref() {
            url = url.replace("{x_gnarly}", x_gnarly);
        } else {
            url = url.replace("{x_gnarly}", "");
        }
    } else if let Some(x_gnarly) = x_gnarly_value.as_deref() {
        url.push_str("&X-Gnarly=");
        url.push_str(x_gnarly);
    }
    if let Ok(value) = env::var("TIKTOK_FEED_X_BOGUS") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            url = url.replace("{x_bogus}", trimmed);
        }
    }

    if url.contains("{x_bogus}") {
        let x_gnarly_fallback = env::var("TIKTOK_FEED_X_GNARLY").unwrap_or_default();
        let sign_seed = url.replace("{x_bogus}", "0").replace(
            "{x_gnarly}",
            if x_gnarly_fallback.is_empty() {
                "0"
            } else {
                x_gnarly_fallback.as_str()
            },
        );
        let sign_target = if let Ok(mut parsed) = Url::parse(&sign_seed) {
            let mut serializer = Serializer::new(String::new());
            for (key, value) in parsed.query_pairs() {
                if key != "X-Bogus" {
                    serializer.append_pair(&key, &value);
                }
            }
            let new_query = serializer.finish();
            if new_query.is_empty() {
                parsed.set_query(None);
            } else {
                parsed.set_query(Some(&new_query));
            }
            parsed.to_string()
        } else {
            sign_seed
                .replace("&X-Bogus=0", "")
                .replace("?X-Bogus=0", "")
        };
        let xbogus = XBogus::new(&user_agent).get_x_bogus(&sign_target);
        url = url.replace("{x_bogus}", &xbogus);
    }

    Ok(strip_empty_query_params(&url))
}

fn extract_sign_query(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let mut serializer = Serializer::new(String::new());
    for (key, value) in parsed.query_pairs() {
        if key != "X-Bogus" && key != "X-Gnarly" {
            serializer.append_pair(&key, &value);
        }
    }
    Some(serializer.finish())
}

fn build_feed_url(referer: &str, account: &Account) -> Result<String, RecorderError> {
    let Some(template) = feed_url_from_env(referer, account) else {
        return Err(RecorderError::ApiError {
            error: "TikTok feed URL not configured".to_string(),
        });
    };
    prepare_feed_url(template)
}

fn build_feed_url_override(
    referer: &str,
    account: &Account,
    override_url: Option<&str>,
) -> Result<String, RecorderError> {
    if let Some(override_url) = override_url {
        let trimmed = override_url.trim();
        if !trimmed.is_empty() {
            return prepare_feed_url(apply_feed_template(trimmed, referer, account));
        }
    }
    build_feed_url(referer, account)
}

fn build_room_info_url(room_id: &str) -> String {
    format!("{TIKTOK_ROOM_INFO_URL}?aid=1988&room_id={room_id}")
}

fn to_mobile_url(url: &str) -> String {
    if url.contains("m.tiktok.com") {
        return url.to_string();
    }
    url.replace("www.tiktok.com", "m.tiktok.com")
}

fn force_mobile_fallback() -> bool {
    std::env::var("LIVE_TIKTOK_FORCE_MOBILE")
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

async fn warm_up_live_pages(client: &Client, account: &Account) {
    let warmup_urls = ["https://www.tiktok.com/", "https://www.tiktok.com/live"];
    for url in warmup_urls {
        let mut headers = build_page_headers(url, USER_AGENT, false);
        headers.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
        if !account.cookies.is_empty() {
            headers.insert("Cookie", account.cookies.parse().unwrap());
        }

        for attempt in 0..2 {
            let response = match client.get(url).headers(headers.clone()).send().await {
                Ok(response) => response,
                Err(_) => break,
            };
            let status = response.status();
            let html_str = match response.text().await {
                Ok(html_str) => html_str,
                Err(_) => break,
            };
            if !status.is_success() {
                break;
            }
            if is_waf_wait_page(&html_str) && attempt == 0 {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }
            break;
        }
    }
}

async fn fetch_ms_token(client: &Client) -> String {
    let payload = serde_json::json!({
        "magic": TIKTOK_MSSDK_MAGIC,
        "version": TIKTOK_MSSDK_VERSION,
        "dataType": TIKTOK_MSSDK_DATA_TYPE,
        "strData": TIKTOK_MSSDK_STR_DATA,
        "tspFromClient": chrono::Utc::now().timestamp_millis(),
    });
    let resp = client
        .post(mssdk_url())
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .await;
    if let Ok(resp) = resp {
        if let Some(token) = collect_cookie_map(resp.headers()).get("msToken").cloned() {
            return token;
        }
    }
    crate::platforms::douyin::params::gen_false_ms_token()
}

async fn fetch_ttwid(client: &Client, cookie_header: &str) -> Option<String> {
    let resp = client
        .post(TIKTOK_TTWID_CHECK_URL)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "text/plain")
        .header("Cookie", cookie_header)
        .body(TIKTOK_TTWID_CHECK_DATA)
        .send()
        .await
        .ok()?;
    collect_cookie_map(resp.headers()).get("ttwid").cloned()
}

async fn bootstrap_tiktok_cookies(
    client: &Client,
    verify_fp: &str,
    ms_token_hint: Option<&str>,
) -> HashMap<String, String> {
    let mut cookies = HashMap::new();
    if let Ok(resp) = client
        .get("https://www.tiktok.com/")
        .header("User-Agent", USER_AGENT)
        .header("Referer", "https://www.tiktok.com/")
        .send()
        .await
    {
        merge_cookie_maps(&mut cookies, collect_cookie_map(resp.headers()));
    }

    if !verify_fp.is_empty() {
        cookies.insert("s_v_web_id".to_string(), verify_fp.to_string());
    }

    let ms_token = if let Some(ms_token) = ms_token_hint {
        ms_token.to_string()
    } else if let Some(ms_token) = cookies.get("msToken").cloned() {
        ms_token
    } else {
        fetch_ms_token(client).await
    };
    cookies.insert("msToken".to_string(), ms_token);

    if !cookies.contains_key("ttwid") {
        let cookie_header = build_cookie_string(&cookies);
        if let Some(ttwid) = fetch_ttwid(client, &cookie_header).await {
            cookies.insert("ttwid".to_string(), ttwid);
        }
    }

    cookies
}

fn build_passport_headers(cookies: &HashMap<String, String>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", USER_AGENT.parse().unwrap());
    headers.insert("referer", "https://www.tiktok.com/".parse().unwrap());
    let cookie_header = build_cookie_string(cookies);
    if !cookie_header.is_empty() {
        headers.insert("cookie", cookie_header.parse().unwrap());
    }
    if let Some(csrf) = cookies.get("passport_csrf_token") {
        headers.insert("x-tt-passport-csrf-token", csrf.parse().unwrap());
    }
    headers
}

fn build_auth_broadcast_query(device_id: &str, verify_fp: &str, ms_token: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let params = vec![
        ("WebIdLastTime".to_string(), now.to_string()),
        ("aid".to_string(), "1988".to_string()),
        ("app_language".to_string(), "zh-Hans".to_string()),
        ("app_name".to_string(), "tiktok_web".to_string()),
        ("browser_language".to_string(), "zh-CN".to_string()),
        ("browser_name".to_string(), "Mozilla".to_string()),
        ("browser_online".to_string(), "true".to_string()),
        ("browser_platform".to_string(), "Win32".to_string()),
        ("browser_version".to_string(), USER_AGENT.to_string()),
        ("channel".to_string(), "tiktok_web".to_string()),
        ("cookie_enabled".to_string(), "true".to_string()),
        ("data_collection_enabled".to_string(), "true".to_string()),
        ("device_id".to_string(), device_id.to_string()),
        ("device_platform".to_string(), "web_pc".to_string()),
        ("focus_state".to_string(), "false".to_string()),
        ("history_len".to_string(), "6".to_string()),
        ("is_fullscreen".to_string(), "false".to_string()),
        ("is_page_visible".to_string(), "true".to_string()),
        ("os".to_string(), "windows".to_string()),
        ("region".to_string(), "TW".to_string()),
        ("screen_height".to_string(), "1440".to_string()),
        ("screen_width".to_string(), "2560".to_string()),
        ("tz_name".to_string(), "Asia/Shanghai".to_string()),
        ("user_is_login".to_string(), "false".to_string()),
        ("verifyFp".to_string(), verify_fp.to_string()),
        ("webcast_language".to_string(), "zh-Hans".to_string()),
        ("msToken".to_string(), ms_token.to_string()),
    ];
    build_query(&params)
}

async fn fetch_auth_broadcast_cookies(
    client: &Client,
    headers: &HeaderMap,
    base_cookies: &HashMap<String, String>,
    device_id: &str,
    verify_fp: &str,
    ms_token: &str,
) -> Result<HashMap<String, String>, RecorderError> {
    let query = build_auth_broadcast_query(device_id, verify_fp, ms_token);
    let hosts = [
        "https://web-va.tiktok.com",
        "https://login-no1a.www.tiktok.com",
        "https://us.tiktok.com",
    ];
    let mut merged = base_cookies.clone();
    for host in hosts {
        let url = format!("{host}/passport/web/auth_broadcast/?{query}");
        let resp = client.post(url).headers(headers.clone()).send().await?;
        let extra = collect_cookie_map(resp.headers());
        if !extra.is_empty() {
            log::info!(
                "[TikTok] auth_broadcast cookies from {}: {}",
                host,
                extra.len()
            );
        } else {
            log::warn!("[TikTok] auth_broadcast no cookies from {}", host);
        }
        merge_cookie_maps(&mut merged, extra);
    }
    Ok(merged)
}

fn extract_next_url(target: &str) -> Option<String> {
    let parsed = Url::parse(target).ok()?;
    for (key, value) in parsed.query_pairs() {
        if key == "next_url" {
            return Some(value.to_string());
        }
    }
    None
}

async fn fetch_cookies_with_redirects(
    headers: &HeaderMap,
    start_url: &str,
) -> Result<HashMap<String, String>, RecorderError> {
    let client = Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|_| RecorderError::ApiError {
            error: "Failed to build TikTok login client".to_string(),
        })?;
    let mut current = start_url.to_string();
    let mut collected = HashMap::new();
    merge_cookie_map_from_url(&mut collected, &current);
    if let Some(cookie_header) = headers.get("cookie").and_then(|v| v.to_str().ok()) {
        merge_cookie_maps(&mut collected, parse_cookie_header(cookie_header));
    }

    for step in 0..6 {
        let mut request_headers = headers.clone();
        let cookie_string = build_cookie_string(&collected);
        if !cookie_string.is_empty() {
            request_headers.insert("cookie", cookie_string.parse().unwrap());
        }
        let resp = client.get(&current).headers(request_headers).send().await?;
        let extra = collect_cookie_map(resp.headers());
        if !extra.is_empty() {
            let names = extra.keys().cloned().collect::<Vec<String>>().join(", ");
            log::info!(
                "[TikTok] QR redirect step {}: {} set-cookie keys: {}",
                step,
                current,
                names
            );
        } else {
            log::info!(
                "[TikTok] QR redirect step {}: {} set-cookie empty",
                step,
                current
            );
        }
        merge_cookie_maps(&mut collected, extra);

        if resp.status().is_redirection() {
            if let Some(location) = resp.headers().get(LOCATION) {
                let location = location.to_str().unwrap_or_default();
                if location.is_empty() {
                    break;
                }
                if let Ok(next_url) = Url::parse(&current).and_then(|base| base.join(location)) {
                    log::info!(
                        "[TikTok] QR redirect step {}: location -> {}",
                        step,
                        next_url
                    );
                    current = next_url.to_string();
                    merge_cookie_map_from_url(&mut collected, &current);
                    continue;
                }
                if let Ok(next_url) = Url::parse(location) {
                    log::info!(
                        "[TikTok] QR redirect step {}: location -> {}",
                        step,
                        next_url
                    );
                    current = next_url.to_string();
                    merge_cookie_map_from_url(&mut collected, &current);
                    continue;
                }
            }
        }
        break;
    }

    Ok(collected)
}

async fn follow_login_chain(headers: &HeaderMap, target: &str) -> Result<String, RecorderError> {
    log::info!("[TikTok] QR follow login chain: {}", target);
    let mut cookie_map = fetch_cookies_with_redirects(headers, target).await?;
    if let Some(next_url) = extract_next_url(target) {
        log::info!("[TikTok] QR follow login chain next_url: {}", next_url);
        let next_map = fetch_cookies_with_redirects(headers, &next_url).await?;
        merge_cookie_maps(&mut cookie_map, next_map);
    }
    Ok(build_cookie_string(&cookie_map))
}

pub async fn get_qr_login(client: &Client) -> Result<TikTokQrInfo, RecorderError> {
    let proxy_url = proxy_url_from_env();
    let request_client = if let Some(proxy_url) = proxy_url.as_deref() {
        build_proxy_client(proxy_url)?
    } else {
        client.clone()
    };

    // Load overrides defaults
    let overrides =
        crate::reverse_generate::tiktok_web::fetch_tiktok_overrides().unwrap_or_default();

    let mut device_id = overrides
        .get("device_id")
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .unwrap_or_default();
    let mut verify_fp = overrides
        .get("verify_fp")
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .unwrap_or_default();
    let mut ttwid_ticket = overrides
        .get("ttwid_migration_ticket")
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .unwrap_or_default();

    // If critical parameters are missing, scrape them from guest state
    if device_id.is_empty() || verify_fp.is_empty() || ttwid_ticket.is_empty() {
        if let Ok(state) = fetch_guest_state(&request_client, &Account::default()).await {
            if device_id.is_empty() {
                device_id = state.device_id.clone();
            }
            if verify_fp.is_empty() {
                verify_fp = state.verify_fp.clone();
            }
            if ttwid_ticket.is_empty() {
                ttwid_ticket = state.ttwid.clone();
            }

            // Persist discovered values
            let _ = crate::reverse_generate::qr_login::update_tiktok_config(
                Some(&device_id),
                Some(&verify_fp),
                Some(&ttwid_ticket),
                true,
            );
        }
    }

    // Fallbacks if scrape failed
    if device_id.is_empty() {
        device_id = gen_device_id();
    }
    if verify_fp.is_empty() {
        verify_fp = crate::platforms::douyin::params::gen_verify_fp();
    }

    // Initial cookies bootstrap (using what we have)
    let mut cookies = HashMap::new();
    if !ttwid_ticket.is_empty() {
        cookies.insert("ttwid".to_string(), ttwid_ticket.clone());
    }
    cookies.insert("s_v_web_id".to_string(), verify_fp.clone());

    // Check if ms_token is overridden
    if let Some(token) = overrides.get("ms_token").filter(|v| !v.is_empty()) {
        cookies.insert("msToken".to_string(), token.to_string());
    }

    // Determine ms_token to use
    let mut ms_token = cookies
        .get("msToken")
        .cloned()
        .unwrap_or_else(crate::platforms::douyin::params::gen_false_ms_token);

    let params = build_qr_params(None, &device_id, &verify_fp, &ms_token);
    let query = build_query(&params);
    let url = format!("https://www.tiktok.com/passport/web/get_qrcode/?{query}");

    let headers = build_passport_headers(&cookies);
    let resp = request_client
        .get(url)
        .headers(headers.clone())
        .send()
        .await?;
    let resp_headers = resp.headers().clone();
    merge_cookie_maps(&mut cookies, collect_cookie_map(&resp_headers));
    if let Some(ms_header) = resp.headers().get("x-ms-token") {
        if let Ok(value) = ms_header.to_str() {
            ms_token = value.to_string();
            cookies.insert("msToken".to_string(), ms_token.clone());
        }
    }
    let json: serde_json::Value = resp.json().await?;
    merge_cookie_map_from_json(&mut cookies, &json);
    log::info!("TikTok get_qrcode response: {}", json);
    let data = json.get("data").ok_or_else(|| RecorderError::ApiError {
        error: "TikTok QR: missing data".to_string(),
    })?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let qrcode_index_url = data
        .get("qrcode_index_url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let ttwid_ticket = data
        .get("ttwid_migration_ticket")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let qrcode = data
        .get("qrcode")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (url, image) = if !qrcode.is_empty() {
        (qrcode_index_url.clone(), Some(qrcode))
    } else if !qrcode_index_url.is_empty() {
        (qrcode_index_url, None)
    } else {
        (String::new(), None)
    };

    let mut oauth_key = format!("{token}|{device_id}|{verify_fp}|{ms_token}");

    // Check for overridden ticket or from response
    let ticket = overrides
        .get("ttwid_migration_ticket")
        .filter(|v| !v.is_empty())
        .map(|v| v.as_str())
        .unwrap_or(&ttwid_ticket);

    if !ticket.is_empty() {
        oauth_key.push('|');
        oauth_key.push_str(ticket);
    }
    let bootstrap_cookies = build_cookie_string(&cookies);
    if !bootstrap_cookies.is_empty() {
        oauth_key.push('|');
        oauth_key.push_str(&general_purpose::STANDARD.encode(bootstrap_cookies.as_bytes()));
    }

    // Attempt to persist if we got a new ttwid
    let migration_ticket = overrides
        .get("ttwid_migration_ticket")
        .map(|v| v.as_str())
        .unwrap_or("");

    if !ttwid_ticket.is_empty() && migration_ticket.is_empty() {
        let _ = crate::reverse_generate::qr_login::update_tiktok_config(
            None,
            None,
            Some(&ttwid_ticket),
            true,
        );
    }

    Ok(TikTokQrInfo {
        oauth_key,
        url,
        image,
    })
}

#[derive(Clone, Debug)]
pub struct TikTokGuestState {
    pub ttwid: String,
    pub ssid: String,
    pub device_id: String,
    pub verify_fp: String,
}

pub async fn fetch_guest_state(
    client: &Client,
    _account: &Account,
) -> Result<TikTokGuestState, RecorderError> {
    log::info!("[TikTok] Fetching guest state from homepage...");
    let url = "https://www.tiktok.com/";
    let headers = build_page_headers(url, USER_AGENT, false);
    let response = client.get(url).headers(headers).send().await?;

    if !response.status().is_success() {
        log::warn!(
            "[TikTok] Failed to fetch homepage: status {}",
            response.status()
        );
    }

    let cookies = collect_cookie_map(response.headers());
    let ttwid = cookies.get("ttwid").cloned().unwrap_or_default();
    let ssid = cookies.get("ssid").cloned().unwrap_or_default();
    let verify_fp = cookies.get("s_v_web_id").cloned().unwrap_or_default();

    let html = response.text().await.unwrap_or_default();

    // Attempt to extract device_id from SIGI_STATE or cookie
    let mut device_id = String::new();

    if let Some(state_value) = extract_state_value(&html) {
        // Try to find device_id in typical locations within SIGI_STATE
        if let Some(did) = state_value
            .get("AppContext")
            .and_then(|v| v.get("wid"))
            .and_then(|v| v.as_str())
        {
            device_id = did.to_string();
        } else if let Some(did) = state_value.get("device_id").and_then(|v| v.as_str()) {
            device_id = did.to_string();
        }
    }

    if device_id.is_empty() {
        // Fallback to random if server didn't provide one
        device_id = gen_device_id();
    }

    log::info!(
        "[TikTok] Guest state: ttwid={}, verify_fp={}, did={}",
        ttwid,
        verify_fp,
        device_id
    );

    Ok(TikTokGuestState {
        ttwid,
        ssid,
        device_id,
        verify_fp,
    })
}

pub async fn get_qr_login_status(
    client: &Client,
    token_key: &str,
) -> Result<TikTokQrStatus, RecorderError> {
    if !tiktok_api_allowed() {
        return Ok(TikTokQrStatus {
            code: 1,
            cookies: String::new(),
            message: "访问太频繁，请稍后再试".to_string(),
        });
    }
    let mut parts = token_key.split('|');
    let token = parts.next().unwrap_or_default();
    let device_id = parts.next().unwrap_or_default();
    let verify_fp = parts.next().unwrap_or_default();
    let ms_token = parts.next().unwrap_or_default();
    let ttwid_ticket = parts.next();
    let bootstrap_cookie_header = parts
        .next()
        .and_then(|encoded| general_purpose::STANDARD.decode(encoded).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();

    let proxy_url = proxy_url_from_env();
    let request_client = if let Some(proxy_url) = proxy_url.as_deref() {
        build_proxy_client(proxy_url)?
    } else {
        client.clone()
    };

    // Initial cookies bootstrap
    let mut cookies = bootstrap_tiktok_cookies(&request_client, verify_fp, Some(ms_token)).await;
    if !bootstrap_cookie_header.trim().is_empty() {
        merge_cookie_maps(&mut cookies, parse_cookie_header(&bootstrap_cookie_header));
    }

    let params = build_qr_params(Some(token), device_id, verify_fp, ms_token);
    let query = build_query(&params);
    let mut headers = build_passport_headers(&cookies);
    if let Some(ticket) = ttwid_ticket {
        if !ticket.is_empty() {
            if let Ok(value) = ticket.parse() {
                headers.insert("x-tt-passport-ttwid-ticket", value);
            }
        }
    }
    apply_tiktok_extra_headers(&mut headers);
    // Add logic to rotate hosts or retry on failure
    let hosts = [
        "https://login-no1a.www.tiktok.com",
        "https://web-va.tiktok.com",
        "https://us.tiktok.com",
    ];
    let mut response_cookies = cookies.clone();
    let mut json: Option<serde_json::Value> = None;

    // Attempt request parameters
    for host in hosts {
        let url = format!("{host}/passport/web/check_qrconnect/?{query}");
        if let Ok(resp) = request_client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await
        {
            merge_cookie_maps(&mut response_cookies, collect_cookie_map(resp.headers()));
            if let Ok(current) = resp.json::<serde_json::Value>().await {
                merge_cookie_map_from_json(&mut response_cookies, &current);
                let is_error = current.get("message").and_then(Value::as_str) == Some("error");
                if is_error {
                    log::warn!("[TikTok] QR check failed on {}: {}", host, current);
                    json = Some(current);
                    continue;
                }
                json = Some(current);
                break;
            }
        }
    }
    let json = json.unwrap_or_else(|| serde_json::json!({}));
    if let (Some(status_code), Some(sub_status_code)) = (
        json.get("status_code").and_then(Value::as_i64),
        json.get("sub_status_code").and_then(Value::as_i64),
    ) {
        log::info!(
            "[TikTok] QR status_code: {}, sub_status_code: {}",
            status_code,
            sub_status_code
        );
    }

    if json.get("message").and_then(Value::as_str) == Some("error") {
        let description = json
            .get("data")
            .and_then(|data| data.get("description"))
            .and_then(value_to_string)
            .unwrap_or_else(|| "TikTok QR error".to_string());
        let is_rate_limited = json
            .get("data")
            .and_then(|data| data.get("error_code"))
            .and_then(Value::as_i64)
            == Some(7)
            || description.contains("\u{8bbf}\u{95ee}\u{592a}\u{9891}\u{7e41}")
            || description.contains("\u{8bf7}\u{7a0d}\u{540e}\u{518d}\u{8bd5}")
            || description.to_ascii_lowercase().contains("rate");
        if is_rate_limited {
            set_tiktok_cooldown("rate limited");
        }
        return Ok(TikTokQrStatus {
            code: if is_rate_limited { 1 } else { 3 },
            cookies: String::new(),
            message: description,
        });
    }

    let status_value = match json.get("data") {
        Some(Value::Array(items)) => items.first().and_then(|item| {
            let status = item.get("status").and_then(value_to_string);
            let target = item.get("target").and_then(value_to_string);
            status.map(|s| (s, target))
        }),
        Some(Value::Object(obj)) => {
            let status = obj.get("status").and_then(value_to_string);
            let target = obj.get("target").and_then(value_to_string);
            status.map(|s| (s, target))
        }
        _ => None,
    };

    let authnext = json
        .get("data")
        .and_then(|data| data.get("authnext").or_else(|| data.get("auth_next")))
        .and_then(value_to_string);
    if let Some(authnext_url) = authnext {
        log::info!("[TikTok] QR authnext received");
        match follow_login_chain(&headers, &authnext_url).await {
            Ok(cookies) if !cookies.is_empty() => {
                log::info!(
                    "[TikTok] QR login cookies recovered via authnext ({} cookies)",
                    cookies.split(';').count()
                );
                return Ok(TikTokQrStatus {
                    code: 0,
                    cookies,
                    message: "ok".to_string(),
                });
            }
            Ok(_) => {
                log::warn!("[TikTok] QR login cookie empty after authnext");
            }
            Err(err) => {
                log::warn!("[TikTok] QR authnext chain failed: {}", err);
            }
        }
    }

    let redirect_url = json
        .get("data")
        .and_then(|data| data.get("redirect_url"))
        .and_then(value_to_string);
    if let Some(redirect_url) = redirect_url {
        log::info!("[TikTok] QR redirect_url received");
        match follow_login_chain(&headers, &redirect_url).await {
            Ok(cookies) if !cookies.is_empty() => {
                log::info!(
                    "[TikTok] QR login cookies recovered via redirect_url ({} cookies)",
                    cookies.split(';').count()
                );
                return Ok(TikTokQrStatus {
                    code: 0,
                    cookies,
                    message: "ok".to_string(),
                });
            }
            Ok(_) => {
                log::warn!("[TikTok] QR login cookie empty after redirect_url");
            }
            Err(err) => {
                log::warn!("[TikTok] QR redirect_url chain failed: {}", err);
            }
        }
    }

    if json.get("status_code").and_then(Value::as_i64) == Some(0)
        && json.get("sub_status_code").and_then(Value::as_i64) == Some(2001)
    {
        if has_login_cookie(&response_cookies) {
            log::info!(
                "[TikTok] QR login cookies recovered from response ({} cookies)",
                response_cookies.len()
            );
            return Ok(TikTokQrStatus {
                code: 0,
                cookies: build_cookie_string(&response_cookies),
                message: "ok".to_string(),
            });
        }
        log::info!("[TikTok] QR check pass, try auth_broadcast");
        match fetch_auth_broadcast_cookies(
            client,
            &headers,
            &response_cookies,
            device_id,
            verify_fp,
            ms_token,
        )
        .await
        {
            Ok(cookies) if has_login_cookie(&cookies) => {
                log::info!(
                    "[TikTok] QR login cookies recovered via auth_broadcast ({} cookies)",
                    cookies.len()
                );
                return Ok(TikTokQrStatus {
                    code: 0,
                    cookies: build_cookie_string(&cookies),
                    message: "ok".to_string(),
                });
            }
            Ok(cookies) => {
                log::warn!("[TikTok] auth_broadcast returned empty cookies");
                if !cookies.is_empty() {
                    log::warn!(
                        "[TikTok] auth_broadcast cookies missing sessionid ({} total)",
                        cookies.len()
                    );
                }
            }
            Err(err) => {
                log::warn!("[TikTok] auth_broadcast failed: {}", err);
            }
        }
        return Ok(TikTokQrStatus {
            code: 2,
            cookies: String::new(),
            message: "waiting_confirm".to_string(),
        });
    }

    if let Some((status, target)) = status_value {
        log::info!("[TikTok] QR status: {}", status);
        if let Some(target_url) = target {
            match follow_login_chain(&headers, &target_url).await {
                Ok(cookies) if !cookies.is_empty() => {
                    return Ok(TikTokQrStatus {
                        code: 0,
                        cookies,
                        message: "ok".to_string(),
                    });
                }
                Ok(_) => {
                    log::warn!("[TikTok] QR login cookie empty after status {}", status);
                }
                Err(err) => {
                    log::warn!("[TikTok] QR login chain failed: {}", err);
                }
            }
        }
        if status == "success" || status == "confirm" || status == "scan" || status == "scanned" {
            return Ok(TikTokQrStatus {
                code: 2,
                cookies: String::new(),
                message: "waiting_confirm".to_string(),
            });
        }
        if status == "new" {
            return Ok(TikTokQrStatus {
                code: 1,
                cookies: String::new(),
                message: status,
            });
        }
        if status == "expired" || status == "cancel" || status == "failed" {
            return Ok(TikTokQrStatus {
                code: 3,
                cookies: String::new(),
                message: status,
            });
        }
        return Ok(TikTokQrStatus {
            code: 1,
            cookies: String::new(),
            message: status,
        });
    }

    log::warn!("TikTok QR status: unexpected response: {}", json);
    Ok(TikTokQrStatus {
        code: 1,
        cookies: String::new(),
        message: "unknown".to_string(),
    })
}

fn find_m3u8_in_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let decoded = decode_json_string(value).unwrap_or_else(|| value.to_string());
            if decoded.contains(".m3u8") && decoded.starts_with("http") {
                Some(decoded)
            } else {
                None
            }
        }
        Value::Object(map) => {
            for child in map.values() {
                if let Some(url) = find_m3u8_in_value(child) {
                    return Some(url);
                }
            }
            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(url) = find_m3u8_in_value(value) {
                    return Some(url);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_username_from_url(url: &str) -> String {
    let url_no_query = url.split('?').next().unwrap_or(url);
    let segments = url_no_query.split('/').filter(|part| !part.is_empty());
    for segment in segments {
        if let Some(stripped) = segment.strip_prefix('@') {
            if !stripped.is_empty() {
                return stripped.to_string();
            }
        }
    }
    String::new()
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

fn get_i64_field(map: &Map<String, Value>, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(value) = map.get(*key) {
            match value {
                Value::Number(value) => return value.as_i64(),
                Value::String(value) => {
                    if let Ok(parsed) = value.parse::<i64>() {
                        return Some(parsed);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn extract_room_data_from_room_info_api(value: &Value) -> Option<Value> {
    let entries = value.get("data").and_then(|v| v.as_array())?;
    for entry in entries {
        if let Some(room_data) = entry.get("data") {
            return Some(room_data.clone());
        }
    }
    None
}

fn parse_stream_data_value(raw: &str) -> Option<Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .or_else(|| decode_json_string(raw).and_then(|decoded| serde_json::from_str(&decoded).ok()))
}

fn append_codec(url: &str, codec: &str) -> String {
    if codec.is_empty() || url.contains("codec=") {
        return url.to_string();
    }
    let separator = if url.contains('?') { "&" } else { "?" };
    format!("{url}{separator}codec={codec}")
}

#[derive(Clone, Debug)]
struct StreamCandidate {
    hls_url: Option<String>,
    flv_url: Option<String>,
    bitrate: i64,
    width: i64,
    height: i64,
    resolution: String,
}

fn extract_stream_candidates_from_stream_data(stream_data_raw: &str) -> Vec<StreamCandidate> {
    let stream_data_value = match parse_stream_data_value(stream_data_raw) {
        Some(value) => value,
        None => return Vec::new(),
    };
    let data = match stream_data_value
        .get("data")
        .and_then(|value| value.as_object())
    {
        Some(value) => value,
        None => return Vec::new(),
    };

    let mut candidates = Vec::new();

    for entry in data.values() {
        let main = entry.get("main").and_then(|value| value.as_object());
        let Some(main) = main else { continue };
        let sdk_params_raw = main
            .get("sdk_params")
            .and_then(|value| value.as_str())
            .unwrap_or("{}");
        let sdk_params = serde_json::from_str::<Value>(sdk_params_raw).unwrap_or(Value::Null);
        let bitrate = sdk_params
            .get("vbitrate")
            .and_then(|value| value.as_i64())
            .or_else(|| {
                sdk_params
                    .get("vbitrate")
                    .and_then(|value| value.as_str())
                    .and_then(|value| value.parse::<i64>().ok())
            })
            .unwrap_or(0);
        let resolution = sdk_params
            .get("resolution")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let (width, height) = resolution
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse::<i64>().ok()?, h.parse::<i64>().ok()?)))
            .unwrap_or((0, 0));
        let vcodec = sdk_params
            .get("VCodec")
            .and_then(|value| value.as_str())
            .unwrap_or("");

        let hls_url = main
            .get("hls")
            .and_then(|value| value.as_str())
            .map(|url| append_codec(url, vcodec));
        let flv_url = main
            .get("flv")
            .and_then(|value| value.as_str())
            .map(|url| append_codec(url, vcodec));

        if hls_url.is_none() && flv_url.is_none() {
            continue;
        }

        candidates.push(StreamCandidate {
            hls_url,
            flv_url,
            bitrate,
            width,
            height,
            resolution: resolution.to_string(),
        });
    }

    candidates.sort_by(|a, b| {
        b.bitrate
            .cmp(&a.bitrate)
            .then_with(|| b.width.cmp(&a.width))
            .then_with(|| b.height.cmp(&a.height))
    });

    candidates
}

fn extract_stream_candidates(live_room_info: &Value) -> Vec<StreamCandidate> {
    let stream_data_raw = match live_room_info
        .get("liveRoom")
        .and_then(|value| value.get("streamData"))
        .and_then(|value| value.get("pull_data"))
        .and_then(|value| value.get("stream_data"))
        .and_then(|value| value.as_str())
    {
        Some(value) => value,
        None => return Vec::new(),
    };

    extract_stream_candidates_from_stream_data(stream_data_raw)
}

fn extract_stream_candidates_from_room_info(room_data: &Value) -> Vec<StreamCandidate> {
    let stream_data_raw = match room_data
        .get("stream_url")
        .or_else(|| room_data.get("streamUrl"))
        .and_then(|value| value.get("live_core_sdk_data"))
        .and_then(|value| value.get("pull_data"))
        .and_then(|value| value.get("stream_data"))
        .and_then(|value| value.as_str())
    {
        Some(value) => value,
        None => return Vec::new(),
    };

    extract_stream_candidates_from_stream_data(stream_data_raw)
}

fn extract_stream_from_live_room(live_room_info: &Value) -> Option<StreamInfo> {
    let candidates = extract_stream_candidates(live_room_info);
    let best = candidates.first()?;
    Some(StreamInfo {
        hls_url: best.hls_url.clone(),
        rtmp_url: best.flv_url.clone(),
        resolution: Some(best.resolution.clone()),
    })
}

async fn check_url_accessible(client: &Client, headers: &HeaderMap, url: &str) -> bool {
    if url.contains(".m3u8") {
        return check_hls_stream_accessible(client, headers, url).await;
    }
    let mut request = client.get(url).headers(headers.clone());
    request = request.header("Range", "bytes=0-1");
    match request.send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

fn extract_first_media_uri(m3u8_text: &str) -> Option<String> {
    for line in m3u8_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

fn resolve_uri(base: &str, uri: &str) -> Option<String> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return Some(uri.to_string());
    }
    let base_url = Url::parse(base).ok()?;
    base_url.join(uri).ok().map(|url| url.to_string())
}

async fn check_hls_stream_accessible(client: &Client, headers: &HeaderMap, m3u8_url: &str) -> bool {
    let response = match client.get(m3u8_url).headers(headers.clone()).send().await {
        Ok(resp) => resp,
        Err(_) => return false,
    };
    if !response.status().is_success() {
        return false;
    }
    let text = match response.text().await {
        Ok(text) => text,
        Err(_) => return false,
    };

    let first_uri = match extract_first_media_uri(&text) {
        Some(uri) => uri,
        None => return false,
    };

    let resolved = match resolve_uri(m3u8_url, &first_uri) {
        Some(url) => url,
        None => return false,
    };

    if resolved.contains(".m3u8") {
        let response = match client.get(&resolved).headers(headers.clone()).send().await {
            Ok(resp) => resp,
            Err(_) => return false,
        };
        if !response.status().is_success() {
            return false;
        }
        let text = match response.text().await {
            Ok(text) => text,
            Err(_) => return false,
        };
        let first_segment = match extract_first_media_uri(&text) {
            Some(uri) => uri,
            None => return false,
        };
        let segment_url = match resolve_uri(&resolved, &first_segment) {
            Some(url) => url,
            None => return false,
        };
        let response = match client
            .get(&segment_url)
            .headers(headers.clone())
            .header("Range", "bytes=0-1")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(_) => return false,
        };
        return response.status().is_success();
    }

    let response = match client
        .get(&resolved)
        .headers(headers.clone())
        .header("Range", "bytes=0-1")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(_) => return false,
    };
    response.status().is_success()
}

async fn select_accessible_stream(
    client: &Client,
    headers: &HeaderMap,
    candidates: &[StreamCandidate],
) -> Option<StreamInfo> {
    for candidate in candidates {
        if let Some(hls_url) = candidate.hls_url.as_deref() {
            if check_url_accessible(client, headers, hls_url).await {
                return Some(StreamInfo {
                    hls_url: candidate.hls_url.clone(),
                    rtmp_url: candidate.flv_url.clone(),
                    resolution: Some(candidate.resolution.clone()),
                });
            }
        }

        if let Some(flv_url) = candidate.flv_url.as_deref() {
            if check_url_accessible(client, headers, flv_url).await {
                return Some(StreamInfo {
                    hls_url: None,
                    rtmp_url: Some(flv_url.to_string()),
                    resolution: Some(candidate.resolution.clone()),
                });
            }
        }
    }

    candidates.first().map(|candidate| StreamInfo {
        hls_url: candidate.hls_url.clone(),
        rtmp_url: candidate.flv_url.clone(),
        resolution: Some(candidate.resolution.clone()),
    })
}

fn extract_live_room_user_info(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            if let Some(live_room) = map.get("LiveRoom") {
                if let Some(info) = live_room.get("liveRoomUserInfo") {
                    return Some(info.clone());
                }
            }
            if let Some(info) = map.get("liveRoomUserInfo") {
                return Some(info.clone());
            }

            for child in map.values() {
                if let Some(info) = extract_live_room_user_info(child) {
                    return Some(info);
                }
            }

            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(info) = extract_live_room_user_info(value) {
                    return Some(info);
                }
            }
            None
        }
        Value::String(value) => {
            parse_first_json_value(value).and_then(|parsed| extract_live_room_user_info(&parsed))
        }
        _ => None,
    }
}

fn extract_first_url(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            if let Some(url) = map.get("url").and_then(|v| v.as_str()) {
                Some(url.to_string())
            } else if let Some(list) = map
                .get("url_list")
                .or_else(|| map.get("urlList"))
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

fn extract_first_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(list) => list.iter().find_map(|v| v.as_str()).map(|s| s.to_string()),
        Value::Object(map) => map.values().find_map(|v| v.as_str()).map(|s| s.to_string()),
        _ => None,
    }
}

fn normalize_image_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("//") {
        format!("https:{}", trimmed)
    } else {
        trimmed.to_string()
    }
}

fn find_cover_url(value: &Value) -> Option<String> {
    const COVER_KEYS: [&str; 10] = [
        "cover",
        "coverUrl",
        "cover_url",
        "coverImage",
        "coverImageUrl",
        "roomCover",
        "roomCoverUrl",
        "liveRoomCover",
        "shareCover",
        "shareImage",
    ];
    match value {
        Value::Object(map) => {
            for key in COVER_KEYS {
                if let Some(value) = map.get(key) {
                    if let Some(url) = extract_first_url(value) {
                        return Some(normalize_image_url(&url));
                    }
                }
            }
            for (key, value) in map {
                if key.to_ascii_lowercase().contains("cover") {
                    if let Some(url) = extract_first_url(value) {
                        return Some(normalize_image_url(&url));
                    }
                }
            }
            for child in map.values() {
                if let Some(url) = find_cover_url(child) {
                    return Some(url);
                }
            }
            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(url) = find_cover_url(value) {
                    return Some(url);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_avatar_url(value: &Value) -> Option<String> {
    const AVATAR_KEYS: [&str; 9] = [
        "avatarThumb",
        "avatar_thumb",
        "avatar",
        "avatarUrl",
        "avatarMedium",
        "avatarLarge",
        "headUrl",
        "head_url",
        "profileImage",
    ];
    match value {
        Value::Object(map) => {
            for key in AVATAR_KEYS {
                if let Some(value) = map.get(key) {
                    if let Some(url) = extract_first_url(value) {
                        return Some(normalize_image_url(&url));
                    }
                }
            }
            for child in map.values() {
                if let Some(url) = find_avatar_url(child) {
                    return Some(url);
                }
            }
            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(url) = find_avatar_url(value) {
                    return Some(url);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_room_info_from_live_room(
    live_room_info: &Value,
    account: &Account,
    url: &str,
) -> Option<RoomInfo> {
    let live_room_map = live_room_info.as_object()?;
    let user_map = live_room_map
        .get("user")
        .and_then(|value| value.as_object());
    let live_room_map = live_room_map
        .get("liveRoom")
        .and_then(|value| value.as_object());

    let user_name = user_map
        .and_then(|map| get_string_field(map, &["nickname", "nickName", "name", "userName"]))
        .unwrap_or_default();
    let user_id = user_map
        .and_then(|map| get_string_field(map, &["uniqueId", "userId", "id", "uid"]))
        .unwrap_or_default();
    let status = user_map.and_then(|map| get_i64_field(map, &["status", "liveStatus"]));
    let live_room_status =
        live_room_map.and_then(|map| get_i64_field(map, &["status", "liveStatus"]));
    let title = live_room_map
        .and_then(|map| get_string_field(map, &["title", "roomTitle"]))
        .unwrap_or_default();

    let user_avatar = user_map
        .and_then(|map| map.get("avatarThumb"))
        .and_then(|v| extract_first_url(v))
        .map(|url| normalize_image_url(&url))
        .unwrap_or_default();
    let room_cover_url = live_room_map
        .and_then(|map| find_cover_url(&Value::Object(map.clone())))
        .unwrap_or_default();

    let status_flag = status.or(live_room_status);
    let live_status = if let Some(flag) = status_flag {
        flag == 2
    } else {
        extract_stream_from_live_room(live_room_info)
            .and_then(|stream| stream.hls_url.or(stream.rtmp_url))
            .is_some()
    };

    let extracted_name = extract_username_from_url(url);
    let final_user_name = if !user_name.is_empty() {
        user_name
    } else if !account.name.is_empty() {
        account.name.clone()
    } else if !extracted_name.is_empty() {
        extracted_name.clone()
    } else {
        "TikTok Live".to_string()
    };
    let final_user_id = if !user_id.is_empty() {
        user_id
    } else if !account.id.is_empty() {
        account.id.clone()
    } else {
        extracted_name
    };

    Some(RoomInfo {
        live_status,
        room_title: if title.is_empty() {
            format!("{}'s live", final_user_name)
        } else {
            title
        },
        room_cover_url: if room_cover_url.is_empty() {
            user_avatar.clone()
        } else {
            room_cover_url
        },
        user_id: final_user_id,
        user_name: final_user_name,
        user_avatar,
    })
}

fn looks_like_room_info(map: &Map<String, Value>) -> bool {
    if !(map.contains_key("status")
        || map.contains_key("liveStatus")
        || map.contains_key("live_status"))
    {
        return false;
    }
    let has_owner = map.contains_key("owner")
        || map.contains_key("ownerInfo")
        || map.contains_key("user")
        || map.contains_key("userInfo")
        || map.contains_key("host")
        || map.contains_key("author");
    let has_stream = map.contains_key("streamUrl")
        || map.contains_key("stream_url")
        || map.contains_key("streamUrlInfo")
        || map.contains_key("stream_url_info");
    let has_title = map.contains_key("title")
        || map.contains_key("roomId")
        || map.contains_key("room_id")
        || map.contains_key("liveRoomId")
        || map.contains_key("live_room_id");
    (has_owner || has_stream) && has_title
}

fn find_room_info(value: &Value) -> Option<SigiRoomInfo> {
    match value {
        Value::Object(map) => {
            if let Some(room_info_value) = map.get("roomInfo") {
                if let Ok(room_info) =
                    serde_json::from_value::<SigiRoomInfo>(room_info_value.clone())
                {
                    return Some(room_info);
                }
            }

            if looks_like_room_info(map) {
                if let Ok(room_info) =
                    serde_json::from_value::<SigiRoomInfo>(Value::Object(map.clone()))
                {
                    return Some(room_info);
                }
            }

            for child in map.values() {
                if let Some(room_info) = find_room_info(child) {
                    return Some(room_info);
                }
            }

            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(room_info) = find_room_info(value) {
                    return Some(room_info);
                }
            }
            None
        }
        Value::String(value) => {
            parse_first_json_value(value).and_then(|parsed| find_room_info(&parsed))
        }
        _ => None,
    }
}

fn looks_like_stream_url(map: &Map<String, Value>) -> bool {
    map.contains_key("hlsPullUrl")
        || map.contains_key("hls_pull_url")
        || map.contains_key("hlsPlayUrl")
        || map.contains_key("rtmpPullUrl")
        || map.contains_key("rtmp_pull_url")
        || map.contains_key("rtmpPlayUrl")
        || map.contains_key("flvPullUrl")
}

fn find_stream_url(value: &Value) -> Option<SigiStreamUrl> {
    match value {
        Value::Object(map) => {
            if looks_like_stream_url(map) {
                if let Ok(stream_url) =
                    serde_json::from_value::<SigiStreamUrl>(Value::Object(map.clone()))
                {
                    return Some(stream_url);
                }
            }

            for child in map.values() {
                if let Some(stream_url) = find_stream_url(child) {
                    return Some(stream_url);
                }
            }

            None
        }
        Value::Array(values) => {
            for value in values {
                if let Some(stream_url) = find_stream_url(value) {
                    return Some(stream_url);
                }
            }
            None
        }
        Value::String(value) => {
            parse_first_json_value(value).and_then(|parsed| find_stream_url(&parsed))
        }
        _ => None,
    }
}

fn extract_room_id_from_room_data(room_data: &Value) -> Option<String> {
    let map = room_data.as_object()?;
    get_string_field(
        map,
        &[
            "id_str",
            "id",
            "room_id",
            "roomId",
            "live_room_id",
            "liveRoomId",
        ],
    )
}

fn owner_matches_handle(owner: &Map<String, Value>, handle: &str) -> bool {
    if handle.is_empty() {
        return true;
    }
    let target = handle.trim_start_matches('@').to_ascii_lowercase();
    let candidate = get_string_field(
        owner,
        &[
            "unique_id",
            "uniqueId",
            "display_id",
            "displayId",
            "user_name",
            "userName",
            "nickname",
            "name",
        ],
    )
    .unwrap_or_default()
    .to_ascii_lowercase();
    if candidate.is_empty() {
        return false;
    }
    candidate == target
}

async fn get_room_id_from_feed(
    client: &Client,
    account: &Account,
    referer: &str,
    feed_override: Option<&str>,
) -> Result<String, RecorderError> {
    let url = build_feed_url_override(referer, account, feed_override)?;
    let headers = build_feed_headers(account);

    let response = client.get(url).headers(headers).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(RecorderError::ApiError {
            error: format!("TikTok feed status: {}", status),
        });
    }
    let body = response.text().await.map_err(|e| RecorderError::ApiError {
        error: format!("Failed to read TikTok feed body: {e}"),
    })?;
    if body.trim().is_empty() {
        return Err(RecorderError::ApiError {
            error: "TikTok feed empty response body".to_string(),
        });
    }
    let preview = body.chars().take(200).collect::<String>();
    let json: Value = serde_json::from_str(&body).map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse TikTok feed JSON: {e}. Body: {preview}"),
    })?;
    let status_code = json
        .get("status_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(-1);
    if status_code != 0 {
        return Err(RecorderError::ApiError {
            error: format!("TikTok feed error status: {}", status_code),
        });
    }

    if let Some(room_id) = json
        .get("base_info")
        .and_then(|value| value.get("room_id"))
        .and_then(|value| value.as_str())
    {
        if !room_id.is_empty() {
            return Ok(room_id.to_string());
        }
    }

    let target_handle = extract_username_from_url(referer);
    let entries = match json.get("data").and_then(|v| v.as_array()) {
        Some(values) => values.as_slice(),
        None => &[],
    };
    for entry in entries {
        let room_data = match entry.get("data") {
            Some(value) => value,
            None => continue,
        };
        let status = room_data
            .get("status")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if status != 2 {
            continue;
        }
        if let Some(owner) = room_data.get("owner").and_then(|v| v.as_object()) {
            if !owner_matches_handle(owner, &target_handle) {
                continue;
            }
        } else if !target_handle.is_empty() {
            continue;
        }
        if let Some(room_id) = extract_room_id_from_room_data(room_data) {
            return Ok(room_id);
        }
    }

    for entry in entries {
        let room_data = match entry.get("data") {
            Some(value) => value,
            None => continue,
        };
        if let Some(room_id) = extract_room_id_from_room_data(room_data) {
            return Ok(room_id);
        }
    }

    if !target_handle.is_empty() {
        return Err(RecorderError::ApiError {
            error: format!("TikTok feed did not include handle: {}", target_handle),
        });
    }

    Err(RecorderError::ApiError {
        error: "TikTok feed missing room id".to_string(),
    })
}

async fn get_room_id_from_check_alive(
    client: &Client,
    account: &Account,
    url: &str,
) -> Result<Option<String>, RecorderError> {
    let handle = extract_username_from_url(url);
    if handle.is_empty() {
        return Ok(None);
    }

    let mut headers = build_page_headers(url, USER_AGENT, false);
    headers.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }

    let mut state = None;
    let mut last_html = String::new();
    for attempt in 0..3 {
        let response = client.get(url).headers(headers.clone()).send().await?;
        let status = response.status();
        let html_str = response.text().await?;
        last_html = html_str.clone();
        if !status.is_success() {
            return Err(RecorderError::ApiError {
                error: format!("TikTok response status: {}", status),
            });
        }

        if is_waf_wait_page(&html_str) {
            if attempt < 2 {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }
        }

        if let Some(value) = extract_state_value(&html_str) {
            state = Some(value);
            break;
        }
        if attempt < 2 {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    let Some(state) = state else {
        if !last_html.is_empty() {
            let waf = is_waf_wait_page(&last_html);
            let preview = last_html.chars().take(200).collect::<String>();
            log::warn!(
                "[TikTok] check_alive failed to extract state (waf={}, len={}): {}",
                waf,
                last_html.len(),
                preview
            );
        }
        return Ok(None);
    };

    let Some(room_id) = extract_room_id_from_state(&state) else {
        return Ok(None);
    };

    let referer = build_live_referer(&handle);
    let check_url = build_check_alive_url(&referer, &[room_id.clone()])?;
    let headers = build_check_alive_headers(account, &referer);
    let response = client.get(check_url).headers(headers).send().await?;
    if !response.status().is_success() {
        return Err(RecorderError::ApiError {
            error: format!("TikTok check_alive status: {}", response.status()),
        });
    }
    let json: Value = response.json().await.map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse TikTok check_alive JSON: {e}"),
    })?;
    let status_code = json
        .get("status_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(-1);
    if status_code != 0 {
        return Err(RecorderError::ApiError {
            error: format!("TikTok check_alive error status: {}", status_code),
        });
    }

    let room_ids = collect_check_alive_room_ids(&json);
    if !room_ids.is_empty() {
        log::info!(
            "[TikTok] check_alive room ids for {}: {}",
            handle,
            room_ids.join(",")
        );
    }

    let resolved = extract_room_id_from_check_alive(&json);
    if resolved.is_none() {
        log::info!(
            "[TikTok] check_alive returned no live room for handle {}",
            handle
        );
    }
    Ok(resolved)
}

async fn get_room_id_from_room_enter(
    client: &Client,
    account: &Account,
    url: &str,
) -> Result<Option<String>, RecorderError> {
    let handle = extract_username_from_url(url);
    if handle.is_empty() {
        return Ok(None);
    }
    let referer = build_live_referer(&handle);
    let body = "enter_source=live_detail&room_id=0";
    let enter_url = build_room_enter_url(&referer, account, body);
    let headers = build_room_enter_headers(account, &referer);
    let response = client
        .post(enter_url)
        .headers(headers)
        .body(body.to_string())
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RecorderError::ApiError {
            error: format!("TikTok room enter status: {}", response.status()),
        });
    }
    let body = response.text().await.map_err(|e| RecorderError::ApiError {
        error: format!("Failed to read TikTok room enter body: {e}"),
    })?;
    if body.trim().is_empty() {
        return Err(RecorderError::ApiError {
            error: "TikTok room enter empty response body".to_string(),
        });
    }
    let preview = body.chars().take(200).collect::<String>();
    let json: Value = serde_json::from_str(&body).map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse TikTok room enter JSON: {e}. Body: {preview}"),
    })?;
    let status_code = json
        .get("status_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(-1);
    if status_code != 0 {
        return Err(RecorderError::ApiError {
            error: format!("TikTok room enter error status: {}", status_code),
        });
    }

    let room_id = json
        .get("data")
        .and_then(|value| value.get("living_room_attrs"))
        .and_then(|value| value.as_object())
        .and_then(|map| get_string_field(map, &["room_id_str", "room_id", "roomId", "roomIdStr"]));

    if let Some(room_id) = room_id.as_ref() {
        log::info!(
            "[TikTok] room enter resolved room_id for {}: {}",
            handle,
            room_id
        );
    }

    Ok(room_id)
}

async fn fetch_room_info_api(
    client: &Client,
    account: &Account,
    room_id: &str,
) -> Result<Value, RecorderError> {
    let url = build_room_info_url(room_id);
    let mut headers = build_page_headers("https://www.tiktok.com/", USER_AGENT, false);
    headers.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }

    let response = client.get(url).headers(headers).send().await?;
    if !response.status().is_success() {
        return Err(RecorderError::ApiError {
            error: format!("TikTok room info status: {}", response.status()),
        });
    }

    let json: Value = response.json().await.map_err(|e| RecorderError::ApiError {
        error: format!("Failed to parse TikTok room info JSON: {e}"),
    })?;
    let status_code = json
        .get("status_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(-1);
    if status_code != 0 {
        return Err(RecorderError::ApiError {
            error: format!("TikTok room info error status: {}", status_code),
        });
    }

    if let Ok(preview) = serde_json::to_string(&json) {
        log::info!(
            "[TikTok] room info response (room_id={}): {}",
            room_id,
            preview
        );
    }

    Ok(json)
}

fn extract_room_info_from_room_data(
    room_data: &Value,
    account: &Account,
    url: &str,
) -> Option<RoomInfo> {
    let map = room_data.as_object()?;
    let owner = map.get("owner").and_then(|value| value.as_object());

    let user_name = owner
        .and_then(|map| get_string_field(map, &["nickname", "uniqueId", "displayId", "userName"]))
        .unwrap_or_default();
    let user_id = owner
        .and_then(|map| get_string_field(map, &["id", "userId", "owner_user_id"]))
        .unwrap_or_default();
    let status = get_i64_field(map, &["status", "liveStatus", "live_status"]);
    let title = get_string_field(map, &["title", "roomTitle", "room_title"]).unwrap_or_default();
    let user_avatar = owner
        .and_then(|map| find_avatar_url(&Value::Object(map.clone())))
        .unwrap_or_default();
    let room_cover_url = find_cover_url(room_data).unwrap_or_default();

    let live_status = if let Some(flag) = status {
        flag == 2
    } else {
        room_data
            .get("stream_url")
            .or_else(|| room_data.get("streamUrl"))
            .is_some()
    };

    let extracted_name = extract_username_from_url(url);
    let final_user_name = if !user_name.is_empty() {
        user_name
    } else if !account.name.is_empty() {
        account.name.clone()
    } else if !extracted_name.is_empty() {
        extracted_name.clone()
    } else {
        "TikTok Live".to_string()
    };
    let final_user_id = if !user_id.is_empty() {
        user_id
    } else if !account.id.is_empty() {
        account.id.clone()
    } else {
        extracted_name
    };

    Some(RoomInfo {
        live_status,
        room_title: if title.is_empty() {
            format!("{}'s live", final_user_name)
        } else {
            title
        },
        room_cover_url: if room_cover_url.is_empty() {
            user_avatar.clone()
        } else {
            room_cover_url
        },
        user_id: final_user_id,
        user_name: final_user_name,
        user_avatar,
    })
}

async fn get_room_info_by_room_id(
    client: &Client,
    account: &Account,
    room_id: &str,
    referer: &str,
) -> Result<RoomInfo, RecorderError> {
    let json = fetch_room_info_api(client, account, room_id).await?;
    let Some(room_data) = extract_room_data_from_room_info_api(&json) else {
        return Err(RecorderError::ApiError {
            error: "TikTok room info missing data".to_string(),
        });
    };
    extract_room_info_from_room_data(&room_data, account, referer).ok_or_else(|| {
        RecorderError::ApiError {
            error: "Failed to parse TikTok room info".to_string(),
        }
    })
}

async fn get_stream_url_from_room_info_api(
    client: &Client,
    account: &Account,
    room_id: &str,
) -> Result<StreamInfo, RecorderError> {
    let json = fetch_room_info_api(client, account, room_id).await?;
    let Some(room_data) = extract_room_data_from_room_info_api(&json) else {
        return Err(RecorderError::ApiError {
            error: "TikTok room info missing data".to_string(),
        });
    };

    let mut headers = build_page_headers("https://www.tiktok.com/", USER_AGENT, false);
    headers.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }

    let candidates = extract_stream_candidates_from_room_info(&room_data);
    if !candidates.is_empty() {
        if let Some(info) = select_accessible_stream(client, &headers, &candidates).await {
            return Ok(info);
        }
    }

    let stream_url = room_data
        .get("stream_url")
        .or_else(|| room_data.get("streamUrl"));
    let mut info = StreamInfo {
        hls_url: stream_url
            .and_then(|v| v.get("hls_pull_url"))
            .and_then(extract_first_string),
        rtmp_url: stream_url
            .and_then(|v| v.get("rtmp_pull_url"))
            .and_then(extract_first_string),
        resolution: None,
    };
    if info.rtmp_url.is_none() {
        info.rtmp_url = stream_url
            .and_then(|v| v.get("flv_pull_url"))
            .and_then(extract_first_string);
    }

    if let Some(hls_url) = info.hls_url.as_deref() {
        if !check_hls_stream_accessible(client, &headers, hls_url).await {
            info.hls_url = None;
        }
    }
    if let Some(rtmp_url) = info.rtmp_url.as_deref() {
        if !check_url_accessible(client, &headers, rtmp_url).await {
            info.rtmp_url = None;
        }
    }

    if info.hls_url.is_none() && info.rtmp_url.is_none() {
        return Err(RecorderError::ApiError {
            error: "No available stream provided (room info)".to_string(),
        });
    }

    Ok(info)
}

async fn get_room_info_with_profile(
    client: &Client,
    account: &Account,
    url: &str,
    user_agent: &str,
    mobile: bool,
) -> Result<RoomInfo, RecorderError> {
    let mut headers = build_page_headers(url, user_agent, mobile);
    headers.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }

    warm_up_live_pages(client, account).await;

    for attempt in 0..5 {
        let response = client.get(url).headers(headers.clone()).send().await?;
        let status = response.status();
        let html_str = response.text().await?;
        if !status.is_success() {
            return Err(RecorderError::ApiError {
                error: format!("TikTok response status: {}", status),
            });
        }

        if is_waf_wait_page(&html_str) {
            if attempt < 4 {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }
        }

        if html_str.contains("We regret to inform you that we have discontinued operating TikTok") {
            return Err(RecorderError::ApiError {
                error: "TikTok is not available in this region. Please use a different proxy."
                    .to_string(),
            });
        }

        if html_str.contains("UNEXPECTED_EOF_WHILE_READING") {
            if attempt < 2 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            } else {
                return Err(RecorderError::ApiError {
                    error: "Failed to load page after 3 attempts".to_string(),
                });
            }
        }

        let sigi_value = match extract_state_value(&html_str) {
            Some(value) => value,
            None => {
                if attempt < 2 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(RecorderError::ApiError {
                    error: "Please check if your network can access TikTok normally. Failed to extract page state JSON.

HTML: ".to_string()
                        + &html_str,
                });
            }
        };

        let extracted_room_info = extract_live_room_user_info(&sigi_value)
            .and_then(|info| extract_room_info_from_live_room(&info, account, url));

        if extracted_room_info.is_none() {
            log::warn!("Failed to extract room info from page data");
            return Ok(RoomInfo {
                live_status: false,
                room_title: "TikTok Live Not Started".to_string(),
                room_cover_url: "".to_string(),
                user_id: "".to_string(),
                user_name: "TikTok Live Not Started".to_string(),
                user_avatar: "".to_string(),
            });
        }

        if let Some(mut room_info) = extracted_room_info {
            if room_info.user_avatar.is_empty() {
                if let Some(found) = find_avatar_url(&sigi_value) {
                    room_info.user_avatar = found;
                }
            }
            if room_info.room_cover_url.is_empty() {
                room_info.room_cover_url = room_info.user_avatar.clone();
            }
            return Ok(room_info);
        }

        return Err(RecorderError::ApiError {
            error: "Failed to extract room info from page data".to_string(),
        });
    }

    Err(RecorderError::ApiError {
        error: "Failed to fetch TikTok page after retries".to_string(),
    })
}

/// Get room information from TikTok page (web first, mobile fallback)
pub async fn get_room_info(
    client: &Client,
    account: &Account,
    url: &str,
) -> Result<RoomInfo, RecorderError> {
    get_room_info_with_feed_override(client, account, url, None).await
}

pub async fn get_live_room_id_with_feed_override(
    client: &Client,
    account: &Account,
    url: &str,
    feed_override: Option<&str>,
) -> Result<String, RecorderError> {
    if !extract_username_from_url(url).is_empty() {
        match get_room_id_from_room_enter(client, account, url).await {
            Ok(Some(room_id)) => {
                return Ok(room_id);
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("TikTok room enter room id failed: {err}");
            }
        }
        match get_room_id_from_check_alive(client, account, url).await {
            Ok(Some(room_id)) => {
                return Ok(room_id);
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("TikTok check_alive room id failed: {err}");
            }
        }
    }
    if !extract_username_from_url(url).is_empty()
        && (feed_override.is_some() || feed_url_from_env(url, account).is_some())
    {
        match get_room_id_from_feed(client, account, url, feed_override).await {
            Ok(room_id) => {
                return Ok(room_id);
            }
            Err(err) => {
                log::warn!("TikTok feed room id failed: {err}");
            }
        }
    }
    Err(RecorderError::ApiError {
        error: "TikTok live room id unavailable".to_string(),
    })
}

/// Get room information from TikTok page (web first, mobile fallback), with optional feed override
pub async fn get_room_info_with_feed_override(
    client: &Client,
    account: &Account,
    url: &str,
    feed_override: Option<&str>,
) -> Result<RoomInfo, RecorderError> {
    if !extract_username_from_url(url).is_empty() {
        match get_room_id_from_room_enter(client, account, url).await {
            Ok(Some(room_id)) => {
                if let Ok(room_info) =
                    get_room_info_by_room_id(client, account, &room_id, url).await
                {
                    return Ok(room_info);
                }
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("TikTok room enter room info failed: {err}");
            }
        }
    }
    if !extract_username_from_url(url).is_empty() {
        match get_room_id_from_check_alive(client, account, url).await {
            Ok(Some(room_id)) => {
                if let Ok(room_info) =
                    get_room_info_by_room_id(client, account, &room_id, url).await
                {
                    return Ok(room_info);
                }
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("TikTok check_alive room info failed: {err}");
            }
        }
    }
    if !extract_username_from_url(url).is_empty()
        && (feed_override.is_some() || feed_url_from_env(url, account).is_some())
    {
        match get_room_id_from_feed(client, account, url, feed_override).await {
            Ok(room_id) => {
                if let Ok(room_info) =
                    get_room_info_by_room_id(client, account, &room_id, url).await
                {
                    return Ok(room_info);
                }
            }
            Err(err) => {
                log::warn!("TikTok feed room info failed: {err}");
            }
        }
    }
    if force_mobile_fallback() {
        let mobile_url = to_mobile_url(url);
        log::warn!("TikTok forced mobile room info: {}", mobile_url);
        return get_room_info_with_profile(client, account, &mobile_url, MOBILE_USER_AGENT, true)
            .await;
    }
    match get_room_info_with_profile(client, account, url, USER_AGENT, false).await {
        Ok(info) => Ok(info),
        Err(err) => {
            let mobile_url = to_mobile_url(url);
            log::warn!(
                "TikTok web room info failed, retry with mobile page: {}",
                mobile_url
            );
            get_room_info_with_profile(client, account, &mobile_url, MOBILE_USER_AGENT, true)
                .await
                .map_err(|fallback_err| {
                    log::warn!("TikTok mobile room info failed: {fallback_err}");
                    err
                })
        }
    }
}

async fn get_stream_url_with_profile(
    client: &Client,
    account: &Account,
    url: &str,
    user_agent: &str,
    mobile: bool,
) -> Result<StreamInfo, RecorderError> {
    let mut headers = build_page_headers(url, user_agent, mobile);
    headers.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }

    warm_up_live_pages(client, account).await;

    for attempt in 0..5 {
        let response = client.get(url).headers(headers.clone()).send().await?;
        let status = response.status();
        let html_str = response.text().await?;
        if !status.is_success() {
            return Err(RecorderError::ApiError {
                error: format!("TikTok response status: {}", status),
            });
        }

        if is_waf_wait_page(&html_str) {
            if attempt < 4 {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }
        }

        if html_str.contains("UNEXPECTED_EOF_WHILE_READING") {
            if attempt < 2 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            } else {
                return Err(RecorderError::ApiError {
                    error: "Failed to load page after 3 attempts".to_string(),
                });
            }
        }

        let sigi_value = match extract_state_value(&html_str) {
            Some(value) => value,
            None => {
                if attempt < 2 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(RecorderError::ApiError {
                    error: "Failed to extract page state JSON".to_string(),
                });
            }
        };

        if let Some(live_room_info) = extract_live_room_user_info(&sigi_value) {
            let candidates = extract_stream_candidates(&live_room_info);
            if let Some(stream_info) = select_accessible_stream(client, &headers, &candidates).await
            {
                return Ok(stream_info);
            }
        }

        let mut stream_url = serde_json::from_value::<SigiStateResponse>(sigi_value.clone())
            .ok()
            .and_then(|state| state.room_store)
            .and_then(|store| store.room_info)
            .and_then(|room_info| room_info.stream_url);

        if stream_url.is_none() {
            stream_url = find_room_info(&sigi_value).and_then(|room_info| room_info.stream_url);
        }

        if stream_url.is_none() {
            stream_url = find_stream_url(&sigi_value);
        }

        if let Some(stream_url) = stream_url {
            let mut info = StreamInfo {
                hls_url: stream_url.hls_pull_url,
                rtmp_url: stream_url.rtmp_pull_url,
                resolution: None,
            };
            if let Some(hls_url) = info.hls_url.as_deref() {
                if !check_hls_stream_accessible(client, &headers, hls_url).await {
                    info.hls_url = None;
                }
            }
            if info.hls_url.is_none() && info.rtmp_url.is_none() {
                return Err(RecorderError::ApiError {
                    error: "No available stream provided".to_string(),
                });
            }
            return Ok(info);
        }

        if let Some(m3u8_url) =
            find_m3u8_in_value(&sigi_value).or_else(|| extract_m3u8_from_html(&html_str))
        {
            return Ok(StreamInfo {
                hls_url: Some(m3u8_url),
                rtmp_url: None,
                resolution: None,
            });
        }

        return Err(RecorderError::ApiError {
            error: "Failed to extract stream URL from page data".to_string(),
        });
    }

    Err(RecorderError::ApiError {
        error: "Failed to fetch TikTok page after retries".to_string(),
    })
}

/// Get stream URL from TikTok page (web first, mobile fallback)
pub async fn get_stream_url(
    client: &Client,
    account: &Account,
    url: &str,
) -> Result<StreamInfo, RecorderError> {
    get_stream_url_with_feed_override(client, account, url, None).await
}

/// Get stream URL from TikTok page (web first, mobile fallback), with optional feed override
pub async fn get_stream_url_with_feed_override(
    client: &Client,
    account: &Account,
    url: &str,
    feed_override: Option<&str>,
) -> Result<StreamInfo, RecorderError> {
    if !extract_username_from_url(url).is_empty() {
        match get_room_id_from_room_enter(client, account, url).await {
            Ok(Some(room_id)) => {
                if let Ok(stream_info) =
                    get_stream_url_from_room_info_api(client, account, &room_id).await
                {
                    return Ok(stream_info);
                }
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("TikTok room enter stream failed: {err}");
            }
        }
    }
    if !extract_username_from_url(url).is_empty() {
        match get_room_id_from_check_alive(client, account, url).await {
            Ok(Some(room_id)) => {
                if let Ok(stream_info) =
                    get_stream_url_from_room_info_api(client, account, &room_id).await
                {
                    return Ok(stream_info);
                }
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("TikTok check_alive stream failed: {err}");
            }
        }
    }
    if !extract_username_from_url(url).is_empty()
        && (feed_override.is_some() || feed_url_from_env(url, account).is_some())
    {
        match get_room_id_from_feed(client, account, url, feed_override).await {
            Ok(room_id) => {
                if let Ok(stream_info) =
                    get_stream_url_from_room_info_api(client, account, &room_id).await
                {
                    return Ok(stream_info);
                }
            }
            Err(err) => {
                log::warn!("TikTok feed stream failed: {err}");
            }
        }
    }
    if force_mobile_fallback() {
        let mobile_url = to_mobile_url(url);
        log::warn!("TikTok forced mobile stream: {}", mobile_url);
        return get_stream_url_with_profile(client, account, &mobile_url, MOBILE_USER_AGENT, true)
            .await;
    }
    match get_stream_url_with_profile(client, account, url, USER_AGENT, false).await {
        Ok(info) => Ok(info),
        Err(err) => {
            let mobile_url = to_mobile_url(url);
            log::warn!(
                "TikTok web stream failed, retry with mobile page: {}",
                mobile_url
            );
            get_stream_url_with_profile(client, account, &mobile_url, MOBILE_USER_AGENT, true)
                .await
                .map_err(|fallback_err| {
                    log::warn!("TikTok mobile stream failed: {fallback_err}");
                    err
                })
        }
    }
}

pub async fn get_stream_url_by_room_id(
    client: &Client,
    account: &Account,
    room_id: &str,
) -> Result<StreamInfo, RecorderError> {
    get_stream_url_from_room_info_api(client, account, room_id).await
}

#[cfg(test)]
mod tests {
    use super::{force_mobile_fallback, to_mobile_url};

    #[test]
    fn mobile_url_rewrite() {
        let web = "https://www.tiktok.com/@abc/live?x=1";
        assert_eq!(to_mobile_url(web), "https://m.tiktok.com/@abc/live?x=1");
        let mobile = "https://m.tiktok.com/@abc/live?x=1";
        assert_eq!(to_mobile_url(mobile), mobile);
    }

    #[test]
    fn force_mobile_env_flag() {
        std::env::remove_var("LIVE_TIKTOK_FORCE_MOBILE");
        assert!(!force_mobile_fallback());

        std::env::set_var("LIVE_TIKTOK_FORCE_MOBILE", "true");
        assert!(force_mobile_fallback());

        std::env::set_var("LIVE_TIKTOK_FORCE_MOBILE", "0");
        assert!(!force_mobile_fallback());

        std::env::remove_var("LIVE_TIKTOK_FORCE_MOBILE");
    }
}

/// Get user information from TikTok
pub async fn get_user_info(
    client: &Client,
    account: &Account,
) -> Result<crate::UserInfo, RecorderError> {
    let mut headers = HeaderMap::new();
    headers.insert("User-Agent", USER_AGENT.parse().unwrap());
    headers.insert(
        "Accept-Language",
        "zh-CN,zh;q=0.8,zh-TW;q=0.7,zh-HK;q=0.5,en-US;q=0.3,en;q=0.2"
            .parse()
            .unwrap(),
    );

    if !account.cookies.is_empty() {
        headers.insert("Cookie", account.cookies.parse().unwrap());
    }

    let proxy_url = proxy_url_from_env();
    let request_client = if let Some(proxy_url) = proxy_url.as_deref() {
        build_proxy_client(proxy_url)?
    } else {
        client.clone()
    };

    // Access TikTok homepage to get user info from state
    let response = request_client
        .get("https://www.tiktok.com/")
        .headers(headers.clone())
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(RecorderError::ApiError {
            error: format!("Failed to fetch TikTok, status: {}", response.status()),
        });
    }

    let html_str = response.text().await?;

    // Check for region block
    if html_str.contains("We regret to inform you that we have discontinued operating TikTok") {
        return Err(RecorderError::ApiError {
            error: "TikTok is not available in this region. Please use a different proxy."
                .to_string(),
        });
    }

    // 1. Try Passport API first (more reliable JSON API)
    if let Ok(info) = get_user_info_from_passport(&request_client, &headers).await {
        return Ok(info);
    }
    if let Ok(info) = get_user_info_from_passport_account(&request_client, account).await {
        return Ok(info);
    }

    let state = extract_state_value(&html_str).ok_or(RecorderError::ApiError {
        error: "Failed to extract TikTok state - please check if your network can access TikTok normally (proxy might be needed).".to_string(),
    })?;

    // Try to find current user in SigiState
    if let Some(user_info) = find_current_user_info(&state) {
        return Ok(user_info);
    }

    // Fallback: extract from specific path if known
    // SIGI_STATE -> UserModule -> users -> [username]
    if let Some(user_module) = state.get("UserModule").and_then(|u| u.get("users")) {
        if let Some(obj) = user_module.as_object() {
            if let Some((_, user_val)) = obj.iter().next() {
                let user_id = get_string_field(
                    user_val.as_object().unwrap_or(&Map::new()),
                    &["id", "secUid"],
                )
                .unwrap_or_default();
                let user_name = get_string_field(
                    user_val.as_object().unwrap_or(&Map::new()),
                    &["nickname", "uniqueId"],
                )
                .unwrap_or_default();
                let user_avatar = find_avatar_url(user_val).unwrap_or_default();

                if !user_id.is_empty() && !user_name.is_empty() {
                    return Ok(crate::UserInfo {
                        user_id,
                        user_name,
                        user_avatar,
                    });
                }
            }
        }
    }

    Err(RecorderError::ApiError {
        error: "Could not find user info in TikTok page".to_string(),
    })
}

pub async fn check_proxy_available(client: &Client) -> Result<TikTokProxyCheck, RecorderError> {
    let proxy_url = proxy_url_from_env();
    let request_client = if let Some(proxy_url) = proxy_url.as_deref() {
        build_proxy_client(proxy_url)?
    } else {
        client.clone()
    };
    let url = "https://www.tiktok.com/";
    let headers = build_page_headers(url, USER_AGENT, false);
    let response = request_client.get(url).headers(headers).send().await?;
    let status = response.status();
    let html_str = response.text().await.unwrap_or_default();
    let waf = is_waf_wait_page(&html_str);
    let ok = status.is_success() && !waf;
    let message = if ok {
        "ok".to_string()
    } else if !status.is_success() {
        format!("status {}", status)
    } else if waf {
        "waf".to_string()
    } else {
        "unexpected".to_string()
    };
    Ok(TikTokProxyCheck {
        ok,
        status: status.as_u16(),
        waf,
        proxy: proxy_url,
        message,
    })
}

pub async fn check_cookie_available(
    client: &Client,
    account: &Account,
) -> Result<TikTokCookieCheck, RecorderError> {
    if account.cookies.trim().is_empty() {
        return Ok(TikTokCookieCheck {
            ok: false,
            user_id: String::new(),
            user_name: String::new(),
            message: "cookies empty".to_string(),
        });
    }
    let proxy_url = proxy_url_from_env();
    let request_client = if let Some(proxy_url) = proxy_url.as_deref() {
        build_proxy_client(proxy_url)?
    } else {
        client.clone()
    };
    match get_user_info_from_passport_account(&request_client, account).await {
        Ok(info) => Ok(TikTokCookieCheck {
            ok: true,
            user_id: info.user_id,
            user_name: info.user_name,
            message: "ok".to_string(),
        }),
        Err(err) => Ok(TikTokCookieCheck {
            ok: false,
            user_id: String::new(),
            user_name: String::new(),
            message: err.to_string(),
        }),
    }
}

fn find_current_user_info(value: &Value) -> Option<crate::UserInfo> {
    // 1. In SIGI_STATE, the current user is often under "AppContext" or "UserModule"
    if let Some(user) = value
        .get("AppContext")
        .and_then(|a| a.get("appContext"))
        .and_then(|c| c.get("user"))
    {
        let user_id = get_string_field(user.as_object().unwrap_or(&Map::new()), &["uid", "id"])
            .unwrap_or_default();
        let user_name = get_string_field(
            user.as_object().unwrap_or(&Map::new()),
            &["nickname", "uniqueId"],
        )
        .unwrap_or_default();
        let user_avatar = find_avatar_url(user).unwrap_or_default();

        if !user_id.is_empty() {
            return Some(crate::UserInfo {
                user_id: user_id.clone(),
                user_name: if user_name.is_empty() {
                    user_id.clone()
                } else {
                    user_name
                },
                user_avatar,
            });
        }
    }

    // 2. In __UNIVERSAL_DATA_FOR_REHYDRATION__, it's under webapp.user-info
    if let Some(obj) = value.as_object() {
        for val in obj.values() {
            if let Some(user) = val
                .get("__DEFAULT_SCOPE__")
                .and_then(|s| s.get("webapp.user-info"))
                .and_then(|u| u.get("data"))
                .and_then(|d| d.get("user"))
            {
                let user_id = get_string_field(
                    user.as_object().unwrap_or(&Map::new()),
                    &["uid", "id", "secUid"],
                )
                .unwrap_or_default();
                let user_name = get_string_field(
                    user.as_object().unwrap_or(&Map::new()),
                    &["nickname", "uniqueId"],
                )
                .unwrap_or_default();
                let user_avatar = find_avatar_url(user).unwrap_or_default();

                if !user_id.is_empty() {
                    return Some(crate::UserInfo {
                        user_id: user_id.clone(),
                        user_name: if user_name.is_empty() {
                            user_id.clone()
                        } else {
                            user_name
                        },
                        user_avatar,
                    });
                }
            }
        }
    }

    None
}

/// Get user info from TikTok passport API
async fn get_user_info_from_passport(
    client: &reqwest::Client,
    headers: &reqwest::header::HeaderMap,
) -> Result<crate::UserInfo, RecorderError> {
    let response = client
        .get("https://www.tiktok.com/passport/web/user/info/")
        .headers(headers.clone())
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(RecorderError::ApiError {
            error: format!(
                "Failed to fetch TikTok passport info, status: {}",
                response.status()
            ),
        });
    }

    let json: Value = response.json().await?;
    if let Some(data) = json.get("data").and_then(|d| d.as_object()) {
        let user_id = get_string_field(data, &["user_id", "uid", "id"]).unwrap_or_default();
        let user_name = get_string_field(data, &["nickname", "unique_id"]).unwrap_or_default();
        let user_avatar = find_avatar_url(&json).unwrap_or_default();

        if !user_id.is_empty() {
            let final_name = if user_name.is_empty() {
                user_id.clone()
            } else {
                user_name
            };
            return Ok(crate::UserInfo {
                user_id,
                user_name: final_name,
                user_avatar,
            });
        }
    }

    Err(RecorderError::ApiError {
        error: "User info not found in passport response".to_string(),
    })
}

/// Get user info from TikTok passport account/info API
async fn get_user_info_from_passport_account(
    client: &reqwest::Client,
    account: &Account,
) -> Result<crate::UserInfo, RecorderError> {
    let handle = account.name.trim().trim_start_matches('@').to_string();
    let handle = if handle.is_empty() {
        account.id.trim().trim_start_matches('@').to_string()
    } else {
        handle
    };
    let referer = build_profile_referer(&handle);
    let url = build_account_info_url(account, &referer);
    let headers = build_account_info_headers(account, &referer);
    let response = client.get(url).headers(headers).send().await?;

    if !response.status().is_success() {
        return Err(RecorderError::ApiError {
            error: format!(
                "Failed to fetch TikTok account info, status: {}",
                response.status()
            ),
        });
    }

    let json: Value = response.json().await?;
    let data = json
        .get("data")
        .and_then(|d| d.as_object())
        .ok_or_else(|| RecorderError::ApiError {
            error: "User info not found in account info response".to_string(),
        })?;

    let user_id =
        get_string_field(data, &["user_id_str", "user_id", "uid", "id"]).unwrap_or_default();
    let user_name = get_string_field(data, &["screen_name", "username", "unique_id", "nickname"])
        .unwrap_or_default();
    let user_avatar = data
        .get("avatar_url")
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .unwrap_or_default();

    if user_id.is_empty() {
        return Err(RecorderError::ApiError {
            error: "User id not found in account info response".to_string(),
        });
    }

    let final_name = if user_name.is_empty() {
        user_id.clone()
    } else {
        user_name
    };

    Ok(crate::UserInfo {
        user_id,
        user_name: final_name,
        user_avatar,
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
