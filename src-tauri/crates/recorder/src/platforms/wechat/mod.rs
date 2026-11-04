//! Video-channel media import adapter, independent of WeChat installation/certificates.
pub mod source;

use crate::{account::Account, core::flv_recorder::FlvRecorder, danmu::DanmuStorage,
    errors::RecorderError, events::RecorderEvent, platforms::PlatformType,
    traits::{RecorderTrait, StreamInfoProvider}, Recorder, RoomInfo, UserInfo};
use async_trait::async_trait;
use chrono::Utc;
use source::WechatSource;
use std::{path::PathBuf, sync::{atomic::{self, Ordering}, Arc}, time::Duration};
use tokio::sync::{broadcast, Mutex, RwLock};

#[derive(Clone)]
pub struct WechatExtra { source: WechatSource }
pub type WechatRecorder = Recorder<WechatExtra>;

#[async_trait]
impl StreamInfoProvider for WechatExtra {
    async fn get_resolution(&self) -> Option<String> { None }
}

impl WechatRecorder {
    pub async fn new(room_id: &str, extra: &str, cache_dir: PathBuf,
        channel: broadcast::Sender<RecorderEvent>, update_interval: Arc<atomic::AtomicU64>, enabled: bool,
    ) -> Result<Self, RecorderError> {
        let source = WechatSource::parse(extra).map_err(|error| RecorderError::ApiError { error })?;
        Ok(Self {
            platform: PlatformType::Wechat, room_id: room_id.into(), account: Account::default(),
            client: crate::utils::no_proxy_client(), event_channel: channel, cache_dir,
            quit: Arc::new(atomic::AtomicBool::new(false)), enabled: Arc::new(atomic::AtomicBool::new(enabled)),
            is_recording: Arc::new(atomic::AtomicBool::new(false)),
            room_info: Arc::new(RwLock::new(RoomInfo { platform: "wechat".into(), room_id: room_id.into(),
                room_title: source.title.clone(), room_cover: String::new(), status: false })),
            user_info: Arc::new(RwLock::new(UserInfo { user_id: room_id.into(), user_name: "视频号".into(), user_avatar: String::new() })),
            platform_live_id: Arc::new(RwLock::new(room_id.into())), live_id: Arc::new(RwLock::new(String::new())),
            danmu_storage: Arc::new(RwLock::new(None)), last_update: Arc::new(atomic::AtomicI64::new(Utc::now().timestamp())),
            last_sequence: Arc::new(atomic::AtomicU64::new(0)), danmu_task: Arc::new(Mutex::new(None)),
            record_task: Arc::new(Mutex::new(None)), update_interval, extra: WechatExtra { source },
        })
    }

    async fn capture(&self, live_id: &str) -> Result<(), RecorderError> {
        let work_dir = self.work_dir(live_id).await;
        tokio::fs::create_dir_all(work_dir.full_path()).await?;
        *self.danmu_storage.write().await = DanmuStorage::new(&work_dir.with_filename("danmu.txt").full_path()).await;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Referer", "https://channels.weixin.qq.com/".parse().unwrap());
        headers.insert("User-Agent", "Mozilla/5.0".parse().unwrap());
        FlvRecorder::new(self.extra.source.url.clone(), headers, work_dir.full_path(),
            self.enabled.clone(), self.event_channel.clone(), live_id.into())
            .without_proxy().start().await
    }
}

#[async_trait]
impl RecorderTrait<WechatExtra> for WechatRecorder {
    async fn run(&self) {
        let recorder = self.clone();
        *self.record_task.lock().await = Some(tokio::spawn(async move {
            while !recorder.quit.load(Ordering::Relaxed) {
                if !recorder.enabled.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                let live_id = Utc::now().timestamp_millis().to_string();
                *recorder.live_id.write().await = live_id.clone();
                recorder.room_info.write().await.status = true;
                recorder.is_recording.store(true, Ordering::Relaxed);
                let _ = recorder.event_channel.send(RecorderEvent::RecordStart { recorder: recorder.info().await });
                if recorder.capture(&live_id).await.is_err() {
                    // Do not expose the signed media URL through logs or notifications.
                    log::warn!("[{}] 视频号录制失败：检查媒体链接有效期、授权或加密状态", recorder.room_id);
                }
                recorder.is_recording.store(false, Ordering::Relaxed);
                recorder.room_info.write().await.status = false;
                // One explicit capture per enable action; never endlessly redownload a replay
                // or hammer an expired live URL. The manager persists this disabled state.
                recorder.enabled.store(false, Ordering::Relaxed);
                let _ = recorder.event_channel.send(RecorderEvent::RecordEnd { recorder: recorder.info().await });
            }
        }));
    }
}
