use std::str::FromStr;
use url::Url;

use crate::danmu2ass;
use crate::database::record::RecordRow;
use crate::database::recorder::RecorderRow;
use crate::database::task::TaskRow;
use crate::progress::progress_reporter::EventEmitter;
use crate::progress::progress_reporter::ProgressReporter;
use crate::progress::progress_reporter::ProgressReporterTrait;
use crate::recorder_manager::RecorderList;
use crate::state_type;
use crate::task::Task;
use crate::task::TaskPriority;
use crate::webhook::events;
use recorder::account::Account;
use recorder::danmu::DanmuEntry;
use recorder::platforms::bilibili;
use recorder::platforms::douyin;
use recorder::platforms::PlatformType;
use recorder::platforms::{huya, kuaishou, tiktok};
use recorder::RecorderInfo;
use serde::Deserialize;
use serde::Serialize;

fn is_reserved_kuaishou_room_id(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "kuaishou" | "live" | "www" | "u" | "profile" | "home"
    )
}

fn normalize_kuaishou_candidate(value: &str) -> Option<String> {
    let candidate = value
        .trim()
        .trim_start_matches('@')
        .trim_matches('/')
        .trim_end_matches(".html")
        .trim()
        .to_string();
    if candidate.is_empty() {
        return None;
    }
    if candidate.contains("://")
        || candidate.contains('/')
        || candidate.contains('?')
        || candidate.contains('&')
        || candidate.contains('=')
        || is_reserved_kuaishou_room_id(&candidate)
    {
        return None;
    }
    Some(candidate)
}

fn normalize_kuaishou_room_id(room_id: &str) -> String {
    let trimmed = room_id.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some((prefix, suffix)) = trimmed.split_once('#') {
        if prefix.trim().eq_ignore_ascii_case("kuaishou") {
            return normalize_kuaishou_candidate(suffix).unwrap_or_default();
        }
    }

    if let Ok(parsed) = Url::parse(trimmed) {
        for (key, value) in parsed.query_pairs() {
            if key.eq_ignore_ascii_case("principalId")
                || key.eq_ignore_ascii_case("userId")
                || key.eq_ignore_ascii_case("user_id")
            {
                if let Some(normalized) = normalize_kuaishou_candidate(value.as_ref()) {
                    return normalized;
                }
            }
        }

        if let Some(mut segments) = parsed.path_segments() {
            let segs: Vec<String> = segments
                .by_ref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();

            if segs.len() >= 2
                && (segs[0].eq_ignore_ascii_case("u") || segs[0].eq_ignore_ascii_case("profile"))
            {
                if let Some(normalized) = normalize_kuaishou_candidate(&segs[1]) {
                    return normalized;
                }
            }

            if segs.len() == 1 {
                if let Some(normalized) = normalize_kuaishou_candidate(&segs[0]) {
                    return normalized;
                }
            }
        }

        if let Some(fragment) = parsed.fragment() {
            if let Some(normalized) = normalize_kuaishou_candidate(fragment) {
                return normalized;
            }
        }

        return String::new();
    }

    if let Some(query) = trimmed.split('?').nth(1) {
        for pair in query.split('&') {
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None => continue,
            };
            let key = key.trim();
            if !key.eq_ignore_ascii_case("principalId")
                && !key.eq_ignore_ascii_case("userId")
                && !key.eq_ignore_ascii_case("user_id")
            {
                continue;
            }
            if let Some(normalized) = normalize_kuaishou_candidate(value) {
                return normalized;
            }
        }
    }
    normalize_kuaishou_candidate(trimmed).unwrap_or_default()
}

