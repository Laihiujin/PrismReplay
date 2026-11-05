use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use base64::Engine;

use crate::database::Database;
use crate::recorder_manager::RecorderManagerError;
use m3u8_rs::parse_media_playlist;
use recorder::entry::EntryStore;
use recorder::platforms::PlatformType;

fn is_safe_path_component(value: &str) -> bool {
    if value.is_empty() || value.ends_with(' ') || value.ends_with('.') {
        return false;
    }
    !value
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
}

fn is_disabled_platform(platform: &str) -> bool {
    matches!(platform, "xiaohongshu" | "weibo")
}

async fn compute_record_stats(
    record_path: &PathBuf,
) -> Result<(f64, u64), Box<dyn std::error::Error>> {
    let mut duration = 0.0f64;
    let mut size = 0u64;

    let entry_file = record_path.join("entries.log");
    if entry_file.exists() {
        if let Some(path_str) = record_path.to_str() {
            let entry_store = EntryStore::new(path_str).await;
            if !entry_store.is_empty() {
                duration = entry_store.total_duration();
                size = entry_store.total_size();
            }
        }
    }

    if duration <= 0.0 {
        for playlist_name in ["playlist.m3u8", "tmp.m3u8"] {
            let playlist_path = record_path.join(playlist_name);
            if !playlist_path.exists() {
                continue;
            }
            if let Ok(bytes) = tokio::fs::read(&playlist_path).await {
                if let Ok((_, playlist)) = parse_media_playlist(&bytes) {
                    duration = playlist
                        .segments
                        .iter()
                        .map(|s| s.duration as f64)
                        .sum::<f64>();
                    if duration > 0.0 {
                        break;
                    }
                }
            }
        }
    }

    if size == 0 {
        let mut stack = vec![record_path.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let file_type = entry.file_type().await?;
                if file_type.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let name = match entry.file_name().to_str() {
                    Some(name) => name.to_lowercase(),
                    None => continue,
                };
                if name == "playlist.m3u8"
                    || name == "entries.log"
                    || name == "tmp.m3u8"
                    || name == "cover.jpg"
                    || name.ends_with(".ass")
                    || name.ends_with(".srt")
                    || name.ends_with(".txt")
                    || name.ends_with(".json")
                    || name.ends_with(".jpg")
                    || name.ends_with(".png")
                {
                    continue;
                }
                let meta = entry.metadata().await?;
                size = size.saturating_add(meta.len());
            }
        }
    }

    Ok((duration, size))
}

pub async fn try_rebuild_archives_from_cache_scan(
    db: &Arc<Database>,
    cache_path: PathBuf,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut entries = match tokio::fs::read_dir(&cache_path).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(err) => return Err(err.into()),
    };
    while let Some(platform_entry) = entries.next_entry().await? {
        if !platform_entry.file_type().await?.is_dir() {
            continue;
        }
        let platform_name = match platform_entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        if is_disabled_platform(&platform_name) {
            continue;
        }
        let platform = match PlatformType::from_str(platform_name.as_str()) {
            Ok(platform) => platform,
            Err(_) => continue,
        };
        let mut room_dirs = tokio::fs::read_dir(platform_entry.path()).await?;
        while let Some(room_entry) = room_dirs.next_entry().await? {
            if !room_entry.file_type().await?.is_dir() {
                continue;
            }
            let room_id = match room_entry.file_name().to_str() {
                Some(id) => id.to_string(),
                None => continue,
            };
            if !is_safe_path_component(&room_id) {
                log::warn!("Skip rebuild archives for unsafe room id: {}", room_id);
                continue;
            }
            let mut live_dirs = tokio::fs::read_dir(room_entry.path()).await?;
            while let Some(live_entry) = live_dirs.next_entry().await? {
                if !live_entry.file_type().await?.is_dir() {
                    continue;
                }
                let live_id = match live_entry.file_name().to_str() {
                    Some(id) => id.to_string(),
                    None => continue,
                };
                if !is_safe_path_component(&live_id) {
                    continue;
                }
                let mut record = db.get_record(&room_id, &live_id).await.ok();
                if record.is_none() {
                    record = Some(
                        db.add_record(
                            platform,
                            &live_id,
                            &live_id,
                            &room_id,
                            &format!("UnknownLive {live_id}"),
                            None,
                        )
                        .await?,
                    );
                    added += 1;
                }

                let (mut duration, mut size) = compute_record_stats(&live_entry.path()).await?;
                if let Some(existing) = record.as_ref() {
                    if duration <= 0.0 {
                        duration = existing.length;
                    }
                    if size == 0 && existing.size > 0 {
                        size = existing.size as u64;
                    }
                    if (duration > 0.0 || size > 0)
                        && (existing.length != duration || existing.size != size as i64)
                    {
                        db.update_record_stats(&live_id, duration, size).await?;
                        updated += 1;
                    }
                } else if duration > 0.0 || size > 0 {
                    db.update_record_stats(&live_id, duration, size).await?;
                    updated += 1;
                }
            }
        }
    }
    Ok((added, updated))
}

