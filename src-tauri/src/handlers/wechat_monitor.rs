//! Watches the video-channel accounts that are rooms in the room list, through each account's own
//! profile page in WeChat, which is the only place their live status is visible. When one goes
//! live, it opens the live room, captures the stream, and hands it to that room's recorder, so a
//! tracked account records every broadcast like rooms on other platforms do.
//!
//! Everything goes through accessibility calls on tabs the user opened: the mouse is not moved and
//! no WeChat API is called. Each account is checked at most once every `MIN_INTERVAL_MINS`.
use crate::recorder_manager::WechatTrackedRoom;
use crate::state::State;
use recorder::platforms::wechat::capture::{CaptureStatus, StreamCandidate, WechatCapture};
use recorder::platforms::wechat::source::{valid_account_id, WechatSource};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};

const MIN_INTERVAL_MINS: u64 = 5;
const STREAM_WAIT: Duration = Duration::from_secs(60);
const QUALITY_SETTLE: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_secs(10);

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

/// Waits for a stream seen after `since_ms`, then takes the highest listed quality.
async fn wait_for_stream(capture: &WechatCapture, since_ms: i64) -> Option<StreamCandidate> {
    let fresh = |status: CaptureStatus| {
        status
            .candidates
            .into_iter()
            .filter(|candidate| candidate.seen_at_ms >= since_ms)
            .collect::<Vec<_>>()
    };
    let deadline = tokio::time::Instant::now() + STREAM_WAIT;
    while tokio::time::Instant::now() < deadline {
        if !fresh(capture.status().await.ok()?).is_empty() {
            tokio::time::sleep(QUALITY_SETTLE).await;
            return fresh(capture.status().await.ok()?)
                .into_iter()
                .max_by_key(quality);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    None
}

/// What the tracker last saw for one tracked room.
#[derive(Clone, Default, serde::Serialize)]
pub struct RoomStatus {
    pub room_id: String,
    pub host: String,
    pub account_id: String,
    /// unknown, offline, live, recording or stopped.
    pub live_state: String,
    pub message: String,
    pub last_check_ms: i64,
    pub last_change_ms: i64,
}

#[derive(Clone, serde::Serialize)]
pub struct MonitorStatus {
    pub interval_mins: u64,
    /// The room being checked right now.
    pub checking: Option<String>,
    pub rooms: Vec<RoomStatus>,
}

pub struct WechatMonitor {
    interval_mins: AtomicU64,
    rooms: Mutex<HashMap<String, RoomStatus>>,
    checking: Mutex<Option<String>>,
}

impl Default for WechatMonitor {
    fn default() -> Self {
        Self {
            interval_mins: AtomicU64::new(MIN_INTERVAL_MINS),
            rooms: Mutex::default(),
            checking: Mutex::default(),
        }
    }
}

impl WechatMonitor {
    fn status(&self) -> MonitorStatus {
        let mut rooms: Vec<_> = self.rooms.lock().unwrap().values().cloned().collect();
        rooms.sort_by(|a, b| a.host.cmp(&b.host));
        MonitorStatus {
            interval_mins: self.interval_mins.load(Ordering::Relaxed),
            checking: self.checking.lock().unwrap().clone(),
            rooms,
        }
    }

    /// Keeps one status per tracked room and returns a copy of each.
    fn sync_rooms(&self, rooms: &[WechatTrackedRoom]) -> HashMap<String, RoomStatus> {
        let mut statuses = self.rooms.lock().unwrap();
        statuses.retain(|id, _| rooms.iter().any(|room| &room.room_id == id));
        for room in rooms {
            let status = statuses
                .entry(room.room_id.clone())
                .or_insert_with(|| RoomStatus {
                    room_id: room.room_id.clone(),
                    live_state: "unknown".into(),
                    message: "等待第一次检查".into(),
                    ..Default::default()
                });
            status.host = room.account.name.clone();
            status.account_id = room.account.account_id.clone();
        }
        statuses.clone()
    }

    /// Records a state without counting it as a check, for rooms that are off or recording.
    fn note(&self, room_id: &str, state: &str, message: impl Into<String>) {
        self.update(room_id, state, message, false);
    }

    fn say(&self, room_id: &str, state: &str, message: impl Into<String>) {
        self.update(room_id, state, message, true);
    }

    fn update(&self, room_id: &str, state: &str, message: impl Into<String>, checked: bool) {
        let mut rooms = self.rooms.lock().unwrap();
        let Some(status) = rooms.get_mut(room_id) else {
            return;
        };
        let now = chrono::Utc::now().timestamp_millis();
        if status.live_state != state {
            status.last_change_ms = now;
        }
        status.live_state = state.into();
        status.message = message.into();
        if checked {
            status.last_check_ms = now;
        }
    }
}

/// Opens the account's live room with capture on and returns the stream it plays.
async fn capture_stream(
    capture: &Arc<WechatCapture>,
    host: &str,
    account_id: &str,
    title: Option<String>,
) -> Result<WechatSource, String> {
    // A capture the user started from the dialog is theirs to stop.
    let was_running = capture.status().await?.running;
    let since_ms = chrono::Utc::now().timestamp_millis();
    capture.start().await?;
    let entered = read_search_page(host, account_id, false, true).await;
    let stream = match entered {
        Ok(PageState::Live { .. }) => wait_for_stream(capture, since_ms).await,
        _ => None,
    };
    // Always give the system proxy back before anything else can fail.
    if !was_running {
        capture.stop().await?;
    }
    let stream = stream.ok_or("进入直播间后没有抓到直播流")?;
    let cover = super::wechat_capture::grab_frame(&stream.url).await.ok();
    let extra = serde_json::json!({
        "url": stream.url,
        "title": stream.room_title.or(title).unwrap_or_else(|| format!("{host}的直播")),
        "name": host,
        "cover": cover,
        "captured_stream": true,
    })
    .to_string();
    WechatSource::parse(&extra)
}

async fn check_room(
    app: &AppHandle,
    monitor: &WechatMonitor,
    capture: &Arc<WechatCapture>,
    room: &WechatTrackedRoom,
) {
    let host = &room.account.name;
    let account_id = &room.account.account_id;
    let id = room.room_id.as_str();
    let manager = &app.state::<State>().recorder_manager;
    let page = read_search_page(host, account_id, true, false).await;
    if !matches!(page, Ok(PageState::Live { .. })) {
        manager.set_wechat_live(id, false).await;
    }
    match page {
        Ok(PageState::NoPage) => monitor.say(
            id,
            "unknown",
            format!("无法确认：请在微信里保持“{host}”本人主页的标签页打开"),
        ),
        Ok(PageState::Unknown) => {
            monitor.say(id, "unknown", "无法确认：页面未完成读取或刷新，请检查微信窗口")
        }
        Ok(PageState::AccountNotFound) => {
            monitor.say(id, "unknown", "无法确认：当前页面不是这个账号的主页")
        }
        Ok(PageState::Ambiguous) => monitor.say(
            id,
            "unknown",
            "无法确认：存在多个同名标签页，请在微信中只保留一个",
        ),
        Ok(PageState::Offline) => monitor.say(id, "offline", "未开播"),
        Ok(PageState::Live { title }) => {
            manager.set_wechat_live(id, true).await;
            let ca_installed = capture
                .status()
                .await
                .map(|status| status.ca_installed)
                .unwrap_or(false);
            if !ca_installed {
                monitor.say(id, "live", "直播中；安装抓取证书后才能自动录制");
                return;
            }
            monitor.say(id, "live", "已开播，正在进入直播间抓流…");
            match capture_stream(capture, host, account_id, title).await {
                Ok(source) => {
                    if manager.offer_wechat_stream(id, source).await {
                        monitor.say(id, "recording", "正在录制");
                    } else {
                        monitor.say(id, "live", "直播中，但这个直播间已关闭或正在录制");
                    }
                }
                Err(error) => monitor.say(id, "live", format!("直播中，但没有录上：{error}")),
            }
        }
        Err(error) => monitor.say(id, "unknown", error),
    }
}

/// Runs for the life of the app. Checks are one at a time, since each one drives the WeChat window.
async fn tracker_loop(app: AppHandle, monitor: Arc<WechatMonitor>) {
    let capture = app.state::<Arc<WechatCapture>>().inner().clone();
    loop {
        let rooms = app
            .state::<State>()
            .recorder_manager
            .wechat_tracked_rooms()
            .await;
        let statuses = monitor.sync_rooms(&rooms);
        let interval_ms = monitor.interval_mins.load(Ordering::Relaxed) as i64 * 60_000;
        for room in &rooms {
            let status = &statuses[&room.room_id];
            if !room.enabled {
                monitor.note(&room.room_id, "stopped", "已关闭，不检查");
                continue;
            }
            if room.recording {
                monitor.note(&room.room_id, "recording", "正在录制");
                continue;
            }
            let since_check = chrono::Utc::now().timestamp_millis() - status.last_check_ms;
            // A stream that failed at once must not make the tracker reopen the room every tick.
            let ended = status.live_state == "recording" && since_check >= 120_000;
            let due = since_check >= interval_ms;
            // A broadcast that just ended, or a room just turned back on, is checked right away.
            if !(due || ended || status.live_state == "stopped") {
                continue;
            }
            *monitor.checking.lock().unwrap() = Some(room.room_id.clone());
            check_room(&app, &monitor, &capture, room).await;
            *monitor.checking.lock().unwrap() = None;
        }
        tokio::time::sleep(TICK).await;
    }
}

/// Starts watching tracked rooms. Call once, after the app state is ready.
pub fn start_tracker(app: &AppHandle) {
    let monitor = app.state::<Arc<WechatMonitor>>().inner().clone();
    tauri::async_runtime::spawn(tracker_loop(app.clone(), monitor));
}

type MonitorState<'a> = tauri::State<'a, Arc<WechatMonitor>>;

#[tauri::command]
pub async fn wechat_monitor_status(monitor: MonitorState<'_>) -> Result<MonitorStatus, String> {
    Ok(monitor.status())
}

/// Reads the account id from the profile page WeChat has open for `host`.
#[tauri::command]
pub async fn wechat_monitor_bind(host: String) -> Result<MonitorTarget, String> {
    let host = host.trim();
    if host.is_empty() || host.chars().count() > 80 || host.chars().any(char::is_control) {
        return Err("请填写目标账号完整显示名".into());
    }
    let output = run_page_reader(host, "", false, false, true).await?;
    parse_target_output(&output, host)
}

#[tauri::command]
pub async fn wechat_monitor_set_interval(
    monitor: MonitorState<'_>,
    interval_mins: u64,
) -> Result<u64, String> {
    let interval = interval_mins.clamp(MIN_INTERVAL_MINS, 1440);
    monitor.interval_mins.store(interval, Ordering::Relaxed);
    Ok(interval)
}

/// Checks every tracked room on the next pass instead of waiting out the interval.
#[tauri::command]
pub async fn wechat_monitor_check_now(monitor: MonitorState<'_>) -> Result<(), String> {
    for status in monitor.rooms.lock().unwrap().values_mut() {
        status.last_check_ms = 0;
    }
    Ok(())
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
