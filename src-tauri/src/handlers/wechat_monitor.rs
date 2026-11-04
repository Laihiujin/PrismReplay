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
const SEARCH_PAGE_SCRIPT: &str = r#"
[Console]::OutputEncoding=[Text.Encoding]::UTF8
Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
$keyword = $env:WECHAT_MONITOR_KEYWORD
$pageName = $keyword + ' - ' + [char]0x641C + [char]0x4E00 + [char]0x641C
$searchLabel = [string][char]0x641C + [char]0x7D22
$liveLabel = [string][char]0x76F4 + [char]0x64AD + [char]0x4E2D
function ByType($type) { New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty,$type) }
$descendants = [Windows.Automation.TreeScope]::Descendants
$page = $null
foreach ($process in Get-Process Weixin -ErrorAction Stop) {
  if ($process.MainWindowHandle -eq 0) { continue }
  $root = [Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
  foreach ($document in $root.FindAll($descendants,(ByType ([Windows.Automation.ControlType]::Document)))) {
    if ($document.Current.Name -eq $pageName) { $page = $document }
  }
}
if ($page -eq $null) { 'NOPAGE'; exit }
function LiveItems { foreach ($item in $page.FindAll($descendants,(ByType ([Windows.Automation.ControlType]::ListItem)))) { if ($item.Current.Name.StartsWith($liveLabel) -and $item.Current.Name.TrimEnd().EndsWith($keyword)) { $item } } }
if ($env:WECHAT_MONITOR_REFRESH -eq '1') {
  foreach ($button in $page.FindAll($descendants,(ByType ([Windows.Automation.ControlType]::Button)))) {
    if ($button.Current.Name -eq $searchLabel) { $button.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Seconds 3; break }
  }
}
foreach ($item in LiveItems) {
  'LIVE ' + $item.Current.Name
  if ($env:WECHAT_MONITOR_ENTER -eq '1') { $item.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke() }
  break
}
"#;

#[derive(Debug, PartialEq)]
enum PageState {
    NoPage,
    Offline,
    Live { title: Option<String> },
}

/// A result reads "直播中 , 6.6万人看过 <topic> <host>".
fn parse_page_output(output: &str, host: &str) -> PageState {
    let mut state = PageState::Offline;
    for line in output.lines().map(str::trim) {
        if line == "NOPAGE" {
            return PageState::NoPage;
        }
        if let Some(item) = line.strip_prefix("LIVE ") {
            let middle = item.strip_suffix(host).unwrap_or(item).trim();
            let title = middle
                .split(' ')
                .filter(|part| !part.is_empty() && *part != "," && !part.starts_with("直播中"))
                .filter(|part| !part.ends_with("看过"))
                .collect::<Vec<_>>()
                .join(" ");
            state = PageState::Live {
                title: Some(title).filter(|title| !title.is_empty()),
            };
        }
    }
    state
}

#[cfg(windows)]
fn read_search_page(host: &str, refresh: bool, enter: bool) -> Result<PageState, String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", SEARCH_PAGE_SCRIPT])
        .env("WECHAT_MONITOR_KEYWORD", host)
        .env("WECHAT_MONITOR_REFRESH", if refresh { "1" } else { "0" })
        .env("WECHAT_MONITOR_ENTER", if enter { "1" } else { "0" })
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("无法读取微信页面：{e}"))?;
    if !output.status.success() {
        return Err("没有找到正在运行的电脑版微信".into());
    }
    Ok(parse_page_output(&String::from_utf8_lossy(&output.stdout), host))
}

#[cfg(not(windows))]
fn read_search_page(_host: &str, _refresh: bool, _enter: bool) -> Result<PageState, String> {
    Err("只支持 Windows".into())
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
    pub interval_mins: u64,
    pub message: String,
    pub last_check_ms: i64,
}

