//! Opt-in capture of video-channel live stream URLs through a local proxy.
//!
//! Boundaries, in order of importance:
//! - The certificate authority is generated per machine and its X.509 name constraints only allow
//!   Tencent domains, so it cannot be used to impersonate any other site.
//! - Only those domains are decrypted; every other CONNECT is tunnelled without being read.
//! - The system proxy changes only between an explicit start and stop. The previous value is
//!   written to disk first, so a crash is repaired on the next launch.
//! - Captured URLs carry signatures: they are shown to the local user and never logged.
use hudsucker::{
    certificate_authority::RcgenAuthority,
    hyper::{Request, Response},
    Body, HttpContext, HttpHandler, Proxy, RequestOrResponse,
};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, GeneralSubtree, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, NameConstraints, SerialNumber,
};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, sync::Mutex, task::JoinHandle};

/// Domains the CA may sign for and the proxy may decrypt. Keep in sync with `source::WechatSource`.
pub const MITM_SUFFIXES: &[&str] = &[
    "qq.com",
    "myqcloud.com",
    "qcloud.com",
    "qcloudcdn.com",
    "wxlivecdn.com",
];
const CA_COMMON_NAME: &str = "LIVE Local Capture CA";
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_CANDIDATES: usize = 20;
const MAX_HOSTS: usize = 200;
const RESTORE_FILE: &str = "proxy-restore.json";

pub fn install_crypto_provider() {
    // Adding the proxy makes two rustls providers available; pick one so that libraries relying on
    // the process default keep working. An error only means that one is already installed.
    let _ = hudsucker::rustls::crypto::ring::default_provider().install_default();
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StreamCandidate {
    pub url: String,
    pub kind: String,
    pub host: String,
    pub seen_at_ms: i64,
    /// Host name and topic, when WeChat's own responses mention this stream.
    pub room_name: Option<String>,
    pub room_title: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureStatus {
    pub supported: bool,
    pub ca_installed: bool,
    pub running: bool,
    pub port: Option<u16>,
    pub remaining_secs: u64,
    pub replaced_existing_proxy: bool,
    pub candidates: Vec<StreamCandidate>,
    /// Hostnames seen during this capture; not persisted, shown when nothing was found.
    pub hosts: Vec<String>,
    /// JSON endpoints seen during this capture, to diagnose a missing room name.
    pub json_endpoints: Vec<String>,
}

fn host_matches(host: &str, suffixes: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    suffixes
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

fn kind_from_path(path: &str) -> Option<&'static str> {
    let path = path.to_ascii_lowercase();
    if path.ends_with(".flv") {
        Some("flv")
    } else if path.ends_with(".m3u8") {
        Some("m3u8")
    } else {
        None
    }
}

fn kind_from_content_type(content_type: &str) -> Option<&'static str> {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("x-flv") {
        Some("flv")
    } else if content_type.contains("mpegurl") {
        Some("m3u8")
    } else {
        None
    }
}

#[derive(Default)]
struct Findings {
    candidates: Vec<StreamCandidate>,
    hosts: BTreeSet<String>,
    rooms: HashMap<String, RoomInfo>,
    json_endpoints: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct RoomInfo {
    name: String,
    title: Option<String>,
}

fn live_id_regex() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"live_\d{6,}").unwrap())
}

fn live_id_of(url: &str) -> Option<String> {
    live_id_regex().find(url).map(|m| m.as_str().to_string())
}

fn first_text(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(120).collect())
}

/// Finds the deepest object that names a host and whose contents mention the stream. Requiring the
/// stream ID avoids mislabelling a stream with the nickname of a commenter or another room.
fn find_room(value: &Value, live_id: &str, found: &mut Option<RoomInfo>) -> bool {
    match value {
        Value::String(text) => text.contains(live_id),
        Value::Array(items) => items
            .iter()
            .fold(false, |seen, item| find_room(item, live_id, found) || seen),
        Value::Object(object) => {
            let mentions = object
                .values()
                .fold(false, |seen, item| find_room(item, live_id, found) || seen);
            if mentions && found.is_none() {
                let name = first_text(
                    object,
                    &["nickname", "nickName", "anchorNickname", "anchor_nickname", "finderNickname"],
                );
                if let Some(name) = name {
                    *found = Some(RoomInfo {
                        name,
                        title: first_text(
                            object,
                            &["liveTitle", "live_title", "topic", "title", "description", "desc"],
                        ),
                    });
                }
            }
            mentions
        }
        _ => false,
    }
}

/// WeChat names the page of an open live room "<host>的直播".
fn host_from_document_name(name: &str) -> Option<String> {
    name.trim()
        .strip_suffix("的直播")
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| host.chars().take(60).collect())
}

/// Reads the page names of WeChat's visible room tab. Only document names are requested, so chat
/// contents are not read. Windows-only and best effort: any failure just leaves the name empty.
#[cfg(windows)]
fn visible_room_host() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const SCRIPT: &str = r#"
[Console]::OutputEncoding=[Text.Encoding]::UTF8
Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
$condition = New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Document)
foreach ($process in Get-Process Weixin -ErrorAction Stop) {
  if ($process.MainWindowHandle -eq 0) { continue }
  $root = [Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
  foreach ($document in $root.FindAll([Windows.Automation.TreeScope]::Descendants,$condition)) {
    if (-not $document.Current.IsOffscreen) { $document.Current.Name }
  }
}
"#;
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(host_from_document_name)
}

/// Reading WeChat's window needs Windows UI Automation; elsewhere the user names the room by hand.
#[cfg(not(windows))]
fn visible_room_host() -> Option<String> {
    None
}

fn extract_rooms(text: &str) -> Vec<(String, RoomInfo)> {
    let ids: BTreeSet<&str> = live_id_regex().find_iter(text).map(|m| m.as_str()).take(20).collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let Ok(document) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    ids.into_iter()
        .filter_map(|id| {
            let mut found = None;
            find_room(&document, id, &mut found);
            found.map(|room| (id.to_string(), room))
        })
        .collect()
}

struct Shared {
    suffixes: Vec<String>,
    findings: StdMutex<Findings>,
}

impl Shared {
    fn new(suffixes: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            suffixes,
            findings: StdMutex::new(Findings::default()),
        })
    }

    /// A stream first seen moments ago belongs to the room the user just opened. Older streams
    /// are left unnamed rather than risk naming them after a room opened later.
    fn name_recent_streams(&self, host: &str) {
        const RECENT_MS: i64 = 30_000;
        let now = chrono::Utc::now().timestamp_millis();
        let mut findings = self.findings.lock().unwrap();
        let recent: Vec<String> = findings
            .candidates
            .iter()
            .filter(|item| now - item.seen_at_ms <= RECENT_MS)
            .filter_map(|item| live_id_of(&item.url))
            .collect();
        for id in recent {
            findings.rooms.entry(id).or_insert_with(|| RoomInfo {
                name: host.to_string(),
                title: Some(format!("{host}的直播")),
            });
        }
    }

    fn reset(&self) {
        *self.findings.lock().unwrap() = Findings::default();
    }

    fn note_host(&self, host: &str) {
        let mut findings = self.findings.lock().unwrap();
        if findings.hosts.len() < MAX_HOSTS {
            findings.hosts.insert(host.to_ascii_lowercase());
        }
    }

    /// The same stream re-requested with a fresh signature replaces its older entry.
    fn note_stream(&self, url: &str, host: &str, kind: &str) {
        let base = url.split('?').next().unwrap_or(url);
        let mut findings = self.findings.lock().unwrap();
        findings
            .candidates
            .retain(|item| item.url.split('?').next().unwrap_or(&item.url) != base);
        findings.candidates.insert(
            0,
            StreamCandidate {
                url: url.to_string(),
                kind: kind.to_string(),
                host: host.to_ascii_lowercase(),
                seen_at_ms: chrono::Utc::now().timestamp_millis(),
                room_name: None,
                room_title: None,
            },
        );
        findings.candidates.truncate(MAX_CANDIDATES);
    }
}