fn normalize_tiktok_room_id(room_id: &str) -> String {
    let trimmed = room_id.trim();
    if let Some(found) = extract_room_id_from_query(trimmed) {
        return found;
    }
    let without_query = trimmed.split('?').next().unwrap_or(trimmed);
    let without_trailing = without_query.trim_end_matches('/');

    if without_trailing.chars().all(|c| c.is_ascii_digit()) {
        return without_trailing.to_string();
    }

    if without_trailing.contains("live.tiktok.com") {
        if let Some(last) = without_trailing.rsplit('/').next() {
            if !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()) {
                return last.to_string();
            }
        }
    }

    if let Some(after_at) = without_trailing.split("/@").nth(1) {
        let name = after_at.split('/').next().unwrap_or("").trim();
        if !name.is_empty() {
            return format!("@{}", name.trim_start_matches('@'));
        }
    }

    if without_trailing.starts_with('@') {
        return without_trailing.to_string();
    }

    if without_trailing.contains("tiktok.com") {
        if let Some(last) = without_trailing.rsplit('/').next() {
            let name = last.trim_start_matches('@');
            if !name.is_empty() && name != "live" {
                return format!("@{}", name);
            }
        }
    }

    if trimmed.is_empty() {
        trimmed.to_string()
    } else {
        format!("@{}", trimmed.trim_start_matches('@'))
    }
}

fn extract_tiktok_feed_override(input: &str) -> Option<(String, String)> {
    if !input.contains("webcast.tiktok.com/webcast/feed")
        && !input.contains("webcast.tiktok.com/webcast/im/fetch")
    {
        return None;
    }
    let parsed = Url::parse(input).ok()?;
    let mut referer = None;
    for (key, value) in parsed.query_pairs() {
        if key == "referer" {
            referer = Some(value.into_owned());
            break;
        }
    }
    let referer = referer?;
    let room_id = normalize_tiktok_room_id(&referer);
    if room_id.is_empty() {
        return None;
    }
    Some((room_id, input.to_string()))
}