#[derive(Default)]
pub struct WechatMonitor {
    status: Mutex<MonitorStatus>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl WechatMonitor {
    fn say(&self, message: impl Into<String>) {
        let mut status = self.status.lock().unwrap();
        status.message = message.into();
        status.last_check_ms = chrono::Utc::now().timestamp_millis();
    }
}

/// Starts capture, opens the room, and adds the stream. Returns the new recorder's room ID.
async fn record_live_room(
    app: &AppHandle,
    capture: &Arc<WechatCapture>,
    host: &str,
    title: Option<String>,
) -> Result<String, String> {
    capture.start().await?;
    let entered = {
        let host = host.to_string();
        tokio::task::spawn_blocking(move || read_search_page(&host, false, true))
            .await
            .map_err(|e| e.to_string())
    };
    let stream = match entered {
        Ok(Ok(PageState::Live { .. })) => wait_for_stream(capture).await,
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
    crate::handlers::recorder::add_recorder(
        state,
        "wechat".to_string(),
        String::new(),
        extra,
    )
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

async fn monitor_loop(app: AppHandle, monitor: Arc<WechatMonitor>, host: String, interval: u64) {
    let capture = app.state::<Arc<WechatCapture>>().inner().clone();
    loop {
        let page = {
            let host = host.clone();
            tokio::task::spawn_blocking(move || read_search_page(&host, true, false)).await
        };
        match page {
            Ok(Ok(PageState::NoPage)) => monitor.say(format!(
                "没找到页面：请在电脑版微信搜索“{host}”，并停在“{host} - 搜一搜”这个标签页"
            )),
            Ok(Ok(PageState::Offline)) => monitor.say("未开播"),
            Ok(Ok(PageState::Live { title })) => {
                monitor.say("已开播，正在进入直播间并抓取…");
                match record_live_room(&app, &capture, &host, title).await {
                    Ok(room_id) => {
                        monitor.say("正在录制");
                        wait_until_recording_ends(&app, &room_id).await;
                        monitor.say("录制已结束，请切回搜索标签页以继续监控");
                    }
                    Err(error) => monitor.say(format!("这次没有录上：{error}")),
                }
            }
            Ok(Err(error)) => monitor.say(error),
            Err(error) => monitor.say(error.to_string()),
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
pub async fn wechat_monitor_start(
    app: AppHandle,
    monitor: MonitorState<'_>,
    host: String,
    interval_mins: u64,
) -> Result<(), String> {
    let host = host.trim().to_string();
    if host.is_empty() {
        return Err("请填写主播名字".into());
    }
    let interval = interval_mins.max(MIN_INTERVAL_MINS);
    let monitor = monitor.inner().clone();
    if let Some(task) = monitor.task.lock().unwrap().take() {
        task.abort();
    }
    *monitor.status.lock().unwrap() = MonitorStatus {
        running: true,
        host: host.clone(),
        interval_mins: interval,
        message: "正在检查…".into(),
        last_check_ms: 0,
    };
    let task = tokio::spawn(monitor_loop(app, monitor.clone(), host, interval));
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
    {
        let mut status = monitor.status.lock().unwrap();
        status.running = false;
        status.message = "已停止".into();
    }
    // A check aborted mid-capture must not leave the system proxy pointing at us.
    capture.stop().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_live_status_and_topic_from_the_search_result() {
        assert_eq!(
            parse_page_output("LIVE 直播中 , 6.6万人看过 长发小寨溯源专场 新华网\n", "新华网"),
            PageState::Live {
                title: Some("长发小寨溯源专场".into())
            }
        );
        assert_eq!(
            parse_page_output("LIVE 直播中 , 新华网", "新华网"),
            PageState::Live { title: None }
        );
    }

    #[test]
    fn missing_page_and_no_live_result_are_told_apart() {
        assert_eq!(parse_page_output("NOPAGE\n", "新华网"), PageState::NoPage);
        assert_eq!(parse_page_output("", "新华网"), PageState::Offline);
    }
}
