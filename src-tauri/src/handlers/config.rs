use crate::config::Config;
#[cfg(feature = "headless")]
use crate::constants::API_PORT;
use crate::danmu2ass::Danmu2AssOptions;
use crate::state_type;
use recorder::platforms::PlatformType;
#[cfg(feature = "gui")]
use tauri::Manager;

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_config(state: state_type!()) -> Result<Config, ()> {
    let mut config = state.config.read().await.clone();
    for entry in &mut config.guest_accounts {
        entry.cookies = String::new();
    }
    for entry in &mut config.login_accounts {
        entry.cookies = String::new();
    }
    Ok(config)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_static_port(_state: state_type!()) -> Result<u16, ()> {
    #[cfg(feature = "headless")]
    {
        Ok(API_PORT)
    }
    #[cfg(not(feature = "headless"))]
    {
        Ok(_state.static_server.port)
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn import_cache_from_path(
    state: state_type!(),
    source_path: String,
) -> Result<ImportCacheResult, String> {
    let source_path = source_path.trim();
    if source_path.is_empty() {
        return Err("Empty cache path".to_string());
    }

    let mut source = std::path::PathBuf::from(source_path);
    if !source.exists() {
        return Err(format!("Source path not found: {source_path}"));
    }
    if !source.is_dir() {
        return Err(format!("Source path is not a directory: {source_path}"));
    }

    // If a cache subfolder exists, prefer it
    let candidate = source.join("cache");
    if candidate.is_dir() {
        let mut has_platform = false;
        for platform in [
            "bilibili",
            "douyin",
            "kuaishou",
            "huya",
            "tiktok",
            "wechat",
            "weibo",
            "xiaohongshu",
        ] {
            if candidate.join(platform).is_dir() {
                has_platform = true;
                break;
            }
        }
        if has_platform {
            source = candidate;
        }
    }

    let target = std::path::PathBuf::from(state.config.read().await.cache.clone());
    let should_copy = target != source;
    if !target.exists() {
        if let Err(err) = std::fs::create_dir_all(&target) {
            return Err(err.to_string());
        }
    }

    let platforms = [
        "bilibili",
        "douyin",
        "kuaishou",
        "huya",
        "tiktok",
        "wechat",
        "weibo",
        "xiaohongshu",
    ];
    if should_copy {
        for platform in platforms {
            let src_dir = source.join(platform);
            if !src_dir.is_dir() {
                continue;
            }
            let dst_dir = target.join(platform);
            if let Err(err) = crate::handlers::utils::copy_dir_all(&src_dir, &dst_dir) {
                return Err(err.to_string());
            }
        }
    }

    let (added, updated) =
        crate::migration::migration_methods::try_rebuild_archives_from_cache_scan(
            &state.db, target,
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(ImportCacheResult { added, updated })
}

#[cfg_attr(feature = "gui", tauri::command)]
#[allow(dead_code)]
pub async fn set_cache_path(state: state_type!(), cache_path: String) -> Result<(), String> {
    let old_cache_path = state.config.read().await.cache.clone();
    log::info!("Try to set cache path: {old_cache_path} -> {cache_path}");
    if old_cache_path == cache_path {
        return Ok(());
    }

    let old_cache_path_obj = std::path::Path::new(&old_cache_path);
    let new_cache_path_obj = std::path::Path::new(&cache_path);
    // check if new cache path is under old cache path
    if new_cache_path_obj.starts_with(old_cache_path_obj) {
        log::error!("New cache path is under old cache path: {old_cache_path} -> {cache_path}");
        return Err("New cache path cannot be under old cache path".to_string());
    }

    state.recorder_manager.set_migrating(true);
    // stop and clear all recorders
    state.recorder_manager.stop_all().await;
    // first switch to new cache
    state.config.write().await.set_cache_path(&cache_path);
    log::info!("Cache path changed: {cache_path}");
    // Copy old cache to new cache
    log::info!("Start copy old cache to new cache");
    state
        .db
        .new_message(
            "缓存目录切换",
            "缓存正在迁移中，根据数据量情况可能花费较长时间，在此期间流预览功能不可用",
        )
        .await?;

    let mut old_cache_entries = vec![];
    if let Ok(entries) = std::fs::read_dir(&old_cache_path) {
        for entry in entries.flatten() {
            // check if entry is the same as new cache path
            if entry.path() == std::path::Path::new(&cache_path) {
                continue;
            }
            old_cache_entries.push(entry.path());
        }
    }

    // copy all entries to new cache
    for entry in &old_cache_entries {
        let new_entry = std::path::Path::new(&cache_path).join(entry.file_name().unwrap());
        // if entry is a folder
        if entry.is_dir() {
            if let Err(e) = crate::handlers::utils::copy_dir_all(entry, &new_entry) {
                log::error!("Copy old cache to new cache error: {e}");
                return Err(e.to_string());
            }
        } else if let Err(e) = std::fs::copy(entry, &new_entry) {
            log::error!("Copy old cache to new cache error: {e}");
            return Err(e.to_string());
        }
    }

    log::info!("Copy old cache to new cache done");
    state.db.new_message("缓存目录切换", "缓存切换完成").await?;

    state.recorder_manager.set_migrating(false);

    // remove all old cache entries
    for entry in old_cache_entries {
        if entry.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&entry) {
                log::error!("Remove old cache error: {e}");
            }
        } else if let Err(e) = std::fs::remove_file(&entry) {
            log::error!("Remove old cache error: {e}");
        }
    }

    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
#[allow(dead_code)]
pub async fn set_output_path(state: state_type!(), output_path: String) -> Result<(), String> {
    let mut config = state.config.write().await;
    let old_output_path = config.output.clone();
    log::info!("Try to set output path: {old_output_path} -> {output_path}");
    if old_output_path == output_path {
        return Ok(());
    }

    let old_output_path_obj = std::path::Path::new(&old_output_path);
    let new_output_path_obj = std::path::Path::new(&output_path);
    // check if new output path is under old output path
    if new_output_path_obj.starts_with(old_output_path_obj) {
        log::error!("New output path is under old output path: {old_output_path} -> {output_path}");
        return Err("New output path cannot be under old output path".to_string());
    }

    // list all file and folder in old output
    let mut old_output_entries = vec![];
    if let Ok(entries) = std::fs::read_dir(&old_output_path) {
        for entry in entries.flatten() {
            // check if entry is the same as new output path
            if entry.path() == std::path::Path::new(&output_path) {
                continue;
            }
            old_output_entries.push(entry.path());
        }
    }

    // rename all entries to new output
    for entry in &old_output_entries {
        let new_entry = std::path::Path::new(&output_path).join(entry.file_name().unwrap());
        // if entry is a folder
        if entry.is_dir() {
            if let Err(e) = crate::handlers::utils::copy_dir_all(entry, &new_entry) {
                log::error!("Copy old output to new output error: {e}");
                return Err(e.to_string());
            }
        } else if let Err(e) = std::fs::copy(entry, &new_entry) {
            log::error!("Copy old output to new output error: {e}");
            return Err(e.to_string());
        }
    }

    // remove all old output entries
    for entry in old_output_entries {
        if entry.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&entry) {
                log::error!("Remove old output error: {e}");
            }
        } else if let Err(e) = std::fs::remove_file(&entry) {
            log::error!("Remove old output error: {e}");
        }
    }

    config.set_output_path(&output_path);
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_notify(
    state: state_type!(),
    live_start_notify: bool,
    live_end_notify: bool,
    clip_notify: bool,
    post_notify: bool,
) -> Result<(), ()> {
    state.config.write().await.live_start_notify = live_start_notify;
    state.config.write().await.live_end_notify = live_end_notify;
    state.config.write().await.clip_notify = clip_notify;
    state.config.write().await.post_notify = post_notify;
    state.config.write().await.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_whisper_model(state: state_type!(), whisper_model: String) -> Result<(), ()> {
    state.config.write().await.whisper_model = whisper_model;
    state.config.write().await.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_subtitle_setting(state: state_type!(), auto_subtitle: bool) -> Result<(), ()> {
    state.config.write().await.auto_subtitle = auto_subtitle;
    state.config.write().await.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_bilibili_post_enabled(
    state: state_type!(),
    bilibili_post_enabled: bool,
) -> Result<(), ()> {
    let mut config = state.config.write().await;
    config.bilibili_post_enabled = bilibili_post_enabled;
    config.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_clip_name_format(
    state: state_type!(),
    clip_name_format: String,
) -> Result<(), ()> {
    state.config.write().await.clip_name_format = clip_name_format;
    state.config.write().await.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_whisper_prompt(state: state_type!(), whisper_prompt: String) -> Result<(), ()> {
    state.config.write().await.whisper_prompt = whisper_prompt;
    state.config.write().await.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_subtitle_generator_type(
    state: state_type!(),
    subtitle_generator_type: String,
) -> Result<(), ()> {
    log::info!("Updating subtitle generator type to {subtitle_generator_type}");
    let mut config = state.config.write().await;
    config.subtitle_generator_type = subtitle_generator_type;
    config.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_openai_api_key(state: state_type!(), openai_api_key: String) -> Result<(), ()> {
    log::info!("Updating openai api key");
    let mut config = state.config.write().await;
    config.openai_api_key = openai_api_key;
    config.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_openai_api_endpoint(
    state: state_type!(),
    openai_api_endpoint: String,
) -> Result<(), ()> {
    log::info!("Updating openai api endpoint to {openai_api_endpoint}");
    let mut config = state.config.write().await;
    config.openai_api_endpoint = openai_api_endpoint;
    config.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub async fn update_network_config(
    state: state_type!(),
    http_proxy: String,
    https_proxy: String,
) -> Result<(), ()> {
    log::info!(
        "Updating network proxy: http_proxy='{}' https_proxy='{}'",
        http_proxy,
        https_proxy
    );
    let mut config = state.config.write().await;
    config.set_network_config(&http_proxy, &https_proxy);
    config.apply_network_env();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_record_protocol_preference(
    state: state_type!(),
    record_protocol_preference: String,
) -> Result<(), ()> {
    let normalized = record_protocol_preference.trim().to_ascii_lowercase();
    let mut config = state.config.write().await;
    config.record_protocol_preference = normalized;
    config.save();
    config.apply_record_protocol_env();
    drop(config);
    if let Err(err) = state
        .recorder_manager
        .restart_recorders_for_platforms(&[PlatformType::Kuaishou, PlatformType::TikTok])
        .await
    {
        log::warn!("Failed to restart recorders after protocol change: {err}");
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_auto_generate(
    state: state_type!(),
    enabled: bool,
    encode_danmu: bool,
    delete_cache_after_clip: bool,
) -> Result<(), String> {
    let mut config = state.config.write().await;
    config.auto_generate.enabled = enabled;
    config.auto_generate.encode_danmu = encode_danmu;
    config.auto_generate.delete_cache_after_clip = delete_cache_after_clip;
    config.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub async fn update_use_login_accounts(
    state: state_type!(),
    use_login_accounts: bool,
) -> Result<(), ()> {
    let mut config = state.config.write().await;
    config.use_login_accounts = use_login_accounts;
    config.save();
    let config_snapshot = config.clone();
    drop(config);
    if use_login_accounts {
        crate::handlers::account::ensure_login_accounts(&state.db, &config_snapshot).await;
    } else {
        crate::handlers::account::remove_login_accounts(&state.db, &config_snapshot).await;
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub async fn update_use_guest_accounts(
    state: state_type!(),
    use_guest_accounts: bool,
) -> Result<(), String> {
    let mut config = state.config.write().await;
    config.use_guest_accounts = use_guest_accounts;
    config.save();
    drop(config);
    if use_guest_accounts {
        crate::handlers::account::refresh_guest_accounts(state.clone()).await?;
    } else {
        // First, get the current guest accounts for deletion BEFORE clearing
        let config_snapshot = state.config.read().await.clone();

        // Delete guest accounts from database using the ORIGINAL config (before clearing)
        crate::handlers::account::remove_guest_accounts(&state.db, &config_snapshot).await;

        // Now clear guest accounts from config
        let mut config = state.config.write().await;
        config.guest_accounts.clear();
        config.save();
        drop(config);

        // Clear guest account cookies from accounts.toml file (keep the structure)
        let accounts_path = crate::config::resolve_accounts_file_write_path();
        if let Some((mut accounts_file, _)) = crate::config::load_accounts_file_or_example() {
            // Only clear cookies, keep the platform entries
            for guest_account in &mut accounts_file.guest_accounts {
                guest_account.cookies.clear();
                guest_account.extra.clear();
            }
            if let Err(e) = crate::config::write_accounts_file(&accounts_path, &accounts_file) {
                log::error!(
                    "Failed to clear guest account cookies from accounts.toml: {}",
                    e
                );
            } else {
                log::info!("Cleared guest account cookies from accounts.toml");
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub async fn update_kuaishou_follow_list_fallback(
    state: state_type!(),
    enabled: bool,
) -> Result<(), ()> {
    let mut config = state.config.write().await;
    config.kuaishou_enable_follow_list_fallback = enabled;
    config.save();
    config.apply_kuaishou_fallback_env();
    drop(config);
    if let Err(err) = state
        .recorder_manager
        .restart_recorders_for_platforms(&[PlatformType::Kuaishou])
        .await
    {
        log::warn!("Failed to restart kuaishou recorders after fallback change: {err}");
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub async fn update_kuaishou_public_page_fallback(
    state: state_type!(),
    enabled: bool,
) -> Result<(), ()> {
    let mut config = state.config.write().await;
    config.kuaishou_enable_public_page_fallback = enabled;
    config.save();
    config.apply_kuaishou_fallback_env();
    drop(config);
    if let Err(err) = state
        .recorder_manager
        .restart_recorders_for_platforms(&[PlatformType::Kuaishou])
        .await
    {
        log::warn!("Failed to restart kuaishou recorders after fallback change: {err}");
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub async fn get_login_account_platforms(state: state_type!()) -> Result<Vec<String>, ()> {
    let config = state.config.read().await;
    let mut platforms = Vec::new();
    for entry in &config.login_accounts {
        if entry.cookies.trim().is_empty() {
            continue;
        }
        if !platforms.contains(&entry.platform) {
            platforms.push(entry.platform.clone());
        }
    }
    Ok(platforms)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_status_check_interval(
    state: state_type!(),
    mut interval: u64,
) -> Result<(), ()> {
    if interval < 10 {
        interval = 10; // Minimum interval of 10 seconds
    }
    log::info!("Updating status check interval to {interval} seconds");
    state
        .config
        .write()
        .await
        .set_status_check_interval(interval);
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_whisper_language(
    state: state_type!(),
    whisper_language: String,
) -> Result<(), ()> {
    log::info!("Updating whisper language to {whisper_language}");
    state.config.write().await.whisper_language = whisper_language;
    state.config.write().await.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_webhook_url(state: state_type!(), webhook_url: String) -> Result<(), ()> {
    log::info!("Updating webhook url to {webhook_url}");
    let _ = state
        .webhook_poster
        .update_config(crate::webhook::poster::WebhookConfig {
            url: webhook_url.clone(),
            ..Default::default()
        })
        .await;
    state.config.write().await.webhook_url = webhook_url;
    state.config.write().await.save();
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn update_danmu_ass_options(
    state: state_type!(),
    font_size: f64,
    opacity: f64,
) -> Result<(), ()> {
    log::info!("Updating danmu ass options");
    state
        .config
        .write()
        .await
        .set_danmu_ass_options(Danmu2AssOptions { font_size, opacity });
    Ok(())
}

#[cfg(feature = "gui")]
#[tauri::command]
pub async fn clear_webview_data(state: state_type!()) -> Result<(), String> {
    let windows = state.app_handle.webview_windows();
    if windows.is_empty() {
        return Err("No webview windows available".to_string());
    }

    let mut errors = Vec::new();
    for (label, window) in windows {
        if let Err(e) = window.clear_all_browsing_data() {
            errors.push(format!("{label}: {e}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Failed to clear browsing data: {}",
            errors.join("; ")
        ))
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub async fn update_powerlive_key(state: state_type!(), powerlive_key: String) -> Result<(), ()> {
    state.config.write().await.powerlive_key = powerlive_key.clone();
    state.config.write().await.save();
    log::info!("Updated powerlive key");
    Ok(())
}
#[derive(serde::Serialize)]
pub struct ImportCacheResult {
    pub added: usize,
    pub updated: usize,
}
