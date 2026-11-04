//! Commands for the opt-in video-channel stream capture. See `recorder::platforms::wechat::capture`.
use recorder::platforms::wechat::capture::{CaptureStatus, WechatCapture};
use std::sync::Arc;

type CaptureState<'a> = tauri::State<'a, Arc<WechatCapture>>;

#[tauri::command]
pub async fn wechat_capture_status(capture: CaptureState<'_>) -> Result<CaptureStatus, String> {
    capture.status().await
}

#[tauri::command]
pub async fn wechat_capture_install_ca(capture: CaptureState<'_>) -> Result<(), String> {
    capture.install_ca().await
}

#[tauri::command]
pub async fn wechat_capture_remove_ca(capture: CaptureState<'_>) -> Result<(), String> {
    capture.remove_ca().await
}

#[tauri::command]
pub async fn wechat_capture_start(capture: CaptureState<'_>) -> Result<(), String> {
    capture.start().await
}

#[tauri::command]
pub async fn wechat_capture_stop(capture: CaptureState<'_>) -> Result<(), String> {
    capture.stop().await
}

/// One small frame of a captured stream, so the user can tell the streams apart.
#[tauri::command]
pub async fn wechat_capture_preview(
    capture: CaptureState<'_>,
    url: String,
) -> Result<String, String> {
    if !capture.knows_stream(&url) {
        return Err("这个地址不是本次抓取到的".into());
    }
    grab_frame(&url).await
}

/// Grabs one frame of a stream as a JPEG data URL.
pub async fn grab_frame(url: &str) -> Result<String, String> {
    use base64::Engine;
    let mut command = tokio::process::Command::new(crate::ffmpeg::ffmpeg_path());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-headers",
            "Referer: https://channels.weixin.qq.com/\r\nUser-Agent: Mozilla/5.0\r\n",
            "-i",
            url,
            "-frames:v",
            "1",
            "-vf",
            "scale=-2:360",
            "-f",
            "image2pipe",
            "-c:v",
            "mjpeg",
            "-q:v",
            "5",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|e| format!("无法运行 ffmpeg：{e}"))?;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| "预览超时".to_string())?
    .map_err(|e| format!("无法获取预览：{e}"))?;
    if output.stdout.is_empty() {
        return Err("这条流取不到画面".into());
    }
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&output.stdout)
    ))
}
