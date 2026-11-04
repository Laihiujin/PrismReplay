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
