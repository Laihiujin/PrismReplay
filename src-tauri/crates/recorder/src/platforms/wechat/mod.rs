//! Video-channel media import adapter, independent of WeChat installation/certificates.
pub mod capture;
pub mod source;

use crate::{
    account::Account,
    core::flv_recorder::FlvRecorder,
    danmu::DanmuStorage,
    errors::RecorderError,
    events::RecorderEvent,
    platforms::PlatformType,
    traits::{RecorderTrait, StreamInfoProvider},
    Recorder, RoomInfo, UserInfo,
};
use async_trait::async_trait;
use chrono::Utc;
use source::{WechatAccount, WechatSource};
use std::{
    path::PathBuf,
    sync::{
        atomic::{self, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};
use tokio::sync::{broadcast, Mutex, RwLock};

#[derive(Clone)]
pub struct WechatExtra {
    /// The stream of a room added from a captured or pasted address.
    source: Option<WechatSource>,
    /// The account of a tracked room, which has no stream until the account goes live.
    account: Option<WechatAccount>,
    /// The next broadcast's stream, handed over by whoever watches the account.
    pending: Arc<StdMutex<Option<WechatSource>>>,
}
pub type WechatRecorder = Recorder<WechatExtra>;

#[async_trait]
impl StreamInfoProvider for WechatExtra {
    async fn get_resolution(&self) -> Option<String> {
        None
    }
}

impl WechatRecorder {
    pub async fn new(
        room_id: &str,
        extra: &str,
        cache_dir: PathBuf,
        channel: broadcast::Sender<RecorderEvent>,
        update_interval: Arc<atomic::AtomicU64>,
        enabled: bool,
    ) -> Result<Self, RecorderError> {
        let error = |error| RecorderError::ApiError { error };
        let (source, account) = match WechatAccount::parse(extra) {
            Some(account) => (None, Some(account.map_err(error)?)),
            None => (Some(WechatSource::parse(extra).map_err(error)?), None),
        };
        let (title, cover, name) = match (&source, &account) {
            (Some(source), _) => (
                source.title.clone(),
                source.cover.clone().unwrap_or_default(),
                source.name.clone().unwrap_or_else(|| "视频号".into()),
            ),
            (None, Some(account)) => (
                format!("{}的直播间", account.name),
                String::new(),
                account.name.clone(),
            ),
            (None, None) => unreachable!(),
        };
        Ok(Self {
            platform: PlatformType::Wechat,
            room_id: room_id.into(),
            account: Account::default(),
            client: crate::utils::no_proxy_client(),
            event_channel: channel,
            cache_dir,
            quit: Arc::new(atomic::AtomicBool::new(false)),
            enabled: Arc::new(atomic::AtomicBool::new(enabled)),
            is_recording: Arc::new(atomic::AtomicBool::new(false)),
            room_info: Arc::new(RwLock::new(RoomInfo {
                platform: "wechat".into(),
                room_id: room_id.into(),
                room_title: title,
                room_cover: cover,
                status: false,
            })),
            user_info: Arc::new(RwLock::new(UserInfo {
                user_id: room_id.into(),
                user_name: name,
                user_avatar: String::new(),
            })),
            platform_live_id: Arc::new(RwLock::new(room_id.into())),
            live_id: Arc::new(RwLock::new(String::new())),
            danmu_storage: Arc::new(RwLock::new(None)),
            last_update: Arc::new(atomic::AtomicI64::new(Utc::now().timestamp())),
            last_sequence: Arc::new(atomic::AtomicU64::new(0)),
            danmu_task: Arc::new(Mutex::new(None)),
            record_task: Arc::new(Mutex::new(None)),
            update_interval,
            extra: WechatExtra {
                source,
                account,
                pending: Arc::new(StdMutex::new(None)),
            },
        })
    }

    /// The account this room tracks, or `None` for a room made from one stream.
    pub fn tracked_account(&self) -> Option<&WechatAccount> {
        self.extra.account.as_ref()
    }

    /// Hands a tracked room the stream of the broadcast that just started. Returns false when the
    /// room is not tracked, is turned off, or is already recording.
    pub async fn offer_stream(&self, source: WechatSource) -> bool {
        if self.extra.account.is_none()
            || !self.enabled.load(Ordering::Relaxed)
            || self.is_recording.load(Ordering::Relaxed)
        {
            return false;
        }
        *self.extra.pending.lock().unwrap() = Some(source);
        true
    }

    /// Shows a tracked room as live or not between recordings.
    pub async fn set_live(&self, live: bool) {
        if self.extra.account.is_some() && !self.is_recording.load(Ordering::Relaxed) {
            self.room_info.write().await.status = live;
        }
    }

    async fn capture(&self, live_id: &str, source: &WechatSource) -> Result<(), RecorderError> {
        let work_dir = self.work_dir(live_id).await;
        tokio::fs::create_dir_all(work_dir.full_path()).await?;
        if let Some(cover) = source.cover_jpeg() {
            let _ = tokio::fs::write(work_dir.with_filename("cover.jpg").full_path(), cover).await;
        }
        *self.danmu_storage.write().await =
            DanmuStorage::new(&work_dir.with_filename("danmu.txt").full_path()).await;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Referer",
            "https://channels.weixin.qq.com/".parse().unwrap(),
        );
        headers.insert("User-Agent", "Mozilla/5.0".parse().unwrap());
        FlvRecorder::new(
            source.url.clone(),
            headers,
            work_dir.full_path(),
            self.enabled.clone(),
            self.event_channel.clone(),
            live_id.into(),
        )
        .without_proxy()
        .start()
        .await
    }

    /// Records one stream from start to end, announcing both.
    async fn record_once(&self, source: &WechatSource) {
        let live_id = Utc::now().timestamp_millis().to_string();
        *self.live_id.write().await = live_id.clone();
        {
            let mut room_info = self.room_info.write().await;
            room_info.status = true;
            if self.extra.account.is_some() {
                room_info.room_title = source.title.clone();
                if let Some(cover) = &source.cover {
                    room_info.room_cover = cover.clone();
                }
            }
        }
        self.is_recording.store(true, Ordering::Relaxed);
        let _ = self.event_channel.send(RecorderEvent::RecordStart {
            recorder: self.info().await,
        });
        if self.capture(&live_id, source).await.is_err() {
            // Do not expose the signed media URL through logs or notifications.
            log::warn!(
                "[{}] 视频号录制失败：检查媒体链接有效期、授权或加密状态",
                self.room_id
            );
        }
        self.is_recording.store(false, Ordering::Relaxed);
        self.room_info.write().await.status = false;
    }
}

#[async_trait]
impl RecorderTrait<WechatExtra> for WechatRecorder {
    async fn run(&self) {
        let recorder = self.clone();
        *self.record_task.lock().await = Some(tokio::spawn(async move {
            while !recorder.quit.load(Ordering::Relaxed) {
                if !recorder.enabled.load(Ordering::Relaxed) {
                    recorder.extra.pending.lock().unwrap().take();
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                if recorder.extra.account.is_some() {
                    // A tracked room stays on and records every broadcast it is handed.
                    let next = recorder.extra.pending.lock().unwrap().take();
                    let Some(source) = next else {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    };
                    recorder.record_once(&source).await;
                } else if let Some(source) = recorder.extra.source.clone() {
                    recorder.record_once(&source).await;
                    // One explicit capture per enable action; never endlessly redownload a replay
                    // or hammer an expired live URL. The manager persists this disabled state.
                    recorder.enabled.store(false, Ordering::Relaxed);
                }
                let _ = recorder.event_channel.send(RecorderEvent::RecordEnd {
                    recorder: recorder.info().await,
                });
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "Requires ffmpeg on PATH; generates only synthetic local media"]
    async fn captures_local_media_and_stops_without_retrying() {
        let output =
            std::env::temp_dir().join(format!("live-wechat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output).unwrap();
        let fixture = output.join("synthetic input.mp4");
        assert!(std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x72:rate=10",
                "-t",
                "2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
                "-y",
            ])
            .arg(&fixture)
            .status()
            .unwrap()
            .success());
        let bytes = Arc::new(std::fs::read(&fixture).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let bytes = bytes.clone();
                tokio::spawn(async move {
                    let mut request = [0; 8192];
                    let _ = socket.read(&mut request).await;
                    let header = format!("HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&bytes).await;
                });
            }
        });
        let (channel, mut events) = broadcast::channel(64);
        let mut recorder = WechatRecorder::new(
            "fixture",
            "https://finder.video.qq.com/fixture.mp4",
            output.clone(),
            channel,
            Arc::new(atomic::AtomicU64::new(10)),
            true,
        )
        .await
        .unwrap();
        // Test-only injected transport. The public parser continues to reject loopback URLs.
        recorder.extra.source.as_mut().unwrap().url = format!("http://{addr}/fixture.mp4");
        recorder.run().await;
        let mut started = false;
        let mut updated = false;
        let finished = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match events.recv().await.unwrap() {
                    RecorderEvent::RecordStart { .. } => started = true,
                    RecorderEvent::RecordUpdate {
                        duration_secs,
                        cached_size_bytes,
                        ..
                    } => {
                        updated |= duration_secs > 0.0 && cached_size_bytes > 0;
                    }
                    RecorderEvent::RecordEnd { recorder } => break recorder,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(
            started && updated,
            "actual ffmpeg capture must emit start and nonempty media updates"
        );
        assert!(!finished.recording && !finished.enabled && !finished.room_info.status);
        let playlist = output
            .join("wechat/fixture")
            .join(&finished.live_id)
            .join("playlist.m3u8");
        let text = std::fs::read_to_string(&playlist).unwrap();
        assert!(text.contains("#EXT-X-ENDLIST"));
        assert!(text.contains(".ts"));
        assert!(
            tokio::time::timeout(Duration::from_millis(750), events.recv())
                .await
                .is_err(),
            "completed replays must not restart"
        );
        recorder.stop().await;
        server.abort();
        println!("Synthetic media capture verified: {}", playlist.display());
    }

    #[tokio::test]
    #[ignore = "Requires ffmpeg on PATH; generates only synthetic local media"]
    async fn tracked_room_records_each_offered_broadcast_and_stays_on() {
        let output =
            std::env::temp_dir().join(format!("live-wechat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output).unwrap();
        let fixture = output.join("synthetic.mp4");
        assert!(std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "testsrc2=size=128x72:rate=10", "-t", "2", "-c:v", "libx264",
                "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-y"])
            .arg(&fixture)
            .status()
            .unwrap()
            .success());
        let bytes = Arc::new(std::fs::read(&fixture).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let bytes = bytes.clone();
                tokio::spawn(async move {
                    let mut request = [0; 8192];
                    let _ = socket.read(&mut request).await;
                    let header = format!("HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&bytes).await;
                });
            }
        });
        let (channel, mut events) = broadcast::channel(64);
        let recorder = WechatRecorder::new(
            "wxa_test",
            r#"{"account_id":"v2_test@finder","name":"测试"}"#,
            output.clone(),
            channel,
            Arc::new(atomic::AtomicU64::new(10)),
            true,
        )
        .await
        .unwrap();
        assert_eq!(recorder.tracked_account().unwrap().name, "测试");
        recorder.run().await;
        // Nothing records until a broadcast is handed over.
        assert!(tokio::time::timeout(Duration::from_millis(1500), events.recv()).await.is_err());
        for broadcast in ["第一场", "第二场"] {
            // Test-only loopback transport; the public parser rejects it.
            let source = WechatSource {
                url: format!("http://{addr}/fixture.mp4"),
                title: broadcast.into(),
                captured_stream: true,
                name: Some("测试".into()),
                cover: None,
            };
            assert!(recorder.offer_stream(source).await);
            let finished = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let RecorderEvent::RecordEnd { recorder } = events.recv().await.unwrap() {
                        break recorder;
                    }
                }
            })
            .await
            .unwrap();
            assert!(finished.enabled, "a tracked room stays on after a broadcast");
            assert!(!finished.recording && !finished.room_info.status);
            assert_eq!(finished.room_info.room_title, broadcast);
        }
        recorder.disable().await;
        assert!(!recorder.offer_stream(WechatSource::parse("https://a.wxlivecdn.com/s.flv").unwrap()).await);
        recorder.stop().await;
        server.abort();
    }
}
