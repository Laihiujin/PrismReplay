//! Watches one video-channel host through WeChat's own search page, which is the only place their
//! live status is visible, and records them when they go live.
//!
//! Everything goes through accessibility calls on a tab the user opened: the mouse is not moved and
//! no WeChat API is called. Checks are spaced at least `MIN_INTERVAL_MINS` apart to stay gentle.
use crate::state::State;
use recorder::platforms::wechat::capture::{CaptureStatus, StreamCandidate, WechatCapture};
use recorder::platforms::wechat::source::WechatSource;
use recorder::platforms::PlatformType;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::task::JoinHandle;

const MIN_INTERVAL_MINS: u64 = 5;
const STREAM_WAIT: Duration = Duration::from_secs(60);
const QUALITY_SETTLE: Duration = Duration::from_secs(5);

/// Prints one line: NOPAGE, or LIVE <item name> for each live result, or nothing if none is live.
#[cfg(windows)]
const SEARCH_PAGE_SCRIPT: &str = include_str!("wechat_monitor_search.ps1");

#[derive(Debug, PartialEq)]
enum PageState {
    NoPage,
    Unknown,
    AccountNotFound,
    Ambiguous,
    Offline,
    Live { title: Option<String> },
}

// Explicitly scoped records only. Empty output, missing accounts and ambiguous
// names must never silently become "offline".
fn parse_page_output(output: &str, host: &str) -> PageState {
    let mut result = None;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let next = match line {
            "NOPAGE" => PageState::NoPage,
            "NOTFOUND" => PageState::AccountNotFound,
            "AMBIGUOUS" => PageState::Ambiguous,
            "UNREADABLE" | "REFRESH_FAILED" => PageState::Unknown,
            _ => {
                let parts: Vec<_> = line.splitn(3, '\t').collect();
                if parts.len() >= 2 && parts[1] == host {
                    match parts[0] {
                        "OFFLINE" if parts.len() == 2 => PageState::Offline,
                        "LIVE" => PageState::Live {
                            title: parts
                                .get(2)
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty()),
                        },
                        _ => PageState::Unknown,
                    }
                } else {
                    PageState::Unknown
                }
            }
        };
        if result.is_some() {
            return PageState::Ambiguous;
        }
        result = Some(next);
    }
    result.unwrap_or(PageState::Unknown)
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct MonitorTarget {
    pub host: String,
    pub account_id: String,
}

fn valid_account_id(value: &str) -> bool {
    value.len() <= 256
        && value.strip_suffix("@finder").is_some_and(|prefix| {
            !prefix.is_empty()
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
}

fn parse_target_output(output: &str, expected_host: &str) -> Result<MonitorTarget, String> {
    let lines: Vec<_> = output.lines().filter(|s| !s.trim().is_empty()).collect();
    if lines.len() != 1 {
        return Err("无法确认唯一账号，请只打开一个目标主页".into());
    }
    let parts: Vec<_> = lines[0].trim().splitn(3, '\t').collect();
    if parts.len() != 3
        || parts[0] != "PROFILE"
        || !valid_account_id(parts[1])
        || parts[2] != expected_host
    {
        return Err("请在微信打开目标本人的视频号主页，确认完整显示名一致后再绑定".into());
    }
    Ok(MonitorTarget {
        host: parts[2].into(),
        account_id: parts[1].into(),
    })
}

#[cfg(windows)]
async fn run_page_reader(
    host: &str,
    account_id: &str,
    refresh: bool,
    enter: bool,
    bind: bool,
) -> Result<String, String> {
    let mut command = tokio::process::Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SEARCH_PAGE_SCRIPT,
        ])
        .env("WECHAT_MONITOR_KEYWORD", host)
        .env("WECHAT_MONITOR_ACCOUNT_ID", account_id)
        .env("WECHAT_MONITOR_BIND", if bind { "1" } else { "0" })
        .env("WECHAT_MONITOR_REFRESH", if refresh { "1" } else { "0" })
        .env("WECHAT_MONITOR_ENTER", if enter { "1" } else { "0" })
        .creation_flags(0x0800_0000)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "读取微信页面超时，当前状态无法确认".to_string())?
        .map_err(|_| "无法启动微信页面读取，当前状态无法确认".to_string())?;
    if !output.status.success() {
        return Err("无法读取电脑版微信，当前状态无法确认".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(windows))]
