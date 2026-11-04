pub mod api;
pub mod response;

use crate::account::Account;
use crate::core::flv_recorder::FlvRecorder;
use crate::core::hls_recorder::{construct_stream_from_variant, HlsRecorder};
use crate::core::rtmp_recorder::RtmpRecorder;
use crate::core::{Codec, Format};
use crate::danmu::DanmuStorage;
use crate::errors::{is_guest_cookie_block_error, RecorderError};
use crate::events::RecorderEvent;
use crate::platforms::PlatformType;
use crate::traits::RecorderTrait;
use crate::{CachePath, Recorder, RoomInfo, UserInfo};
use async_trait::async_trait;
use chrono::Utc;
use danmu_stream::danmu_stream::DanmuStream;
use danmu_stream::provider::ProviderType;
use reqwest::StatusCode;
use regex::Regex;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{atomic, Arc};
use std::time::Duration;
use tokio::sync::{broadcast, Mutex, RwLock};
use url::Url;

const KUAISHOU_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const KUAISHOU_MOBILE_USER_AGENT: &str =
    "ios/7.830 (ios 17.0; ; iPhone 15 (A2846/A3089/A3090/A3092))";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KuaishouProtocol {
    Hls,
    Flv,
    Rtmp,
}

impl KuaishouProtocol {
    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hls" | "m3u8" => Some(Self::Hls),
            "flv" => Some(Self::Flv),
            "rtmp" => Some(Self::Rtmp),
            _ => None,
        }
    }

    fn matches_url(self, url: &str) -> bool {
        match self {
            Self::Hls => url.contains(".m3u8"),
            Self::Flv => url.contains(".flv"),
            Self::Rtmp => url.starts_with("rtmp://") || url.starts_with("rtmps://"),
        }
    }
}

#[derive(Clone)]
pub struct KuaishouExtra {
    stream_url: Arc<RwLock<Option<String>>>,
    stream_list: Arc<RwLock<Vec<api::StreamInfo>>>,
    pre_live_id: Arc<RwLock<Option<String>>>,
    should_continue: Arc<AtomicBool>,
    last_error_ts: Arc<AtomicI64>,
    rate_limit_until_ts: Arc<AtomicI64>,
    rate_limit_streak: Arc<AtomicU32>,
    resolution: Arc<RwLock<Option<String>>>,
}

pub type KuaishouRecorder = Recorder<KuaishouExtra>;

impl KuaishouRecorder {
    fn merge_cookie_headers(primary: &str, extra: &str) -> String {
        let mut merged = std::collections::BTreeMap::new();
        for source in [extra, primary] {
            for part in source.split(';').map(str::trim) {
                if part.is_empty() {
                    continue;
                }
                let Some((key, value)) = part.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                let value = value.trim();
                if key.is_empty() || value.is_empty() {
                    continue;
                }
                merged.insert(key.to_string(), value.to_string());
            }
        }
        merged
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn is_placeholder_text(value: &str) -> bool {
        matches!(value.trim(), "" | "Kuaishou" | "Kuaishou Live")
    }

    pub async fn new(
        room_id: &str,
        account: &Account,
        cache_dir: PathBuf,
        event_channel: broadcast::Sender<RecorderEvent>,
        update_interval: Arc<atomic::AtomicU64>,
        enabled: bool,
    ) -> Result<Self, RecorderError> {
        let mut default_headers = reqwest::header::HeaderMap::new();
        default_headers.insert("Referer", "https://live.kuaishou.com/".parse().unwrap());
        default_headers.insert("User-Agent", KUAISHOU_USER_AGENT.parse().unwrap());

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .cookie_store(true)
            .build()
            .map_err(|e| RecorderError::ApiError {
                error: e.to_string(),
            })?;
        let extra = KuaishouExtra {
            stream_url: Arc::new(RwLock::new(None)),
            stream_list: Arc::new(RwLock::new(Vec::new())),
            pre_live_id: Arc::new(RwLock::new(None)),
            should_continue: Arc::new(AtomicBool::new(false)),
            last_error_ts: Arc::new(AtomicI64::new(0)),
            rate_limit_until_ts: Arc::new(AtomicI64::new(0)),
            rate_limit_streak: Arc::new(AtomicU32::new(0)),
            resolution: Arc::new(RwLock::new(None)),
        };

        let recorder = Self {
            platform: PlatformType::Kuaishou,
            room_id: room_id.to_string(),
            account: account.clone(),
            client,
            event_channel,
            cache_dir,
            quit: Arc::new(atomic::AtomicBool::new(false)),
            enabled: Arc::new(atomic::AtomicBool::new(enabled)),
            update_interval,
            is_recording: Arc::new(atomic::AtomicBool::new(false)),
            room_info: Arc::new(RwLock::new(RoomInfo::default())),
            user_info: Arc::new(RwLock::new(UserInfo::default())),
            platform_live_id: Arc::new(RwLock::new(String::new())),
            live_id: Arc::new(RwLock::new(String::new())),
            danmu_storage: Arc::new(RwLock::new(None)),
            last_update: Arc::new(atomic::AtomicI64::new(Utc::now().timestamp())),
            last_sequence: Arc::new(atomic::AtomicU64::new(0)),
            danmu_task: Arc::new(Mutex::new(None)),
            record_task: Arc::new(Mutex::new(None)),
            extra,
        };

        log::info!("[Kuaishou][{}]Recorder created", room_id);

        Ok(recorder)
    }

    fn log_info(&self, message: &str) {
        log::info!("[Kuaishou][{}]{}", self.room_id, message);
    }

    fn log_error(&self, message: &str) {
        log::error!("[Kuaishou][{}]{}", self.room_id, message);
    }

    fn room_lookup_input(room_id: &str) -> String {
        let trimmed = room_id.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return trimmed.to_string();
        }
        if let Some((prefix, suffix)) = trimmed.split_once('#') {
            if prefix.trim().eq_ignore_ascii_case("kuaishou") {
                let principal_id = suffix.trim();
                if !principal_id.is_empty() {
                    return principal_id.to_string();
                }
            }
        }
        trimmed.trim_start_matches('@').to_string()
    }

