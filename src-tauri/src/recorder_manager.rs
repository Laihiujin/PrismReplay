use crate::config::Config;
use crate::danmu2ass;
use crate::database::account::AccountRow;
use crate::database::record::RecordRow;
use crate::database::recorder::RecorderRow;
use crate::database::video::VideoRow;
use crate::database::{Database, DatabaseError};
use crate::ffmpeg::{encode_video_danmu, transcode, Range};
use crate::progress::progress_reporter::{EventEmitter, ProgressReporter, ProgressReporterTrait};
use crate::subtitle_generator::item_to_srt;
use crate::task::{Task, TaskManager, TaskPriority};
use crate::webhook::events::{self, Payload};
use crate::webhook::poster::WebhookPoster;
use chrono::DateTime;
use m3u8_rs::{MediaPlaylist, MediaPlaylistType};
use recorder::account::Account;
use recorder::danmu::{DanmuEntry, DanmuStorage};
use recorder::errors::RecorderError;
use recorder::events::RecorderEvent;
use recorder::platforms::bilibili::BiliRecorder;
use recorder::platforms::douyin::DouyinRecorder;
use recorder::platforms::huya::HuyaRecorder;
use recorder::platforms::kuaishou::KuaishouRecorder;
use recorder::platforms::tiktok::TikTokRecorder;
use recorder::platforms::weibo::WeiboRecorder;
use recorder::platforms::wechat::WechatRecorder;
use recorder::platforms::xiaohongshu::XiaohongshuRecorder;
use recorder::platforms::PlatformType;
use recorder::traits::RecorderTrait;
use recorder::RoomInfo;
use recorder::UserInfo;
use recorder::{CachePath, RecorderInfo};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(feature = "gui")]
use tauri_plugin_notification::NotificationExt;
use thiserror::Error;
use tokio::fs::{remove_file, write, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

#[cfg(not(feature = "headless"))]
use tauri::{AppHandle, Manager};

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct RecorderList {
    pub count: usize,
    pub recorders: Vec<RecorderInfo>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ClipRangeParams {
    pub title: String,
    pub note: String,
    pub cover: String,
    pub platform: String,
    pub room_id: String,
    pub live_id: String,
    pub ranges: Vec<Range>,
    /// Encode danmu after clip
    pub danmu: bool,
    pub local_offset: i64,
    /// Fix encoding after clip
    pub fix_encoding: bool,
    /// Transition effect between multiple clipped ranges
    pub transition: Option<String>,
}

pub struct RelatedPlaylist {
    pub live_id: String,
    pub title: String,
    pub path: PathBuf,
}

pub enum RecorderType {
    BiliBili(BiliRecorder),
    Douyin(DouyinRecorder),
    Huya(HuyaRecorder),
    Kuaishou(KuaishouRecorder),
    Xiaohongshu(XiaohongshuRecorder),
    TikTok(TikTokRecorder),
    Weibo(WeiboRecorder),
    Wechat(WechatRecorder),
}

impl RecorderType {
    async fn run(&self) {
        match self {
            RecorderType::BiliBili(recorder) => recorder.run().await,
            RecorderType::Douyin(recorder) => recorder.run().await,
            RecorderType::Huya(recorder) => recorder.run().await,
            RecorderType::Kuaishou(recorder) => recorder.run().await,
            RecorderType::Xiaohongshu(recorder) => recorder.run().await,
            RecorderType::TikTok(recorder) => recorder.run().await,
            RecorderType::Weibo(recorder) => recorder.run().await,
            RecorderType::Wechat(recorder) => recorder.run().await,
        }
    }

    async fn stop(&self) {
        match self {
            RecorderType::BiliBili(recorder) => recorder.stop().await,
            RecorderType::Douyin(recorder) => recorder.stop().await,
            RecorderType::Huya(recorder) => recorder.stop().await,
            RecorderType::Kuaishou(recorder) => recorder.stop().await,
            RecorderType::Xiaohongshu(recorder) => recorder.stop().await,
            RecorderType::TikTok(recorder) => recorder.stop().await,
            RecorderType::Weibo(recorder) => recorder.stop().await,
            RecorderType::Wechat(recorder) => recorder.stop().await,
        }
    }

    async fn info(&self) -> RecorderInfo {
        match self {
            RecorderType::BiliBili(recorder) => recorder.info().await,
            RecorderType::Douyin(recorder) => recorder.info().await,
            RecorderType::Huya(recorder) => recorder.info().await,
            RecorderType::Kuaishou(recorder) => recorder.info().await,
            RecorderType::Xiaohongshu(recorder) => recorder.info().await,
            RecorderType::TikTok(recorder) => recorder.info().await,
            RecorderType::Weibo(recorder) => recorder.info().await,
            RecorderType::Wechat(recorder) => recorder.info().await,
        }
    }

    async fn enable(&self) {
        match self {
            RecorderType::BiliBili(recorder) => recorder.enable().await,
            RecorderType::Douyin(recorder) => recorder.enable().await,
            RecorderType::Huya(recorder) => recorder.enable().await,
            RecorderType::Kuaishou(recorder) => recorder.enable().await,
            RecorderType::Xiaohongshu(recorder) => recorder.enable().await,
            RecorderType::TikTok(recorder) => recorder.enable().await,
            RecorderType::Weibo(recorder) => recorder.enable().await,
            RecorderType::Wechat(recorder) => recorder.enable().await,
        }
    }

    async fn disable(&self) {
        match self {
            RecorderType::BiliBili(recorder) => recorder.disable().await,
            RecorderType::Douyin(recorder) => recorder.disable().await,
            RecorderType::Huya(recorder) => recorder.disable().await,
            RecorderType::Kuaishou(recorder) => recorder.disable().await,
            RecorderType::Xiaohongshu(recorder) => recorder.disable().await,
            RecorderType::TikTok(recorder) => recorder.disable().await,
            RecorderType::Weibo(recorder) => recorder.disable().await,
            RecorderType::Wechat(recorder) => recorder.disable().await,
        }
    }
}

fn is_placeholder_name(value: &str) -> bool {
    matches!(value.trim(), "" | "Kuaishou" | "Kuaishou Live")
}

fn is_login_account_uid(uid: &str) -> bool {
    let trimmed = uid.trim();
    trimmed.starts_with("login:") || trimmed.starts_with("manual:")
}

fn is_guest_account_uid(uid: &str) -> bool {
    let trimmed = uid.trim();
    trimmed.starts_with("guest:")
        || trimmed.starts_with("cookie_")
        || trimmed.starts_with("guest_")
}

fn cookie_pair_count(cookies: &str) -> usize {
    cookies
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty() && part.contains('='))
        .count()
}

fn account_quality_score(account: &AccountRow) -> usize {
    let mut score = 0usize;
    if !account.uid.trim().is_empty() {
        score += 1;
    }
    if !account.name.trim().is_empty() {
        score += 1;
    }
    if !account.avatar.trim().is_empty() {
        score += 1;
    }
    if !account.csrf.trim().is_empty() {
        score += 1;
    }
    if !account.cookies.trim().is_empty() {
        score += 1;
    }

    score * 100 + cookie_pair_count(&account.cookies)
}

fn account_priority(account: &AccountRow) -> usize {
    if is_login_account_uid(&account.uid) {
        3
    } else if is_guest_account_uid(&account.uid) {
        1
    } else {
        2
    }
}

#[derive(Clone)]
pub struct RecorderManager {
    #[cfg(not(feature = "headless"))]
    app_handle: AppHandle,
    emitter: EventEmitter,
    db: Arc<Database>,
    config: Arc<RwLock<Config>>,
    task_manager: Arc<TaskManager>,
    recorders: Arc<RwLock<HashMap<String, RecorderType>>>,
    to_remove: Arc<RwLock<HashSet<String>>>,
    reload_cooldowns: Arc<RwLock<HashMap<String, Instant>>>,
    missing_account_retry: Arc<RwLock<HashMap<String, Instant>>>,
    event_tx: broadcast::Sender<RecorderEvent>,
    is_migrating: Arc<AtomicBool>,
    webhook_poster: WebhookPoster,
}

#[derive(Error, Debug)]
pub enum RecorderManagerError {
    #[error("Recorder already exists: {room_id}")]
    AlreadyExisted { room_id: String },
    #[error("Recorder not found: {room_id}")]
    NotFound { room_id: String },
    #[error("Invalid platform type: {platform}")]
    InvalidPlatformType { platform: String },
    #[error("Recorder error: {0}")]
    RecorderError(#[from] RecorderError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("HLS error: {err}")]
    HLSError { err: String },
    #[error("Database error: {0}")]
    DatabaseError(#[from] DatabaseError),
    #[error("Recording: {live_id}")]
    Recording { live_id: String },
    #[error("Clip error: {err}")]
    ClipError { err: String },
    #[error("M3u8 parse failed: {content}")]
    M3u8ParseFailed { content: String },
    #[error("Empty playlist")]
    EmptyPlaylist,
    #[error("Subtitle not found: {live_id}")]
    SubtitleNotFound { live_id: String },
    #[error("Subtitle generation failed: {error}")]
    SubtitleGenerationFailed { error: String },
    #[error("Invalid live id, not timestamp str")]
    InvalidLiveID,
    #[error("Archive danmu ass generation failed: {error}")]
    ArchiveDanmuAssGenerationFailed { error: String },
}

impl From<RecorderManagerError> for String {
    fn from(err: RecorderManagerError) -> Self {
        err.to_string()
    }
}

/// Map an HLS request path (`platform/room_id/live_id/...`) to a file in `cache_dir`.
///
/// Every `/`-separated segment must be a single plain file name: `..`, `.`, empty
/// segments, absolute paths and Windows prefixes or `\` separators are rejected,
/// so the result can never escape `cache_dir`. Names made only of dots and spaces
/// (`.. `, `...`) are rejected too, since Windows trims trailing dots and spaces
/// and could read them as `..`.
fn resolve_hls_path(cache_dir: &Path, path: &str) -> Option<PathBuf> {
    let path = path.replace("%7C", "|");
    let mut resolved = cache_dir.to_path_buf();
    for segment in path.split('/') {
        let mut components = Path::new(segment).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None)
                if !name
                    .to_string_lossy()
                    .trim_end_matches([' ', '.'])
                    .is_empty() =>
            {
                resolved.push(name)
            }
            _ => return None,
        }
    }
    Some(resolved)
}

impl RecorderManager {
    pub fn new(
        #[cfg(not(feature = "headless"))] app_handle: AppHandle,
        emitter: EventEmitter,
        db: Arc<Database>,
        config: Arc<RwLock<Config>>,
        task_manager: Arc<TaskManager>,
        webhook_poster: WebhookPoster,
    ) -> RecorderManager {
        let (event_tx, _) = broadcast::channel(2048);
        let manager = RecorderManager {
            #[cfg(not(feature = "headless"))]
            app_handle,
            emitter,
            db,
            config,
            task_manager,
            recorders: Arc::new(RwLock::new(HashMap::new())),
            to_remove: Arc::new(RwLock::new(HashSet::new())),
            reload_cooldowns: Arc::new(RwLock::new(HashMap::new())),
            missing_account_retry: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            is_migrating: Arc::new(AtomicBool::new(false)),
            webhook_poster,
        };

        // Start event listener
        let manager_clone = manager.clone();
        tokio::spawn(async move {
            manager_clone.handle_events().await;
        });

        let manager_clone = manager.clone();
        tokio::spawn(async move {
            manager_clone.monitor_recorders().await;
        });

        manager
    }

    pub fn get_event_sender(&self) -> broadcast::Sender<RecorderEvent> {
        self.event_tx.clone()
    }

    fn reload_cooldown_for(platform: PlatformType) -> Duration {
        if matches!(platform, PlatformType::Kuaishou) {
            Duration::from_secs(15)
        } else {
            Duration::from_secs(3)
        }
    }

    pub async fn check_reload_cooldown(
        &self,
        platform: PlatformType,
        room_id: &str,
    ) -> Option<Duration> {
        let recorder_id = format!("{}:{}", platform.as_str(), room_id);
        let cooldown = Self::reload_cooldown_for(platform);
        let now = Instant::now();
        let mut reloads = self.reload_cooldowns.write().await;
        reloads.retain(|_, ts| now.saturating_duration_since(*ts) < cooldown);

        if let Some(last_reload) = reloads.get(&recorder_id) {
            let elapsed = now.saturating_duration_since(*last_reload);
            if elapsed < cooldown {
                return Some(cooldown - elapsed);
            }
        }

        reloads.insert(recorder_id, now);
        None
    }

    async fn stop_recorder_in_manager(&self, platform: PlatformType, room_id: &str) {
        let mut recorder_id = format!("{}:{}", platform.as_str(), room_id);
        let existing_id = {
            let recorders = self.recorders.read().await;
            if recorders.contains_key(&recorder_id) {
                Some(recorder_id.clone())
            } else {
                recorders
                    .keys()
                    .find(|key| key.as_str().ends_with(&format!(":{room_id}")))
                    .cloned()
            }
        };

        if let Some(found_id) = existing_id {
            recorder_id = found_id;
            if let Some(recorder) = self.recorders.write().await.remove(&recorder_id) {
                recorder.stop().await;
            }
        }
    }

    pub async fn select_account_for_platform(
        &self,
        platform: PlatformType,
    ) -> Result<Option<Account>, DatabaseError> {
        let config = self.config.read().await.clone();
        let platform_str = platform.as_str();
        let accounts = self.db.get_accounts().await?;
        let platform_accounts: Vec<AccountRow> = accounts
            .into_iter()
            .filter(|account| account.platform == platform_str && !account.cookies.trim().is_empty())
            .collect();

        let choose_best = |items: Vec<AccountRow>| {
            items.into_iter().max_by(|a, b| {
                account_priority(a)
                    .cmp(&account_priority(b))
                    .then_with(|| account_quality_score(a).cmp(&account_quality_score(b)))
                    .then_with(|| a.created_at.cmp(&b.created_at))
            })
        };

        if config.use_login_accounts {
            let login_accounts: Vec<AccountRow> = platform_accounts
                .iter()
                .filter(|account| is_login_account_uid(&account.uid))
                .cloned()
                .collect();
            if let Some(account) = choose_best(login_accounts) {
                log::info!(
                    "[Account] Auto-selected login account for platform {}: {}",
                    platform_str,
                    account.uid
                );
                return Ok(Some(account.to_account()));
            }
        }

        if config.use_guest_accounts {
            let guest_accounts: Vec<AccountRow> = platform_accounts
                .iter()
                .filter(|account| is_guest_account_uid(&account.uid))
                .cloned()
                .collect();
            if let Some(account) = choose_best(guest_accounts) {
                log::info!(
                    "[Account] Auto-selected guest account for platform {}: {}",
                    platform_str,
                    account.uid
                );
                return Ok(Some(account.to_account()));
            }
        }

        if let Some(account) = choose_best(platform_accounts) {
            log::info!(
                "[Account] Auto-selected fallback account for platform {}: {}",
                platform_str,
                account.uid
            );
            return Ok(Some(account.to_account()));
        }

        Ok(None)
    }

    async fn handle_events(&self) {
        let mut rx = self.event_tx.subscribe();
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("Recorder event listener lagged; skipped {skipped} events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match event {
                RecorderEvent::LiveStart { recorder } => {
                    let event = events::new_webhook_event(
                        events::LIVE_STARTED,
                        Payload::Room(recorder.clone()),
                    );
                    let _ = self.webhook_poster.post_event(&event).await;
                    if self.config.read().await.live_start_notify {
                        #[cfg(feature = "gui")]
                        self.app_handle
                            .notification()
                            .builder()
                            .title("PrismReplay - 直播开始")
                            .body(format!(
                                "{} 开启了直播：{}",
                                recorder.user_info.user_name, recorder.room_info.room_title
                            ))
                            .show()
                            .unwrap();
                    }
                }
                RecorderEvent::LiveEnd {
                    platform,
                    room_id,
                    recorder,
                } => {
                    let event = events::new_webhook_event(
                        events::LIVE_ENDED,
                        Payload::Room(recorder.clone()),
                    );
                    let _ = self.webhook_poster.post_event(&event).await;
                    self.handle_live_end(platform, &room_id, &recorder).await;
                    if self.config.read().await.live_end_notify {
                        #[cfg(feature = "gui")]
                        self.app_handle
                            .notification()
                            .builder()
                            .title("PrismReplay - 直播结束")
                            .body(format!(
                                "{} 结束了直播：{}",
                                recorder.user_info.user_name, recorder.room_info.room_title
                            ))
                            .show()
                            .unwrap();
                    }
                }
                RecorderEvent::RecordStart { recorder } => {
                    // add record entry into db
                    let platform = PlatformType::from_str(&recorder.room_info.platform).unwrap();
                    let room_id = recorder.room_info.room_id.clone();
                    log::info!("Record start: {recorder:?}");

                    // Store cover path in format: {platform}/{room_id}/{live_id}/cover.jpg
                    let cover_path = if !recorder.room_info.room_cover.is_empty() {
                        Some(format!(
                            "{}/{}/{}/cover.jpg",
                            platform.as_str(),
                            &room_id,
                            &recorder.live_id
                        ))
                    } else {
                        None
                    };

                    if let Err(e) = self
                        .db
                        .add_record(
                            platform,
                            &recorder.platform_live_id,
                            &recorder.live_id,
                            &room_id,
                            &recorder.room_info.room_title,
                            cover_path,
                        )
                        .await
                    {
                        log::error!("Failed to add record entry into db: {e}");
                    }

                    // Save resolution if available
                    if let Some(resolution) = &recorder.resolution {
                        if let Err(e) = self
                            .db
                            .update_record_resolution(&recorder.live_id, Some(resolution.clone()))
                            .await
                        {
                            log::warn!("Failed to save resolution to db: {e}");
                        }
                    }

                    let event =
                        events::new_webhook_event(events::RECORD_STARTED, Payload::Room(recorder));
                    let _ = self.webhook_poster.post_event(&event).await;
                }
                RecorderEvent::RecordUpdate {
                    live_id,
                    duration_secs,
                    cached_size_bytes,
                } => {
                    let _ = self
                        .db
                        .update_record_delta(&live_id, duration_secs, cached_size_bytes)
                        .await;
                }
                RecorderEvent::RecordEnd { recorder } => {
                    if recorder.room_info.platform == "wechat" {
                        let _ = self.db.update_recorder(PlatformType::Wechat, &recorder.room_info.room_id, recorder.enabled).await;
                    }
                    log::info!("Record end: {recorder:?}");
                    let live_id = recorder.live_id.clone();
                    if live_id.is_empty() {
                        log::warn!(
                            "Ignoring record end without live id for room {}",
                            recorder.room_info.room_id
                        );
                        continue;
                    }
                    let event = events::new_webhook_event(
                        events::RECORD_ENDED,
                        Payload::Room(recorder.clone()),
                    );
                    let _ = self.webhook_poster.post_event(&event).await;
                    // check record in db, if length is 0, delete it
                    let room_id = recorder.room_info.room_id.clone();
                    let record = match self.db.get_record(&room_id, &live_id).await {
                        Ok(r) => r,
                        Err(e) => {
                            log::error!("Record not found in db: {recorder:?}, err={e:?}");
                            continue;
                        }
                    };
                    if record.size == 0 {
                        let _ = self.db.remove_record(&live_id).await;
                        // remove record folder
                        let cache_folder = Path::new(self.config.read().await.cache.as_str())
                            .join(
                                PlatformType::from_str(&recorder.room_info.platform)
                                    .unwrap_or(PlatformType::BiliBili)
                                    .as_str(),
                            )
                            .join(room_id)
                            .join(live_id);
                        let _ = tokio::fs::remove_dir_all(&cache_folder).await;
                        log::info!("Record folder removed: {cache_folder:?}");
                    }
                }
                RecorderEvent::ProgressUpdate { id, content } => {
                    self.emitter
                        .emit(&RecorderEvent::ProgressUpdate { id, content });
                }
                RecorderEvent::ProgressFinished {
                    id,
                    success,
                    message,
                } => {
                    self.emitter.emit(&RecorderEvent::ProgressFinished {
                        id,
                        success,
                        message,
                    });
                }
                RecorderEvent::GuestCookieRefreshRequested { platform, reason } => {
                    log::warn!(
                        "[Account] Guest cookie refresh requested by {}: {}",
                        platform.as_str(),
                        reason
                    );
                    #[cfg(feature = "gui")]
                    {
                        let state = self
                            .app_handle
                            .state::<crate::state::State>()
                            .inner()
                            .clone();
                        let refresh_reason = format!("{}: {}", platform.as_str(), reason);
                        let target_platform = platform;
                        tokio::spawn(async move {
                            match crate::handlers::account::refresh_guest_accounts_on_demand_owned(
                                state.clone(),
                                refresh_reason,
                            )
                            .await
                            {
                                Ok(changed_platforms) => {
                                    if changed_platforms
                                        .iter()
                                        .any(|platform| *platform == target_platform)
                                    {
                                        if let Err(err) = state
                                            .recorder_manager
                                            .restart_recorders_for_platforms(&[target_platform])
                                            .await
                                        {
                                            log::warn!(
                                                "Failed to restart recorders after guest refresh: {err}"
                                            );
                                        }
                                    }
                                }
                                Err(err) => {
                                    log::warn!("[Account] Guest cookie refresh failed: {}", err);
                                }
                            }
                        });
                    }
                }
                RecorderEvent::DanmuReceived { room, ts, content } => {
                    self.emitter
                        .emit(&RecorderEvent::DanmuReceived { room, ts, content });
                }
            }
        }
    }

    async fn handle_live_end(
        &self,
        platform: PlatformType,
        room_id: &str,
        recorder: &RecorderInfo,
    ) {
        if !self.config.read().await.auto_generate.enabled {
            return;
        }

        let recorder_id = format!("{}:{}", platform.as_str(), room_id);
        log::info!("Start auto generate for {recorder_id}");
        let live_id = recorder.live_id.clone();
        let live_record = self.db.get_record(room_id, &live_id).await;
        if live_record.is_err() {
            log::error!("Live not found in record: {room_id} {live_id}");
            return;
        }

        let live_record = live_record.unwrap();

        let Ok(task) = self
            .db
            .generate_task(
                "generate_whole_clip",
                "",
                &serde_json::json!({
                    "platform": platform.as_str(),
                    "room_id": room_id,
                    "parent_id": live_record.parent_id,
                })
                .to_string(),
            )
            .await
        else {
            log::error!("Failed to generate task");
            return;
        };

        let Ok(reporter) = ProgressReporter::new(self.db.clone(), &self.emitter, &task.id).await
        else {
            log::error!("Failed to create reporter");
            let _ = self
                .db
                .update_task(&task.id, "failed", "Failed to create reporter", None)
                .await;
            return;
        };

        log::info!("Create task: {} {}", task.id, task.task_type);

        let self_clone = self.clone();
        let task_id = task.id.clone();
        let room_id = room_id.to_string();
        let _ = self
            .task_manager
            .add_task(Task::new(
                task_id.clone(),
                TaskPriority::Normal,
                async move {
                    if let Err(e) = self_clone
                        .generate_whole_clip(
                            Some(&reporter),
                            self_clone.config.read().await.auto_generate.encode_danmu,
                            self_clone
                                .config
                                .read()
                                .await
                                .auto_generate
                                .delete_cache_after_clip,
                            platform.as_str().to_string(),
                            &room_id,
                            live_record.parent_id,
                            None,
                        )
                        .await
                    {
                        log::error!("Failed to generate whole clip: {e}");
                        let _ = reporter
                            .finish(false, &format!("Failed to generate whole clip: {e}"))
                            .await;
                        let _ = self_clone
                            .db
                            .update_task(
                                &task_id,
                                "failed",
                                &format!("Failed to generate whole clip: {e}"),
                                None,
                            )
                            .await;
                        return Err(format!("Failed to generate whole clip: {e}"));
                    }

                    let _ = reporter
                        .finish(true, "Whole clip generated successfully")
                        .await;
                    let _ = self_clone
                        .db
                        .update_task(
                            &task_id,
                            "success",
                            "Whole clip generated successfully",
                            None,
                        )
                        .await;
                    Ok(())
                },
            ))
            .await;
    }

    pub fn set_migrating(&self, migrating: bool) {
        self.is_migrating
            .store(migrating, std::sync::atomic::Ordering::Relaxed);
    }

    async fn monitor_recorders(&self) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
        loop {
            if self.is_migrating.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                continue;
            }
            // get a list of recorders in db, if not created yet, create them
            let recorders = self.db.get_recorders().await;
            if recorders.is_err() {
                log::error!(
                    "Failed to get recorders from db: {}",
                    recorders.err().unwrap()
                );
                return;
            }
            let recorders = recorders.unwrap();
            let mut recorder_map = HashMap::new();
            for recorder in recorders {
                let platform = PlatformType::from_str(&recorder.platform).unwrap();
                let room_id = recorder.room_id;
                if matches!(platform, PlatformType::Xiaohongshu | PlatformType::Weibo) {
                    log::info!(
                        "Skip disabled platform recorder: {} {}",
                        platform.as_str(),
                        room_id
                    );
                    continue;
                }
                let auto_start = recorder.auto_start;
                let extra = recorder.extra;
                recorder_map.insert((platform, room_id), (auto_start, extra));
            }
            let mut recorders_to_add = Vec::new();
            for (platform, room_id) in recorder_map.keys() {
                let recorder_id = format!("{}:{}", platform.as_str(), room_id);
                if !self.recorders.read().await.contains_key(&recorder_id)
                    && !self.to_remove.read().await.contains(&recorder_id)
                {
                    recorders_to_add.push((*platform, room_id.clone()));
                }
            }
            for (platform, room_id) in recorders_to_add {
                if self.is_migrating.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if matches!(platform, PlatformType::Xiaohongshu | PlatformType::Weibo) {
                    continue;
                }
                let (auto_start, extra) = recorder_map.get(&(platform, room_id.clone())).unwrap();
                let account_required = !matches!(platform, PlatformType::Huya | PlatformType::Wechat);
                let recorder_id = format!("{}:{}", platform.as_str(), room_id);
                if account_required {
                    let now = Instant::now();
                    let next_retry = {
                        let retry_map = self.missing_account_retry.read().await;
                        retry_map.get(&recorder_id).cloned()
                    };
                    if let Some(next_retry) = next_retry {
                        if next_retry > now {
                            continue;
                        }
                    }
                }
                let account = match self.select_account_for_platform(platform).await {
                    Ok(account) => account,
                    Err(e) => {
                        log::error!("Failed to load account for {platform:?}: {e}");
                        None
                    }
                };
                if account_required && account.is_none() {
                    log::info!("Skip recorder without account: {platform:?} {room_id}");
                    let next_retry = Instant::now() + Duration::from_secs(300);
                    self.missing_account_retry
                        .write()
                        .await
                        .insert(recorder_id, next_retry);
                    continue;
                }
                if account_required {
                    self.missing_account_retry
                        .write()
                        .await
                        .remove(&recorder_id);
                }
                let account = account.unwrap_or_default();

                if let Err(e) = self
                    .add_recorder(&account, platform, &room_id, extra, *auto_start)
                    .await
                {
                    log::error!(
                        "Failed to add recorder: {} {} {}",
                        platform.as_str(),
                        room_id,
                        e
                    );
                }
            }
            interval.tick().await;
        }
    }

    pub async fn add_recorder(
        &self,
        account: &Account,
        platform: PlatformType,
        room_id: &str,
        extra: &str,
        enabled: bool,
    ) -> Result<(), RecorderManagerError> {
        if matches!(platform, PlatformType::Xiaohongshu | PlatformType::Weibo) {
            return Err(RecorderManagerError::InvalidPlatformType {
                platform: format!("{} (temporarily disabled)", platform.as_str()),
            });
        }
        let recorder_id = format!("{}:{}", platform.as_str(), room_id);
        if self.recorders.read().await.contains_key(&recorder_id) {
            return Err(RecorderManagerError::AlreadyExisted {
                room_id: room_id.to_string(),
            });
        }

        if matches!(platform, PlatformType::Kuaishou) {
            self.config.read().await.apply_kuaishou_ws_env();
        }

        let cache_dir = self.config.read().await.cache.clone();
        let cache_dir = PathBuf::from(&cache_dir);

        let event_tx = self.get_event_sender();
        let update_interval = self.config.read().await.update_interval.clone();
        let recorder: RecorderType = match platform {
            PlatformType::Wechat => RecorderType::Wechat(
                WechatRecorder::new(room_id, extra, cache_dir, event_tx, update_interval, enabled).await?,
            ),
            PlatformType::BiliBili => RecorderType::BiliBili(
                BiliRecorder::new(
                    room_id,
                    account,
                    cache_dir,
                    event_tx,
                    update_interval,
                    enabled,
                )
                .await?,
            ),
            PlatformType::Douyin => RecorderType::Douyin(
                DouyinRecorder::new(
                    room_id,
                    extra,
                    account,
                    cache_dir,
                    event_tx,
                    update_interval,
                    enabled,
                )
                .await?,
            ),
            PlatformType::Huya => RecorderType::Huya(
                HuyaRecorder::new(
                    room_id,
                    account,
                    cache_dir,
                    event_tx,
                    update_interval,
                    enabled,
                )
                .await?,
            ),
            PlatformType::Kuaishou => RecorderType::Kuaishou(
                KuaishouRecorder::new(
                    room_id,
                    account,
                    cache_dir,
                    event_tx,
                    update_interval,
                    enabled,
                )
                .await?,
            ),
            PlatformType::Xiaohongshu => RecorderType::Xiaohongshu(
                XiaohongshuRecorder::new(
                    room_id,
                    account,
                    cache_dir,
                    event_tx,
                    update_interval,
                    enabled,
                )
                .await?,
            ),
            PlatformType::TikTok => RecorderType::TikTok(
                TikTokRecorder::new(
                    room_id,
                    extra,
                    account,
                    cache_dir,
                    event_tx,
                    update_interval,
                    enabled,
                )
                .await?,
            ),
            PlatformType::Weibo => RecorderType::Weibo(
                WeiboRecorder::new(
                    room_id,
                    account,
                    cache_dir,
                    event_tx,
                    update_interval,
                    enabled,
                )
                .await?,
            ),
            _ => {
                return Err(RecorderManagerError::InvalidPlatformType {
                    platform: platform.as_str().to_string(),
                })
            }
        };
        self.recorders
            .write()
            .await
            .insert(recorder_id.clone(), recorder);
        if let Some(recorder_ref) = self.recorders.read().await.get(&recorder_id) {
            recorder_ref.run().await;
        }
        Ok(())
    }

    pub async fn restart_recorders_for_platforms(
        &self,
        platforms: &[PlatformType],
    ) -> Result<(), RecorderManagerError> {
        if platforms.is_empty() {
            return Ok(());
        }
        let platform_set: HashSet<PlatformType> = platforms.iter().cloned().collect();
        let rows = self.db.get_recorders().await?;
        for row in rows {
            let platform = PlatformType::from_str(&row.platform).map_err(|_| {
                RecorderManagerError::InvalidPlatformType {
                    platform: row.platform.clone(),
                }
            })?;
            if !platform_set.contains(&platform) {
                continue;
            }

            let room_id = row.room_id.clone();
            let extra = row.extra.clone();
            let enabled = row.auto_start;

            self.stop_recorder_in_manager(platform, &room_id).await;

            let account = self
                .select_account_for_platform(platform)
                .await?
                .unwrap_or_default();

            if let Err(err) = self
                .add_recorder(&account, platform, &room_id, &extra, enabled)
                .await
            {
                log::warn!(
                    "Failed to restart recorder: {} {} ({err})",
                    platform.as_str(),
                    room_id
                );
            }
        }

        Ok(())
    }

    pub async fn stop_all(&self) {
        for recorder_ref in self.recorders.read().await.values() {
            recorder_ref.stop().await;
        }

        // remove all recorders
        self.recorders.write().await.clear();
    }

    /// Remove a recorder from the manager
    ///
    /// This will stop the recorder and remove it from the manager.
    pub async fn remove_recorder(
        &self,
        platform: PlatformType,
        room_id: &str,
    ) -> Result<RecorderRow, RecorderManagerError> {
        // check recorder exists in manager, otherwise fall back to DB removal
        let mut recorder_id = format!("{}:{}", platform.as_str(), room_id);
        if !self.recorders.read().await.contains_key(&recorder_id) {
            if let Some(found_id) = self
                .recorders
                .read()
                .await
                .keys()
                .find(|key| key.as_str().ends_with(&format!(":{room_id}")))
                .cloned()
            {
                recorder_id = found_id;
            } else {
                let recorder = self.db.remove_recorder(room_id).await?;
                return Ok(recorder);
            }
        }

        // remove from db
        let recorder = self.db.remove_recorder(room_id).await?;

        // add to to_remove
        log::debug!("Add to to_remove: {recorder_id}");
        self.to_remove.write().await.insert(recorder_id.clone());

        // stop recorder
        log::debug!("Stop recorder: {recorder_id}");
        if let Some(recorder_ref) = self.recorders.read().await.get(&recorder_id) {
            recorder_ref.stop().await;
        }

        // remove recorder
        log::debug!("Remove recorder from manager: {recorder_id}");
        self.recorders.write().await.remove(&recorder_id);

        // remove from to_remove
        log::debug!("Remove from to_remove: {recorder_id}");
        self.to_remove.write().await.remove(&recorder_id);

        Ok(recorder)
    }

    async fn load_playlist_bytes(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<Vec<u8>, RecorderManagerError> {
        let cache_path = self.config.read().await.cache.clone();
        let cache_path = Path::new(&cache_path);
        let playlist_path = cache_path
            .join(platform.as_str())
            .join(room_id)
            .join(live_id)
            .join("playlist.m3u8");
        if !playlist_path.exists() {
            return Err(RecorderManagerError::IoError(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Playlist file not found",
            )));
        }
        let mut bytes: Vec<u8> = Vec::new();
        tokio::fs::File::open(playlist_path)
            .await
            .unwrap()
            .read_to_end(&mut bytes)
            .await
            .unwrap();
        Ok(bytes)
    }

    async fn playlist_path(&self, platform: PlatformType, room_id: &str, live_id: &str) -> PathBuf {
        let cache_path = self.config.read().await.cache.clone();
        Path::new(&cache_path)
            .join(platform.as_str())
            .join(room_id)
            .join(live_id)
            .join("playlist.m3u8")
    }

    async fn wait_for_playlist(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
        timeout_ms: u64,
    ) -> bool {
        let path = self.playlist_path(platform, room_id, live_id).await;
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            if path.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        path.exists()
    }

    /// Check if the playlist is outdated
    ///
    /// This will check if the current recorder live id is the same as the live id
    /// and if the current recorder is recording
    /// and if the current recorder is recording, return false
    /// otherwise, return true
    async fn is_outdated_playlist(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> bool {
        // check current recorder live id is the same as the live id
        let recorder = self.get_recorder_info(platform, room_id).await;
        let Some(recorder) = recorder else {
            return true;
        };

        if recorder.live_id != live_id {
            return true;
        }

        false
    }

    async fn load_playlist(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<MediaPlaylist, RecorderManagerError> {
        let bytes = self.load_playlist_bytes(platform, room_id, live_id).await?;
        if let Result::Ok((_, mut pl)) = m3u8_rs::parse_media_playlist(&bytes) {
            if self.is_outdated_playlist(platform, room_id, live_id).await {
                pl.end_list = true;
                pl.playlist_type = Some(MediaPlaylistType::Vod);
            }
            return Ok(pl);
        }
        Err(RecorderManagerError::M3u8ParseFailed {
            content: String::from_utf8(bytes).unwrap(),
        })
    }

    async fn playlist_range(
        &self,
        playlist: &MediaPlaylist,
        range: Option<Range>,
    ) -> Result<MediaPlaylist, RecorderManagerError> {
        let mut playlist = playlist.clone();
        if let Some(range) = range {
            let mut duration = 0.0f64;
            let mut segments = Vec::new();
            for s in playlist.segments {
                if range.is_in(duration) || range.is_in(duration + s.duration as f64) {
                    segments.push(s.clone());
                }
                duration += s.duration as f64;
            }
            playlist.segments = segments;
            playlist.end_list = true;
            playlist.playlist_type = Some(MediaPlaylistType::Vod);
        }

        Ok(playlist)
    }

    async fn first_segment_timestamp(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<i64, RecorderManagerError> {
        let playlist = self.load_playlist(platform, room_id, live_id).await?;
        if playlist.segments.is_empty() {
            return Err(RecorderManagerError::EmptyPlaylist);
        }

        let first_segment = playlist.segments.first().unwrap();
        if let Some(program_date_time) = first_segment.program_date_time {
            return Ok(program_date_time.timestamp_millis());
        }

        // else, find in unknown tags
        let program_date_time = first_segment
            .unknown_tags
            .iter()
            .find(|t| t.tag == "X-PROGRAM-DATE-TIME");

        let Some(program_date_time) = program_date_time else {
            return live_id
                .parse::<i64>()
                .map_err(|_| RecorderManagerError::InvalidLiveID);
        };

        let Some(value) = &program_date_time.rest else {
            return live_id
                .parse::<i64>()
                .map_err(|_| RecorderManagerError::InvalidLiveID);
        };

        // example: "2025-10-18T17:18:17.004+0800"
        // convert to timestamp
        let timestamp = DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.3f%z")
            .unwrap()
            .timestamp_millis();
        Ok(timestamp)
    }

    pub async fn load_danmus(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<Vec<DanmuEntry>, RecorderManagerError> {
        let cache_path = self.config.read().await.cache.clone();
        let cache_path = Path::new(&cache_path);
        let danmus_path = cache_path
            .join(platform.as_str())
            .join(room_id)
            .join(live_id)
            .join("danmu.txt");
        if !danmus_path.exists() {
            return Ok(Vec::new());
        }
        let Some(storage) = DanmuStorage::new(&danmus_path).await else {
            log::error!("Failed to load danmu storage: {danmus_path:?}");
            return Ok(Vec::new());
        };
        Ok(storage.get_entries(0).await)
    }

    /// Get related playlists by parent id
    ///
    /// This will return a list of tuples, the first element is the title of the archive,
    /// the second element is the path of the playlist
    async fn get_related_playlists(
        &self,
        platform: &PlatformType,
        room_id: &str,
        parent_id: &str,
    ) -> Vec<RelatedPlaylist> {
        let cache_path = self.config.read().await.cache.clone();
        let cache_path = Path::new(&cache_path);
        let archives = self.db.get_archives_by_parent_id(room_id, parent_id).await;
        if let Err(e) = archives {
            log::error!(
                "[{}] Failed to get all related playlists: {} {}",
                room_id,
                parent_id,
                e
            );
            return Vec::new();
        }

        let archives: Vec<(String, String)> = archives
            .unwrap()
            .iter()
            .map(|a| (a.title.clone(), a.live_id.clone()))
            .collect();

        let playlists = archives
            .iter()
            .map(async |a| {
                let work_dir =
                    CachePath::new(cache_path.to_path_buf(), *platform, room_id, a.1.as_str());

                RelatedPlaylist {
                    live_id: a.1.clone(),
                    title: a.0.clone(),
                    path: work_dir.with_filename("playlist.m3u8").full_path(),
                }
            })
            .collect::<Vec<_>>();

        let playlists = futures::future::join_all(playlists).await;

        playlists
    }

    async fn get_related_playlists_by_live_ids(
        &self,
        platform: &PlatformType,
        room_id: &str,
        live_ids: &[String],
    ) -> Vec<RelatedPlaylist> {
        if live_ids.is_empty() {
            return Vec::new();
        }
        let cache_path = self.config.read().await.cache.clone();
        let cache_path = Path::new(&cache_path);
        let mut archives: Vec<RecordRow> = Vec::new();
        for live_id in live_ids {
            match self.db.get_record(room_id, live_id).await {
                Ok(record) => archives.push(record),
                Err(e) => {
                    log::warn!("Failed to load record {} {}: {}", room_id, live_id, e);
                }
            }
        }
        archives.sort_by(|a, b| a.created_at.cmp(&b.created_at));

        let playlists = archives
            .iter()
            .map(async |record| {
                let work_dir = CachePath::new(
                    cache_path.to_path_buf(),
                    *platform,
                    room_id,
                    &record.live_id,
                );

                RelatedPlaylist {
                    live_id: record.live_id.clone(),
                    title: record.title.clone(),
                    path: work_dir.with_filename("playlist.m3u8").full_path(),
                }
            })
            .collect::<Vec<_>>();

        futures::future::join_all(playlists).await
    }

    pub async fn clip_range(
        &self,
        reporter: Option<&ProgressReporter>,
        clip_file: PathBuf,
        params: &ClipRangeParams,
    ) -> Result<PathBuf, RecorderManagerError> {
        let cache_path = self.config.read().await.cache.clone();
        let cache_path = Path::new(&cache_path);
        let playlist_path = cache_path
            .join(params.platform.clone())
            .join(params.room_id.clone())
            .join(params.live_id.clone())
            .join("playlist.m3u8");

        if !playlist_path.exists() {
            log::error!("Playlist file not found: {}", playlist_path.display());
            return Err(RecorderManagerError::ClipError {
                err: "Playlist file not found".to_string(),
            });
        }

        if params.ranges.is_empty() {
            crate::ffmpeg::playlist::clip_from_playlist(reporter, &playlist_path, &clip_file, None)
                .await
                .map_err(|e| RecorderManagerError::ClipError { err: e.to_string() })?;
        } else {
            crate::ffmpeg::playlist::clip_multiple_from_playlist(
                reporter,
                &playlist_path,
                &clip_file,
                &params.ranges,
                params.transition.as_deref(),
            )
            .await
            .map_err(|e| RecorderManagerError::ClipError { err: e.to_string() })?;
        }

        if params.fix_encoding {
            // transcode clip_file
            let tmp_clip_file = clip_file.with_extension("tmp.mp4");
            if let Err(e) = transcode(reporter, &clip_file, &tmp_clip_file, false).await {
                log::error!("Failed to transcode clip file: {e}");
                return Err(RecorderManagerError::ClipError { err: e.to_string() });
            }

            // remove clip_file
            let _ = tokio::fs::remove_file(&clip_file).await;

            // rename tmp_clip_file to clip_file
            let _ = tokio::fs::rename(tmp_clip_file, &clip_file).await;
        }

        if !params.danmu {
            log::info!("Skip danmu encoding");
            return Ok(clip_file);
        }

        let Ok(platform) = PlatformType::from_str(&params.platform) else {
            return Err(RecorderManagerError::InvalidPlatformType {
                platform: params.platform.clone(),
            });
        };
        let stream_start_timestamp_milis = self
            .first_segment_timestamp(platform, &params.room_id, &params.live_id)
            .await?;

        let danmus = self
            .load_danmus(platform, &params.room_id, &params.live_id)
            .await;
        if danmus.is_err() {
            log::error!(
                "Failed to get danmus, skip danmu encoding: {}",
                danmus.err().unwrap()
            );
            return Ok(clip_file);
        }

        let mut danmus = danmus.unwrap();
        log::debug!("First danmu entry: {:?}", danmus.first());
        log::debug!("Last danmu entry: {:?}", danmus.last());
        log::debug!("Stream start timestamp: {}", stream_start_timestamp_milis);
        log::debug!("Local offset: {}", params.local_offset);
        log::debug!("Range: {:?}", params.ranges);

        // update danmu entry ts to relative offset
        for d in &mut danmus {
            d.ts -= stream_start_timestamp_milis + params.local_offset * 1000;
        }

        let mut range_anchors = vec![0; params.ranges.len()];
        for i in 0..params.ranges.len() {
            if i == 0 {
                continue;
            }
            range_anchors[i] =
                (params.ranges[i - 1].duration() * 1000.0) as i64 + range_anchors[i - 1];
        }

        log::debug!("Range anchors: {:?}", range_anchors);

        let mut filtered_danmus = Vec::<DanmuEntry>::new();
        for (i, range) in params.ranges.iter().enumerate() {
            filtered_danmus.extend(self.filter_danmus_in_range(
                danmus.clone(),
                range,
                range_anchors[i],
            ));
        }

        let ass_content = danmu2ass::danmu_to_ass(
            filtered_danmus,
            self.config.read().await.danmu_ass_options.clone(),
        );
        // dump ass_content into a temp file
        let ass_file_path = clip_file.with_extension("ass");
        if let Err(e) = write(&ass_file_path, ass_content).await {
            log::error!(
                "Failed to write temp ass file: {} {}",
                ass_file_path.display(),
                e
            );
            return Ok(clip_file);
        }

        let result = encode_video_danmu(reporter, &clip_file, &ass_file_path).await;
        // clean ass file
        let _ = remove_file(ass_file_path).await;
        let _ = remove_file(clip_file).await;

        result.map_err(|e| RecorderManagerError::ClipError { err: e })
    }

    fn filter_danmus_in_range(
        &self,
        mut danmus: Vec<DanmuEntry>,
        range: &Range,
        anchor: i64,
    ) -> Vec<DanmuEntry> {
        for d in &mut danmus {
            d.ts -= (range.start * 1000.0) as i64;
        }
        if range.duration() > 0.0 {
            danmus.retain(|x| x.ts >= 0 && x.ts <= (range.duration() * 1000.0).round() as i64);
        }

        for d in &mut danmus {
            d.ts += anchor;
        }

        danmus
    }

    async fn generate_archive_danmu_ass(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<PathBuf, RecorderManagerError> {
        log::info!(
            "Generate archive danmu ass file for {} {} {}",
            platform.as_str(),
            room_id,
            live_id
        );
        let first_segment_timestamp_milis = self
            .first_segment_timestamp(platform, room_id, live_id)
            .await?;
        let mut danmus = self.load_danmus(platform, room_id, live_id).await?;
        danmus.retain(|x| x.ts >= first_segment_timestamp_milis);
        for d in &mut danmus {
            d.ts -= first_segment_timestamp_milis;
        }
        let ass_content =
            danmu2ass::danmu_to_ass(danmus, self.config.read().await.danmu_ass_options.clone());
        let work_dir = CachePath::new(
            self.config.read().await.cache.clone().into(),
            platform,
            room_id,
            live_id,
        );
        let ass_file_path = work_dir.with_filename("danmu.ass");
        if let Err(e) = write(&ass_file_path.full_path(), ass_content).await {
            log::error!(
                "Failed to write archive danmu ass file: {} {}",
                ass_file_path.full_path().display(),
                e
            );
            return Err(RecorderManagerError::ArchiveDanmuAssGenerationFailed {
                error: e.to_string(),
            });
        }
        Ok(ass_file_path.full_path())
    }

    pub async fn get_recorder_list(&self) -> RecorderList {
        let mut summary = RecorderList {
            count: 0,
            recorders: Vec::new(),
        };

        // get recorders from db
        let recorders = self.db.get_recorders().await;
        if recorders.is_err() {
            log::error!(
                "Failed to get recorders from db: {}",
                recorders.err().unwrap()
            );
            return summary;
        }
        let recorders = recorders.unwrap();

        let mut db_map: HashMap<String, RecorderRow> = HashMap::new();
        for recorder in &recorders {
            let key = format!("{}:{}", recorder.platform, recorder.room_id);
            db_map.insert(key, recorder.clone());
        }

        // initialized recorder set
        let mut recorder_set = HashSet::new();
        for recorder_ref in self.recorders.read().await.iter() {
            let recorder_info = recorder_ref.1.info().await;
            let key = format!(
                "{}:{}",
                recorder_info.room_info.platform, recorder_info.room_info.room_id
            );
            summary.recorders.push(recorder_info.clone());
            recorder_set.insert(key);
        }
        summary.count = recorders.len();
        for recorder in recorders {
            // check if recorder is in recorder_set
            let key = format!("{}:{}", recorder.platform, recorder.room_id);
            if !recorder_set.contains(&key) {
                let room_title = recorder
                    .room_title
                    .clone()
                    .unwrap_or_else(|| recorder.room_id.to_string());
                let room_cover = recorder.room_cover.clone().unwrap_or_default();
                let user_name = recorder.user_name.clone().unwrap_or_default();
                let user_avatar = recorder.user_avatar.clone().unwrap_or_default();
                summary.recorders.push(RecorderInfo {
                    platform_live_id: "".to_string(),
                    live_id: "".to_string(),
                    recording: false,
                    enabled: false,
                    room_info: RoomInfo {
                        platform: recorder.platform.as_str().to_string(),
                        status: false,
                        room_id: recorder.room_id.to_string(),
                        room_title,
                        room_cover,
                    },
                    user_info: UserInfo {
                        user_id: "".to_string(),
                        user_name,
                        user_avatar,
                    },
                    resolution: None,
                });
            }
        }

        for recorder in summary.recorders.iter_mut() {
            let key = format!(
                "{}:{}",
                recorder.room_info.platform, recorder.room_info.room_id
            );
            if let Some(db_row) = db_map.get(&key) {
                if (recorder.room_info.platform == "kuaishou"
                    && is_placeholder_name(&recorder.room_info.room_title))
                    || recorder.room_info.room_title.is_empty()
                {
                    if let Some(title) = &db_row.room_title {
                        if !title.trim().is_empty() {
                            recorder.room_info.room_title = title.clone();
                        }
                    }
                }
                if recorder.room_info.room_cover.is_empty() {
                    if let Some(cover) = &db_row.room_cover {
                        recorder.room_info.room_cover = cover.clone();
                    }
                }
                if (recorder.room_info.platform == "kuaishou"
                    && is_placeholder_name(&recorder.user_info.user_name))
                    || recorder.user_info.user_name.is_empty()
                {
                    if let Some(name) = &db_row.user_name {
                        if !name.trim().is_empty() {
                            recorder.user_info.user_name = name.clone();
                        }
                    }
                }
                if recorder.user_info.user_avatar.is_empty() {
                    if let Some(avatar) = &db_row.user_avatar {
                        recorder.user_info.user_avatar = avatar.clone();
                    }
                }
                if recorder.user_info.user_id.is_empty() {
                    match recorder.room_info.platform.as_str() {
                        "douyin" => {
                            if !db_row.extra.is_empty() {
                                recorder.user_info.user_id = db_row.extra.clone();
                            }
                        }
                        "kuaishou" | "tiktok" => {
                            if !recorder.room_info.room_id.is_empty() {
                                recorder.user_info.user_id = recorder.room_info.room_id.clone();
                            }
                        }
                        _ => {}
                    }
                }
            } else if recorder.user_info.user_id.is_empty() {
                match recorder.room_info.platform.as_str() {
                    "kuaishou" | "tiktok" => {
                        if !recorder.room_info.room_id.is_empty() {
                            recorder.user_info.user_id = recorder.room_info.room_id.clone();
                        }
                    }
                    _ => {}
                }
            }
        }

        for recorder in summary.recorders.iter() {
            if recorder.room_info.room_title.is_empty()
                && recorder.room_info.room_cover.is_empty()
                && recorder.user_info.user_name.is_empty()
                && recorder.user_info.user_avatar.is_empty()
            {
                continue;
            }

            let key = format!(
                "{}:{}",
                recorder.room_info.platform, recorder.room_info.room_id
            );
            if let Some(db_row) = db_map.get(&key) {
                let mut changed = false;
                if !recorder.room_info.room_title.is_empty()
                    && db_row.room_title.as_deref().unwrap_or("") != recorder.room_info.room_title
                {
                    changed = true;
                }
                if !recorder.room_info.room_cover.is_empty()
                    && db_row.room_cover.as_deref().unwrap_or("") != recorder.room_info.room_cover
                {
                    changed = true;
                }
                if !recorder.user_info.user_name.is_empty()
                    && db_row.user_name.as_deref().unwrap_or("") != recorder.user_info.user_name
                {
                    changed = true;
                }
                if !recorder.user_info.user_avatar.is_empty()
                    && db_row.user_avatar.as_deref().unwrap_or("") != recorder.user_info.user_avatar
                {
                    changed = true;
                }

                if changed {
                    if let Ok(platform) = PlatformType::from_str(&recorder.room_info.platform) {
                        if let Err(e) = self
                            .db
                            .update_recorder_cached_info(
                                platform,
                                &recorder.room_info.room_id,
                                &recorder.room_info.room_title,
                                &recorder.room_info.room_cover,
                                &recorder.user_info.user_name,
                                &recorder.user_info.user_avatar,
                            )
                            .await
                        {
                            log::warn!(
                                "Failed to update cached recorder info ({} {}): {e}",
                                recorder.room_info.platform,
                                recorder.room_info.room_id
                            );
                        }
                    }
                }
            }
        }

        summary
            .recorders
            .sort_by(|a, b| a.room_info.room_id.cmp(&b.room_info.room_id));
        summary
    }

    pub async fn get_recorder_info(
        &self,
        platform: PlatformType,
        room_id: &str,
    ) -> Option<RecorderInfo> {
        let recorder_id = format!("{}:{}", platform.as_str(), room_id);
        if let Some(recorder_ref) = self.recorders.read().await.get(&recorder_id) {
            let room_info = recorder_ref.info().await;
            Some(room_info)
        } else {
            None
        }
    }

    pub async fn get_archive_disk_usage(&self) -> Result<i64, RecorderManagerError> {
        Ok(self.db.get_record_disk_usage().await?)
    }

    pub async fn get_archives(
        &self,
        room_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<RecordRow>, RecorderManagerError> {
        Ok(self.db.get_records(room_id, offset, limit).await?)
    }

    pub async fn get_archive(
        &self,
        room_id: &str,
        live_id: &str,
    ) -> Result<RecordRow, RecorderManagerError> {
        Ok(self.db.get_record(room_id, live_id).await?)
    }

    pub async fn get_archive_subtitle(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<String, RecorderManagerError> {
        // read subtitle file under work_dir
        let work_dir = CachePath::new(
            self.config.read().await.cache.clone().into(),
            platform,
            room_id,
            live_id,
        );
        let subtitle_file_path = work_dir.with_filename("subtitle.srt");
        let subtitle_file = File::open(subtitle_file_path.full_path()).await;
        if subtitle_file.is_err() {
            return Err(RecorderManagerError::SubtitleNotFound {
                live_id: live_id.to_string(),
            });
        }
        let subtitle_file = subtitle_file.unwrap();
        let mut subtitle_file = BufReader::new(subtitle_file);
        let mut subtitle_content = String::new();
        subtitle_file.read_to_string(&mut subtitle_content).await?;
        Ok(subtitle_content)
    }

    pub async fn generate_archive_subtitle(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<String, RecorderManagerError> {
        // generate subtitle file under work_dir
        let work_dir = CachePath::new(
            self.config.read().await.cache.clone().into(),
            platform,
            room_id,
            live_id,
        );
        let subtitle_file_path = work_dir.with_filename("subtitle.srt");
        let mut subtitle_file = File::create(subtitle_file_path.full_path()).await?;
        // first generate a tmp clip file
        // generate a tmp m3u8 index file
        let m3u8_index_file_path = work_dir.with_filename("tmp.m3u8");
        let mut playlist = self.load_playlist(platform, room_id, live_id).await?;
        playlist.end_list = true;
        playlist.playlist_type = Some(MediaPlaylistType::Vod);

        let mut v: Vec<u8> = Vec::new();
        playlist.write_to(&mut v).unwrap();
        let m3u8_content: &str = std::str::from_utf8(&v).unwrap();
        tokio::fs::write(&m3u8_index_file_path.full_path(), m3u8_content).await?;
        log::info!(
            "[{}]M3U8 index file generated: {}",
            room_id,
            m3u8_index_file_path.full_path().display()
        );
        // generate a tmp clip file
        let clip_file_path = work_dir.with_filename("tmp.mp4");
        if let Err(e) = crate::ffmpeg::playlist::clip_from_playlist(
            None::<&crate::progress::progress_reporter::ProgressReporter>,
            Path::new(&m3u8_index_file_path.full_path()),
            Path::new(&clip_file_path.full_path()),
            None,
        )
        .await
        {
            return Err(RecorderManagerError::SubtitleGenerationFailed {
                error: e.to_string(),
            });
        }
        log::info!("[{}]Temp clip file generated: {}", room_id, clip_file_path);
        // generate subtitle file
        let config = self.config.read().await;
        let result = crate::ffmpeg::generate_video_subtitle(
            None,
            Path::new(&clip_file_path.full_path()),
            "whisper",
            &config.whisper_model,
            &config.whisper_prompt,
            &config.openai_api_key,
            &config.openai_api_endpoint,
            &config.whisper_language,
        )
        .await;
        // write subtitle file
        if let Err(e) = result {
            return Err(RecorderManagerError::SubtitleGenerationFailed {
                error: e.to_string(),
            });
        }
        log::info!("[{room_id}]Subtitle generated");
        let result = result.unwrap();
        let subtitle_content = result
            .subtitle_content
            .iter()
            .map(item_to_srt)
            .collect::<String>();
        subtitle_file.write_all(subtitle_content.as_bytes()).await?;
        log::info!("[{room_id}]Subtitle file written");
        // remove tmp file
        tokio::fs::remove_file(&m3u8_index_file_path.full_path()).await?;
        tokio::fs::remove_file(&clip_file_path.full_path()).await?;
        log::info!("[{room_id}]Tmp file removed");
        Ok(subtitle_content)
    }

    pub async fn delete_archive(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_id: &str,
    ) -> Result<RecordRow, RecorderManagerError> {
        log::info!("Deleting archive {room_id}:{live_id}");
        let to_delete = self.db.remove_record(live_id).await?;
        let cache_folder = Path::new(self.config.read().await.cache.as_str())
            .join(platform.as_str())
            .join(room_id)
            .join(live_id);
        let _ = tokio::fs::remove_dir_all(cache_folder).await;
        Ok(to_delete)
    }

    pub async fn delete_archives(
        &self,
        platform: PlatformType,
        room_id: &str,
        live_ids: &[&str],
    ) -> Result<Vec<RecordRow>, RecorderManagerError> {
        log::info!("Deleting archives in batch: {live_ids:?}");
        let mut to_deletes = Vec::new();
        for live_id in live_ids {
            let to_delete = self.delete_archive(platform, room_id, live_id).await?;
            to_deletes.push(to_delete);
        }
        Ok(to_deletes)
    }

    pub async fn delete_zero_size_archives(&self) -> Result<usize, RecorderManagerError> {
        let records = self.db.get_zero_size_records().await?;
        let mut deleted = 0usize;
        for record in records {
            let platform = match PlatformType::from_str(&record.platform) {
                Ok(platform) => platform,
                Err(_) => continue,
            };
            let _ = self
                .delete_archive(platform, &record.room_id, &record.live_id)
                .await?;
            deleted += 1;
        }
        Ok(deleted)
    }

    pub async fn delete_small_size_archives(
        &self,
        max_size: i64,
    ) -> Result<usize, RecorderManagerError> {
        let records = self.db.get_records_below_size(max_size).await?;
        let mut deleted = 0usize;
        for record in records {
            let platform = match PlatformType::from_str(&record.platform) {
                Ok(platform) => platform,
                Err(_) => continue,
            };
            let _ = self
                .delete_archive(platform, &record.room_id, &record.live_id)
                .await?;
            deleted += 1;
        }
        Ok(deleted)
    }

    pub async fn handle_hls_request(&self, uri: &str) -> Result<Vec<u8>, RecorderManagerError> {
        let cache_path = self.config.read().await.cache.clone();
        let path = uri.split('?').next().unwrap_or(uri);
        let params = uri.split('?').nth(1).unwrap_or("");
        let path_segs: Vec<&str> = path.split('/').collect();

        if path_segs.len() < 4 {
            log::warn!("Invalid request path: {path}");
            return Err(RecorderManagerError::HLSError {
                err: "Invalid hls path".into(),
            });
        }
        // Resolve before branching so room_id / live_id are vetted for playlists too.
        let Some(file_path) = resolve_hls_path(Path::new(&cache_path), path) else {
            log::warn!("Rejected request path outside the cache dir: {path}");
            return Err(RecorderManagerError::HLSError {
                err: "Invalid hls path".into(),
            });
        };
        // parse recorder type
        let platform = path_segs[0];
        // parse room id
        let room_id = path_segs[1];
        // parse live id
        let live_id = path_segs[2];

        let params = Some(params);

        // parse params, example: start=10&end=20
        // start and end are optional
        // split params by &, and then split each param by =
        let params = if let Some(params) = params {
            let params = params
                .split('&')
                .map(|param| param.split('=').collect::<Vec<&str>>())
                .collect::<Vec<Vec<&str>>>();
            Some(params)
        } else {
            None
        };

        let start = if let Some(params) = &params {
            params
                .iter()
                .find(|param| param[0] == "start")
                .map_or(0, |param| param[1].parse::<i64>().unwrap())
        } else {
            0
        };
        let end = if let Some(params) = &params {
            params
                .iter()
                .find(|param| param[0] == "end")
                .map_or(0, |param| param[1].parse::<i64>().unwrap())
        } else {
            0
        };

        let platform = PlatformType::from_str(platform).map_err(|_| {
            RecorderManagerError::InvalidPlatformType {
                platform: platform.to_string(),
            }
        })?;

        let range = if start != 0 || end != 0 {
            Some(Range {
                start: start as f64,
                end: end as f64,
            })
        } else {
            None
        };

        let remaining_path = path_segs[3..].join("/");

        if remaining_path == "playlist.m3u8" || remaining_path.ends_with("/playlist.m3u8") {
            let playlist = match self.load_playlist(platform, room_id, live_id).await {
                Ok(playlist) => playlist,
                Err(RecorderManagerError::IoError(err))
                    if err.kind() == std::io::ErrorKind::NotFound =>
                {
                    if self
                        .wait_for_playlist(platform, room_id, live_id, 2000)
                        .await
                    {
                        self.load_playlist(platform, room_id, live_id).await?
                    } else {
                        return Err(RecorderManagerError::IoError(err));
                    }
                }
                Err(err) => return Err(err),
            };
            let playlist = self.playlist_range(&playlist, range).await?;
            let mut bytes: Vec<u8> = Vec::new();
            playlist.write_to(&mut bytes).unwrap();
            Ok(bytes)
        } else {
            // try to find requested ts file in recorder's cache
            // cache files are stored in {cache_dir}/{platform}/{room_id}/{live_id}/{ts_file}
            tokio::fs::read(&file_path).await.map_err(|e| {
                log::warn!("Failed to read segment file {}: {e}", file_path.display());
                RecorderManagerError::HLSError {
                    err: "Segment file not found".into(),
                }
            })
        }
    }

    pub async fn set_enable(&self, platform: PlatformType, room_id: &str, enabled: bool) {
        // update RecordRow auto_start field
        if let Err(e) = self.db.update_recorder(platform, room_id, enabled).await {
            log::error!("Failed to update recorder auto_start: {e}");
        }

        let recorder_id = format!("{}:{}", platform.as_str(), room_id);
        if let Some(recorder_ref) = self.recorders.read().await.get(&recorder_id) {
            if enabled {
                recorder_ref.enable().await;
            } else {
                recorder_ref.disable().await;
            }
        }
    }

    pub async fn generate_whole_clip(
        &self,
        reporter: Option<&ProgressReporter>,
        encode_danmu: bool,
        delete_cache_after_clip: bool,
        platform: String,
        room_id: &str,
        parent_id: String,
        live_ids: Option<Vec<String>>,
    ) -> Result<(), RecorderManagerError> {
        let platform = PlatformType::from_str(&platform).map_err(|_| {
            RecorderManagerError::InvalidPlatformType {
                platform: platform.to_string(),
            }
        })?;

        let playlists = if let Some(live_ids) = live_ids.as_ref() {
            self.get_related_playlists_by_live_ids(&platform, room_id, live_ids)
                .await
        } else {
            self.get_related_playlists(&platform, room_id, &parent_id)
                .await
        };
        if playlists.is_empty() {
            if let Some(live_ids) = live_ids.as_ref() {
                log::error!("No related playlists found: {room_id} {live_ids:?}");
            } else {
                log::error!("No related playlists found: {parent_id}");
            }
            return Ok(());
        }

        let title = playlists.first().unwrap().title.clone();
        let parent_tag = if let Some(live_ids) = live_ids.as_ref() {
            if !parent_id.is_empty() {
                parent_id.clone()
            } else {
                live_ids.first().cloned().unwrap_or_default()
            }
        } else {
            parent_id.clone()
        };

        // generate archive danmu ass file for all playlists
        let danmu_ass_files = if encode_danmu {
            let danmu_ass_files = playlists
                .iter()
                .map(async |p| {
                    (self
                        .generate_archive_danmu_ass(platform, room_id, &p.live_id)
                        .await)
                        .ok()
                })
                .collect::<Vec<_>>();

            futures::future::join_all(danmu_ass_files).await
        } else {
            vec![None; playlists.len()]
        };

        let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();

        let sanitized_filename = sanitize_filename::sanitize(format!(
            "[full][{platform:?}][{room_id}][{parent_tag}][{timestamp}]{title}.mp4"
        ));
        let output_filename = Path::new(&sanitized_filename);
        let cover_filename = output_filename.with_extension("jpg");

        let output_path =
            Path::new(&self.config.read().await.output.as_str()).join(output_filename);

        let playlists_refs: Vec<&Path> = playlists.iter().map(|p| p.path.as_path()).collect();

        log::info!("Concat playlists: {playlists_refs:?}");
        log::info!("Output path: {output_path:?}");

        if let Err(e) = crate::ffmpeg::playlist::concat_playlists_to_video(
            reporter,
            &playlists_refs,
            danmu_ass_files,
            &output_path,
        )
        .await
        {
            log::error!("Failed to concat playlists: {e}");
            return Err(RecorderManagerError::HLSError {
                err: "Failed to concat playlists".into(),
            });
        }

        let metadata = std::fs::metadata(&output_path);
        if metadata.is_err() {
            return Err(RecorderManagerError::HLSError {
                err: "Failed to get file metadata".into(),
            });
        }
        let size = metadata.unwrap().len() as i64;

        let video_metadata = crate::ffmpeg::extract_video_metadata(Path::new(&output_path)).await;
        let mut length = 0;
        if let Ok(video_metadata) = video_metadata {
            length = video_metadata.duration as i64;
        } else {
            log::error!(
                "Failed to get video metadata: {}",
                video_metadata.err().unwrap()
            );
        }

        let _ = crate::ffmpeg::generate_thumbnail(Path::new(&output_path), 0.0).await;
        let _ = crate::ffmpeg::extract_audio_sample(Path::new(&output_path)).await;

        let video = self
            .db
            .add_video(&VideoRow {
                id: 0,
                status: 0,
                room_id: room_id.to_string(),
                created_at: chrono::Local::now().to_rfc3339(),
                cover: cover_filename.to_string_lossy().to_string(),
                file: output_filename.to_string_lossy().to_string(),
                note: "".into(),
                length,
                size,
                bvid: String::new(),
                title: String::new(),
                desc: String::new(),
                tags: String::new(),
                area: 0,
                platform: platform.as_str().to_string(),
            })
            .await?;

        let event =
            events::new_webhook_event(events::CLIP_GENERATED, events::Payload::Clip(video.clone()));
        if let Err(e) = self.webhook_poster.post_event(&event).await {
            log::error!("Post webhook event error: {e}");
        }

        if delete_cache_after_clip {
            for rl in playlists {
                let live_id = rl.live_id.clone();
                // Remove DB record
                if let Err(e) = self.db.remove_record(&live_id).await {
                    log::error!("[{room_id}][{live_id}] Failed to remove DB record: {}", e);
                }

                // Remove physical files
                let cache_path = self.config.read().await.cache.clone();
                let work_dir =
                    CachePath::new(PathBuf::from(cache_path), platform, room_id, &live_id);
                if let Err(e) = tokio::fs::remove_dir_all(work_dir.full_path()).await {
                    log::error!(
                        "[{room_id}][{live_id}] Failed to remove archive cache dir: {}",
                        e
                    );
                } else {
                    log::info!("[{room_id}][{live_id}] Archive cache deleted successfully");
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod hls_path_tests {
    use super::*;

    #[test]
    fn resolves_segment_paths_inside_the_cache_dir() {
        let cache = Path::new("cache");
        assert_eq!(
            resolve_hls_path(cache, "bilibili/1/2/some_dir/a%7Cb.ts"),
            Some(cache.join("bilibili/1/2/some_dir/a|b.ts"))
        );
    }

    #[test]
    fn rejects_paths_that_could_leave_the_cache_dir() {
        for path in [
            "bilibili/1/2/../../../../secret.txt",
            "bilibili/../2/segment.ts",
            "bilibili/1/2/./segment.ts",
            "bilibili/1//segment.ts",
            "/etc/passwd",
            "bilibili/1/2/.. ",
            "bilibili/1/2/...",
        ] {
            assert_eq!(resolve_hls_path(Path::new("cache"), path), None, "{path}");
        }
    }
}