/// Reads a JSON response for room details, then passes it on decoded with identical content.
async fn inspect_json(shared: &Shared, endpoint: String, res: Response<Body>) -> Response<Body> {
    const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;
    use hudsucker::hyper::header::{CONTENT_ENCODING, CONTENT_LENGTH};
    let too_large = res
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok()?.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_JSON_BYTES);
    let known_encoding = res.headers().get(CONTENT_ENCODING).is_none_or(|value| {
        matches!(
            value.to_str().unwrap_or_default().to_ascii_lowercase().as_str(),
            "gzip" | "x-gzip" | "deflate" | "br" | "zstd" | "identity"
        )
    });
    if too_large || !known_encoding {
        return res;
    }
    let Ok(res) = hudsucker::decode_response(res) else {
        return Response::builder()
            .status(502)
            .body(Body::empty())
            .expect("static response");
    };
    let (mut parts, body) = res.into_parts();
    let Ok(collected) = body.collect().await else {
        return Response::builder()
            .status(502)
            .body(Body::empty())
            .expect("static response");
    };
    let bytes = collected.to_bytes();
    parts.headers.remove(CONTENT_ENCODING);
    parts.headers.insert(CONTENT_LENGTH, bytes.len().into());
    {
        let mut findings = shared.findings.lock().unwrap();
        if findings.json_endpoints.len() < MAX_HOSTS {
            findings.json_endpoints.insert(endpoint);
        }
        if let Ok(text) = std::str::from_utf8(&bytes) {
            for (id, room) in extract_rooms(text) {
                findings.rooms.insert(id, room);
            }
        }
    }
    Response::from_parts(parts, Body::from(bytes))
}

#[derive(Clone)]
struct CaptureHandler {
    shared: Arc<Shared>,
    pending: Option<(String, String)>,
}

impl HttpHandler for CaptureHandler {
    async fn handle_request(&mut self, _ctx: &HttpContext, req: Request<Body>) -> RequestOrResponse {
        self.pending = None;
        if let Some(host) = req.uri().host().map(str::to_string) {
            if host_matches(&host, &self.shared.suffixes) {
                let url = req.uri().to_string();
                if let Some(kind) = kind_from_path(req.uri().path()) {
                    self.shared.note_stream(&url, &host, kind);
                }
                self.pending = Some((url, host));
            }
        }
        req.into()
    }

    async fn handle_response(&mut self, _ctx: &HttpContext, res: Response<Body>) -> Response<Body> {
        if let Some((url, host)) = self.pending.take() {
            let kind = res
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .and_then(kind_from_content_type);
            if let (Some(kind), true) = (kind, res.status().is_success()) {
                self.shared.note_stream(&url, &host, kind);
            }
            let is_json = res
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().contains("json"));
            if is_json && res.status().is_success() {
                let path = url.split('?').next().unwrap_or(&url);
                let endpoint = path.split("://").nth(1).unwrap_or(path).to_string();
                return inspect_json(&self.shared, endpoint, res).await;
            }
        }
        res
    }

    async fn should_intercept_connect(&mut self, _ctx: &HttpContext, req: &Request<Body>) -> bool {
        let Some(host) = req.uri().host() else {
            return false;
        };
        self.shared.note_host(host);
        host_matches(host, &self.shared.suffixes)
    }
}

// --- certificate authority ---------------------------------------------------------------------

#[derive(Clone)]
struct LocalCa {
    cert_pem: String,
    key_pem: String,
    serial_hex: String,
}

#[derive(Serialize, Deserialize)]
struct CaMeta {
    serial_hex: String,
    not_after_ms: i64,
    /// Domains the certificate may sign for. A certificate that lacks one must be replaced.
    #[serde(default)]
    suffixes: Vec<String>,
    /// Replaced certificates, removed from the trust store when the new one is installed.
    #[serde(default)]
    retired_serials: Vec<String>,
}

fn generate_ca(suffixes: &[String]) -> Result<(LocalCa, i64), String> {
    use chrono::Datelike;
    let err = |e: rcgen::Error| format!("生成抓取证书失败：{e}");
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, CA_COMMON_NAME);
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: suffixes
            .iter()
            .map(|suffix| GeneralSubtree::DnsName(suffix.clone()))
            .collect(),
        excluded_subtrees: Vec::new(),
    });
    let mut serial: [u8; 16] = rand::random();
    serial[0] = (serial[0] & 0x7f).max(1);
    params.serial_number = Some(SerialNumber::from_slice(&serial));
    let now = chrono::Utc::now();
    let start = now - chrono::Duration::days(1);
    params.not_before = rcgen::date_time_ymd(start.year(), start.month() as u8, start.day() as u8);
    let end_year = now.year() + 2;
    let end_day = now.day().min(28);
    params.not_after = rcgen::date_time_ymd(end_year, now.month() as u8, end_day as u8);
    let not_after_ms = chrono::NaiveDate::from_ymd_opt(end_year, now.month(), end_day)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc().timestamp_millis())
        .ok_or("证书日期无效")?;
    let key = KeyPair::generate().map_err(err)?;
    let cert = params.self_signed(&key).map_err(err)?;
    Ok((
        LocalCa {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            serial_hex: serial.iter().map(|b| format!("{b:02x}")).collect(),
        },
        not_after_ms,
    ))
}

fn load_or_create_ca(dir: &Path, suffixes: &[String]) -> Result<LocalCa, String> {
    let (cert_path, key_path, meta_path) =
        (dir.join("ca.crt"), dir.join("ca.key"), dir.join("ca.json"));
    let previous: Option<CaMeta> = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let existing = (|| {
        let meta = previous.as_ref()?;
        let week_ms = 7 * 24 * 3600 * 1000;
        if meta.not_after_ms < chrono::Utc::now().timestamp_millis() + week_ms
            || meta.suffixes != suffixes
        {
            return None;
        }
        Some(LocalCa {
            cert_pem: std::fs::read_to_string(&cert_path).ok()?,
            key_pem: std::fs::read_to_string(&key_path).ok()?,
            serial_hex: meta.serial_hex.clone(),
        })
    })();
    if let Some(ca) = existing {
        return Ok(ca);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("无法创建证书目录：{e}"))?;
    let (ca, not_after_ms) = generate_ca(suffixes)?;
    let write_err = |e: std::io::Error| format!("无法保存证书：{e}");
    std::fs::write(&key_path, &ca.key_pem).map_err(write_err)?;
    std::fs::write(&cert_path, &ca.cert_pem).map_err(write_err)?;
    let mut retired_serials = Vec::new();
    if let Some(previous) = previous {
        retired_serials = previous.retired_serials;
        retired_serials.push(previous.serial_hex);
    }
    let meta = CaMeta {
        serial_hex: ca.serial_hex.clone(),
        not_after_ms,
        suffixes: suffixes.to_vec(),
        retired_serials,
    };
    std::fs::write(&meta_path, serde_json::to_string(&meta).unwrap()).map_err(write_err)?;
    Ok(ca)
}

fn authority(ca: &LocalCa) -> Result<RcgenAuthority, String> {
    let err = |e: rcgen::Error| format!("读取抓取证书失败：{e}");
    let key = KeyPair::from_pem(&ca.key_pem).map_err(err)?;
    let issuer = Issuer::from_ca_cert_pem(&ca.cert_pem, key).map_err(err)?;
    Ok(RcgenAuthority::new(
        issuer,
        1_000,
        hudsucker::rustls::crypto::ring::default_provider(),
    ))
}

#[cfg(windows)]
fn certutil(args: &[&str]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("certutil")
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("无法运行 certutil：{e}"))
}

#[cfg(windows)]
fn trust_store_contains(serial_hex: &str) -> bool {
    certutil(&["-user", "-store", "Root"])
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .to_ascii_lowercase()
                .contains(serial_hex)
        })
        .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn trust_store_contains(_serial_hex: &str) -> bool {
    false
}

/// macOS keeps trust per certificate in the user's keychain. `verify-cert` succeeds only when the
/// certificate is a trusted root, which is what the capture needs.
#[cfg(target_os = "macos")]
fn is_trusted(dir: &Path, _serial_hex: &str) -> bool {
    let cert = dir.join("ca.crt");
    cert.exists()
        && std::process::Command::new("security")
            .args(["verify-cert", "-c"])
            .arg(&cert)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn is_trusted(_dir: &Path, serial_hex: &str) -> bool {
    trust_store_contains(serial_hex)
}

#[cfg(target_os = "macos")]
fn login_keychain() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Keychains/login.keychain-db"))
        .ok_or_else(|| "无法确定登录钥匙串位置".to_string())
}

/// Removes every certificate with our name, which also covers replaced and retired ones.
#[cfg(target_os = "macos")]
fn delete_our_certificates(keychain: &Path) -> bool {
    let mut removed_any = false;
    for _ in 0..8 {
        let removed = std::process::Command::new("security")
            .args(["delete-certificate", "-t", "-c", CA_COMMON_NAME])
            .arg(keychain)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !removed {
            break;
        }
        removed_any = true;
    }
    removed_any
}

// --- system proxy ------------------------------------------------------------------------------

const PROXY_FLAG_DIRECT: u32 = 0x1;
const PROXY_FLAG_PROXY: u32 = 0x2;
const PROXY_FLAG_AUTO_URL: u32 = 0x4;