fn extract_room_id_from_query(raw: &str) -> Option<String> {
    let query = raw.split('?').nth(1)?;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("room_id") {
            let value = value.trim().trim_matches('/');
            if !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()) {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_recorder_list(state: state_type!()) -> Result<RecorderList, ()> {
    Ok(state.recorder_manager.get_recorder_list().await)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn add_recorder(
    state: state_type!(),
    platform: String,
    room_id: String,
    mut extra: String,
) -> Result<RecorderRow, String> {
    let platform_str = platform;
    let platform = PlatformType::from_str(&platform_str).map_err(|_| "不支持的平台".to_string())?;
    if matches!(platform, PlatformType::Xiaohongshu | PlatformType::Weibo) {
        return Err("平台暂未支持".to_string());
    }
    let mut room_id = room_id;
    if platform == PlatformType::Wechat {
        let input = if extra.trim().is_empty() { &room_id } else { &extra };
        let source = recorder::platforms::wechat::source::WechatSource::parse(input)?;
        room_id = source.room_id();
        extra = source.to_extra()?;
    }
    if platform == PlatformType::Kuaishou {
        let normalized = normalize_kuaishou_room_id(&room_id);
        if normalized != room_id {
            log::info!("Normalized kuaishou room id: {} -> {}", room_id, normalized);
            room_id = normalized;
        }
        if room_id.is_empty() {
            return Err("快手直播间ID无效，请输入 live.kuaishou.com/u/后的ID".to_string());
        }
    }
    if platform == PlatformType::TikTok {
        if let Some((feed_room_id, feed_url)) = extract_tiktok_feed_override(&room_id) {
            log::info!(
                "Using tiktok feed override: {} -> {}",
                room_id,
                feed_room_id
            );
            room_id = feed_room_id;
            extra = feed_url;
        } else if room_id.contains("webcast.tiktok.com/webcast/feed")
            || room_id.contains("webcast.tiktok.com/webcast/im/fetch")
        {
            return Err("TikTok feed URL missing referer".to_string());
        }
        let normalized = normalize_tiktok_room_id(&room_id);
        if normalized != room_id {
            log::info!("Normalized tiktok room id: {} -> {}", room_id, normalized);
            room_id = normalized;
        }
    }
    log::info!("Add recorder: {} {}", platform.as_str(), room_id);
    let account = match platform {
        PlatformType::BiliBili => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                log::error!("No available bilibili account found");
                Err("没有可用账号，请先添加账号".to_string())
            }
        }
        PlatformType::Douyin => {
            let client = reqwest::Client::new();
            let sec_uid = douyin::api::get_room_owner_sec_uid(&client, &room_id)
                .await
                .map_err(|e| e.to_string())?;
            extra = sec_uid;

            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                log::error!("No available douyin account found");
                Err("没有可用账号，请先添加账号".to_string())
            }
        }
        PlatformType::Huya => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Ok(Account::default())
            }
        }
        PlatformType::Kuaishou => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Ok(Account::default())
            }
        }
        PlatformType::TikTok => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Ok(Account::default())
            }
        }
        PlatformType::Wechat => Ok(Account::default()),
        PlatformType::Weibo => Err("微博暂未支持".to_string()),
        PlatformType::Xiaohongshu => Err("小红书暂未支持".to_string()),
        _ => Err("不支持的平台".to_string()),
    };

    match account {
        Ok(account) => match state
            .recorder_manager
            .add_recorder(&account, platform, &room_id, &extra, true)
            .await
        {
            Ok(()) => {
                let room = state.db.add_recorder(platform, &room_id, &extra).await?;
                if let Some(info) = state.recorder_manager.get_recorder_info(platform, &room_id).await
                {
                    if let Err(err) = state
                        .db
                        .update_recorder_cached_info(
                            platform,
                            &room_id,
                            &info.room_info.room_title,
                            &info.room_info.room_cover,
                            &info.user_info.user_name,
                            &info.user_info.user_avatar,
                        )
                        .await
                    {
                        log::warn!(
                            "Failed to persist recorder cached info after add ({} {}): {}",
                            platform.as_str(),
                            room_id,
                            err
                        );
                    }
                }
                state
                    .db
                    .new_message("添加直播间", &format!("添加了新直播间 {room_id}"))
                    .await?;
                // post webhook event
                let event = events::new_webhook_event(
                    events::RECORDER_ADDED,
                    events::Payload::Recorder(room.clone()),
                );
                if let Err(e) = state.webhook_poster.post_event(&event).await {
                    log::error!("Post webhook event error: {e}");
                }
                Ok(room)
            }
            Err(e) => {
                log::error!("Failed to add recorder: {e}");
                Err(format!("添加失败: {e}"))
            }
        },
        Err(e) => {
            log::error!("Failed to add recorder: {e}");
            Err(format!("添加失败: {e}"))
        }
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remove_recorder(
    state: state_type!(),
    platform: String,
    room_id: String,
) -> Result<(), String> {
    log::info!("Remove recorder: {platform} {room_id}");
    let platform = PlatformType::from_str(&platform).unwrap();
    match state
        .recorder_manager
        .remove_recorder(platform, &room_id)
        .await
    {
        Ok(recorder) => {
            state
                .db
                .new_message("移除直播间", &format!("移除了直播间 {room_id}"))
                .await?;
            // post webhook event
            let event = events::new_webhook_event(
                events::RECORDER_REMOVED,
                events::Payload::Recorder(recorder),
            );
            if let Err(e) = state.webhook_poster.post_event(&event).await {
                log::error!("Post webhook event error: {e}");
            }
            log::info!("Removed recorder: {} {}", platform.as_str(), room_id);
            Ok(())
        }
        Err(e) => {
            log::error!("Failed to remove recorder: {e}");
            Err(e.to_string())
        }
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn reload_recorder(
    state: state_type!(),
    platform: String,
    room_id: String,
) -> Result<(), String> {
    log::info!("Reload recorder: {platform} {room_id}");
    let platform = PlatformType::from_str(&platform).map_err(|_| "Invalid platform".to_string())?;
    if let Some(remaining) = state
        .recorder_manager
        .check_reload_cooldown(platform, &room_id)
        .await
    {
        let secs = remaining.as_secs().max(1);
        let platform_name = platform.as_str();
        log::warn!(
            "Skip reload during cooldown: {} {} ({}s remaining)",
            platform_name,
            room_id,
            secs
        );
        return Err(format!("重载过于频繁，请 {secs} 秒后再试"));
    }
    let removed = state
        .recorder_manager
        .remove_recorder(platform, &room_id)
        .await
        .map_err(|e| e.to_string())?;
    let extra = removed.extra.clone();
    let auto_start = removed.auto_start;

    let account = match platform {
        PlatformType::BiliBili => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Err("没有可用账号，请先添加账号".to_string())
            }
        }
        PlatformType::Douyin => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Err("没有可用账号，请先添加账号".to_string())
            }
        }
        PlatformType::Huya => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Ok(Account::default())
            }
        }
        PlatformType::Kuaishou => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Ok(Account::default())
            }
        }
        PlatformType::TikTok => {
            if let Ok(Some(account)) = state.recorder_manager.select_account_for_platform(platform).await
            {
                Ok(account)
            } else {
                Ok(Account::default())
            }
        }
        PlatformType::Wechat => Ok(Account::default()),
        PlatformType::Weibo => Err("微博暂未支持".to_string()),
        PlatformType::Xiaohongshu => Err("小红书暂未支持".to_string()),
        _ => Err("不支持的平台".to_string()),
    };
    let account = match account {
        Ok(account) => account,
        Err(e) => {
            let _ = state.db.add_recorder(platform, &room_id, &extra).await;
            let _ = state
                .db
                .update_recorder(platform, &room_id, auto_start)
                .await;
            let _ = state
                .db
                .update_recorder_cached_info(
                    platform,
                    &room_id,
                    removed.room_title.as_deref().unwrap_or(""),
                    removed.room_cover.as_deref().unwrap_or(""),
                    removed.user_name.as_deref().unwrap_or(""),
                    removed.user_avatar.as_deref().unwrap_or(""),
                )
                .await;
            return Err(e);
        }
    };

    let add_res = state
        .recorder_manager
        .add_recorder(&account, platform, &room_id, &extra, auto_start)
        .await;

    match add_res {
        Ok(()) => {
            let _ = state.db.add_recorder(platform, &room_id, &extra).await?;
            if !auto_start {
                let _ = state
                    .db
                    .update_recorder(platform, &room_id, auto_start)
                    .await;
            }
            let _ = state
                .db
                .update_recorder_cached_info(
                    platform,
                    &room_id,
                    removed.room_title.as_deref().unwrap_or(""),
                    removed.room_cover.as_deref().unwrap_or(""),
                    removed.user_name.as_deref().unwrap_or(""),
                    removed.user_avatar.as_deref().unwrap_or(""),
                )
                .await;
            state
                .db
                .new_message("重载直播间", &format!("重载了直播间 {room_id}"))
                .await
                .ok();
            Ok(())
        }
        Err(e) => {
            let _ = state.db.add_recorder(platform, &room_id, &extra).await;
            let _ = state
                .db
                .update_recorder(platform, &room_id, auto_start)
                .await;
            let _ = state
                .db
                .update_recorder_cached_info(
                    platform,
                    &room_id,
                    removed.room_title.as_deref().unwrap_or(""),
                    removed.room_cover.as_deref().unwrap_or(""),
                    removed.user_name.as_deref().unwrap_or(""),
                    removed.user_avatar.as_deref().unwrap_or(""),
                )
                .await;
            Err(format!("重载失败: {e}"))
        }
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_room_info(
    state: state_type!(),
    platform: String,
    room_id: String,
) -> Result<RecorderInfo, String> {
    let platform = PlatformType::from_str(&platform).unwrap();
    if let Some(info) = state
        .recorder_manager
        .get_recorder_info(platform, &room_id)
        .await
    {
        if let Err(err) = state
            .db
            .update_recorder_cached_info(
                platform,
                &room_id,
                &info.room_info.room_title,
                &info.room_info.room_cover,
                &info.user_info.user_name,
                &info.user_info.user_avatar,
            )
            .await
        {
            log::warn!(
                "Failed to update recorder cached info for {} {}: {}",
                platform.as_str(),
                room_id,
                err
            );
        }
        Ok(info)
    } else {
        match state.db.get_recorder(platform, &room_id).await {
            Ok(recorder) => Ok(RecorderInfo {
                room_info: recorder::RoomInfo {
                    platform: recorder.platform.clone(),
                    room_id: recorder.room_id.clone(),
                    room_title: recorder.room_title.unwrap_or_default(),
                    room_cover: recorder.room_cover.unwrap_or_default(),
                    status: false,
                },
                user_info: recorder::UserInfo {
                    user_id: recorder.room_id.clone(),
                    user_name: recorder.user_name.unwrap_or_default(),
                    user_avatar: recorder.user_avatar.unwrap_or_default(),
                },
                platform_live_id: String::new(),
                live_id: String::new(),
                recording: false,
                enabled: recorder.auto_start,
                resolution: None,
            }),
            Err(_) => Err("Not found".to_string()),
        }
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_archive_disk_usage(state: state_type!()) -> Result<i64, String> {
    Ok(state.recorder_manager.get_archive_disk_usage().await?)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_archives(
    state: state_type!(),
    room_id: String,
    offset: i64,
    limit: i64,
) -> Result<Vec<RecordRow>, String> {
    Ok(state
        .recorder_manager
        .get_archives(&room_id, offset, limit)
        .await?)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_archive(
    state: state_type!(),
    room_id: String,
    live_id: String,
) -> Result<RecordRow, String> {
    Ok(state
        .recorder_manager
        .get_archive(&room_id, &live_id)
        .await?)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_archives_by_parent_id(
    state: state_type!(),
    room_id: String,
    parent_id: String,
) -> Result<Vec<RecordRow>, String> {
    Ok(state
        .db
        .get_archives_by_parent_id(&room_id, &parent_id)
        .await?)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_archive_subtitle(
    state: state_type!(),
    platform: String,
    room_id: String,
    live_id: String,
) -> Result<String, String> {
    let platform = PlatformType::from_str(&platform)?;
    Ok(state
        .recorder_manager
        .get_archive_subtitle(platform, &room_id, &live_id)
        .await?)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn generate_archive_subtitle(
    state: state_type!(),
    platform: String,
    room_id: String,
    live_id: String,
) -> Result<String, String> {
    let platform = PlatformType::from_str(&platform)?;
    Ok(state
        .recorder_manager
        .generate_archive_subtitle(platform, &room_id, &live_id)
        .await?)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn delete_archive(
    state: state_type!(),
    platform: String,
    room_id: String,
    live_id: String,
) -> Result<(), String> {
    let platform = PlatformType::from_str(&platform)?;
    let to_delete = state
        .recorder_manager
        .delete_archive(platform, &room_id, &live_id)
        .await?;
    state
        .db
        .new_message(
            "删除历史缓存",
            &format!("删除了房间 {room_id} 的历史缓存 {live_id}"),
        )
        .await?;
    // post webhook event
    let event =
        events::new_webhook_event(events::ARCHIVE_DELETED, events::Payload::Archive(to_delete));
    if let Err(e) = state.webhook_poster.post_event(&event).await {
        log::error!("Post webhook event error: {e}");
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn delete_archives(
    state: state_type!(),
    platform: String,
    room_id: String,
    live_ids: Vec<String>,
) -> Result<(), String> {
    let platform = PlatformType::from_str(&platform)?;
    let to_deletes = state
        .recorder_manager
        .delete_archives(
            platform,
            &room_id,
            &live_ids
                .iter()
                .map(std::string::String::as_str)
                .collect::<Vec<&str>>(),
        )
        .await?;
    state
        .db
        .new_message(
            "删除历史缓存",
            &format!("删除了房间 {} 的历史缓存 {}", room_id, live_ids.join(", ")),
        )
        .await?;
    for to_delete in to_deletes {
        // post webhook event
        let event =
            events::new_webhook_event(events::ARCHIVE_DELETED, events::Payload::Archive(to_delete));
        if let Err(e) = state.webhook_poster.post_event(&event).await {
            log::error!("Post webhook event error: {e}");
        }
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_danmu_record(
    state: state_type!(),
    platform: String,
    room_id: String,
    live_id: String,
) -> Result<Vec<DanmuEntry>, String> {
    let platform = PlatformType::from_str(&platform)?;
    Ok(state
        .recorder_manager
        .load_danmus(platform, &room_id, &live_id)
        .await?)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDanmuOptions {
    platform: String,
    room_id: String,
    live_id: String,
    x: i64,
    y: i64,
    ass: bool,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn export_danmu(
    state: state_type!(),
    options: ExportDanmuOptions,
) -> Result<String, String> {
    let platform = PlatformType::from_str(&options.platform)?;
    let mut danmus = state
        .recorder_manager
        .load_danmus(platform, &options.room_id, &options.live_id)
        .await?;

    log::debug!("First danmu entry: {:?}", danmus.first());
    // update entry ts to offset
    for d in &mut danmus {
        d.ts -= (options.x + options.y) * 1000;
    }
    if options.x != 0 || options.y != 0 {
        danmus.retain(|e| e.ts >= 0 && e.ts <= (options.y - options.x) * 1000);
    }

    if options.ass {
        Ok(danmu2ass::danmu_to_ass(
            danmus,
            danmu2ass::Danmu2AssOptions::default(),
        ))
    } else {
        // map and join entries
        Ok(danmus
            .iter()
            .map(|e| format!("{}:{}", e.ts, e.content))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn send_danmaku(
    state: state_type!(),
    uid: String,
    room_id: String,
    message: String,
) -> Result<(), String> {
    let account = state.db.get_account("bilibili", &uid).await?;
    let client = reqwest::Client::new();
    match bilibili::api::send_danmaku(&client, &account.to_account(), &room_id, &message).await {
        Ok(()) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn delete_zero_size_archives(state: state_type!()) -> Result<usize, String> {
    match state.recorder_manager.delete_zero_size_archives().await {
        Ok(count) => Ok(count),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn delete_small_size_archives(
    state: state_type!(),
    max_size: i64,
) -> Result<usize, String> {
    match state
        .recorder_manager
        .delete_small_size_archives(max_size)
        .await
    {
        Ok(count) => Ok(count),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_total_length(state: state_type!()) -> Result<f64, String> {
    match state.db.get_total_length().await {
        Ok(total_length) => Ok(total_length),
        Err(e) => Err(format!("Failed to get total length: {e}")),
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_today_record_count(state: state_type!()) -> Result<i64, String> {
    match state.db.get_today_record_count().await {
        Ok(count) => Ok(count),
        Err(e) => Err(format!("Failed to get today record count: {e}")),
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_recent_record(
    state: state_type!(),
    room_id: String,
    offset: i64,
    limit: i64,
) -> Result<Vec<RecordRow>, String> {
    match state.db.get_recent_record(&room_id, offset, limit).await {
        Ok(records) => Ok(records),
        Err(e) => Err(format!("Failed to get recent record: {e}")),
    }
}

#[derive(serde::Serialize)]
pub struct RefreshRoomInfoResult {
    pub rooms_checked: usize,
    pub records_updated: usize,
    pub rooms_failed: usize,
}

fn build_account_from_config(config: &crate::config::Config, platform: &str) -> Account {
    let mut cookies = String::new();
    if config.use_login_accounts {
        if let Some(entry) = config
            .login_accounts
            .iter()
            .find(|entry| entry.platform == platform && !entry.cookies.trim().is_empty())
        {
            cookies = entry.cookies.clone();
        }
    }
    if cookies.is_empty() && config.use_guest_accounts {
        if let Some(entry) = config
            .guest_accounts
            .iter()
            .find(|entry| entry.platform == platform && !entry.cookies.trim().is_empty())
        {
            cookies = entry.cookies.clone();
        }
    }
    Account {
        platform: platform.to_string(),
        cookies,
        ..Account::default()
    }
}

fn is_unknown_title(title: &str) -> bool {
    let trimmed = title.trim();
    trimmed.is_empty() || trimmed.starts_with("UnknownLive")
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn refresh_archive_room_info(
    state: state_type!(),
) -> Result<RefreshRoomInfoResult, String> {
    let config_snapshot = state.config.read().await.clone();
    let client = reqwest::Client::new();
    let rooms = state
        .db
        .get_distinct_record_rooms()
        .await
        .map_err(|e| e.to_string())?;

    let mut rooms_checked = 0usize;
    let mut rooms_failed = 0usize;
    let mut records_updated = 0usize;

    for (platform, room_id) in rooms {
        let platform_lower = platform.to_lowercase();
        let account = build_account_from_config(&config_snapshot, &platform_lower);

        let room_info = match platform_lower.as_str() {
            "bilibili" => bilibili::api::get_room_info(&client, &account, &room_id)
                .await
                .ok()
                .map(|info| recorder::RoomInfo {
                    platform: "bilibili".to_string(),
                    room_id: room_id.clone(),
                    room_title: info.room_title,
                    room_cover: info.room_cover_url,
                    status: info.live_status == 1,
                }),
            "huya" => huya::api::get_room_info(&client, &account, &room_id)
                .await
                .ok()
                .map(|(_user, room, _stream)| room),
            "kuaishou" => {
                let url = if room_id.starts_with("http") {
                    room_id.clone()
                } else {
                    format!("https://live.kuaishou.com/u/{room_id}")
                };
                kuaishou::api::get_room_info(&client, &account, &url)
                    .await
                    .ok()
                    .map(|info| recorder::RoomInfo {
                        platform: "kuaishou".to_string(),
                        room_id: room_id.clone(),
                        room_title: info.room_title,
                        room_cover: info.room_cover_url,
                        status: info.live_status,
                    })
            }
            "tiktok" => {
                let url = if room_id.starts_with("http") {
                    room_id.clone()
                } else if room_id.starts_with("@") {
                    format!("https://www.tiktok.com/{room_id}")
                } else {
                    format!("https://www.tiktok.com/@{room_id}")
                };
                tiktok::api::get_room_info(&client, &account, &url)
                    .await
                    .ok()
                    .map(|info| recorder::RoomInfo {
                        platform: "tiktok".to_string(),
                        room_id: room_id.clone(),
                        room_title: info.room_title,
                        room_cover: info.room_cover_url,
                        status: info.live_status,
                    })
            }
            _ => None,
        };

        rooms_checked += 1;
        let Some(room_info) = room_info else {
            rooms_failed += 1;
            continue;
        };

        let records = state
            .db
            .get_records_by_room_platform(&platform, &room_id)
            .await
            .map_err(|e| e.to_string())?;
        for record in records {
            let mut updated = false;
            if is_unknown_title(&record.title) && !room_info.room_title.trim().is_empty() {
                state
                    .db
                    .update_record_title(&record.live_id, &room_info.room_title)
                    .await
                    .ok();
                updated = true;
            }
            if record.cover.is_none() && !room_info.room_cover.trim().is_empty() {
                state
                    .db
                    .update_record_cover(&record.live_id, Some(room_info.room_cover.clone()))
                    .await
                    .ok();
                updated = true;
            }
            if updated {
                records_updated += 1;
            }
        }
    }

    Ok(RefreshRoomInfoResult {
        rooms_checked,
        records_updated,
        rooms_failed,
    })
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn set_enable(
    state: state_type!(),
    platform: String,
    room_id: String,
    enabled: bool,
) -> Result<(), String> {
    log::info!("Set enable for recorder {platform} {room_id} {enabled}");
    let platform = PlatformType::from_str(&platform)?;
    state
        .recorder_manager
        .set_enable(platform, &room_id, enabled)
        .await;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fetch_hls(state: state_type!(), uri: String) -> Result<Vec<u8>, String> {
    // Handle wildcard pattern in the URI
    let uri = if uri.contains("/hls/") {
        uri.split("/hls/").last().unwrap_or(&uri).to_string()
    } else {
        uri
    };
    state
        .recorder_manager
        .handle_hls_request(&uri)
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn generate_whole_clip(
    state: state_type!(),
    encode_danmu: bool,
    delete_cache_after_clip: bool,
    platform: String,
    room_id: String,
    parent_id: String,
    live_ids: Option<Vec<String>>,
) -> Result<TaskRow, String> {
    log::info!("Generate whole clip for {platform} {room_id} {parent_id}, delete_cache={delete_cache_after_clip}");

    let task = state
        .db
        .generate_task(
            "generate_whole_clip",
            "",
            &serde_json::json!({
                "platform": platform,
                "room_id": room_id,
                "parent_id": parent_id,
                "live_ids": live_ids,
                "encode_danmu": encode_danmu,
                "delete_cache_after_clip": delete_cache_after_clip,
            })
            .to_string(),
        )
        .await?;

    #[cfg(feature = "gui")]
    let emitter = EventEmitter::new(state.app_handle.clone());
    #[cfg(feature = "headless")]
    let emitter = EventEmitter::new(state.progress_manager.get_event_sender());
    let reporter = ProgressReporter::new(state.db.clone(), &emitter, &task.id).await?;

    log::info!("Create task: {} {}", task.id, task.task_type);
    // create a tokio task to run in background
    #[cfg(feature = "gui")]
    let state_clone = (*state).clone();
    #[cfg(feature = "headless")]
    let state_clone = state.clone();

    let task_id = task.id.clone();
    state
        .task_manager
        .add_task(Task::new(
            task_id.clone(),
            TaskPriority::Normal,
            async move {
                match state_clone
                    .recorder_manager
                    .generate_whole_clip(
                        Some(&reporter),
                        encode_danmu,
                        delete_cache_after_clip,
                        platform,
                        &room_id,
                        parent_id,
                        live_ids,
                    )
                    .await
                {
                    Ok(()) => {
                        reporter.finish(true, "切片生成完成").await;
                        let _ = state_clone
                            .db
                            .update_task(&task_id, "success", "切片生成完成", None)
                            .await;
                        Ok(())
                    }
                    Err(e) => {
                        reporter.finish(false, &format!("切片生成失败: {e}")).await;
                        let _ = state_clone
                            .db
                            .update_task(&task_id, "failed", &format!("切片生成失败: {e}"), None)
                            .await;
                        Err(format!("切片生成失败: {e}"))
                    }
                }
            },
        ))
        .await?;
    Ok(task)
}

/// Fix cover paths for existing archives that have cover.jpg files but no database entry
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fix_archive_covers(state: state_type!()) -> Result<usize, String> {
    use recorder::platforms::PlatformType;
    use std::path::PathBuf;

    log::info!("Starting to fix archive covers...");
    let mut fixed_count = 0;

    // Get all recorders to know which rooms to check
    let recorders = state.db.get_recorders().await.map_err(|e| e.to_string())?;

    for recorder_row in recorders {
        let platform = PlatformType::from_str(&recorder_row.platform).map_err(|e| e.to_string())?;
        let room_id = &recorder_row.room_id;

        // Get all records for this room
        let records = state
            .db
            .get_records(room_id, 0, 1000)
            .await
            .map_err(|e| e.to_string())?;

        for record in records {
            // Skip if already has cover
            if record.cover.is_some() {
                continue;
            }

            // Check if cover.jpg exists in cache directory
            let cache_path = PathBuf::from(&state.config.read().await.cache);
            let cover_path = cache_path.join(format!(
                "{}/{}/{}/cover.jpg",
                platform.as_str(),
                room_id,
                record.live_id
            ));

            if cover_path.exists() {
                // Update database with cover path
                let cover_db_path = format!(
                    "{}/{}/{}/cover.jpg",
                    platform.as_str(),
                    room_id,
                    record.live_id
                );

                match state
                    .db
                    .update_record_cover(&record.live_id, Some(cover_db_path.clone()))
                    .await
                {
                    Ok(_) => {
                        log::info!(
                            "Fixed cover for {}/{}: {}",
                            platform.as_str(),
                            room_id,
                            record.live_id
                        );
                        fixed_count += 1;
                    }
                    Err(e) => {
                        log::error!("Failed to update cover for {}: {}", record.live_id, e);
                    }
                }
            }
        }
    }

    log::info!("Fixed {} archive covers", fixed_count);
    Ok(fixed_count)
}
