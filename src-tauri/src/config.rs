use chrono::Local;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{self, AtomicU64};
use std::sync::Arc;

#[cfg(target_os = "windows")]
use winreg::enums::HKEY_CURRENT_USER;
#[cfg(target_os = "windows")]
use winreg::RegKey;

use crate::{danmu2ass::Danmu2AssOptions, recorder_manager::ClipRangeParams};

#[derive(Deserialize, Serialize, Clone)]
pub struct Config {
    pub cache: String,
    pub output: String,
    #[serde(default = "default_http_proxy")]
    pub http_proxy: String,
    #[serde(default = "default_https_proxy")]
    pub https_proxy: String,
    pub live_start_notify: bool,
    pub live_end_notify: bool,
    pub clip_notify: bool,
    pub post_notify: bool,
    #[serde(default = "default_bilibili_post_enabled")]
    pub bilibili_post_enabled: bool,
    #[serde(default = "default_auto_subtitle")]
    pub auto_subtitle: bool,
    #[serde(default = "default_subtitle_generator_type")]
    pub subtitle_generator_type: String,
    #[serde(default = "default_whisper_model")]
    pub whisper_model: String,
    #[serde(default = "default_whisper_prompt")]
    pub whisper_prompt: String,
    #[serde(default = "default_openai_api_endpoint")]
    pub openai_api_endpoint: String,
    #[serde(default = "default_openai_api_key")]
    pub openai_api_key: String,
    #[serde(default = "default_clip_name_format")]
    pub clip_name_format: String,
    #[serde(default = "default_auto_generate_config")]
    pub auto_generate: AutoGenerateConfig,
    #[serde(default = "default_status_check_interval")]
    pub status_check_interval: u64,
    #[serde(default = "default_record_protocol_preference")]
    pub record_protocol_preference: String,
    #[serde(skip)]
    pub config_path: String,
    #[serde(default = "default_whisper_language")]
    pub whisper_language: String,
    #[serde(default = "default_webhook_url")]
    pub webhook_url: String,
    #[serde(default = "default_danmu_ass_options")]
    pub danmu_ass_options: Danmu2AssOptions,
    #[serde(skip)]
    pub update_interval: Arc<AtomicU64>,
    #[serde(default = "default_powerlive_key")]
    pub powerlive_key: String,
    #[serde(default = "default_reverse_generate_path")]
    pub reverse_generate_path: String,
    #[serde(
        skip_serializing,
        skip_deserializing,
        default = "default_douyin_passport"
    )]
    pub douyin_passport: DouyinPassportConfig,
    #[serde(
        skip_serializing,
        skip_deserializing,
        default = "default_guest_accounts"
    )]
    pub guest_accounts: Vec<DefaultAccountConfig>,
    #[serde(default = "default_use_guest_accounts")]
    pub use_guest_accounts: bool,
    #[serde(
        skip_serializing,
        skip_deserializing,
        default = "default_login_accounts"
    )]
    pub login_accounts: Vec<DefaultAccountConfig>,
    #[serde(default = "default_use_login_accounts")]
    pub use_login_accounts: bool,
    #[serde(default = "default_kuaishou_enable_follow_list_fallback")]
    pub kuaishou_enable_follow_list_fallback: bool,
    #[serde(default = "default_kuaishou_enable_public_page_fallback")]
    pub kuaishou_enable_public_page_fallback: bool,
    #[serde(skip_serializing, skip_deserializing, default = "default_tiktok_feed")]
    pub tiktok_feed: TikTokFeedConfig,
    #[serde(default = "default_kuaishou_ws_token")]
    pub kuaishou_ws_token: String,
    #[serde(default = "default_kuaishou_ws_urls")]
    pub kuaishou_ws_urls: String,
    #[serde(default = "default_kuaishou_ws_updated_at")]
    pub kuaishou_ws_updated_at: i64,
    #[serde(default = "default_kuaishou_danmu_cookie")]
    pub kuaishou_danmu_cookie: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct AutoGenerateConfig {
    pub enabled: bool,
    pub encode_danmu: bool,
    #[serde(default)]
    pub delete_cache_after_clip: bool,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct DouyinPassportConfig {
    pub provider_url: String,
    pub sign: String,
    pub qs: String,
    pub passport_jssdk_version: String,
    pub passport_jssdk_type: String,
    pub is_from_ttaccountsdk: String,
    pub aid: String,
    pub language: String,
    pub account_app_language: String,
    pub next: String,
    pub need_short_url: String,
    pub need_logo: String,
    pub is_new_login: String,
    pub is_from_iesaccountsaas: String,
    pub account_sdk_source: String,
    pub account_sdk_source_info: String,
    pub service: String,
    pub p_ui: String,
    pub p_ca: String,
    pub p_ca_real: String,
    pub p_js_v: String,
    pub p_js_t: String,
    pub p_zt: String,
    pub p_ver: String,
    pub p_ver_real: String,
    pub request_host: String,
    pub p_bd: String,
    pub p_ts: String,
    pub p_no: String,
    pub biz_trace_id: String,
    pub device_platform: String,
    pub verify_fp: String,
    pub fp: String,
    pub ms_token: String,
    pub a_bogus: String,
    pub x_tt_passport_csrf_token: String,
    pub x_tt_passport_aid_sign: String,
    pub x_tt_passport_trace_id: String,
    pub x_tt_passport_verify_portrait: String,
    pub x_tt_session_dtrait: String,
    pub qr_origin: String,
    pub qr_referer: String,
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct QrLoginConfig {
    #[serde(default)]
    pub douyin: DouyinQrLoginOverrides,
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct DouyinQrLoginOverrides {
    #[serde(default)]
    pub verify_fp: String,
    #[serde(default)]
    pub fp: String,
    #[serde(default)]
    pub ms_token: String,
    #[serde(default)]
    pub a_bogus: String,
    #[serde(default)]
    pub sign: String,
    #[serde(default)]
    pub qs: String,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub device_platform: String,
    #[serde(default)]
    pub qr_origin: String,
    #[serde(default)]
    pub qr_referer: String,
    #[serde(default)]
    pub params_raw: String,
    #[serde(default)]
    pub params_raw_status: String,
    #[serde(default)]
    pub challenge_params_raw: String,
    #[serde(default)]
    pub challenge_body: String,
    #[serde(default)]
    pub challenge_content_type: String,
    #[serde(default)]
    pub x_tt_passport_csrf_token: String,
    #[serde(default)]
    pub x_tt_passport_aid_sign: String,
    #[serde(default)]
    pub x_tt_passport_trace_id: String,
    #[serde(default)]
    pub x_tt_passport_verify_portrait: String,
    #[serde(default)]
    pub x_tt_session_dtrait: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct TikTokFeedConfig {
    pub url: String,
    pub url_template: String,
    pub x_gnarly: String,
    pub x_bogus: String,
    pub user_agent: String,
    pub device_id: String,
    pub verify_fp: String,
    pub ms_token: String,
    pub root_referer: String,
}

#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct DefaultAccountConfig {
    pub platform: String,
    pub cookies: String,
    #[serde(default)]
    pub extra: String,
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct AccountsFile {
    #[serde(default)]
    pub guest_accounts: Vec<DefaultAccountConfig>,
    #[serde(default)]
    pub login_accounts: Vec<DefaultAccountConfig>,
}

fn default_danmu_ass_options() -> Danmu2AssOptions {
    Danmu2AssOptions::default()
}

fn default_auto_subtitle() -> bool {
    false
}

fn default_bilibili_post_enabled() -> bool {
    false
}

fn default_subtitle_generator_type() -> String {
    "whisper".to_string()
}

fn default_whisper_model() -> String {
    "whisper_model.bin".to_string()
}

fn default_whisper_prompt() -> String {
    "这是一段中文 你们好".to_string()
}

fn default_openai_api_endpoint() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_openai_api_key() -> String {
    String::new()
}

fn default_clip_name_format() -> String {
    "[{room_id}][{live_id}][{title}][{created_at}].mp4".to_string()
}

fn default_auto_generate_config() -> AutoGenerateConfig {
    AutoGenerateConfig {
        enabled: false,
        encode_danmu: false,
        delete_cache_after_clip: false,
    }
}

fn default_status_check_interval() -> u64 {
    77
}

fn default_record_protocol_preference() -> String {
    "hls".to_string()
}

fn default_whisper_language() -> String {
    "auto".to_string()
}

fn default_webhook_url() -> String {
    String::new()
}

fn default_powerlive_key() -> String {
    String::new()
}

fn default_reverse_generate_path() -> String {
    String::new()
}

fn default_guest_accounts() -> Vec<DefaultAccountConfig> {
    Vec::new()
}

fn default_use_guest_accounts() -> bool {
    true // 默认启用访客模式
}

fn default_douyin_passport() -> DouyinPassportConfig {
    DouyinPassportConfig {
        provider_url: String::new(),
        sign: String::new(),
        qs: String::new(),
        passport_jssdk_version: "3.1.3".to_string(),
        passport_jssdk_type: "normal".to_string(),
        is_from_ttaccountsdk: "1".to_string(),
        aid: "6383".to_string(),
        language: "zh".to_string(),
        account_app_language: "zh-CN".to_string(),
        next: "https://live.douyin.com".to_string(),
        need_short_url: "true".to_string(),
        need_logo: "false".to_string(),
        is_new_login: "1".to_string(),
        is_from_iesaccountsaas: "1".to_string(),
        account_sdk_source: "web".to_string(),
        account_sdk_source_info: String::new(),
        service: "https://live.douyin.com".to_string(),
        p_ui: "2.1.4".to_string(),
        p_ca: "4.0.17".to_string(),
        p_ca_real: "1.0.0.729".to_string(),
        p_js_v: "3.1.3".to_string(),
        p_js_t: "pro".to_string(),
        p_zt: "3.3.10".to_string(),
        p_ver: "1.1.3".to_string(),
        p_ver_real: "0".to_string(),
        request_host: "https://live.douyin.com".to_string(),
        p_bd: "1.0.1.19-fix.01".to_string(),
        p_ts: String::new(),
        p_no: String::new(),
        biz_trace_id: String::new(),
        device_platform: "web_app".to_string(),
        verify_fp: String::new(),
        fp: String::new(),
        ms_token: String::new(),
        a_bogus: String::new(),
        x_tt_passport_csrf_token: String::new(),
        x_tt_passport_aid_sign: String::new(),
        x_tt_passport_trace_id: String::new(),
        x_tt_passport_verify_portrait: String::new(),
        x_tt_session_dtrait: String::new(),
        qr_origin: "https://live.douyin.com".to_string(),
        qr_referer: "https://live.douyin.com/".to_string(),
    }
}

fn default_login_accounts() -> Vec<DefaultAccountConfig> {
    Vec::new()
}

fn default_use_login_accounts() -> bool {
    true
}

fn default_kuaishou_enable_follow_list_fallback() -> bool {
    false
}

fn default_kuaishou_enable_public_page_fallback() -> bool {
    false
}

fn locate_accounts_file(name: &str) -> Option<PathBuf> {
    if cfg!(feature = "isolated-build") {
        let dirs = platform_dirs::AppDirs::new(Some(crate::constants::APP_DATA_ID), false)?;
        return [dirs.config_dir.join(name), dirs.data_dir.join(name)].into_iter().find(|p| p.exists());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("src-tauri").join(name));
        candidates.push(cwd.join(name));
    }
    if let Some(app_dirs) = platform_dirs::AppDirs::new(Some(crate::constants::APP_DATA_ID), false) {
        candidates.push(app_dirs.config_dir.join(name));
        candidates.push(app_dirs.data_dir.join(name));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
            candidates.push(dir.join("resources").join(name));
        }
    }
    candidates.into_iter().find(|path| path.exists())
}

pub(crate) fn resolve_accounts_file_write_path() -> PathBuf {
    if let Some(path) = locate_accounts_file("accounts.toml") {
        return path;
    }
    if cfg!(feature = "isolated-build") {
        if let Some(dirs) = platform_dirs::AppDirs::new(Some(crate::constants::APP_DATA_ID), false) {
            return dirs.config_dir.join("accounts.toml");
        }
    }
    if let Some(example_path) = locate_accounts_file("accounts.example.toml") {
        if let Some(parent) = example_path.parent() {
            return parent.join("accounts.toml");
        }
    }
    if let Some(app_dirs) = platform_dirs::AppDirs::new(Some(crate::constants::APP_DATA_ID), false) {
        return app_dirs.config_dir.join("accounts.toml");
    }
    if let Ok(cwd) = env::current_dir() {
        let src_tauri = cwd.join("src-tauri");
        if src_tauri.is_dir() {
            return src_tauri.join("accounts.toml");
        }
    }
    PathBuf::from("accounts.toml")
}

pub(crate) fn load_accounts_file() -> Option<(AccountsFile, PathBuf)> {
    let path = locate_accounts_file("accounts.toml")?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed = toml::from_str::<AccountsFile>(&raw).ok()?;
    Some((parsed, path))
}

fn load_accounts_example_file() -> Option<(AccountsFile, PathBuf)> {
    let path = locate_accounts_file("accounts.example.toml")?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed = toml::from_str::<AccountsFile>(&raw).ok()?;
    Some((parsed, path))
}

pub(crate) fn load_accounts_file_or_example() -> Option<(AccountsFile, PathBuf)> {
    load_accounts_file().or_else(load_accounts_example_file)
}

pub(crate) fn write_accounts_file(
    path: &Path,
    accounts: &AccountsFile,
) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let serialized = toml::to_string_pretty(accounts).unwrap_or_default();
    std::fs::write(path, serialized)
}

fn default_tiktok_feed() -> TikTokFeedConfig {
    TikTokFeedConfig {
        url: String::new(),
        url_template: String::new(),
        x_gnarly: String::new(),
        x_bogus: String::new(),
        user_agent: String::new(),
        device_id: String::new(),
        verify_fp: String::new(),
        ms_token: String::new(),
        root_referer: String::new(),
    }
}

fn default_kuaishou_ws_token() -> String {
    String::new()
}

fn default_kuaishou_ws_urls() -> String {
    String::new()
}

fn default_kuaishou_ws_updated_at() -> i64 {
    0
}

fn default_kuaishou_danmu_cookie() -> String {
    String::new()
}

fn default_http_proxy() -> String {
    String::new()
}

fn default_https_proxy() -> String {
    String::new()
}

fn normalize_proxy_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut value = trimmed.to_string();
    if trimmed.contains('=') {
        let mut selected = None;
        for entry in trimmed.split(';') {
            let entry = entry.trim();
            if let Some(stripped) = entry.strip_prefix("https=") {
                selected = Some(stripped.trim());
                break;
            }
            if let Some(stripped) = entry.strip_prefix("http=") {
                selected = Some(stripped.trim());
            }
        }
        if let Some(selected) = selected {
            value = selected.to_string();
        }
    }

    if value.contains("://") {
        Some(value)
    } else {
        Some(format!("http://{value}"))
    }
}