async fn run_page_reader(
    _host: &str,
    _account_id: &str,
    _refresh: bool,
    _enter: bool,
    _bind: bool,
) -> Result<String, String> {
    Err("只支持 Windows".into())
}

async fn read_search_page(
    host: &str,
    account_id: &str,
    refresh: bool,
    enter: bool,
) -> Result<PageState, String> {
    let output = run_page_reader(host, account_id, refresh, enter, false).await?;
    Ok(parse_page_output(&output, account_id))
}

fn quality(candidate: &StreamCandidate) -> u32 {
    candidate
        .url
        .split(['?', '&'])
        .find_map(|part| part.strip_prefix("q="))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// Waits for the stream that opening the room produces, then takes the highest listed quality.
async fn wait_for_stream(capture: &WechatCapture) -> Option<StreamCandidate> {
    let deadline = tokio::time::Instant::now() + STREAM_WAIT;
    while tokio::time::Instant::now() < deadline {
        let status: CaptureStatus = capture.status().await.ok()?;
        if !status.candidates.is_empty() {
            tokio::time::sleep(QUALITY_SETTLE).await;
            let status = capture.status().await.ok()?;
            return status.candidates.into_iter().max_by_key(quality);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    None
}

#[derive(Clone, Default, serde::Serialize)]
pub struct MonitorStatus {
    pub running: bool,
    pub host: String,
    pub account_id: String,
    pub interval_mins: u64,
    pub message: String,
    pub last_check_ms: i64,
    pub last_change_ms: i64,
    pub live_state: String,
    pub auto_record: bool,
}

#[derive(Default)]
pub struct WechatMonitor {
    status: Mutex<MonitorStatus>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl WechatMonitor {
    fn say_state(&self, state: &str, message: impl Into<String>) {
        let mut status = self.status.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        if status.live_state != state {
            status.last_change_ms = now;
        }
        status.live_state = state.into();
        status.message = message.into();
        status.last_check_ms = now;
    }
}

/// Starts capture, opens the room, and adds the stream. Returns the new recorder's room ID.
async fn record_live_room(
    app: &AppHandle,
    capture: &Arc<WechatCapture>,
    host: &str,
    account_id: &str,
    title: Option<String>,
) -> Result<String, String> {
    capture.start().await?;
    let entered = read_search_page(host, account_id, false, true).await;
    let stream = match entered {
        Ok(PageState::Live { .. }) => wait_for_stream(capture).await,
        _ => None,
    };
    // Always give the system proxy back before anything else can fail.
    capture.stop().await?;
    let stream = stream.ok_or("进入直播间后没有抓到直播流")?;
    let cover = super::wechat_capture::grab_frame(&stream.url).await.ok();
    let extra = serde_json::json!({
        "url": stream.url,
        "title": title.unwrap_or_else(|| format!("{host}的直播")),
        "name": host,
        "cover": cover,
        "captured_stream": true,
    })
    .to_string();
    let room_id = WechatSource::parse(&extra)?.room_id();
    let state = app.state::<State>();
    crate::handlers::recorder::add_recorder(state, "wechat".to_string(), String::new(), extra)
        .await
        .map_err(|e| format!("添加录制失败：{e}"))?;
    Ok(room_id)
}

async fn wait_until_recording_ends(app: &AppHandle, room_id: &str) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let state = app.state::<State>();
        let info = state
            .recorder_manager
            .get_recorder_info(PlatformType::Wechat, room_id)
            .await;
        if !info.is_some_and(|info| info.recording) {
            return;
        }
    }
}

async fn monitor_loop(
    app: AppHandle,
    monitor: Arc<WechatMonitor>,
    host: String,
    account_id: String,
    interval: u64,
    auto_record: bool,
) {
    let capture = app.state::<Arc<WechatCapture>>().inner().clone();
    loop {
        let page = read_search_page(&host, &account_id, true, false).await;
        match page {
            Ok(PageState::NoPage) => monitor.say_state(
                "unknown",
                format!("无法确认：请保持“{host}”本人主页打开；不会改为跟踪其他账号"),
            ),
            Ok(PageState::Unknown) => {
                monitor.say_state("unknown", "无法确认：页面未完成读取或刷新，请检查微信窗口")
            }
            Ok(PageState::AccountNotFound) => {
                monitor.say_state("unknown", "无法确认：当前页面不是已绑定的账号主页")
            }
            Ok(PageState::Ambiguous) => monitor.say_state(
                "unknown",
                "无法确认：存在多个同名结果，请在微信中核对目标账号",
            ),
            Ok(PageState::Offline) => {
                monitor.say_state("offline", "已核对绑定的账号 ID，当前主页未检测到开播标记")
            }
            Ok(PageState::Live { title }) => {
                monitor.say_state("live", "已检测到目标账号直播中");
                if auto_record {
                    monitor.say_state("live", "已开播，正在进入目标直播间并抓取…");
                    match record_live_room(&app, &capture, &host, &account_id, title).await {
                        Ok(room_id) => {
                            monitor.say_state("recording", "正在录制目标直播间");
                            wait_until_recording_ends(&app, &room_id).await;
                            monitor.say_state(
                                "unknown",
                                "录制结束，请返回已绑定的账号主页以继续状态检查",
                            );
                        }
                        Err(error) => {
                            monitor.say_state("live", format!("已检测到开播，但没有录上：{error}"))
                        }
                    }
                }
            }
            Err(error) => monitor.say_state("unknown", error),
        }
        tokio::time::sleep(Duration::from_secs(interval * 60)).await;
    }
}

type MonitorState<'a> = tauri::State<'a, Arc<WechatMonitor>>;

#[tauri::command]
pub async fn wechat_monitor_status(monitor: MonitorState<'_>) -> Result<MonitorStatus, String> {
    Ok(monitor.status.lock().unwrap().clone())
}

#[tauri::command]
pub async fn wechat_monitor_bind(
    monitor: MonitorState<'_>,
    host: String,
) -> Result<MonitorTarget, String> {
    let host = host.trim();
    if host.is_empty() || host.chars().count() > 80 || host.chars().any(char::is_control) {
        return Err("请填写目标账号完整显示名".into());
    }
    if monitor.status.lock().unwrap().running {
        return Err("请先停止监控再更换绑定账号".into());
    }
    let output = run_page_reader(host, "", false, false, true).await?;
    let target = parse_target_output(&output, host)?;
    let mut status = monitor.status.lock().unwrap();
    if status.running {
        return Err("监控已经启动，请先停止".into());
    }
    status.host = target.host.clone();
    status.account_id = target.account_id.clone();
    status.live_state = "unknown".into();
    status.message = "已绑定本人主页账号 ID，尚未开始监控".into();
    Ok(target)
}

#[tauri::command]
pub async fn wechat_monitor_start(
    app: AppHandle,
    monitor: MonitorState<'_>,
    host: String,
    account_id: Option<String>,
    interval_mins: u64,
    auto_record: Option<bool>,
) -> Result<(), String> {
    let host = host.trim().to_string();
    if host.is_empty() || host.chars().count() > 80 || host.chars().any(char::is_control) {
        return Err("请填写目标账号的完整显示名（1–80 字，不含控制字符）".into());
    }
    let account_id = account_id
        .filter(|value| valid_account_id(value))
        .ok_or_else(|| "请先绑定微信当前打开的本人主页".to_string())?;
    {
        let bound = monitor.status.lock().unwrap();
        if bound.account_id != account_id || bound.host != host {
            return Err("账号或名字已变化，请重新绑定本人主页".into());
        }
    }
    let auto_record = auto_record.unwrap_or(false);
    if auto_record {
        let capture = app.state::<Arc<WechatCapture>>();
        if !capture.status().await?.ca_installed {
            return Err("自动录制需先由您确认安装抓取证书；仅状态监控不需要证书".into());
        }
    }
    let interval = interval_mins.clamp(MIN_INTERVAL_MINS, 1440);
    let monitor = monitor.inner().clone();
    let previous_auto_record = monitor.status.lock().unwrap().auto_record;
    if let Some(task) = monitor.task.lock().unwrap().take() {
        task.abort();
    }
    if previous_auto_record {
        app.state::<Arc<WechatCapture>>().stop().await?;
    }
    *monitor.status.lock().unwrap() = MonitorStatus {
        running: true,
        host: host.clone(),
        account_id: account_id.clone(),
        interval_mins: interval,
        message: "正在检查…".into(),
        last_check_ms: 0,
        last_change_ms: 0,
        live_state: "unknown".into(),
        auto_record,
    };
    let task = tokio::spawn(monitor_loop(
        app,
        monitor.clone(),
        host,
        account_id,
        interval,
        auto_record,
    ));
    *monitor.task.lock().unwrap() = Some(task);
    Ok(())
}

#[tauri::command]
pub async fn wechat_monitor_stop(
    monitor: MonitorState<'_>,
    capture: tauri::State<'_, Arc<WechatCapture>>,
) -> Result<(), String> {
    if let Some(task) = monitor.task.lock().unwrap().take() {
        task.abort();
    }
    let auto_record = {
        let mut status = monitor.status.lock().unwrap();
        status.running = false;
        status.live_state = "stopped".into();
        status.message = "已停止".into();
        status.auto_record
    };
    // A check aborted mid-capture must not leave the system proxy pointing at us.
    if auto_record {
        capture.stop().await
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_target_live() {
        assert_eq!(
            parse_page_output("LIVE\tperson@finder\t测试直播", "person@finder"),
            PageState::Live {
                title: Some("测试直播".into())
            }
        );
    }
    #[test]
    fn another_account_cannot_report_target_live() {
        assert_eq!(
            parse_page_output("LIVE\tstudio@finder\t测试直播", "person@finder"),
            PageState::Unknown
        );
        assert_eq!(
            parse_page_output("OFFLINE\tstudio@finder", "person@finder"),
            PageState::Unknown
        );
    }
    #[test]
    fn empty_output_is_unknown_not_offline() {
        assert_eq!(parse_page_output("", "陈赫"), PageState::Unknown);
    }
    #[test]
    fn missing_page_is_distinct_from_offline() {
        assert_eq!(parse_page_output("NOPAGE", "陈赫"), PageState::NoPage);
    }
    #[test]
    fn account_must_be_observed_before_offline() {
        assert_eq!(
            parse_page_output("OFFLINE\tperson@finder", "person@finder"),
            PageState::Offline
        );
    }
    #[test]
    fn missing_account_is_not_offline() {
        assert_eq!(
            parse_page_output("NOTFOUND", "陈赫"),
            PageState::AccountNotFound
        );
    }
    #[test]
    fn ambiguous_names_are_not_live() {
        assert_eq!(parse_page_output("AMBIGUOUS", "陈赫"), PageState::Ambiguous);
    }
    #[test]
    fn duplicate_records_are_ambiguous() {
        assert_eq!(
            parse_page_output(
                "LIVE\tperson@finder\t一\nLIVE\tperson@finder\t二",
                "person@finder"
            ),
            PageState::Ambiguous
        );
    }
    #[test]
    fn refresh_failure_is_not_offline() {
        assert_eq!(
            parse_page_output("REFRESH_FAILED", "陈赫"),
            PageState::Unknown
        );
    }
    #[test]
    fn unknown_protocol_is_not_offline() {
        assert_eq!(parse_page_output("loading...", "陈赫"), PageState::Unknown);
    }
    #[test]
    fn binds_only_the_confirmed_profile_identity() {
        let target = parse_target_output("PROFILE\tperson@finder\t陈赫", "陈赫").unwrap();
        assert_eq!(target.account_id, "person@finder");
        assert!(parse_target_output("PROFILE\tstudio@finder\t陈赫工作室", "陈赫").is_err());
    }
    #[test]
    fn rejects_session_urls_instead_of_saving_them() {
        assert!(!valid_account_id(
            "https://channels.weixin.qq.com/web/pages/profile?exportkey=secret"
        ));
        assert!(!valid_account_id("person@finder\tsecret"));
        assert!(valid_account_id("v2_060000test@finder"));
    }
}
