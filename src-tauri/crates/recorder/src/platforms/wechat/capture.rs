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
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
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
const CA_COMMON_NAME: &str = "PrismReplay Local Capture CA";
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
            },
        );
        findings.candidates.truncate(MAX_CANDIDATES);
    }
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

#[cfg(not(windows))]
fn trust_store_contains(_serial_hex: &str) -> bool {
    false
}

// --- system proxy ------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProxySnapshot {
    pub enabled: bool,
    pub server: Option<String>,
    #[serde(default)]
    pub auto_config_url: Option<String>,
}

pub trait ProxyStore: Send + Sync {
    fn read(&self) -> Result<ProxySnapshot, String>;
    fn write(&self, enabled: bool, server: Option<&str>) -> Result<(), String>;
}

#[derive(Serialize, Deserialize)]
struct RestoreRecord {
    previous: ProxySnapshot,
    ours: String,
}

/// Writes the restore record before touching the proxy, so that a crash can always be repaired.
fn take_over_proxy(store: &dyn ProxyStore, dir: &Path, port: u16) -> Result<ProxySnapshot, String> {
    let previous = store.read()?;
    if previous.auto_config_url.is_some() {
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
    if let Err(error) = store.write(true, Some(&ours)) {
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
        let current = store.read()?;
        if current.enabled && current.server.as_deref() == Some(record.ours.as_str()) {
            store.write(record.previous.enabled, record.previous.server.as_deref())?;
        }
    }
    std::fs::remove_file(&path).map_err(|e| format!("无法清除恢复记录：{e}"))
}

#[cfg(windows)]
pub struct SystemProxyStore;

#[cfg(windows)]
impl ProxyStore for SystemProxyStore {
    fn read(&self) -> Result<ProxySnapshot, String> {
        let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
            .map_err(|e| format!("无法读取系统代理：{e}"))?;
        Ok(ProxySnapshot {
            enabled: key.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0,
            server: key.get_value::<String, _>("ProxyServer").ok(),
            auto_config_url: key
                .get_value::<String, _>("AutoConfigURL")
                .ok()
                .filter(|url| !url.is_empty()),
        })
    }

    fn write(&self, enabled: bool, server: Option<&str>) -> Result<(), String> {
        use windows_sys::Win32::Networking::WinInet::{
            InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
        };
        let err = |e: std::io::Error| format!("无法修改系统代理：{e}");
        let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey_with_flags(
                r"Software\Microsoft\Windows\CurrentVersion\Internet Settings",
                winreg::enums::KEY_READ | winreg::enums::KEY_SET_VALUE,
            )
            .map_err(err)?;
        match server {
            Some(server) => key.set_value("ProxyServer", &server).map_err(err)?,
            None => {
                let _ = key.delete_value("ProxyServer");
            }
        }
        key.set_value("ProxyEnable", &u32::from(enabled))
            .map_err(err)?;
        // SAFETY: both options take no buffer; a null handle applies them to the whole process.
        unsafe {
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

/// Run at startup: repairs the proxy if the previous run ended while capturing.
#[cfg(windows)]
pub fn recover_after_crash(dir: &Path) {
    if let Err(error) = restore_proxy(&SystemProxyStore, dir) {
        log::warn!("Failed to restore the system proxy after a previous capture: {error}");
    }
}

#[cfg(not(windows))]
pub fn recover_after_crash(_dir: &Path) {}

// --- service -----------------------------------------------------------------------------------

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
                Some(current && trust_store_contains(&meta.serial_hex))
            })
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or(false);
        let running = self.running.lock().await;
        let findings = self.shared.findings.lock().unwrap();
        Ok(CaptureStatus {
            supported: cfg!(windows),
            ca_installed,
            running: running.is_some(),
            port: running.as_ref().map(|item| item.port),
            remaining_secs: running.as_ref().map_or(0, |item| {
                CAPTURE_TIMEOUT
                    .saturating_sub(item.started.elapsed())
                    .as_secs()
            }),
            replaced_existing_proxy: running.as_ref().is_some_and(|i| i.replaced_existing_proxy),
            candidates: findings.candidates.clone(),
            hosts: findings.hosts.iter().cloned().collect(),
        })
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

    #[cfg(windows)]
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }
        let ca = self.ca()?;
        if !trust_store_contains(&ca.serial_hex) {
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
        let previous = match take_over_proxy(&SystemProxyStore, &self.dir, port) {
            Ok(previous) => previous,
            Err(error) => {
                let _ = shutdown.send(());
                return Err(error);
            }
        };
        let this = self.clone();
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep(CAPTURE_TIMEOUT).await;
            if let Err(error) = this.stop().await {
                log::warn!("Failed to stop WeChat capture after timeout: {error}");
            }
        });
        *running = Some(Running {
            port,
            shutdown,
            started: std::time::Instant::now(),
            replaced_existing_proxy: previous.enabled,
            watchdog,
        });
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let Some(running) = self.running.lock().await.take() else {
            return Ok(());
        };
        // Restore first: a stopped proxy with the setting still in place would cut off the network.
        #[cfg(windows)]
        let restored = restore_proxy(&SystemProxyStore, &self.dir);
        #[cfg(not(windows))]
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

    #[derive(Default)]
    struct FakeStore(StdMutex<Option<ProxySnapshot>>);

    impl FakeStore {
        fn with(enabled: bool, server: Option<&str>) -> Self {
            Self(StdMutex::new(Some(ProxySnapshot {
                enabled,
                server: server.map(str::to_string),
                auto_config_url: None,
            })))
        }
        fn get(&self) -> ProxySnapshot {
            self.0.lock().unwrap().clone().unwrap()
        }
    }

    impl ProxyStore for FakeStore {
        fn read(&self) -> Result<ProxySnapshot, String> {
            Ok(self.get())
        }
        fn write(&self, enabled: bool, server: Option<&str>) -> Result<(), String> {
            let mut value = self.0.lock().unwrap();
            let auto_config_url = value.as_ref().and_then(|v| v.auto_config_url.clone());
            *value = Some(ProxySnapshot {
                enabled,
                server: server.map(str::to_string),
                auto_config_url,
            });
            Ok(())
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prismreplay-capture-{}", uuid::Uuid::new_v4()));
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
        let store = FakeStore::with(true, Some("127.0.0.1:7897"));
        take_over_proxy(&store, &dir, 5555).unwrap();
        assert_eq!(store.get().server.as_deref(), Some("127.0.0.1:5555"));
        restore_proxy(&store, &dir).unwrap();
        let after = store.get();
        assert!(after.enabled);
        assert_eq!(after.server.as_deref(), Some("127.0.0.1:7897"));
        assert!(!dir.join(RESTORE_FILE).exists());
    }

    #[test]
    fn crash_recovery_restores_a_disabled_proxy() {
        let dir = temp_dir();
        let store = FakeStore::with(false, None);
        take_over_proxy(&store, &dir, 5555).unwrap();
        assert!(store.get().enabled);
        // A new process only has the record on disk.
        restore_proxy(&store, &dir).unwrap();
        assert!(!store.get().enabled);
        assert_eq!(store.get().server, None);
        // Nothing to do on a clean start.
        restore_proxy(&store, &dir).unwrap();
    }

    #[test]
    fn later_proxy_changes_by_other_tools_are_left_alone() {
        let dir = temp_dir();
        let store = FakeStore::with(false, Some("127.0.0.1:7897"));
        take_over_proxy(&store, &dir, 5555).unwrap();
        store.write(true, Some("127.0.0.1:9999")).unwrap();
        restore_proxy(&store, &dir).unwrap();
        assert_eq!(store.get().server.as_deref(), Some("127.0.0.1:9999"));
        assert!(store.get().enabled);
    }

    #[test]
    fn pac_configuration_is_refused_without_changes() {
        let dir = temp_dir();
        let store = FakeStore(StdMutex::new(Some(ProxySnapshot {
            enabled: false,
            server: None,
            auto_config_url: Some("http://pac.example/p.pac".into()),
        })));
        assert!(take_over_proxy(&store, &dir, 5555).is_err());
        assert!(!store.get().enabled);
        assert!(!dir.join(RESTORE_FILE).exists());
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
                    let content_type = if request.starts_with("GET /plain") {
                        "text/html"
                    } else {
                        "video/x-flv"
                    };
                    let body = "FLVDATA";
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = socket.write_all(response.as_bytes()).await;
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
            let mut buffer = [0; 64];
            let read = socket.read(&mut buffer).await.unwrap();
            socket.write_all(&buffer[..read]).await.unwrap();
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
        let read = client.read(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..read], b"opaque-bytes");
        let findings = fixture.shared.findings.lock().unwrap();
        assert!(findings.candidates.is_empty());
        assert!(findings.hosts.contains("127.0.0.1"));
    }
}