fn detect_env_proxy() -> Option<String> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = env::var(key) {
            if let Some(proxy_url) = normalize_proxy_url(&value) {
                return Some(proxy_url);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn detect_windows_proxy() -> Option<String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").ok()?;
    if enabled != 1 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    normalize_proxy_url(&server)
}

impl Config {
    fn infer_project_root() -> Option<PathBuf> {
        let Ok(cwd) = env::current_dir() else {
            return None;
        };
        if cwd.file_name().and_then(|s| s.to_str()) == Some("src-tauri") {
            return cwd.parent().map(|path| path.to_path_buf()).or(Some(cwd));
        }
        Some(cwd)
    }

    fn resolve_reverse_generate_paths(&self) -> Option<(PathBuf, PathBuf)> {
        let raw = self.reverse_generate_path.trim();
        if raw.is_empty() {
            return None;
        }

        let mut path = PathBuf::from(raw);
        if path.is_relative() {
            if let Some(root) = Self::infer_project_root() {
                path = root.join(path);
            }
        }

        let base_dir = if path.is_dir() {
            path.clone()
        } else {
            path.parent().unwrap_or(&path).to_path_buf()
        };
        let qr_login_path = if path.is_file()
            && path.file_name().and_then(|name| name.to_str()) == Some("qr_login.toml")
        {
            path
        } else {
            base_dir.join("qr_login.toml")
        };
        let tiktok_web_path = base_dir.join("tiktok_web.toml");

        Some((qr_login_path, tiktok_web_path))
    }

    fn infer_reverse_generate_dir() -> Option<PathBuf> {
        Self::infer_project_root().map(|root| {
            root.join("src-tauri")
                .join("crates")
                .join("recorder")
                .join("src")
                .join("ReverseGenerate")
        })
    }

    fn normalize_account_switches(&mut self) -> bool {
        // Allow both to coexist; priority will be handled in recorder_manager
        false
    }

    fn normalize_storage_paths(&mut self, default_cache: &Path, default_output: &Path) -> bool {
        let mut changed = false;
        if self.cache.trim().is_empty() || Path::new(&self.cache).is_relative() {
            self.cache = default_cache.to_str().unwrap().into();
            changed = true;
        }
        if self.output.trim().is_empty() || Path::new(&self.output).is_relative() {
            self.output = default_output.to_str().unwrap().into();
            changed = true;
        }
        changed
    }

    fn ensure_storage_dirs(&self) {
        for path in [&self.cache, &self.output] {
            let dir_path = Path::new(path);
            if let Err(e) = std::fs::create_dir_all(dir_path) {
                log::warn!("Failed to create storage dir {dir_path:?}: {e}");
            }
        }
    }

    fn apply_login_account_override(&mut self) -> bool {
        let Some((accounts, _path)) = load_accounts_file_or_example() else {
            return false;
        };
        let mut changed = false;
        if self.guest_accounts != accounts.guest_accounts {
            self.guest_accounts = accounts.guest_accounts.clone();
            changed = true;
        }
        if self.login_accounts != accounts.login_accounts {
            self.login_accounts = accounts.login_accounts.clone();
            changed = true;
        }
        changed
    }

    fn apply_qr_login_overrides(&mut self, _config_path: &Path) {
        let Some((qr_login_path, tiktok_web_path)) = self.resolve_reverse_generate_paths() else {
            log::warn!("ReverseGenerate path is empty or invalid; skip QR login overrides");
            return;
        };

        let _ = recorder::reverse_generate::qr_login::ensure_qr_login_defaults(&qr_login_path);
        let _ =
            recorder::reverse_generate::tiktok_web::ensure_tiktok_web_defaults(&tiktok_web_path);
        let Ok(content) = std::fs::read_to_string(&qr_login_path) else {
            return;
        };
        let Ok(mut qr_login) = toml::from_str::<QrLoginConfig>(&content) else {
            return;
        };
        let mut updated = false;
        let mut generated_verify_fp: Option<String> = None;
        {
            let douyin = &mut qr_login.douyin;
            if douyin.verify_fp.trim().is_empty() {
                if !douyin.fp.trim().is_empty() {
                    douyin.verify_fp = douyin.fp.trim().to_string();
                    updated = true;
                } else {
                    let value = recorder::platforms::douyin::params::gen_verify_fp();
                    douyin.verify_fp = value.clone();
                    douyin.fp = value.clone();
                    generated_verify_fp = Some(value);
                    updated = true;
                }
            }
            if douyin.fp.trim().is_empty() {
                let value = generated_verify_fp
                    .clone()
                    .unwrap_or_else(|| douyin.verify_fp.trim().to_string());
                if !value.is_empty() {
                    douyin.fp = value;
                    updated = true;
                }
            }
        }
        if updated {
            if let Ok(serialized) = toml::to_string_pretty(&qr_login) {
                if let Err(err) = std::fs::write(&qr_login_path, serialized) {
                    log::warn!(
                        "Failed to persist QR login overrides to {:?}: {}",
                        qr_login_path,
                        err
                    );
                }
            }
        }
        let douyin = &qr_login.douyin;
        if !douyin.verify_fp.trim().is_empty() {
            self.douyin_passport.verify_fp = douyin.verify_fp.trim().to_string();
        }
        if !douyin.fp.trim().is_empty() {
            self.douyin_passport.fp = douyin.fp.trim().to_string();
        }
        if !douyin.ms_token.trim().is_empty() {
            self.douyin_passport.ms_token = douyin.ms_token.trim().to_string();
        }
        if !douyin.a_bogus.trim().is_empty() {
            self.douyin_passport.a_bogus = douyin.a_bogus.trim().to_string();
        }
        if !douyin.sign.trim().is_empty() {
            self.douyin_passport.sign = douyin.sign.trim().to_string();
        }
        if !douyin.qs.trim().is_empty() {
            self.douyin_passport.qs = douyin.qs.trim().to_string();
        }
        if !douyin.user_agent.trim().is_empty() {
            let ua = douyin.user_agent.trim().to_string();
            std::env::set_var("DOUYIN_PASSPORT_USER_AGENT", &ua);
            std::env::set_var("DOUYIN_USER_AGENT", &ua);
        }
        if !douyin.device_platform.trim().is_empty() {
            std::env::set_var("DOUYIN_DEVICE_PLATFORM", douyin.device_platform.trim());
        }
        if !douyin.qr_origin.trim().is_empty() {
            std::env::set_var("DOUYIN_QR_ORIGIN", douyin.qr_origin.trim());
        }
        if !douyin.qr_referer.trim().is_empty() {
            std::env::set_var("DOUYIN_QR_REFERER", douyin.qr_referer.trim());
        }
        if !douyin.params_raw.trim().is_empty() {
            std::env::set_var("DOUYIN_PASSPORT_PARAMS_RAW", douyin.params_raw.trim());
        }
        if !douyin.params_raw_status.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_PASSPORT_PARAMS_RAW_STATUS",
                douyin.params_raw_status.trim(),
            );
        }
        if !douyin.challenge_params_raw.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_PASSPORT_CHALLENGE_PARAMS_RAW",
                douyin.challenge_params_raw.trim(),
            );
        }
        if !douyin.challenge_body.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_PASSPORT_CHALLENGE_BODY",
                douyin.challenge_body.trim(),
            );
        }
        if !douyin.challenge_content_type.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_PASSPORT_CHALLENGE_CONTENT_TYPE",
                douyin.challenge_content_type.trim(),
            );
        }
        if !douyin.x_tt_passport_csrf_token.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_X_TT_PASSPORT_CSRF_TOKEN",
                douyin.x_tt_passport_csrf_token.trim(),
            );
        }
        if !douyin.x_tt_passport_aid_sign.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_X_TT_PASSPORT_AID_SIGN",
                douyin.x_tt_passport_aid_sign.trim(),
            );
        }
        if !douyin.x_tt_passport_trace_id.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_X_TT_PASSPORT_TRACE_ID",
                douyin.x_tt_passport_trace_id.trim(),
            );
        }
        if !douyin.x_tt_passport_verify_portrait.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_X_TT_PASSPORT_VERIFY_PORTRAIT",
                douyin.x_tt_passport_verify_portrait.trim(),
            );
        }
        if !douyin.x_tt_session_dtrait.trim().is_empty() {
            std::env::set_var(
                "DOUYIN_X_TT_SESSION_DTRAIT",
                douyin.x_tt_session_dtrait.trim(),
            );
        }
        if !douyin.sign.trim().is_empty() {
            std::env::set_var("DOUYIN_SIGN", douyin.sign.trim());
        }
        if !douyin.qs.trim().is_empty() {
            std::env::set_var("DOUYIN_QS", douyin.qs.trim());
        }
        log::info!("Loaded QR login overrides from {:?}", qr_login_path);
    }

    pub fn load(
        config_path: &PathBuf,
        default_cache: &Path,
        default_output: &Path,
    ) -> Result<Self, String> {
        if let Ok(content) = std::fs::read_to_string(config_path) {
            if let Ok(mut config) = toml::from_str::<Config>(&content) {
                let mut needs_save = false;
                config.config_path = config_path.to_str().unwrap().into();
                config.update_interval = Arc::new(AtomicU64::new(config.status_check_interval));
                if config.reverse_generate_path.trim().is_empty() {
                    if let Some(path) = Self::infer_reverse_generate_dir() {
                        config.reverse_generate_path = path.to_string_lossy().into_owned();
                        needs_save = true;
                    }
                }
                if config.record_protocol_preference.trim().is_empty() {
                    config.record_protocol_preference = default_record_protocol_preference();
                    needs_save = true;
                }
                if content.contains("BSR_KUAISHOU_ENABLE_FOLLOW_LIST_FALLBACK")
                    || content.contains("BSR_KUAISHOU_ENABLE_PUBLIC_PAGE_FALLBACK")
                {
                    needs_save = true;
                }
                if config.normalize_storage_paths(default_cache, default_output) {
                    needs_save = true;
                }
                if config.apply_login_account_override() {
                    needs_save = true;
                }
                if config.normalize_account_switches() {
                    needs_save = true;
                }
                if content.contains("[douyin_passport]") || content.contains("[tiktok_feed]") {
                    needs_save = true;
                }
                if needs_save {
                    config.save();
                }
                config.apply_qr_login_overrides(config_path);
                config.ensure_storage_dirs();
                return Ok(config);
            }
        }

        if let Some(dir_path) = PathBuf::from(config_path).parent() {
            if let Err(e) = std::fs::create_dir_all(dir_path) {
                return Err(format!("Failed to create config dir: {e}"));
            }
        }

        let config = Config {
            cache: default_cache.to_str().unwrap().into(),
            output: default_output.to_str().unwrap().into(),
            http_proxy: default_http_proxy(),
            https_proxy: default_https_proxy(),
            live_start_notify: true,
            live_end_notify: true,
            clip_notify: true,
            post_notify: true,
            bilibili_post_enabled: default_bilibili_post_enabled(),
            auto_subtitle: false,
            subtitle_generator_type: default_subtitle_generator_type(),
            whisper_model: default_whisper_model(),
            whisper_prompt: default_whisper_prompt(),
            openai_api_endpoint: default_openai_api_endpoint(),
            openai_api_key: default_openai_api_key(),
            clip_name_format: default_clip_name_format(),
            auto_generate: default_auto_generate_config(),
            status_check_interval: default_status_check_interval(),
            record_protocol_preference: default_record_protocol_preference(),
            config_path: config_path.to_str().unwrap().into(),
            whisper_language: default_whisper_language(),
            webhook_url: default_webhook_url(),
            danmu_ass_options: default_danmu_ass_options(),
            update_interval: Arc::new(AtomicU64::new(default_status_check_interval())),
            powerlive_key: default_powerlive_key(),
            reverse_generate_path: Self::infer_reverse_generate_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            douyin_passport: default_douyin_passport(),
            guest_accounts: default_guest_accounts(),
            use_guest_accounts: default_use_guest_accounts(),
            login_accounts: default_login_accounts(),
            use_login_accounts: default_use_login_accounts(),
            kuaishou_enable_follow_list_fallback: default_kuaishou_enable_follow_list_fallback(),
            kuaishou_enable_public_page_fallback: default_kuaishou_enable_public_page_fallback(),
            tiktok_feed: default_tiktok_feed(),
            kuaishou_ws_token: default_kuaishou_ws_token(),
            kuaishou_ws_urls: default_kuaishou_ws_urls(),
            kuaishou_ws_updated_at: default_kuaishou_ws_updated_at(),
            kuaishou_danmu_cookie: default_kuaishou_danmu_cookie(),
        };

        config.ensure_storage_dirs();
        config.save();
        let mut config = config;
        config.apply_qr_login_overrides(config_path);

        Ok(config)
    }

    pub fn save(&self) {
        let content = toml::to_string(&self).unwrap();
        if let Err(e) = std::fs::write(self.config_path.clone(), content) {
            log::error!("Failed to save config: {} {}", e, self.config_path);
        }
    }

    #[allow(dead_code)]
    pub fn set_cache_path(&mut self, path: &str) {
        self.cache = path.to_string();
        self.save();
    }

    #[allow(dead_code)]
    pub fn set_output_path(&mut self, path: &str) {
        self.output = path.into();
        self.save();
    }

    #[allow(dead_code)]
    pub fn set_whisper_language(&mut self, language: &str) {
        self.whisper_language = language.to_string();
        self.save();
    }

    #[allow(dead_code)]
    pub fn set_danmu_ass_options(&mut self, options: Danmu2AssOptions) {
        self.danmu_ass_options = options;
        self.save();
    }

    pub fn generate_clip_name(&self, params: &ClipRangeParams) -> PathBuf {
        // get format config
        // filter special characters from title to make sure file name is valid
        let title = params
            .title
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>();
        let format_config = self.clip_name_format.clone();
        let format_config = format_config.replace("{title}", &title);
        let format_config = format_config.replace("{platform}", &params.platform);
        let format_config = format_config.replace("{room_id}", &params.room_id.to_string());
        let format_config = format_config.replace("{live_id}", &params.live_id);
        let format_config = format_config.replace("{note}", &params.note);
        let format_config = format_config.replace(
            "{x}",
            &params
                .ranges
                .first()
                .map_or("0".to_string(), |r| r.start.to_string()),
        );
        let format_config = format_config.replace(
            "{y}",
            &params
                .ranges
                .last()
                .map_or("0".to_string(), |r| r.end.to_string()),
        );
        let format_config = format_config.replace(
            "{created_at}",
            &Local::now().format("%Y-%m-%d_%H-%M-%S").to_string(),
        );
        let duration = params.ranges.iter().map(|r| r.duration()).sum::<f64>();
        let format_config = format_config.replace("{length}", &duration.to_string());

        let sanitized = sanitize_filename::sanitize(&format_config);
        let output = self.output.clone();

        Path::new(&output).join(&sanitized)
    }

    pub fn set_status_check_interval(&mut self, interval: u64) {
        self.status_check_interval = interval;
        self.update_interval
            .store(interval, atomic::Ordering::Relaxed);
        self.save();
    }

    pub fn set_network_config(&mut self, http_proxy: &str, https_proxy: &str) {
        let http = http_proxy.trim();
        let https = https_proxy.trim();
        self.http_proxy = http.to_string();
        self.https_proxy = https.to_string();
        let normalized_http = normalize_proxy_url(http);
        let normalized_https = normalize_proxy_url(https).or_else(|| normalized_http.clone());
        if let Some(proxy_url) = normalized_http {
            std::env::set_var("http_proxy", &proxy_url);
            std::env::set_var("HTTP_PROXY", &proxy_url);
        } else {
            std::env::remove_var("http_proxy");
            std::env::remove_var("HTTP_PROXY");
        }
        if let Some(proxy_url) = normalized_https {
            std::env::set_var("https_proxy", &proxy_url);
            std::env::set_var("HTTPS_PROXY", &proxy_url);
        } else {
            std::env::remove_var("https_proxy");
            std::env::remove_var("HTTPS_PROXY");
        }
        self.save();
    }

    pub fn apply_network_env(&self) {
        let http = normalize_proxy_url(&self.http_proxy);
        let https = normalize_proxy_url(&self.https_proxy).or_else(|| http.clone());

        if let Some(proxy_url) = http.clone() {
            std::env::set_var("http_proxy", &proxy_url);
            std::env::set_var("HTTP_PROXY", &proxy_url);
        } else {
            std::env::remove_var("http_proxy");
            std::env::remove_var("HTTP_PROXY");
        }

        if let Some(proxy_url) = https.clone() {
            std::env::set_var("https_proxy", &proxy_url);
            std::env::set_var("HTTPS_PROXY", &proxy_url);
        } else {
            std::env::remove_var("https_proxy");
            std::env::remove_var("HTTPS_PROXY");
        }

        log::info!(
            "Applied network proxy env: http_proxy={:?} https_proxy={:?}",
            http,
            https
        );

        self.apply_proxy_env();
    }

    pub fn apply_proxy_env(&self) {
        let detected = detect_env_proxy().or_else(|| {
            #[cfg(target_os = "windows")]
            {
                detect_windows_proxy()
            }
            #[cfg(not(target_os = "windows"))]
            {
                None
            }
        });
        if let Some(proxy_url) = detected {
            std::env::set_var("TIKTOK_PROXY_URL", proxy_url);
        } else {
            std::env::remove_var("TIKTOK_PROXY_URL");
        }
    }

    pub fn apply_record_protocol_env(&self) {
        let value = self.record_protocol_preference.trim();
        if value.is_empty() {
            std::env::remove_var("BSR_KUAISHOU_PREFER_PROTOCOL");
            std::env::remove_var("BSR_TIKTOK_PREFER_PROTOCOL");
        } else {
            std::env::set_var("BSR_KUAISHOU_PREFER_PROTOCOL", value);
            std::env::set_var("BSR_TIKTOK_PREFER_PROTOCOL", value);
        }
    }

    pub fn apply_kuaishou_fallback_env(&self) {
        if self.kuaishou_enable_follow_list_fallback {
            std::env::set_var("BSR_KUAISHOU_ENABLE_FOLLOW_LIST_FALLBACK", "true");
        } else {
            std::env::remove_var("BSR_KUAISHOU_ENABLE_FOLLOW_LIST_FALLBACK");
        }

        if self.kuaishou_enable_public_page_fallback {
            std::env::set_var("BSR_KUAISHOU_ENABLE_PUBLIC_PAGE_FALLBACK", "true");
        } else {
            std::env::remove_var("BSR_KUAISHOU_ENABLE_PUBLIC_PAGE_FALLBACK");
        }
    }

    pub fn apply_douyin_passport_env(&self) {
        let cfg = &self.douyin_passport;
        let set_if_missing = |key: &str, value: &str| {
            let has_value = std::env::var(key)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .is_some();
            if has_value {
                return;
            }
            if value.trim().is_empty() {
                std::env::remove_var(key);
            } else {
                std::env::set_var(key, value.trim());
            }
        };
        set_if_missing("DOUYIN_PASSPORT_PROVIDER_URL", &cfg.provider_url);
        set_if_missing("DOUYIN_SIGN", &cfg.sign);
        set_if_missing("DOUYIN_QS", &cfg.qs);
        set_if_missing("DOUYIN_PASSPORT_JSSDK_VERSION", &cfg.passport_jssdk_version);
        set_if_missing("DOUYIN_PASSPORT_JSSDK_TYPE", &cfg.passport_jssdk_type);
        set_if_missing("DOUYIN_IS_FROM_TTACCOUNTSDK", &cfg.is_from_ttaccountsdk);
        set_if_missing("DOUYIN_AID", &cfg.aid);
        set_if_missing("DOUYIN_LANGUAGE", &cfg.language);
        set_if_missing("DOUYIN_ACCOUNT_APP_LANGUAGE", &cfg.account_app_language);
        set_if_missing("DOUYIN_NEXT", &cfg.next);
        set_if_missing("DOUYIN_NEED_SHORT_URL", &cfg.need_short_url);
        set_if_missing("DOUYIN_NEED_LOGO", &cfg.need_logo);
        set_if_missing("DOUYIN_IS_NEW_LOGIN", &cfg.is_new_login);
        set_if_missing("DOUYIN_IS_FROM_IESACCOUNTSAAS", &cfg.is_from_iesaccountsaas);
        set_if_missing("DOUYIN_ACCOUNT_SDK_SOURCE", &cfg.account_sdk_source);
        set_if_missing(
            "DOUYIN_ACCOUNT_SDK_SOURCE_INFO",
            &cfg.account_sdk_source_info,
        );
        set_if_missing("DOUYIN_SERVICE", &cfg.service);
        set_if_missing("DOUYIN_P_UI", &cfg.p_ui);
        set_if_missing("DOUYIN_P_CA", &cfg.p_ca);
        set_if_missing("DOUYIN_P_CA_REAL", &cfg.p_ca_real);
        set_if_missing("DOUYIN_P_JS_V", &cfg.p_js_v);
        set_if_missing("DOUYIN_P_JS_T", &cfg.p_js_t);
        set_if_missing("DOUYIN_P_ZT", &cfg.p_zt);
        set_if_missing("DOUYIN_P_VER", &cfg.p_ver);
        set_if_missing("DOUYIN_P_VER_REAL", &cfg.p_ver_real);
        set_if_missing("DOUYIN_REQUEST_HOST", &cfg.request_host);
        set_if_missing("DOUYIN_P_BD", &cfg.p_bd);
        set_if_missing("DOUYIN_P_TS", &cfg.p_ts);
        set_if_missing("DOUYIN_P_NO", &cfg.p_no);
        set_if_missing("DOUYIN_BIZ_TRACE_ID", &cfg.biz_trace_id);
        set_if_missing("DOUYIN_DEVICE_PLATFORM", &cfg.device_platform);
        set_if_missing("DOUYIN_VERIFY_FP", &cfg.verify_fp);
        set_if_missing("DOUYIN_FP", &cfg.fp);
        set_if_missing("DOUYIN_MS_TOKEN", &cfg.ms_token);
        set_if_missing("DOUYIN_A_BOGUS", &cfg.a_bogus);
        set_if_missing(
            "DOUYIN_X_TT_PASSPORT_CSRF_TOKEN",
            &cfg.x_tt_passport_csrf_token,
        );
        set_if_missing("DOUYIN_X_TT_PASSPORT_AID_SIGN", &cfg.x_tt_passport_aid_sign);
        set_if_missing("DOUYIN_X_TT_PASSPORT_TRACE_ID", &cfg.x_tt_passport_trace_id);
        set_if_missing(
            "DOUYIN_X_TT_PASSPORT_VERIFY_PORTRAIT",
            &cfg.x_tt_passport_verify_portrait,
        );
        set_if_missing("DOUYIN_X_TT_SESSION_DTRAIT", &cfg.x_tt_session_dtrait);
        set_if_missing("DOUYIN_QR_ORIGIN", &cfg.qr_origin);
        set_if_missing("DOUYIN_QR_REFERER", &cfg.qr_referer);
    }

    pub fn apply_tiktok_feed_env(&self) {
        let cfg = &self.tiktok_feed;
        let set_or_clear = |key: &str, value: &str| {
            if value.trim().is_empty() {
                std::env::remove_var(key);
            } else {
                std::env::set_var(key, value.trim());
            }
        };
        set_or_clear("TIKTOK_FEED_URL", &cfg.url);
        set_or_clear("TIKTOK_FEED_URL_TEMPLATE", &cfg.url_template);
        set_or_clear("TIKTOK_FEED_X_GNARLY", &cfg.x_gnarly);
        set_or_clear("TIKTOK_FEED_X_BOGUS", &cfg.x_bogus);
        set_or_clear("TIKTOK_FEED_USER_AGENT", &cfg.user_agent);
        set_or_clear("TIKTOK_FEED_DEVICE_ID", &cfg.device_id);
        set_or_clear("TIKTOK_FEED_VERIFY_FP", &cfg.verify_fp);
        set_or_clear("TIKTOK_FEED_MS_TOKEN", &cfg.ms_token);
        set_or_clear("TIKTOK_FEED_ROOT_REFERER", &cfg.root_referer);
    }

    pub fn apply_kuaishou_ws_env(&self) {
        let token = self.kuaishou_ws_token.trim();
        if token.is_empty() {
            std::env::remove_var("KUAISHOU_WS_TOKEN");
        } else {
            std::env::set_var("KUAISHOU_WS_TOKEN", token);
        }
        let urls = self.kuaishou_ws_urls.trim();
        if urls.is_empty() {
            std::env::remove_var("KUAISHOU_WS_URLS");
        } else {
            std::env::set_var("KUAISHOU_WS_URLS", urls);
        }
    }
}