/// The current user's proxy settings for the default connection, as Windows stores them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProxySnapshot {
    pub flags: u32,
    pub server: Option<String>,
    pub bypass: Option<String>,
    pub auto_config_url: Option<String>,
    /// macOS keeps proxies per network service; each one is recorded so that it is restored as it
    /// was. Empty on Windows, and in the takeover value, which applies to every service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceProxy>,
}

/// One macOS network service's web (HTTP) and secure web (HTTPS) proxies.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServiceProxy {
    pub service: String,
    pub web: ProxyEndpoint,
    pub secure: ProxyEndpoint,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProxyEndpoint {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl ProxySnapshot {
    fn enabled(&self) -> bool {
        self.flags & PROXY_FLAG_PROXY != 0
    }
}

pub trait ProxyStore: Send + Sync {
    fn read(&self) -> Result<ProxySnapshot, String>;
    fn write(&self, snapshot: &ProxySnapshot) -> Result<(), String>;

    /// Puts `previous` back only where the proxy is still `ours`: another tool's later change is
    /// left alone.
    fn restore(&self, previous: &ProxySnapshot, ours: &str) -> Result<(), String> {
        let current = self.read()?;
        if current.enabled() && current.server.as_deref() == Some(ours) {
            self.write(previous)?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct RestoreRecord {
    previous: ProxySnapshot,
    ours: String,
}

/// Writes the restore record before touching the proxy, so that a crash can always be repaired.
fn take_over_proxy(store: &dyn ProxyStore, dir: &Path, port: u16) -> Result<ProxySnapshot, String> {
    let previous = store.read()?;
    if previous.flags & PROXY_FLAG_AUTO_URL != 0 {
        return Err("系统正在使用自动配置脚本（PAC），请先在系统代理设置里关闭后再抓取".into());
    }
    let ours = format!("127.0.0.1:{port}");
    let record = RestoreRecord {
        previous: previous.clone(),
        ours: ours.clone(),
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("无法写入恢复记录：{e}"))?;
    std::fs::write(
        dir.join(RESTORE_FILE),
        serde_json::to_string(&record).unwrap(),
    )
    .map_err(|e| format!("无法写入恢复记录：{e}"))?;
    let takeover = ProxySnapshot {
        flags: PROXY_FLAG_DIRECT | PROXY_FLAG_PROXY,
        server: Some(ours),
        bypass: previous.bypass.clone(),
        auto_config_url: None,
        services: Vec::new(),
    };
    if let Err(error) = store.write(&takeover) {
        let _ = restore_proxy(store, dir);
        return Err(error);
    }
    Ok(previous)
}

/// Restores only if the proxy is still ours: another tool's later change is left alone.
pub fn restore_proxy(store: &dyn ProxyStore, dir: &Path) -> Result<(), String> {
    let path = dir.join(RESTORE_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    if let Ok(record) = serde_json::from_str::<RestoreRecord>(&text) {
        store.restore(&record.previous, &record.ours)?;
    }
    std::fs::remove_file(&path).map_err(|e| format!("无法清除恢复记录：{e}"))
}

/// Uses the WinINet per-connection API. Writing only `ProxyServer`/`ProxyEnable` in the registry is
/// not enough: Chromium-based apps such as WeChat read `DefaultConnectionSettings`, which this
/// API keeps in step with those values.
#[cfg(windows)]
pub struct SystemProxyStore;

#[cfg(windows)]
impl ProxyStore for SystemProxyStore {
    fn read(&self) -> Result<ProxySnapshot, String> {
        use windows_sys::Win32::{
            Foundation::GlobalFree,
            Networking::WinInet::{
                InternetQueryOptionW, INTERNET_OPTION_PER_CONNECTION_OPTION,
                INTERNET_PER_CONN_AUTOCONFIG_URL, INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_OPTIONW,
                INTERNET_PER_CONN_OPTION_LISTW, INTERNET_PER_CONN_PROXY_BYPASS,
                INTERNET_PER_CONN_PROXY_SERVER,
            },
        };
        // SAFETY: the option structs are plain data; zero is valid for every field.
        let mut options: [INTERNET_PER_CONN_OPTIONW; 4] = unsafe { std::mem::zeroed() };
        options[0].dwOption = INTERNET_PER_CONN_FLAGS;
        options[1].dwOption = INTERNET_PER_CONN_PROXY_SERVER;
        options[2].dwOption = INTERNET_PER_CONN_PROXY_BYPASS;
        options[3].dwOption = INTERNET_PER_CONN_AUTOCONFIG_URL;
        let mut list = INTERNET_PER_CONN_OPTION_LISTW {
            dwSize: std::mem::size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
            pszConnection: std::ptr::null_mut(),
            dwOptionCount: options.len() as u32,
            dwOptionError: 0,
            pOptions: options.as_mut_ptr(),
        };
        let mut size = list.dwSize;
        // SAFETY: `list` and `options` outlive the call and `size` is the size of `list`.
        let ok = unsafe {
            InternetQueryOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_PER_CONNECTION_OPTION,
                std::ptr::addr_of_mut!(list).cast(),
                &mut size,
            )
        };
        if ok == 0 {
            return Err(format!(
                "无法读取系统代理：{}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: on success Windows filled the union member matching each option; the strings
        // are null or NUL-terminated allocations that this code owns and must free.
        let take_string = |index: usize| unsafe {
            let ptr = options[index].Value.pszValue;
            if ptr.is_null() {
                return None;
            }
            let len = (0..).take_while(|&i| *ptr.add(i) != 0).count();
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            GlobalFree(ptr as _);
            Some(text).filter(|text| !text.is_empty())
        };
        Ok(ProxySnapshot {
            // SAFETY: option 0 is `INTERNET_PER_CONN_FLAGS`, a DWORD.
            flags: unsafe { options[0].Value.dwValue },
            server: take_string(1),
            bypass: take_string(2),
            auto_config_url: take_string(3),
            services: Vec::new(),
        })
    }

    fn write(&self, snapshot: &ProxySnapshot) -> Result<(), String> {
        use windows_sys::Win32::Networking::WinInet::{
            InternetSetOptionW, INTERNET_OPTION_PER_CONNECTION_OPTION, INTERNET_OPTION_REFRESH,
            INTERNET_OPTION_SETTINGS_CHANGED, INTERNET_PER_CONN_AUTOCONFIG_URL,
            INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_OPTIONW, INTERNET_PER_CONN_OPTION_LISTW,
            INTERNET_PER_CONN_PROXY_BYPASS, INTERNET_PER_CONN_PROXY_SERVER,
        };
        let wide = |text: &Option<String>| -> Vec<u16> {
            text.as_deref()
                .unwrap_or("")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect()
        };
        let (mut server, mut bypass, mut auto_url) = (
            wide(&snapshot.server),
            wide(&snapshot.bypass),
            wide(&snapshot.auto_config_url),
        );
        // SAFETY: the option structs are plain data; zero is valid for every field.
        let mut options: [INTERNET_PER_CONN_OPTIONW; 4] = unsafe { std::mem::zeroed() };
        options[0].dwOption = INTERNET_PER_CONN_FLAGS;
        options[0].Value.dwValue = snapshot.flags;
        options[1].dwOption = INTERNET_PER_CONN_PROXY_SERVER;
        options[1].Value.pszValue = server.as_mut_ptr();
        options[2].dwOption = INTERNET_PER_CONN_PROXY_BYPASS;
        options[2].Value.pszValue = bypass.as_mut_ptr();
        options[3].dwOption = INTERNET_PER_CONN_AUTOCONFIG_URL;
        options[3].Value.pszValue = auto_url.as_mut_ptr();
        let list = INTERNET_PER_CONN_OPTION_LISTW {
            dwSize: std::mem::size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
            pszConnection: std::ptr::null_mut(),
            dwOptionCount: options.len() as u32,
            dwOptionError: 0,
            pOptions: options.as_mut_ptr(),
        };
        // SAFETY: `list`, `options` and the strings all outlive these calls. The notifications
        // take no buffer, and a null handle applies them to every WinINet client.
        unsafe {
            if InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_PER_CONNECTION_OPTION,
                std::ptr::addr_of!(list).cast(),
                list.dwSize,
            ) == 0
            {
                return Err(format!(
                    "无法修改系统代理：{}",
                    std::io::Error::last_os_error()
                ));
            }
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                std::ptr::null(),
                0,
            );
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_REFRESH,
                std::ptr::null(),
                0,
            );
        }
        Ok(())
    }
}

/// Drives `networksetup`. macOS keeps proxies per network service and apps follow the primary
/// service, which changes with Wi-Fi, Ethernet or a VPN, so the takeover applies to every enabled
/// service and each one is recorded to be restored as it was.
#[cfg(target_os = "macos")]
pub struct SystemProxyStore;

#[cfg(target_os = "macos")]
fn networksetup(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("networksetup")
        .args(args)
        .output()
        .map_err(|e| format!("无法运行 networksetup：{e}"))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() && !text.contains("** Error") {
        Ok(text)
    } else {
        Err(format!(
            "networksetup 失败：{}{}",
            text.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// `networksetup -listallnetworkservices`: a header line, then one service per line, with `*` in
/// front of disabled ones.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_network_services(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('*'))
        .map(String::from)
        .collect()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
        .map(str::trim)
}

/// `networksetup -getwebproxy` / `-getsecurewebproxy` output.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_endpoint(text: &str) -> ProxyEndpoint {
    ProxyEndpoint {
        enabled: field(text, "Enabled") == Some("Yes"),
        host: field(text, "Server").unwrap_or_default().to_string(),
        port: field(text, "Port").and_then(|port| port.parse().ok()).unwrap_or(0),
    }
}

/// Summarises every service the way the shared takeover logic expects: the proxy counts as on when
/// any service has one, and the server shown is the first one in use, preferring secure web.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn summarise_services(services: Vec<ServiceProxy>, auto_config_url: Option<String>) -> ProxySnapshot {
    let in_use = services
        .iter()
        .flat_map(|service| [&service.secure, &service.web])
        .find(|endpoint| endpoint.enabled && !endpoint.host.is_empty());
    let mut flags = PROXY_FLAG_DIRECT;
    if in_use.is_some() {
        flags |= PROXY_FLAG_PROXY;
    }
    if auto_config_url.is_some() {
        flags |= PROXY_FLAG_AUTO_URL;
    }
    ProxySnapshot {
        flags,
        server: in_use.map(|endpoint| format!("{}:{}", endpoint.host, endpoint.port)),
        bypass: None,
        auto_config_url,
        services,
    }
}

/// The HTTPS proxy apps actually use: the top level of `scutil --proxy`, which follows the primary
/// service. Per-interface values under `__SCOPED__` are nested deeper and ignored, since ordinary
/// connections do not read them.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_effective_https_proxy(text: &str) -> Option<String> {
    let mut values = HashMap::new();
    for line in text.lines() {
        // Top-level keys are indented by exactly two spaces; nested ones by more.
        let Some(rest) = line.strip_prefix("  ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue;
        }
        let Some((key, value)) = rest.split_once(" : ") else {
            continue;
        };
        values.insert(key.trim().to_string(), value.trim().to_string());
    }
    if values.get("HTTPSEnable").map(String::as_str) != Some("1") {
        return None;
    }
    Some(format!("{}:{}", values.get("HTTPSProxy")?, values.get("HTTPSPort")?))
}

/// `host:port` as `networksetup` takes it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn split_server(server: &str) -> Option<(String, u16)> {
    let (host, port) = server.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

/// Whether to put `recorded` back over `current`. With `ours`, only an endpoint that still points at
/// our proxy is put back; one another tool changed since the takeover is left alone.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn endpoint_needs_restore(current: &ProxyEndpoint, recorded: &ProxyEndpoint, ours: Option<&str>) -> bool {
    let still_ours = ours.is_none_or(|ours| {
        current.enabled && format!("{}:{}", current.host, current.port) == ours
    });
    still_ours && current != recorded
}

#[cfg(target_os = "macos")]
fn network_services() -> Result<Vec<String>, String> {
    let services = parse_network_services(&networksetup(&["-listallnetworkservices"])?);
    if services.is_empty() {
        return Err("找不到可用的网络连接，无法读取系统代理".into());
    }
    Ok(services)
}

#[cfg(target_os = "macos")]
fn read_service(service: &str) -> Result<ServiceProxy, String> {
    Ok(ServiceProxy {
        service: service.to_string(),
        web: parse_endpoint(&networksetup(&["-getwebproxy", service])?),
        secure: parse_endpoint(&networksetup(&["-getsecurewebproxy", service])?),
    })
}

#[cfg(target_os = "macos")]
fn write_endpoint(service: &str, kind: &str, endpoint: &ProxyEndpoint) -> Result<(), String> {
    let set = format!("-set{kind}proxy");
    if !endpoint.host.is_empty() && endpoint.port != 0 {
        let port = endpoint.port.to_string();
        networksetup(&[&set, service, &endpoint.host, &port])?;
    } else if !endpoint.enabled {
        // The service had no server before the takeover. Clearing it is cosmetic: a disabled
        // proxy is never used, so a refusal is ignored.
        let _ = networksetup(&[&set, service, "", "0"]);
    }
    let state = if endpoint.enabled { "on" } else { "off" };
    networksetup(&[&format!("-set{kind}proxystate"), service, state])?;
    Ok(())
}

/// What decides where traffic goes: which proxies are on, and their servers. A disabled proxy's
/// leftover server is ignored.
#[cfg(all(test, target_os = "macos"))]
fn in_effect(endpoint: &ProxyEndpoint) -> Option<(String, u16)> {
    endpoint
        .enabled
        .then(|| (endpoint.host.clone(), endpoint.port))
}

/// Puts one service back as recorded, endpoint by endpoint (see `endpoint_needs_restore`). A
/// service removed since the takeover has nothing to restore.
#[cfg(target_os = "macos")]
fn restore_service(recorded: &ServiceProxy, ours: Option<&str>) -> Result<(), String> {
    let Ok(current) = read_service(&recorded.service) else {
        return Ok(());
    };
    if endpoint_needs_restore(&current.web, &recorded.web, ours) {
        write_endpoint(&recorded.service, "web", &recorded.web)?;
    }
    if endpoint_needs_restore(&current.secure, &recorded.secure, ours) {
        write_endpoint(&recorded.service, "secureweb", &recorded.secure)?;
    }
    Ok(())
}

/// Restores every service; one that fails does not stop the others.
#[cfg(target_os = "macos")]
fn restore_services(services: &[ServiceProxy], ours: Option<&str>) -> Result<(), String> {
    let errors: Vec<String> = services
        .iter()
        .filter_map(|service| {
            restore_service(service, ours)
                .err()
                .map(|error| format!("{}：{error}", service.service))
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

#[cfg(target_os = "macos")]
impl ProxyStore for SystemProxyStore {
    fn read(&self) -> Result<ProxySnapshot, String> {
        let mut services = Vec::new();
        let mut auto_config_url = None;
        for service in network_services()? {
            // Some services (for example a VPN without settings) cannot be queried; skip them.
            let Ok(proxy) = read_service(&service) else {
                continue;
            };
            if let Ok(auto) = networksetup(&["-getautoproxyurl", &service]) {
                if field(&auto, "Enabled") == Some("Yes") && auto_config_url.is_none() {
                    auto_config_url = Some(
                        field(&auto, "URL")
                            .filter(|url| *url != "(null)")
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
            }
            services.push(proxy);
        }
        if services.is_empty() {
            return Err("无法读取任何网络连接的系统代理".into());
        }
        Ok(summarise_services(services, auto_config_url))
    }

    /// Applies recorded services exactly; without them, sets the one server on every service.
    fn write(&self, snapshot: &ProxySnapshot) -> Result<(), String> {
        if !snapshot.services.is_empty() {
            return restore_services(&snapshot.services, None);
        }
        let services = network_services()?;
        let Some(server) = snapshot.server.as_deref().filter(|_| snapshot.enabled()) else {
            for service in &services {
                let _ = networksetup(&["-setwebproxystate", service, "off"]);
                let _ = networksetup(&["-setsecurewebproxystate", service, "off"]);
            }
            return Ok(());
        };
        let (host, port) =
            split_server(server).ok_or_else(|| format!("代理地址格式不对：{server}"))?;
        let endpoint = ProxyEndpoint {
            enabled: true,
            host,
            port,
        };
        // A service that refuses (for example one managed by a VPN app) must not stop the others;
        // whether the result is in effect is checked separately.
        let mut last_error = None;
        let mut applied = 0;
        for service in &services {
            let result = write_endpoint(service, "web", &endpoint)
                .and_then(|_| write_endpoint(service, "secureweb", &endpoint));
            match result {
                Ok(()) => applied += 1,
                Err(error) => last_error = Some(error),
            }
        }
        match (applied, last_error) {
            (0, Some(error)) => Err(error),
            _ => Ok(()),
        }
    }

    /// Checked per service and per endpoint: the takeover touched every service, and another tool
    /// may since have changed any one of them.
    fn restore(&self, previous: &ProxySnapshot, ours: &str) -> Result<(), String> {
        if !previous.services.is_empty() {
            return restore_services(&previous.services, Some(ours));
        }
        // A record from a version that did not track services holds one server for all of them.
        let endpoint = previous
            .server
            .as_deref()
            .filter(|_| previous.enabled())
            .and_then(split_server)
            .map(|(host, port)| ProxyEndpoint {
                enabled: true,
                host,
                port,
            })
            .unwrap_or_default();
        let services: Vec<ServiceProxy> = network_services()?
            .into_iter()
            .map(|service| ServiceProxy {
                service,
                web: endpoint.clone(),
                secure: endpoint.clone(),
            })
            .collect();
        restore_services(&services, Some(ours))
    }
}

/// The network interface of the default route, e.g. `en0` or a VPN's `utun3`.
#[cfg(target_os = "macos")]
fn default_route_interface() -> Option<String> {
    let output = std::process::Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("interface:"))
        .map(|name| name.trim().to_string())
}

/// Waits for the takeover to become the proxy apps actually use. Tools that route traffic through
/// a virtual interface (Quantumult X, Surge, Clash in enhanced/TUN mode, or any VPN) become the
/// primary service, and its proxy settings win; capturing would then silently find nothing.
#[cfg(target_os = "macos")]
fn wait_until_proxy_effective(ours: &str) -> Result<(), String> {
    for _ in 0..25 {
        let effective = std::process::Command::new("scutil")
            .arg("--proxy")
            .output()
            .ok()
            .and_then(|output| parse_effective_https_proxy(&String::from_utf8_lossy(&output.stdout)));
        if effective.as_deref() == Some(ours) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let interface = default_route_interface().unwrap_or_default();
    let virtual_interface = ["utun", "ipsec", "ppp", "tun", "tap"]
        .iter()
        .any(|prefix| interface.starts_with(prefix));
    Err(if virtual_interface {
        format!(
            "系统代理没有生效：网络当前由虚拟网卡 {interface} 接管，多半是 Quantumult X、Surge、Clash 等工具的增强模式（TUN）或 VPN。请先关闭增强模式或断开 VPN 再开始抓取，原代理设置已恢复。"
        )
    } else {
        "系统代理没有生效，可能被其他工具锁定，原代理设置已恢复。可以在“系统设置 → 网络 → 详细信息 → 代理”里检查。".to_string()
    })
}

/// Takes over the system proxy and, on macOS, confirms that apps will actually use it.
#[cfg(any(windows, target_os = "macos"))]
fn take_over_and_confirm(dir: &Path, port: u16) -> Result<ProxySnapshot, String> {
    let previous = take_over_proxy(&SystemProxyStore, dir, port)?;
    #[cfg(target_os = "macos")]
    if let Err(error) = wait_until_proxy_effective(&format!("127.0.0.1:{port}")) {
        if let Err(restore_error) = restore_proxy(&SystemProxyStore, dir) {
            return Err(format!("{error}（恢复失败：{restore_error}，请手动关闭系统代理）"));
        }
        return Err(error);
    }
    Ok(previous)
}

/// Run at startup: repairs the proxy if the previous run ended while capturing.
#[cfg(any(windows, target_os = "macos"))]
pub fn recover_after_crash(dir: &Path) {
    if let Err(error) = restore_proxy(&SystemProxyStore, dir) {
        log::warn!("Failed to restore the system proxy after a previous capture: {error}");
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn recover_after_crash(_dir: &Path) {}

// --- service -----------------------------------------------------------------------------------

#[cfg(not(any(windows, target_os = "macos")))]
const WINDOWS_ONLY: &str = "视频号自动抓取目前只支持 Windows 和 macOS";

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Running {
    port: u16,
    shutdown: oneshot::Sender<()>,
    started: std::time::Instant,
    replaced_existing_proxy: bool,
    watchdog: JoinHandle<()>,
}

pub struct WechatCapture {
    dir: PathBuf,
    shared: Arc<Shared>,
    running: Mutex<Option<Running>>,
}

impl WechatCapture {
    pub fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            dir,
            shared: Shared::new(MITM_SUFFIXES.iter().map(|s| s.to_string()).collect()),
            running: Mutex::new(None),
        })
    }

    fn ca(&self) -> Result<LocalCa, String> {
        load_or_create_ca(&self.dir, &self.shared.suffixes)
    }

    /// Previews only open URLs that this capture actually observed.
    pub fn knows_stream(&self, url: &str) -> bool {
        let findings = self.shared.findings.lock().unwrap();
        findings.candidates.iter().any(|item| item.url == url)
    }

    pub async fn status(&self) -> Result<CaptureStatus, String> {
        // Starting requires the certificate, so the store is only queried while idle.
        let capturing = self.running.lock().await.is_some();
        let dir = self.dir.clone();
        let suffixes = self.shared.suffixes.clone();
        let ca_installed = capturing
            || tokio::task::spawn_blocking(move || {
                let meta = std::fs::read_to_string(dir.join("ca.json")).ok()?;
                let meta: CaMeta = serde_json::from_str(&meta).ok()?;
                let current = meta.suffixes == suffixes;
                Some(current && is_trusted(&dir, &meta.serial_hex))
            })
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or(false);
        let running = self.running.lock().await;
        let findings = self.shared.findings.lock().unwrap();
        Ok(CaptureStatus {
            supported: cfg!(any(windows, target_os = "macos")),
            ca_installed,
            running: running.is_some(),
            port: running.as_ref().map(|item| item.port),
            remaining_secs: running.as_ref().map_or(0, |item| {
                CAPTURE_TIMEOUT
                    .saturating_sub(item.started.elapsed())
                    .as_secs()
            }),
            replaced_existing_proxy: running.as_ref().is_some_and(|i| i.replaced_existing_proxy),
            candidates: findings
                .candidates
                .iter()
                .cloned()
                .map(|mut candidate| {
                    let room = live_id_of(&candidate.url).and_then(|id| findings.rooms.get(&id));
                    if let Some(room) = room {
                        candidate.room_name = Some(room.name.clone());
                        candidate.room_title = room.title.clone();
                    }
                    candidate
                })
                .collect(),
            hosts: findings.hosts.iter().cloned().collect(),
            json_endpoints: findings.json_endpoints.iter().cloned().collect(),
        })
    }

    /// macOS asks for the user's password (or Touch ID) itself; nothing is trusted unless they accept.
    #[cfg(target_os = "macos")]
    pub async fn install_ca(&self) -> Result<(), String> {
        self.ca()?;
        let cert = self.dir.join("ca.crt");
        let keychain = login_keychain()?;
        let output = tokio::task::spawn_blocking(move || {
            // A replaced certificate must not stay trusted next to the new one.
            delete_our_certificates(&keychain);
            std::process::Command::new("security")
                .args(["add-trusted-cert", "-r", "trustRoot", "-k"])
                .arg(&keychain)
                .arg(&cert)
                .output()
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("无法运行 security：{e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err("证书没有安装。如果你在 macOS 的授权窗口里取消了，可以重新点安装。".into())
        }
    }

    #[cfg(target_os = "macos")]
    pub async fn remove_ca(&self) -> Result<(), String> {
        self.stop().await?;
        let keychain = login_keychain()?;
        let removed = tokio::task::spawn_blocking(move || delete_our_certificates(&keychain))
            .await
            .map_err(|e| e.to_string())?;
        if removed || !is_trusted(&self.dir, "") {
            Ok(())
        } else {
            Err("证书没有移除，可以在“钥匙串访问”里手动删除名为抓取证书的条目。".into())
        }
    }

    /// Windows shows its own confirmation dialog; nothing is trusted unless the user accepts it.
    #[cfg(windows)]
    pub async fn install_ca(&self) -> Result<(), String> {
        self.ca()?;
        let cert = self.dir.join("ca.crt");
        let output = tokio::task::spawn_blocking(move || {
            certutil(&["-user", "-addstore", "Root", &cert.to_string_lossy()])
        })
        .await
        .map_err(|e| e.to_string())??;
        if output.status.success() {
            self.remove_retired_certificates().await;
            Ok(())
        } else {
            Err("证书没有安装。如果你在 Windows 的提示里选了“否”，可以重新点安装。".into())
        }
    }

    /// Replaced certificates must not stay trusted. A refusal is retried on the next install.
    #[cfg(windows)]
    async fn remove_retired_certificates(&self) {
        let meta_path = self.dir.join("ca.json");
        let Some(mut meta) = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|text| serde_json::from_str::<CaMeta>(&text).ok())
        else {
            return;
        };
        let retired = std::mem::take(&mut meta.retired_serials);
        for serial in retired {
            let target = serial.clone();
            let keep = tokio::task::spawn_blocking(move || {
                trust_store_contains(&target)
                    && !certutil(&["-user", "-delstore", "Root", &target])
                        .map(|output| output.status.success())
                        .unwrap_or(false)
            })
            .await
            .unwrap_or(true);
            if keep {
                meta.retired_serials.push(serial);
            }
        }
        if let Ok(text) = serde_json::to_string(&meta) {
            let _ = std::fs::write(&meta_path, text);
        }
    }

    #[cfg(windows)]
    pub async fn remove_ca(&self) -> Result<(), String> {
        self.stop().await?;
        let serial = self.ca()?.serial_hex;
        self.remove_retired_certificates().await;
        let output = tokio::task::spawn_blocking(move || {
            certutil(&["-user", "-delstore", "Root", &serial])
        })
        .await
        .map_err(|e| e.to_string())??;
        if output.status.success() {
            Ok(())
        } else {
            Err("证书没有移除。如果你在 Windows 的提示里选了“否”，可以重新点移除。".into())
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }
        let ca = self.ca()?;
        if !is_trusted(&self.dir, &ca.serial_hex) {
            return Err("请先安装抓取证书".into());
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("无法启动本地代理：{e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("无法启动本地代理：{e}"))?
            .port();
        self.shared.reset();
        let authority = authority(&ca)?;
        let (shutdown, shutdown_rx) = oneshot::channel();
        spawn_proxy(
            Proxy::builder()
                .with_listener(listener)
                .with_ca(authority)
                .with_rustls_connector(hudsucker::rustls::crypto::ring::default_provider())
                .with_http_handler(CaptureHandler {
                    shared: self.shared.clone(),
                    pending: None,
                })
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .build()
                .map_err(|e| format!("无法启动本地代理：{e}"))?,
        );
        // `networksetup` runs one process per call and per network service, so keep it off the
        // async workers.
        let dir = self.dir.clone();
        let takeover = tokio::task::spawn_blocking(move || take_over_and_confirm(&dir, port))
            .await
            .map_err(|e| e.to_string())
            .and_then(|result| result);
        let previous = match takeover {
            Ok(previous) => previous,
            Err(error) => {
                let _ = shutdown.send(());
                return Err(error);
            }
        };
        let namer_shared = self.shared.clone();
        let this = self.clone();
        let watchdog = tokio::spawn(async move {
            let namer = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let has_streams = !namer_shared.findings.lock().unwrap().candidates.is_empty();
                    if !has_streams {
                        continue;
                    }
                    let host = tokio::task::spawn_blocking(visible_room_host)
                        .await
                        .ok()
                        .flatten();
                    if let Some(host) = host {
                        namer_shared.name_recent_streams(&host);
                    }
                }
            });
            let _abort_namer = AbortOnDrop(namer);
            tokio::time::sleep(CAPTURE_TIMEOUT).await;
            if let Err(error) = this.stop().await {
                log::warn!("Failed to stop WeChat capture after timeout: {error}");
            }
        });
        *running = Some(Running {
            port,
            shutdown,
            started: std::time::Instant::now(),
            replaced_existing_proxy: previous.enabled(),
            watchdog,
        });
        Ok(())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub async fn install_ca(&self) -> Result<(), String> {
        Err(WINDOWS_ONLY.into())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub async fn remove_ca(&self) -> Result<(), String> {
        Err(WINDOWS_ONLY.into())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        Err(WINDOWS_ONLY.into())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let Some(running) = self.running.lock().await.take() else {
            return Ok(());
        };
        // Restore first: a stopped proxy with the setting still in place would cut off the network.
        #[cfg(any(windows, target_os = "macos"))]
        let restored = {
            let dir = self.dir.clone();
            tokio::task::spawn_blocking(move || restore_proxy(&SystemProxyStore, &dir))
                .await
                .map_err(|e| e.to_string())
                .and_then(|result| result)
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let restored: Result<(), String> = Ok(());
        let _ = running.shutdown.send(());
        running.watchdog.abort();
        restored.map_err(|error| format!("{error}。请在系统代理设置里手动关闭代理。"))
    }
}

fn spawn_proxy<C, CA, H, W, F>(proxy: Proxy<C, CA, H, W, F>)
where
    C: hudsucker::hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static,
    CA: hudsucker::certificate_authority::CertificateAuthority,
    H: HttpHandler,
    W: hudsucker::WebSocketHandler,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(error) = proxy.start().await {
            log::warn!("WeChat capture proxy stopped with an error: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use hudsucker::hyper_util::client::legacy::connect::HttpConnector;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct FakeStore(StdMutex<ProxySnapshot>);

    impl FakeStore {
        fn with(flags: u32, server: Option<&str>) -> Self {
            Self(StdMutex::new(ProxySnapshot {
                flags,
                server: server.map(str::to_string),
                bypass: Some("<local>".into()),
                auto_config_url: None,
                services: Vec::new(),
            }))
        }
        fn get(&self) -> ProxySnapshot {
            self.0.lock().unwrap().clone()
        }
    }

    impl ProxyStore for FakeStore {
        fn read(&self) -> Result<ProxySnapshot, String> {
            Ok(self.get())
        }
        fn write(&self, snapshot: &ProxySnapshot) -> Result<(), String> {
            *self.0.lock().unwrap() = snapshot.clone();
            Ok(())
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("live-capture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_tencent_domains_are_decrypted() {
        let suffixes: Vec<String> = MITM_SUFFIXES.iter().map(|s| s.to_string()).collect();
        for host in [
            "finder.video.qq.com",
            "qq.com",
            "A.MYQCLOUD.COM",
            "x.qcloudcdn.com.",
            "pull.wxlivecdn.com",
        ] {
            assert!(host_matches(host, &suffixes), "{host}");
        }
        for host in ["qq.com.evil.test", "notqq.com", "bank.example.com", "weixin.com"] {
            assert!(!host_matches(host, &suffixes), "{host}");
        }
    }

    #[test]
    fn same_stream_keeps_only_the_newest_signature() {
        let shared = Shared::new(vec![]);
        shared.note_stream("https://a.qq.com/l.flv?sig=1", "a.qq.com", "flv");
        shared.note_stream("https://a.qq.com/m.m3u8?sig=1", "a.qq.com", "m3u8");
        shared.note_stream("https://a.qq.com/l.flv?sig=2", "a.qq.com", "flv");
        let findings = shared.findings.lock().unwrap();
        assert_eq!(findings.candidates.len(), 2);
        assert_eq!(findings.candidates[0].url, "https://a.qq.com/l.flv?sig=2");
    }

    #[test]
    fn proxy_is_restored_exactly_and_marker_removed() {
        let dir = temp_dir();
        let store = FakeStore::with(PROXY_FLAG_DIRECT | PROXY_FLAG_PROXY, Some("127.0.0.1:7897"));
        let before = store.get();
        take_over_proxy(&store, &dir, 5555).unwrap();
        assert_eq!(store.get().server.as_deref(), Some("127.0.0.1:5555"));
        assert!(store.get().enabled());
        restore_proxy(&store, &dir).unwrap();
        assert_eq!(store.get(), before);
        assert!(!dir.join(RESTORE_FILE).exists());
    }

    #[test]
    fn crash_recovery_restores_a_disabled_proxy() {
        let dir = temp_dir();
        let store = FakeStore::with(PROXY_FLAG_DIRECT | 0x8, None);
        let before = store.get();
        take_over_proxy(&store, &dir, 5555).unwrap();
        assert!(store.get().enabled());
        // A new process only has the record on disk.
        restore_proxy(&store, &dir).unwrap();
        assert_eq!(store.get(), before);
        // Nothing to do on a clean start.
        restore_proxy(&store, &dir).unwrap();
    }

    #[test]
    fn later_proxy_changes_by_other_tools_are_left_alone() {
        let dir = temp_dir();
        let store = FakeStore::with(PROXY_FLAG_DIRECT, Some("127.0.0.1:7897"));
        take_over_proxy(&store, &dir, 5555).unwrap();
        let other = ProxySnapshot {
            flags: PROXY_FLAG_DIRECT | PROXY_FLAG_PROXY,
            server: Some("127.0.0.1:9999".into()),
            bypass: None,
            auto_config_url: None,
            services: Vec::new(),
        };
        store.write(&other).unwrap();
        restore_proxy(&store, &dir).unwrap();
        assert_eq!(store.get(), other);
    }

    #[test]
    fn pac_configuration_is_refused_without_changes() {
        let dir = temp_dir();
        let store = FakeStore::with(PROXY_FLAG_DIRECT | PROXY_FLAG_AUTO_URL, None);
        let before = store.get();
        assert!(take_over_proxy(&store, &dir, 5555).is_err());
        assert_eq!(store.get(), before);
        assert!(!dir.join(RESTORE_FILE).exists());
    }

    /// Read-only: checks the query path against the real settings without changing them.
    #[cfg(windows)]
    #[test]
    fn reads_the_real_proxy_settings() {
        let snapshot = SystemProxyStore.read().unwrap();
        println!("flags={:#x} server={:?}", snapshot.flags, snapshot.server);
        assert_ne!(snapshot.flags, 0);
    }

    /// Read-only: checks the `networksetup` query path against the real settings.
    #[cfg(target_os = "macos")]
    #[test]
    fn reads_the_real_macos_proxy_settings() {
        let snapshot = SystemProxyStore.read().unwrap();
        println!("flags={:#x} server={:?}", snapshot.flags, snapshot.server);
        assert_ne!(snapshot.flags, 0);
        assert!(!snapshot.services.is_empty());
    }

    #[test]
    fn macos_service_list_skips_header_and_disabled_services() {
        let text = "An asterisk (*) denotes that a network service is disabled.\nThunderbolt Bridge\nWi-Fi\n*Old VPN\nQuantumult X 2\n";
        assert_eq!(
            parse_network_services(text),
            vec!["Thunderbolt Bridge", "Wi-Fi", "Quantumult X 2"]
        );
    }

    #[test]
    fn macos_endpoints_and_summary() {
        let web = parse_endpoint("Enabled: No\nServer: 127.0.0.1\nPort: 18899\nAuthenticated Proxy Enabled: 0\n");
        assert_eq!(
            web,
            ProxyEndpoint {
                enabled: false,
                host: "127.0.0.1".into(),
                port: 18899
            }
        );
        let off = summarise_services(
            vec![ServiceProxy {
                service: "Wi-Fi".into(),
                web: web.clone(),
                secure: web.clone(),
            }],
            None,
        );
        assert!(!off.enabled());
        assert_eq!(off.server, None);
        // Web and secure web may differ; both are kept for the restore, and HTTPS is shown.
        let secure = parse_endpoint("Enabled: Yes\nServer: 10.0.0.2\nPort: 3128\n");
        let on = summarise_services(
            vec![
                ServiceProxy {
                    service: "Thunderbolt Bridge".into(),
                    ..Default::default()
                },
                ServiceProxy {
                    service: "Wi-Fi".into(),
                    web: web.clone(),
                    secure,
                },
            ],
            Some(String::new()),
        );
        assert!(on.enabled());
        assert_eq!(on.server.as_deref(), Some("10.0.0.2:3128"));
        assert_ne!(on.flags & PROXY_FLAG_AUTO_URL, 0);
        assert_eq!(on.services[1].web, web);
        // Older restore records without per-service details still load.
        let old: ProxySnapshot =
            serde_json::from_str(r#"{"flags":1,"server":null,"bypass":null,"auto_config_url":null}"#)
                .unwrap();
        assert!(old.services.is_empty());
    }

    /// Captured from a Mac whose primary service is a TUN/VPN: the Wi-Fi proxy is listed only under
    /// `__SCOPED__`, so apps do not use it.
    const SCUTIL_VPN_PRIMARY: &str = "<dictionary> {\n  FTPPassive : 1\n  __SCOPED__ : <dictionary> {\n    en0 : <dictionary> {\n      ExceptionsList : <array> {\n        0 : *.local\n        1 : 169.254/16\n      }\n      FTPPassive : 1\n      HTTPEnable : 1\n      HTTPPort : 5555\n      HTTPProxy : 127.0.0.1\n      HTTPSEnable : 1\n      HTTPSPort : 5555\n      HTTPSProxy : 127.0.0.1\n    }\n    utun6 : <dictionary> {\n      FTPPassive : 1\n    }\n  }\n}\n";

    const SCUTIL_IN_EFFECT: &str = "<dictionary> {\n  ExceptionsList : <array> {\n    0 : *.local\n    1 : 169.254/16\n  }\n  FTPPassive : 1\n  HTTPEnable : 1\n  HTTPPort : 5555\n  HTTPProxy : 127.0.0.1\n  HTTPSEnable : 1\n  HTTPSPort : 5555\n  HTTPSProxy : 127.0.0.1\n  __SCOPED__ : <dictionary> {\n    en0 : <dictionary> {\n      HTTPSEnable : 1\n      HTTPSPort : 7777\n      HTTPSProxy : 10.0.0.1\n    }\n  }\n}\n";

    #[test]
    fn only_the_global_https_proxy_counts_as_effective() {
        assert_eq!(parse_effective_https_proxy(SCUTIL_VPN_PRIMARY), None);
        assert_eq!(
            parse_effective_https_proxy(SCUTIL_IN_EFFECT).as_deref(),
            Some("127.0.0.1:5555")
        );
        assert_eq!(
            parse_effective_https_proxy("<dictionary> {\n  HTTPSEnable : 0\n  HTTPSPort : 1\n  HTTPSProxy : x\n}\n"),
            None
        );
        // Nested values never count, wherever the nested block appears.
        let scoped_first = "<dictionary> {\n  __SCOPED__ : <dictionary> {\n    en0 : <dictionary> {\n      HTTPSEnable : 1\n      HTTPSPort : 7777\n      HTTPSProxy : 10.0.0.1\n    }\n  }\n  HTTPSEnable : 1\n  HTTPSPort : 5555\n  HTTPSProxy : 127.0.0.1\n}\n";
        assert_eq!(
            parse_effective_https_proxy(scoped_first).as_deref(),
            Some("127.0.0.1:5555")
        );
    }

    #[test]
    fn only_endpoints_still_pointing_at_our_proxy_are_restored() {
        let ours = ProxyEndpoint {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 5555,
        };
        let recorded = ProxyEndpoint {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 18899,
        };
        let other_tool = ProxyEndpoint {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 7890,
        };
        let switched_off = ProxyEndpoint {
            enabled: false,
            ..ours.clone()
        };
        let ours_text = Some("127.0.0.1:5555");
        assert!(endpoint_needs_restore(&ours, &recorded, ours_text));
        assert!(!endpoint_needs_restore(&other_tool, &recorded, ours_text));
        assert!(!endpoint_needs_restore(&switched_off, &recorded, ours_text));
        assert!(!endpoint_needs_restore(&recorded, &recorded, ours_text));
        // Without `ours`, a recorded snapshot is applied as it is.
        assert!(endpoint_needs_restore(&other_tool, &recorded, None));
        assert!(!endpoint_needs_restore(&recorded, &recorded, None));
        assert_eq!(split_server("127.0.0.1:5555"), Some(("127.0.0.1".to_string(), 5555)));
        assert_eq!(split_server("127.0.0.1"), None);
    }

    /// Changes the real system proxy for about three seconds, then checks that every service is
    /// back exactly as before. Run by hand: `cargo test -p recorder real_macos_takeover -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "Changes the real system proxy briefly"]
    fn real_macos_takeover_is_confirmed_or_refused_and_always_restored() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = temp_dir();
        let before = SystemProxyStore.read().unwrap();
        let result = take_over_and_confirm(&dir, port);
        println!("takeover: {:?}", result.as_ref().map(|_| "in effect"));
        if result.is_ok() {
            restore_proxy(&SystemProxyStore, &dir).unwrap();
        }
        assert!(!dir.join(RESTORE_FILE).exists());
        let routing = |snapshot: &ProxySnapshot| -> Vec<_> {
            snapshot
                .services
                .iter()
                .map(|s| (s.service.clone(), in_effect(&s.web), in_effect(&s.secure)))
                .collect()
        };
        let after = SystemProxyStore.read().unwrap();
        assert_eq!(routing(&after), routing(&before));
        assert_eq!(after.server, before.server);
        assert_eq!(after.flags, before.flags);
    }

    #[test]
    fn ca_is_reused_and_constrained() {
        let dir = temp_dir();
        let suffixes: Vec<String> = MITM_SUFFIXES.iter().map(|s| s.to_string()).collect();
        let first = load_or_create_ca(&dir, &suffixes).unwrap();
        let second = load_or_create_ca(&dir, &suffixes).unwrap();
        assert_eq!(first.serial_hex, second.serial_hex);
        use base64::Engine;
        let body: String = first.cert_pem.lines().filter(|line| !line.starts_with("-----")).collect();
        let der = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        // The nameConstraints extension (OID 2.5.29.30) must list every permitted domain.
        assert!(der.windows(3).any(|w| w == [0x55, 0x1d, 0x1e]));
        for suffix in MITM_SUFFIXES {
            assert!(der.windows(suffix.len()).any(|w| w == suffix.as_bytes()), "{suffix}");
        }
        authority(&first).unwrap();
    }

    #[test]
    fn ca_lacking_a_domain_is_replaced_and_the_old_one_retired() {
        let dir = temp_dir();
        let old = load_or_create_ca(&dir, &["qq.com".to_string()]).unwrap();
        let new = load_or_create_ca(&dir, &["qq.com".to_string(), "wxlivecdn.com".to_string()])
            .unwrap();
        assert_ne!(old.serial_hex, new.serial_hex);
        let meta: CaMeta =
            serde_json::from_str(&std::fs::read_to_string(dir.join("ca.json")).unwrap()).unwrap();
        assert_eq!(meta.retired_serials, vec![old.serial_hex]);
        assert_eq!(meta.serial_hex, new.serial_hex);
    }

    /// `certutil -dump` is read-only and prints serials in the format the trust-store check expects.
    #[cfg(windows)]
    #[test]
    fn certutil_prints_the_serial_format_we_search_for() {
        let dir = temp_dir();
        let ca = load_or_create_ca(&dir, &["qq.com".to_string()]).unwrap();
        let output = certutil(&["-dump", &dir.join("ca.crt").to_string_lossy()]).unwrap();
        let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        assert!(text.contains(&ca.serial_hex), "{text}");
    }

    const GZIP_ROOM_JSON: &str = "H4sIAAAAAAAC/6tWSkksSVSyUqhWystMzs5LzE0FcpTSqzILdIvy83OVdBSUcjLLUkMyS3IQMiVgHlCquKQoNTEXJJ5RUlJQbKWvn5OfnJiTkV9cog/SFm8EA/EVemk5ZUq1tQBDTOD2cgAAAA==";

    #[test]
    fn host_name_is_read_from_the_room_page_title() {
        assert_eq!(host_from_document_name("新华网的直播").as_deref(), Some("新华网"));
        assert_eq!(host_from_document_name("的直播"), None);
        assert_eq!(host_from_document_name("视频号"), None);
    }

    #[test]
    fn only_recently_seen_streams_take_the_visible_room_name() {
        let shared = Shared::new(vec![]);
        shared.note_stream("https://a/live_111111111.flv", "a", "flv");
        shared.note_stream("https://a/live_222222222.flv", "a", "flv");
        shared.findings.lock().unwrap().candidates[1].seen_at_ms -= 60_000;
        shared.name_recent_streams("host");
        let findings = shared.findings.lock().unwrap();
        assert_eq!(findings.rooms["live_222222222"].name, "host");
        assert!(!findings.rooms.contains_key("live_111111111"));
    }

    #[test]
    fn room_details_come_only_from_objects_mentioning_the_stream() {
        let rooms = extract_rooms(
            r#"{"comments":[{"nickname":"viewer","text":"hi"}],
                "object":{"nickname":"host","liveInfo":{"title":"topic-a"},
                  "desc":"about","streams":[{"url":"https://a/live_123456789_x.flv?q=1"}]}}"#,
        );
        assert_eq!(
            rooms,
            vec![(
                "live_123456789".to_string(),
                RoomInfo {
                    name: "host".into(),
                    title: Some("about".into())
                }
            )]
        );
        // A response that does not mention a stream yields nothing, even with a nickname.
        assert!(extract_rooms(r#"{"nickname":"viewer"}"#).is_empty());
        assert!(extract_rooms("live_123456789 is not json").is_empty());
    }

    #[tokio::test]
    async fn room_details_are_read_and_responses_still_reach_the_client() {
        install_crypto_provider();
        let fixture = fixture().await;
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", fixture.proxy_addr)).unwrap())
            .add_root_certificate(
                reqwest::Certificate::from_pem(fixture.ca.cert_pem.as_bytes()).unwrap(),
            )
            .build()
            .unwrap();
        let port = fixture.upstream_port;
        let plain = client
            .get(format!("https://localhost:{port}/api/room?x=1"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(plain.contains("plain-room"));
        let unzipped = client
            .get(format!("https://localhost:{port}/api/gz"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(unzipped.contains("gzip-room"), "{unzipped}");
        let findings = fixture.shared.findings.lock().unwrap();
        assert_eq!(findings.rooms["live_111111111"].name, "plain-room");
        assert_eq!(findings.rooms["live_222222222"].name, "gzip-room");
        assert_eq!(findings.rooms["live_222222222"].title.as_deref(), Some("gzip-title"));
        assert!(findings.json_endpoints.iter().any(|e| e.ends_with("/api/room")));
    }

    struct Fixture {
        proxy_addr: std::net::SocketAddr,
        upstream_port: u16,
        shared: Arc<Shared>,
        ca: LocalCa,
        _shutdown: oneshot::Sender<()>,
    }

    /// The real proxy and CA, constrained to `localhost`. Upstream is plain TCP via a test connector;
    /// the client side of the connection is real TLS verified by the OS against the constrained CA.
    async fn fixture() -> Fixture {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = upstream.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buffer = [0; 4096];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    use base64::Engine;
                    let (content_type, encoding, body): (&str, &str, Vec<u8>) =
                        if request.starts_with("GET /plain") {
                            ("text/html", "", b"FLVDATA".to_vec())
                        } else if request.starts_with("GET /api/gz") {
                            (
                                "application/json",
                                "Content-Encoding: gzip\r\n",
                                base64::engine::general_purpose::STANDARD
                                    .decode(GZIP_ROOM_JSON)
                                    .unwrap(),
                            )
                        } else if request.starts_with("GET /api/room") {
                            (
                                "application/json; charset=utf-8",
                                "",
                                br#"{"comments":[{"nickname":"a-viewer"}],"live":{"nickname":"plain-room","title":"plain-title","url":"https://localhost/live_111111111_x.flv?sig=1"}}"#.to_vec(),
                            )
                        } else {
                            ("video/x-flv", "", b"FLVDATA".to_vec())
                        };
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{encoding}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        let suffixes = vec!["localhost".to_string()];
        let dir = temp_dir();
        let ca = load_or_create_ca(&dir, &suffixes).unwrap();
        let shared = Shared::new(suffixes);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let (shutdown, shutdown_rx) = oneshot::channel::<()>();
        spawn_proxy(
            Proxy::builder()
                .with_listener(listener)
                .with_ca(authority(&ca).unwrap())
                .with_http_connector(connector)
                .with_http_handler(CaptureHandler {
                    shared: shared.clone(),
                    pending: None,
                })
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .build()
                .unwrap(),
        );
        Fixture {
            proxy_addr,
            upstream_port,
            shared,
            ca,
            _shutdown: shutdown,
        }
    }

    #[tokio::test]
    async fn captures_streams_through_real_tls_and_passes_bodies_unchanged() {
        install_crypto_provider();
        let fixture = fixture().await;
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", fixture.proxy_addr)).unwrap())
            .add_root_certificate(
                reqwest::Certificate::from_pem(fixture.ca.cert_pem.as_bytes()).unwrap(),
            )
            .build()
            .unwrap();
        let port = fixture.upstream_port;

        // Matched by path, signature kept verbatim.
        let by_path = format!("https://localhost:{port}/live/a.flv?sig=A%2FB&t=1");
        assert_eq!(client.get(&by_path).send().await.unwrap().text().await.unwrap(), "FLVDATA");
        // Matched by content type only.
        let by_type = format!("https://localhost:{port}/stream?token=x");
        assert_eq!(client.get(&by_type).send().await.unwrap().text().await.unwrap(), "FLVDATA");
        // Ordinary traffic passes through and is not listed.
        let plain = format!("https://localhost:{port}/plain");
        assert_eq!(client.get(&plain).send().await.unwrap().text().await.unwrap(), "FLVDATA");

        let findings = fixture.shared.findings.lock().unwrap();
        let urls: Vec<&str> = findings.candidates.iter().map(|c| c.url.as_str()).collect();
        assert!(urls.contains(&by_path.as_str()), "{urls:?}");
        assert!(urls.contains(&by_type.as_str()), "{urls:?}");
        assert_eq!(urls.len(), 2, "{urls:?}");
    }

    #[tokio::test]
    async fn other_hosts_are_tunnelled_without_being_read() {
        install_crypto_provider();
        let fixture = fixture().await;
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = echo.accept().await.unwrap();
            let (mut reader, mut writer) = socket.split();
            let _ = tokio::io::copy(&mut reader, &mut writer).await;
        });
        let mut client = tokio::net::TcpStream::connect(fixture.proxy_addr).await.unwrap();
        client
            .write_all(format!("CONNECT {echo_addr} HTTP/1.1\r\nHost: {echo_addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut buffer = [0; 256];
        let read = client.read(&mut buffer).await.unwrap();
        assert!(String::from_utf8_lossy(&buffer[..read]).contains("200"));
        client.write_all(b"opaque-bytes").await.unwrap();
        // The tunnel may deliver the echo in several pieces.
        let mut echoed = [0; 12];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"opaque-bytes");
        let findings = fixture.shared.findings.lock().unwrap();
        assert!(findings.candidates.is_empty());
        assert!(findings.hosts.contains("127.0.0.1"));
    }
}