    fn get_cookie_value_ci(cookies: &str, key: &str) -> Option<String> {
        let target = key.to_ascii_lowercase();
        for part in cookies.split(';').map(str::trim) {
            if let Some((k, v)) = part.split_once('=') {
                if k.trim().to_ascii_lowercase() == target {
                    let value = v.trim();
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
        None
    }

    fn append_cookie_pair(mut cookie: String, key: &str, value: &str) -> String {
        let value = value.trim();
        if value.is_empty() {
            return cookie;
        }
        if !cookie.is_empty() {
            cookie.push_str("; ");
        }
        cookie.push_str(key);
        cookie.push('=');
        cookie.push_str(value);
        cookie
    }

    fn infer_live_stream_id_from_stream_url(stream_url: &str) -> Option<String> {
        if let Ok(url) = Url::parse(stream_url) {
            for (k, v) in url.query_pairs() {
                let key = k.to_ascii_lowercase();
                if key == "livestreamid" || key == "live_stream_id" {
                    let value = v.trim();
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }

            if let Some(last) = url.path_segments().and_then(|mut seg| seg.next_back()) {
                let stem = last.split('.').next().unwrap_or(last);
                if let Some((id, _)) = stem.split_once("_Game") {
                    let id = id.trim();
                    if !id.is_empty() {
                        return Some(id.to_string());
                    }
                }
            }
        }

        let patterns = [
            r"(?i)(?:liveStreamId|live_stream_id)=([^&?#]+)",
            r"/([A-Za-z0-9_-]{8,})_Game[A-Za-z0-9_-]*",
        ];
        for pattern in patterns {
            if let Ok(re) = Regex::new(pattern) {
                if let Some(captures) = re.captures(stream_url) {
                    if let Some(m) = captures.get(1) {
                        let value = m.as_str().trim();
                        if !value.is_empty() {
                            return Some(value.to_string());
                        }
                    }
                }
            }
        }

        None
    }

    fn is_nonfatal_danmu_error(err_text: &str) -> bool {
        let lower = err_text.to_ascii_lowercase();
        lower.contains("livestreamid missing")
            || lower.contains("room is not live")
            || lower.contains("room not live")
    }

    fn build_stream_headers(is_mobile_stream: bool, cookies: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if is_mobile_stream {
            headers.insert("Referer", "https://www.kuaishou.com/".parse().unwrap());
            headers.insert("Origin", "https://www.kuaishou.com".parse().unwrap());
            headers.insert("User-Agent", KUAISHOU_MOBILE_USER_AGENT.parse().unwrap());
        } else {
            headers.insert("Referer", "https://live.kuaishou.com/".parse().unwrap());
            headers.insert("Origin", "https://live.kuaishou.com".parse().unwrap());
            headers.insert("User-Agent", KUAISHOU_USER_AGENT.parse().unwrap());
        }
        if !cookies.is_empty() {
            headers.insert("Cookie", cookies.parse().unwrap());
        }
        headers
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

    async fn check_hls_stream_accessible(
        client: &reqwest::Client,
        headers: &reqwest::header::HeaderMap,
        m3u8_url: &str,
    ) -> bool {
        let response = match client.get(m3u8_url).headers(headers.clone()).send().await {
            Ok(resp) => resp,
            Err(_) => return false,
        };
        if !response.status().is_success() {
            return false;
        }
        let mut playlist_text = match response.text().await {
            Ok(text) => text,
            Err(_) => return false,
        };
        let mut playlist_url = m3u8_url.to_string();

        let first_uri = match Self::extract_first_media_uri(&playlist_text) {
            Some(uri) => uri,
            None => return false,
        };

        if first_uri.contains(".m3u8") {
            let resolved = match Self::resolve_uri(m3u8_url, &first_uri) {
                Some(url) => url,
                None => return false,
            };
            let response = match client.get(&resolved).headers(headers.clone()).send().await {
                Ok(resp) => resp,
                Err(_) => return false,
            };
            if !response.status().is_success() {
                return false;
            }
            playlist_text = match response.text().await {
                Ok(text) => text,
                Err(_) => return false,
            };
            playlist_url = resolved;
        }

        let first_segment = match Self::extract_first_media_uri(&playlist_text) {
            Some(uri) => uri,
            None => return false,
        };
        let stream =
            match construct_stream_from_variant("probe", &playlist_url, Format::TS, Codec::Avc)
                .await
            {
                Ok(stream) => stream,
                Err(_) => return false,
            };
        let segment_url = stream.ts_url(&first_segment);
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
        response.status().is_success()
    }

    fn parse_bool_env(value: &str) -> Option<bool> {
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }

    fn prefer_flv_env() -> Option<bool> {
        std::env::var("LIVE_KUAISHOU_PREFER_FLV")
            .ok()
            .and_then(|v| Self::parse_bool_env(&v))
    }

    fn prefer_protocol() -> KuaishouProtocol {
        if let Ok(value) = std::env::var("LIVE_KUAISHOU_PREFER_PROTOCOL") {
            if let Some(protocol) = KuaishouProtocol::from_str(&value) {
                return protocol;
            }
        }
        if let Some(prefer_flv) = Self::prefer_flv_env() {
            if prefer_flv {
                return KuaishouProtocol::Flv;
            }
        }
        KuaishouProtocol::Hls
    }

    fn protocol_preference_overridden() -> bool {
        std::env::var("LIVE_KUAISHOU_PREFER_PROTOCOL")
            .ok()
            .and_then(|value| KuaishouProtocol::from_str(&value))
            .is_some()
            || Self::prefer_flv_env().is_some()
    }

    fn startup_stagger_max_secs() -> u64 {
        std::env::var("LIVE_KUAISHOU_STARTUP_STAGGER_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(2)
    }

    fn startup_stagger_secs(&self) -> u64 {
        let max = Self::startup_stagger_max_secs();
        if max == 0 {
            return 0;
        }
        let mut hasher = DefaultHasher::new();
        self.room_id.hash(&mut hasher);
        hasher.finish() % (max + 1)
    }

    fn read_env_u64(key: &str, default: u64) -> u64 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(default)
    }

    fn rate_limit_base_backoff_secs() -> u64 {
        Self::read_env_u64("LIVE_KUAISHOU_RATE_LIMIT_BASE_BACKOFF_SECS", 30)
    }

    fn rate_limit_max_backoff_secs() -> u64 {
        Self::read_env_u64("LIVE_KUAISHOU_RATE_LIMIT_MAX_BACKOFF_SECS", 300)
    }

    fn max_idle_poll_secs() -> u64 {
        Self::read_env_u64("LIVE_KUAISHOU_MAX_IDLE_POLL_SECS", 18)
    }

    fn fast_probe_rounds() -> u64 {
        Self::read_env_u64("LIVE_KUAISHOU_FAST_PROBE_ROUNDS", 4)
    }

    fn apply_rate_limit_backoff(&self) {
        let streak = self
            .extra
            .rate_limit_streak
            .fetch_add(1, atomic::Ordering::Relaxed)
            .saturating_add(1);
        let exp = streak.saturating_sub(1).min(6);
        let base = Self::rate_limit_base_backoff_secs();
        let max_backoff = Self::rate_limit_max_backoff_secs();
        let mut backoff = base.saturating_mul(1_u64 << exp);
        if max_backoff > 0 {
            backoff = backoff.min(max_backoff);
        }
        let jitter = rand::random::<u64>() % 11;
        let until = Utc::now()
            .timestamp()
            .saturating_add((backoff + jitter) as i64);
        self.extra
            .rate_limit_until_ts
            .store(until, atomic::Ordering::Relaxed);
        self.extra
            .last_error_ts
            .store(Utc::now().timestamp(), atomic::Ordering::Relaxed);
        self.log_info(&format!(
            "Rate limited: streak={}, backoff={}s(+{}s), next retry at {}",
            streak,
            backoff,
            jitter,
            until
        ));
    }

    fn clear_rate_limit_backoff(&self) {
        self.extra
            .rate_limit_streak
            .store(0, atomic::Ordering::Relaxed);
        self.extra
            .rate_limit_until_ts
            .store(0, atomic::Ordering::Relaxed);
    }

    fn select_stream_url(
        streams: &[api::StreamInfo],
        prefer: KuaishouProtocol,
        prefer_login_direct: bool,
    ) -> Option<String> {
        let is_direct = |url: &str| url.contains("pull.yximgs.com");

        if prefer_login_direct {
            if let Some(selected) = streams
                .iter()
                .find(|stream| is_direct(&stream.url) && prefer.matches_url(&stream.url))
                .map(|stream| stream.url.clone())
            {
                return Some(selected);
            }
            if let Some(selected) = streams
                .iter()
                .find(|stream| is_direct(&stream.url) && KuaishouProtocol::Flv.matches_url(&stream.url))
                .map(|stream| stream.url.clone())
            {
                return Some(selected);
            }
            if let Some(selected) = streams
                .iter()
                .find(|stream| is_direct(&stream.url))
                .map(|stream| stream.url.clone())
            {
                return Some(selected);
            }
        }

        let mut selected = streams
            .iter()
            .find(|stream| prefer.matches_url(&stream.url))
            .map(|stream| stream.url.clone());

        if selected.is_none() {
            selected = streams
                .iter()
                .find(|stream| KuaishouProtocol::Hls.matches_url(&stream.url))
                .map(|stream| stream.url.clone());
        }

        if selected.is_none() {
            selected = streams
                .iter()
                .find(|stream| KuaishouProtocol::Flv.matches_url(&stream.url))
                .map(|stream| stream.url.clone());
        }

        if selected.is_none() {
            selected = streams
                .iter()
                .find(|stream| KuaishouProtocol::Rtmp.matches_url(&stream.url))
                .map(|stream| stream.url.clone());
        }

        if selected.is_none() {
            selected = streams.first().map(|stream| stream.url.clone());
        }

        selected
    }

    pub async fn reset(&self) {
        *self.extra.stream_url.write().await = None;
        self.last_update
            .store(Utc::now().timestamp(), atomic::Ordering::Relaxed);
        *self.danmu_storage.write().await = None;
        *self.platform_live_id.write().await = String::new();
        *self.live_id.write().await = String::new();
        self.extra.last_error_ts.store(0, atomic::Ordering::Relaxed);
        if let Some(danmu_task) = self.danmu_task.lock().await.take() {
            danmu_task.abort();
            let _ = danmu_task.await;
            self.log_info("Danmu task aborted");
        }
    }

    async fn check_status(&self) -> bool {
        let pre_live_status = self.room_info.read().await.status;

        // Keep original principal identifier (supports KUAISHOU#xxxx format).
        let url = Self::room_lookup_input(&self.room_id);

        let account = self.account.clone();

        match api::get_room_info(&self.client, &account, &url).await {
            Ok(room_info) => {
                self.clear_rate_limit_backoff();
                let prev_room = self.room_info.read().await.clone();
                let prev_user = self.user_info.read().await.clone();
                let final_title = if room_info.room_title.is_empty()
                    || Self::is_placeholder_text(&room_info.room_title)
                {
                    prev_room.room_title.clone()
                } else {
                    room_info.room_title.clone()
                };
                let final_cover = if room_info.room_cover_url.is_empty() {
                    prev_room.room_cover.clone()
                } else {
                    room_info.room_cover_url.clone()
                };
                let final_user_name = if room_info.user_name.is_empty()
                    || Self::is_placeholder_text(&room_info.user_name)
                {
                    prev_user.user_name.clone()
                } else {
                    room_info.user_name.clone()
                };
                let final_avatar = if room_info.user_avatar.is_empty() {
                    prev_user.user_avatar.clone()
                } else {
                    room_info.user_avatar.clone()
                };

                *self.room_info.write().await = RoomInfo {
                    platform: "kuaishou".to_string(),
                    room_id: self.room_id.to_string(),
                    room_title: final_title,
                    room_cover: final_cover,
                    status: room_info.live_status,
                };

                // Update user info
                let should_update_user = {
                    let current = self.user_info.read().await;
                    current.user_id != room_info.user_id
                        || current.user_name != final_user_name
                        || current.user_avatar != final_avatar
                };
                if should_update_user {
                    *self.user_info.write().await = UserInfo {
                        user_id: room_info.user_id.to_string(),
                        user_name: final_user_name,
                        user_avatar: final_avatar,
                    }
                }

                let live_status = room_info.live_status;
                if pre_live_status != live_status {
                    self.log_info(&format!(
                        "Live status changed to {}, enabled: {}",
                        live_status,
                        self.enabled.load(atomic::Ordering::Relaxed)
                    ));

                    if live_status {
                        let _ = self.event_channel.send(RecorderEvent::LiveStart {
                            recorder: self.info().await,
                        });
                    } else {
                        let _ = self.event_channel.send(RecorderEvent::LiveEnd {
                            platform: PlatformType::Kuaishou,
                            room_id: self.room_id.to_string(),
                            recorder: self.info().await,
                        });
                        *self.live_id.write().await = String::new();
                    }

                    self.reset().await;
                }

                *self.platform_live_id.write().await = Utc::now().timestamp().to_string();

                if !live_status {
                    return false;
                }

                // No need to poll aggressively if should not record.
                if !self.should_record().await {
                    return false;
                }

                let mut streams = room_info.streams.clone();
                if streams.is_empty() {
                    self.log_info("Room info has no stream list, retry stream list via livedetail");
                    match api::get_stream_urls(&self.client, &self.account, &url).await {
                        Ok(fetched) => streams = fetched,
                        Err(e) => {
                            self.log_error(&format!("Fetch stream failed: {}", e));
                            return false;
                        }
                    }
                }

                let prefer = Self::prefer_protocol();
                let prefer_login_direct =
                    !self.account.is_guest() && !Self::protocol_preference_overridden();
                let selected_url = Self::select_stream_url(&streams, prefer, prefer_login_direct);

                if let Some(url) = selected_url {
                    *self.extra.stream_list.write().await = streams.clone();

                    // Find resolution
                    let resolution = streams
                        .iter()
                        .find(|s| s.url == url)
                        .map(|s| s.quality.clone());
                    *self.extra.resolution.write().await = resolution;

                    let pre_stream = self.extra.stream_url.read().await.clone();
                    *self.extra.stream_url.write().await = Some(url.clone());
                    self.last_update
                        .store(Utc::now().timestamp(), atomic::Ordering::Relaxed);

                    self.log_info(&format!(
                        "Update to new stream: {:?} => {}",
                        pre_stream, url
                    ));
                    true
                } else {
                    self.log_error("No stream URLs found");
                    false
                }
            }
            Err(e) => {
                if self.account.is_guest() && is_guest_cookie_block_error(&e) {
                    let _ = self
                        .event_channel
                        .send(RecorderEvent::GuestCookieRefreshRequested {
                            platform: PlatformType::Kuaishou,
                            reason: format!("kuaishou guest cookie blocked: {}", e),
                        });
                }
                if api::is_rate_limited_error(&e) {
                    self.apply_rate_limit_backoff();
                    return false;
                }
                if api::is_captcha_error(&e) {
                    self.log_info("Captcha required, pause polling and wait manual verification");
                    self.apply_rate_limit_backoff();
                    return false;
                }
                if api::is_room_disabled_error(&e) {
                    self.log_info("Room not enabled, skipping polling");
                    self.extra
                        .last_error_ts
                        .store(Utc::now().timestamp(), atomic::Ordering::Relaxed);
                    return false;
                }
                self.log_error(&format!("Update room status failed: {}", e));
                self.extra
                    .last_error_ts
                    .store(Utc::now().timestamp(), atomic::Ordering::Relaxed);
                pre_live_status
            }
        }
    }

    async fn danmu(&self, cookies: String) -> Result<(), RecorderError> {
        self.log_info(&format!(
            "Danmu cookie prepared: has_liveStreamId={}, has_web_st={}, has_pass_token={}, has_kwscode={}, has_kwfv1={}",
            cookies.contains("liveStreamId="),
            Self::get_cookie_value_ci(&cookies, "kuaishou.live.web_st").is_some(),
            Self::get_cookie_value_ci(&cookies, "passToken").is_some(),
            Self::get_cookie_value_ci(&cookies, "kwscode").is_some(),
            Self::get_cookie_value_ci(&cookies, "kwfv1").is_some(),
        ));

        let room_id = self.room_id.clone();
        let danmu_stream_res = DanmuStream::new(ProviderType::Kuaishou, &cookies, &room_id).await;

        let danmu_stream = match danmu_stream_res {
            Ok(stream) => stream,
            Err(err) => {
                let err_text = err.to_string();
                if Self::is_nonfatal_danmu_error(&err_text) {
                    self.log_info(&format!("Skip danmu init: {err_text}"));
                    return Ok(());
                }
                self.log_error(&format!("Failed to create danmu stream: {err_text}"));
                return Err(RecorderError::DanmuStreamError(err));
            }
        };

        let mut start_fut = Box::pin(danmu_stream.start());

        loop {
            tokio::select! {
                start_res = &mut start_fut => {
                    match start_res {
                        Ok(_) => {
                            self.log_info("Danmu stream finished");
                            return Ok(());
                        }
                        Err(err) => {
                            let err_text = err.to_string();
                            if Self::is_nonfatal_danmu_error(&err_text) {
                                self.log_info(&format!("Skip danmu start: {err_text}"));
                                return Ok(());
                            }
                            self.log_error(&format!("Danmu stream start error: {err_text}"));
                            return Err(RecorderError::DanmuStreamError(err));
                        }
                    }
                }
                recv_res = danmu_stream.recv() => {
                    match recv_res {
                        Ok(Some(msg)) => {
                            match msg {
                                danmu_stream::DanmuMessageType::DanmuMessage(danmu) => {
                                    let ts = Utc::now().timestamp_millis();
                                    let _ = self.event_channel.send(RecorderEvent::DanmuReceived {
                                        room: self.room_id.clone(),
                                        ts,
                                        content: danmu.message.clone(),
                                    });
                                    if let Some(storage) = self.danmu_storage.write().await.as_ref() {
                                        storage.add_line(ts, &danmu.message).await;
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            self.log_info("Danmu stream closed");
                            return Ok(());
                        }
                        Err(err) => {
                            self.log_error(&format!("Failed to receive danmu message: {err}"));
                            return Err(RecorderError::DanmuStreamError(err));
                        }
                    }
                }
            }
        }
    }

    /// Update entries for a new live
    async fn update_entries(&self, live_id: &str) -> Result<(), RecorderError> {
        let current_stream_url = self.extra.stream_url.read().await.clone();
        let Some(stream_url) = current_stream_url else {
            return Err(RecorderError::NoStreamAvailable);
        };
        let stream_list = self.extra.stream_list.read().await.clone();
        let fallback_hls = stream_list
            .iter()
            .find(|stream| stream.url.contains(".m3u8"))
            .map(|stream| stream.url.clone());
        let fallback_flv = stream_list
            .iter()
            .find(|stream| stream.url.contains(".flv"))
            .map(|stream| stream.url.clone());

        let work_dir = self.work_dir(live_id).await;
        self.log_info(&format!("New record started: {}", live_id));

        let _ = tokio::fs::create_dir_all(&work_dir.full_path()).await;

        // Download cover
        let room_info = self.room_info.read().await.clone();
        let cover_url = room_info.room_cover.clone();
        let cover_path = work_dir.with_filename("cover.jpg");
        let _ = api::download_file(&self.client, &cover_url, &cover_path.full_path()).await;

        let is_mobile_stream =
            stream_url.contains("auth_key=") || stream_url.contains("pull.yximgs.com");
        // Try to find the exact cookie used for this stream
        let selected_stream = stream_list.iter().find(|s| s.url == stream_url);
        let stream_cookie = selected_stream.and_then(|s| s.cookie.clone());

        let merged_cookie = match stream_cookie.as_deref() {
            Some(stream_cookie) if !stream_cookie.trim().is_empty() => {
                Self::merge_cookie_headers(&self.account.cookies, stream_cookie)
            }
            _ => self.account.cookies.clone(),
        };
        let cookies = api::build_kuaishou_runtime_cookie(&merged_cookie, &self.room_id);
        self.log_info(&format!(
            "Prepared stream cookie: stream_cookie_present={}, has_web_st={}, has_pass_token={}, has_kwscode={}, has_kwfv1={}, has_liveStreamId={}",
            stream_cookie
                .as_deref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false),
            Self::get_cookie_value_ci(&cookies, "kuaishou.live.web_st").is_some(),
            Self::get_cookie_value_ci(&cookies, "passToken").is_some(),
            Self::get_cookie_value_ci(&cookies, "kwscode").is_some(),
            Self::get_cookie_value_ci(&cookies, "kwfv1").is_some(),
            Self::get_cookie_value_ci(&cookies, "liveStreamId").is_some(),
        ));

        let mut danmu_cookie = cookies.clone();
        let has_live_stream_id = Self::get_cookie_value_ci(&danmu_cookie, "liveStreamId").is_some();
        if !has_live_stream_id {
            if let Some(live_stream_id) = Self::infer_live_stream_id_from_stream_url(&stream_url) {
                danmu_cookie = Self::append_cookie_pair(danmu_cookie, "liveStreamId", &live_stream_id);
                self.log_info(&format!("Inject liveStreamId into danmu cookie: {}", live_stream_id));
            } else {
                self.log_info("No liveStreamId inferred from stream URL for danmu");
            }
        }

        let danmu_path = work_dir.with_filename("danmu.txt");
        *self.danmu_storage.write().await = DanmuStorage::new(&danmu_path.full_path()).await;

        *self.live_id.write().await = live_id.to_string();

        let self_clone = self.clone();
        self.log_info(&format!("Start fetching danmu for live {live_id}"));
        *self.danmu_task.lock().await = Some(tokio::spawn(async move {
            let _ = self_clone.danmu(danmu_cookie).await;
        }));

        // Send record start event
        let _ = self.event_channel.send(RecorderEvent::RecordStart {
            recorder: self.info().await,
        });

        self.is_recording.store(true, atomic::Ordering::Relaxed);

        let web_headers = Self::build_stream_headers(false, &cookies);
        let h5_headers = Self::build_stream_headers(true, &cookies);
        let mut headers = if is_mobile_stream {
            h5_headers.clone()
        } else {
            web_headers.clone()
        };

        if stream_url.contains(".m3u8") {
            let web_ok =
                Self::check_hls_stream_accessible(&self.client, &web_headers, &stream_url).await;
            let h5_ok =
                Self::check_hls_stream_accessible(&self.client, &h5_headers, &stream_url).await;
            let selected = match (web_ok, h5_ok) {
                (true, false) => Some(("web", web_headers.clone())),
                (false, true) => Some(("h5", h5_headers.clone())),
                (true, true) => Some(("web", web_headers.clone())),
                (false, false) => None,
            };
            match selected {
                Some((label, chosen)) => {
                    headers = chosen;
                    self.log_info(&format!(
                        "HLS preflight: web={}, h5={}, using {} headers",
                        if web_ok { "ok" } else { "fail" },
                        if h5_ok { "ok" } else { "fail" },
                        label
                    ));
                }
                None => {
                    self.log_info(&format!(
                        "HLS preflight failed: web={}, h5={}, keep default headers",
                        if web_ok { "ok" } else { "fail" },
                        if h5_ok { "ok" } else { "fail" }
                    ));
                }
            }
        }

        if stream_url.starts_with("rtmp://") || stream_url.starts_with("rtmps://") {
            self.log_info("Using RTMP recorder");
            let rtmp_recorder = RtmpRecorder::new(
                stream_url,
                work_dir.full_path(),
                self.enabled.clone(),
                self.event_channel.clone(),
                live_id.to_string(),
            );
            if let Err(e) = rtmp_recorder.start().await {
                self.log_error(&format!("Rtmp recorder quit with error: {}", e));
                return Err(e);
            }
            return Ok(());
        }
        if stream_url.contains(".flv") {
            self.log_info("Using FLV recorder");
            let flv_recorder = FlvRecorder::new(
                stream_url.clone(),
                headers.clone(),
                work_dir.full_path(),
                self.enabled.clone(),
                self.event_channel.clone(),
                live_id.to_string(),
            );
            if let Err(e) = flv_recorder.start().await {
                self.log_error(&format!("Flv recorder quit with error: {}", e));
                if let Some(hls_url) = fallback_hls {
                    self.log_info("FLV failed, fallback to HLS recorder");
                    return self
                        .run_hls_recorder(
                            &hls_url,
                            &headers,
                            &work_dir,
                            live_id,
                            Some(cookies.clone()),
                        )
                        .await;
                }
                return Err(e);
            }
            return Ok(());
        }

        // Create HLS stream
        // Kuaishou stream URLs are direct m3u8 URLs
        if let Err(e) = self
            .run_hls_recorder(
                &stream_url,
                &headers,
                &work_dir,
                live_id,
                Some(cookies.clone()),
            )
            .await
        {
            let should_retry = matches!(
                &e,
                RecorderError::InvalidResponseStatus { status } if *status == StatusCode::FORBIDDEN
            );
            if should_retry {
                let alt_headers = Self::build_stream_headers(!is_mobile_stream, &cookies);
                self.log_info("HLS 403, retrying with alternate headers");
                if self
                    .run_hls_recorder(
                        &stream_url,
                        &alt_headers,
                        &work_dir,
                        live_id,
                        Some(cookies.clone()),
                    )
                    .await
                    .is_ok()
                {
                    return Ok(());
                }
                headers = alt_headers;
            }

            self.log_error(&format!("Hls recorder quit with error: {}", e));
            if let Some(flv_url) = fallback_flv {
                self.log_info("HLS failed, fallback to FLV recorder");
                let flv_recorder = FlvRecorder::new(
                    flv_url,
                    headers,
                    work_dir.full_path(),
                    self.enabled.clone(),
                    self.event_channel.clone(),
                    live_id.to_string(),
                );
                if let Err(err) = flv_recorder.start().await {
                    self.log_error(&format!("Flv recorder quit with error: {}", err));
                    return Err(err);
                }
                return Ok(());
            }
            return Err(e);
        }

        Ok(())
    }

    async fn run_hls_recorder(
        &self,
        stream_url: &str,
        headers: &reqwest::header::HeaderMap,
        work_dir: &CachePath,
        live_id: &str,
        cookie_header: Option<String>,
    ) -> Result<(), RecorderError> {
        let hls_stream = construct_stream_from_variant(live_id, stream_url, Format::TS, Codec::Avc)
            .await
            .map_err(|_| RecorderError::NoStreamAvailable)?;

        let hls_recorder = HlsRecorder::new(
            self.room_id.to_string(),
            Arc::new(hls_stream),
            self.client.clone(),
            cookie_header.filter(|v| !v.trim().is_empty()),
            Some(headers.clone()),
            self.event_channel.clone(),
            work_dir.full_path(),
            self.enabled.clone(),
        )
        .await?;

        hls_recorder.start().await
    }
}

#[async_trait]
impl RecorderTrait<KuaishouExtra> for KuaishouRecorder {
    async fn run(&self) {
        let self_clone = self.clone();
        *self.record_task.lock().await = Some(tokio::spawn(async move {
            self_clone.log_info("Start running recorder");
            let startup_stagger = self_clone.startup_stagger_secs();
            if startup_stagger > 0 {
                self_clone.log_info(&format!(
                    "Apply startup stagger {}s to avoid multi-room burst",
                    startup_stagger
                ));
                tokio::time::sleep(Duration::from_secs(startup_stagger)).await;
            }
            let mut fast_probe_budget = Self::fast_probe_rounds();
            while !self_clone.quit.load(atomic::Ordering::Relaxed) {
                let now = Utc::now().timestamp();
                let rate_limit_until = self_clone
                    .extra
                    .rate_limit_until_ts
                    .load(atomic::Ordering::Relaxed);
                if rate_limit_until > now {
                    let wait_secs = (rate_limit_until - now) as u64;
                    tokio::time::sleep(Duration::from_secs(wait_secs)).await;
                    continue;
                }
                if self_clone.check_status().await {
                    // Live status is ok, start recording
                    if self_clone.should_record().await {
                        let live_id;
                        // If should continue with previous recording, use the same live id
                        if self_clone.extra.should_continue.load(Ordering::Relaxed)
                            && self_clone.extra.pre_live_id.read().await.is_some()
                        {
                            live_id = self_clone.extra.pre_live_id.read().await.clone().unwrap();
                            self_clone
                                .extra
                                .should_continue
                                .store(false, Ordering::Relaxed);
                        } else {
                            live_id = Utc::now().timestamp_millis().to_string();
                            self_clone
                                .extra
                                .pre_live_id
                                .write()
                                .await
                                .replace(live_id.clone());
                        }

                        if let Err(e) = self_clone.update_entries(&live_id).await {
                            match e {
                                RecorderError::StreamExpired { expire } => {
                                    self_clone
                                        .extra
                                        .should_continue
                                        .store(true, Ordering::Relaxed);
                                    self_clone.log_info(&format!("Stream expired at {}", expire));
                                }
                                _ => {
                                    self_clone.log_error(&format!("Update entries error: {}", e));
                                }
                            }
                        }

                        fast_probe_budget = Self::fast_probe_rounds();
                        let _ = self_clone.event_channel.send(RecorderEvent::RecordEnd {
                            recorder: self_clone.info().await,
                        });
                    }

                    self_clone
                        .is_recording
                        .store(false, atomic::Ordering::Relaxed);

                    self_clone.reset().await;
                    // If should continue with previous recording, no need to sleep
                    if self_clone.extra.should_continue.load(Ordering::Relaxed) {
                        continue;
                    }
                    let error_backoff = std::env::var("LIVE_KUAISHOU_ERROR_BACKOFF_SECS")
                        .ok()
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .unwrap_or(60);
                    let error_window = std::env::var("LIVE_KUAISHOU_ERROR_WINDOW_SECS")
                        .ok()
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .unwrap_or(120);
                    let last_error_ts = self_clone
                        .extra
                        .last_error_ts
                        .load(atomic::Ordering::Relaxed);
                    if last_error_ts > 0 {
                        let now = Utc::now().timestamp();
                        if now.saturating_sub(last_error_ts) <= error_window as i64 {
                            let interval =
                                self_clone.update_interval.load(atomic::Ordering::Relaxed);
                            let mut sleep_secs = crate::utils::jitter_interval_secs(interval, 10);
                            sleep_secs = sleep_secs.max(interval);
                            sleep_secs = sleep_secs.saturating_add(error_backoff);
                            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
                            continue;
                        }
                    }
                    // Go check status again after random 2-5 secs
                    let secs = rand::random::<u64>() % 4 + 2;
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                    continue;
                }

                let interval = self_clone.update_interval.load(atomic::Ordering::Relaxed);
                let effective_interval = interval.min(Self::max_idle_poll_secs()).max(3);

                if fast_probe_budget > 0 {
                    fast_probe_budget = fast_probe_budget.saturating_sub(1);
                    let quick_sleep = 2 + (rand::random::<u64>() % 3);
                    tokio::time::sleep(Duration::from_secs(quick_sleep)).await;
                    continue;
                }

                let mut sleep_secs = crate::utils::jitter_interval_secs(effective_interval, 6);
                sleep_secs = sleep_secs.max(effective_interval);
                let error_backoff = std::env::var("LIVE_KUAISHOU_ERROR_BACKOFF_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(60);
                let error_window = std::env::var("LIVE_KUAISHOU_ERROR_WINDOW_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(120);
                let last_error_ts = self_clone
                    .extra
                    .last_error_ts
                    .load(atomic::Ordering::Relaxed);
                if last_error_ts > 0 {
                    let now = Utc::now().timestamp();
                    if now.saturating_sub(last_error_ts) <= error_window as i64 {
                        sleep_secs = sleep_secs.saturating_add(error_backoff);
                    }
                }
                tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
            }
        }));
    }
}

#[async_trait]
impl crate::traits::StreamInfoProvider for KuaishouExtra {
    async fn get_resolution(&self) -> Option<String> {
        self.resolution.read().await.clone()
    }
}