pub async fn try_rebuild_archives(
    db: &Arc<Database>,
    cache_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let rooms = db.get_recorders().await?;
    for room in rooms {
        if is_disabled_platform(&room.platform) {
            continue;
        }
        let room_id = room.room_id;
        if !is_safe_path_component(&room_id) {
            log::warn!("Skip rebuild archives for unsafe room id: {}", room_id);
            continue;
        }
        let room_cache_path = cache_path.join(format!("{}/{}", room.platform, room_id));
        match tokio::fs::metadata(&room_cache_path).await {
            Ok(meta) => {
                if !meta.is_dir() {
                    continue;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(err) => return Err(err.into()),
        }
        let mut files = tokio::fs::read_dir(&room_cache_path).await?;
        while let Some(file) = files.next_entry().await? {
            if file.file_type().await?.is_dir() {
                // use folder name as live_id
                let live_id = file.file_name();
                let live_id = live_id.to_str().unwrap();
                if !is_safe_path_component(live_id) {
                    continue;
                }
                let record_path = file.path();
                let entry_store = EntryStore::new(record_path.to_string_lossy().as_ref()).await;
                let existing_record = db.get_record(&room_id, live_id).await;

                // Empty folders are left behind when recording fails before the first segment.
                // They are not archives and must not be restored as 0-byte records.
                if entry_store.is_empty() {
                    match existing_record {
                        Ok(record) if record.size == 0 => {
                            db.remove_record(live_id).await?;
                            tokio::fs::remove_dir_all(&record_path).await?;
                            log::info!("removed empty archive folder: {}", record_path.display());
                        }
                        Ok(_) => {
                            log::warn!(
                                "archive {} has database data but no cached entries; preserving it",
                                record_path.display()
                            );
                        }
                        Err(_) => {
                            tokio::fs::remove_dir_all(&record_path).await?;
                            log::info!("removed empty archive folder: {}", record_path.display());
                        }
                    }
                    continue;
                }

                // check if live_id is in db
                if let Ok(record) = existing_record {
                    if record.size == 0 {
                        db.update_record_delta(
                            live_id,
                            entry_store.total_duration(),
                            entry_store.total_size(),
                        )
                        .await?;
                    }
                    continue;
                }

                // create a record for this live_id
                let record = db
                    .add_record(
                        PlatformType::from_str(room.platform.as_str()).map_err(|_| {
                            RecorderManagerError::InvalidPlatformType {
                                platform: room.platform.to_string(),
                            }
                        })?,
                        live_id,
                        live_id,
                        &room_id,
                        &format!("UnknownLive {live_id}"),
                        None,
                    )
                    .await?;
                db.update_record_delta(
                    live_id,
                    entry_store.total_duration(),
                    entry_store.total_size(),
                )
                .await?;

                log::info!("rebuild archive {record:?}");
            }
        }
    }
    Ok(())
}

pub async fn try_convert_live_covers(
    db: &Arc<Database>,
    cache_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let rooms = db.get_recorders().await?;
    for room in rooms {
        if is_disabled_platform(&room.platform) {
            continue;
        }
        let room_id = room.room_id;
        if !is_safe_path_component(&room_id) {
            log::warn!("Skip convert live covers for unsafe room id: {}", room_id);
            continue;
        }
        let room_cache_path = cache_path.join(format!("{}/{}", room.platform, room_id));
        let records = db.get_records(&room_id, 0, 999_999_999).await?;
        for record in &records {
            let record_path = room_cache_path.join(record.live_id.clone());
            if !is_safe_path_component(&record.live_id) {
                continue;
            }
            let cover = record.cover.clone();
            if cover.is_none() {
                continue;
            }

            let cover = cover.unwrap();
            if cover.starts_with("data:") {
                let base64 = cover.split("base64,").nth(1).unwrap();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(base64)
                    .unwrap();
                let path = record_path.join("cover.jpg");
                tokio::fs::write(&path, bytes).await?;

                log::info!("convert live cover: {}", path.display());
                // update record
                db.update_record_cover(
                    record.live_id.as_str(),
                    Some(format!(
                        "{}/{}/{}/cover.jpg",
                        room.platform, room_id, record.live_id
                    )),
                )
                .await?;
            }
        }
    }
    Ok(())
}

pub async fn try_convert_clip_covers(
    db: &Arc<Database>,
    output_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let videos = db.get_all_videos().await?;
    log::debug!("videos: {}", videos.len());
    for video in &videos {
        let cover = video.cover.clone();
        if cover.starts_with("data:") {
            let base64 = cover.split("base64,").nth(1).unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(base64)
                .unwrap();

            let video_file_path = output_path.join(video.file.clone());
            let cover_file_path = video_file_path.with_extension("jpg");
            log::debug!("cover_file_path: {}", cover_file_path.display());
            tokio::fs::write(&cover_file_path, bytes).await?;

            log::info!("convert clip cover: {}", cover_file_path.display());
            // update record
            db.update_video_cover(
                video.id,
                cover_file_path.file_name().unwrap().to_str().unwrap(),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn try_add_parent_id_to_records(
    db: &Arc<Database>,
) -> Result<(), Box<dyn std::error::Error>> {
    let rooms = db.get_recorders().await?;
    for room in &rooms {
        if is_disabled_platform(&room.platform) {
            continue;
        }
        let records = db.get_records(&room.room_id, 0, 999_999_999).await?;
        for record in &records {
            if record.parent_id.is_empty() {
                db.update_record_parent_id(record.live_id.as_str(), record.live_id.as_str())
                    .await?;
            }
        }
    }
    Ok(())
}

pub async fn try_convert_entry_to_m3u8(
    db: &Arc<Database>,
    cache_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let rooms = db.get_recorders().await?;
    for room in &rooms {
        if is_disabled_platform(&room.platform) {
            continue;
        }
        if !is_safe_path_component(&room.room_id) {
            log::warn!(
                "Skip convert entry to m3u8 for unsafe room id: {}",
                room.room_id
            );
            continue;
        }
        let records = db.get_records(&room.room_id, 0, 999_999_999).await?;
        for record in &records {
            if !is_safe_path_component(&record.live_id) {
                continue;
            }
            let record_path = cache_path.join(format!(
                "{}/{}/{}",
                room.platform, room.room_id, record.live_id
            ));
            let entry_file = record_path.join("entries.log");
            let m3u8_file_path = record_path.join("playlist.m3u8");
            if !entry_file.exists() || m3u8_file_path.exists() {
                continue;
            }
            let entry_store = EntryStore::new(record_path.to_str().unwrap()).await;
            if entry_store.is_empty() {
                continue;
            }
            let m3u8_content = entry_store.manifest(true, true, None);

            tokio::fs::write(&m3u8_file_path, m3u8_content).await?;
            log::info!(
                "Convert entry to m3u8: {} => {}",
                entry_file.display(),
                m3u8_file_path.display()
            );
        }
    }

    Ok(())
}
