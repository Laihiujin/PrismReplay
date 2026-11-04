#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod constants;
mod danmu2ass;
mod database;
mod ffmpeg;
mod handlers;
#[cfg(feature = "headless")]
mod http_server;
mod migration;
mod progress;
mod recorder_manager;
mod state;
mod static_server;
mod subtitle_generator;
mod task;
#[cfg(feature = "gui")]
mod tray;
mod utils;
mod webhook;

use async_std::fs;
use chrono::Utc;
use config::Config;
use database::Database;
use migration::migration_methods::try_add_parent_id_to_records;
use migration::migration_methods::try_convert_clip_covers;
use migration::migration_methods::try_convert_entry_to_m3u8;
use migration::migration_methods::try_convert_live_covers;
use migration::migration_methods::try_rebuild_archives;
use recorder_manager::RecorderManager;
use simplelog::ConfigBuilder;
use state::State;
use std::fs::File;
use std::path::Path;
#[cfg(any(target_os = "windows", feature = "headless"))]
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::MetadataExt;

#[cfg(target_os = "windows")]
use std::os::windows::fs::MetadataExt;

#[cfg(feature = "gui")]
use {
    sqlx::Row,
    tauri::{Manager, WindowEvent},
    tauri_plugin_sql::{Migration, MigrationKind},
};

#[cfg(feature = "headless")]
use {
    clap::{arg, command, Parser},
    futures_core::future::BoxFuture,
    migration::{Migration, MigrationKind},
    sqlx::error::BoxDynError,
    sqlx::migrate::Migration as SqlxMigration,
    sqlx::migrate::MigrationSource,
    sqlx::{
        migrate::{MigrateDatabase, Migrator},
        Pool, Sqlite,
    },
};

/// open a log file, if file size exceeds 1MB, backup log file and create a new one.
async fn open_log_file(log_dir: &Path) -> Result<File, Box<dyn std::error::Error>> {
    let log_filename = log_dir.join("bsr.log");

    if let Ok(meta) = fs::metadata(&log_filename).await {
        #[cfg(target_os = "windows")]
        let file_size = meta.file_size();
        #[cfg(not(target_os = "windows"))]
        let file_size = meta.size();
        if file_size > 1024 * 1024 {
            // move original file to backup
            let date_str = Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string();
            let backup_filename = log_dir.join(format!("bsr-{date_str}.log"));
            fs::rename(&log_filename, backup_filename).await?;
        }
    }

    Ok(File::options()
        .create(true)
        .append(true)
        .open(&log_filename)?)
}

#[cfg(target_os = "windows")]
fn choose_windows_output_path(default_output: &Path) -> PathBuf {
    let default_str = default_output.to_string_lossy();
    let default_drive = default_str.chars().next().map(|c| c.to_ascii_uppercase());
    if default_drive != Some('C') {
        return default_output.to_path_buf();
    }

    for drive in b'D'..=b'Z' {
        let root = format!("{}:\\", drive as char);
        if Path::new(&root).exists() {
            return PathBuf::from(root).join(crate::constants::APP_DATA_ID).join("output");
        }
    }

    default_output.to_path_buf()
}

#[cfg(target_os = "windows")]
fn choose_windows_cache_path(default_cache: &Path) -> PathBuf {
    let default_str = default_cache.to_string_lossy();
    let default_drive = default_str.chars().next().map(|c| c.to_ascii_uppercase());
    if default_drive != Some('C') {
        return default_cache.to_path_buf();
    }

    for drive in b'D'..=b'Z' {
        let root = format!("{}:\\", drive as char);
        if Path::new(&root).exists() {
            return PathBuf::from(root).join(crate::constants::APP_DATA_ID).join("cache");
        }
    }

    default_cache.to_path_buf()
}

async fn setup_logging(log_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // mkdir if not exists
    if !log_dir.exists() {
        std::fs::create_dir_all(log_dir)?;
    }

    let file = open_log_file(log_dir).await?;

    let config = ConfigBuilder::new()
        .set_target_level(simplelog::LevelFilter::Debug)
        .set_location_level(simplelog::LevelFilter::Debug)
        .add_filter_ignore_str("tokio")
        .add_filter_ignore_str("hyper")
        .add_filter_ignore_str("sqlx")
        .add_filter_ignore_str("reqwest")
        .add_filter_ignore_str("h2")
        .add_filter_ignore_str("cookie_store")
        .add_filter_ignore_str("html5ever")
        .add_filter_ignore_str("selectors")
        .build();

    let term_config = config.clone();

    simplelog::CombinedLogger::init(vec![
        simplelog::TermLogger::new(
            simplelog::LevelFilter::Debug,
            term_config,
            simplelog::TerminalMode::Mixed,
            simplelog::ColorChoice::Auto,
        ),
        simplelog::WriteLogger::new(simplelog::LevelFilter::Info, config, file),
    ])?;

    // logging current package version
    log::info!("Current version: {}", env!("CARGO_PKG_VERSION"));

    Ok(())
}

fn get_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            description: "create_initial_tables",
            sql: r"
                CREATE TABLE accounts (uid INTEGER, platform TEXT NOT NULL DEFAULT 'bilibili', name TEXT, avatar TEXT, csrf TEXT, cookies TEXT, extra TEXT, created_at TEXT, PRIMARY KEY(uid, platform));
                CREATE TABLE recorders (room_id INTEGER PRIMARY KEY, platform TEXT NOT NULL DEFAULT 'bilibili', created_at TEXT);
                CREATE TABLE records (live_id TEXT PRIMARY KEY, platform TEXT NOT NULL DEFAULT 'bilibili', room_id INTEGER, title TEXT, length INTEGER, size INTEGER, cover BLOB, created_at TEXT);
                CREATE TABLE danmu_statistics (live_id TEXT PRIMARY KEY, room_id INTEGER, value INTEGER, time_point TEXT);
                CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT, content TEXT, read INTEGER, created_at TEXT);
                CREATE TABLE videos (id INTEGER PRIMARY KEY AUTOINCREMENT, room_id INTEGER, cover TEXT, file TEXT, length INTEGER, size INTEGER, status INTEGER, bvid TEXT, title TEXT, desc TEXT, tags TEXT, area INTEGER, created_at TEXT);
                ",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 2,
            description: "add_auto_start_column",
            sql: r"ALTER TABLE recorders ADD COLUMN auto_start INTEGER NOT NULL DEFAULT 1;",
            kind: MigrationKind::Up,
        },
        // add platform column to videos table
        Migration {
            version: 3,
            description: "add_platform_column",
            sql: r"ALTER TABLE videos ADD COLUMN platform TEXT;",
            kind: MigrationKind::Up,
        },
        // add task table to record encode/upload task
        Migration {
            version: 4,
            description: "add_task_table",
            sql: r"CREATE TABLE tasks (id TEXT PRIMARY KEY, type TEXT, status TEXT, message TEXT, metadata TEXT, created_at TEXT);",
            kind: MigrationKind::Up,
        },
        // add id_str column to support string IDs like Douyin sec_uid while keeping uid for Bilibili compatibility
        Migration {
            version: 5,
            description: "add_id_str_column",
            sql: r"ALTER TABLE accounts ADD COLUMN id_str TEXT;",
            kind: MigrationKind::Up,
        },
        // add extra column to recorders
        Migration {
            version: 6,
            description: "add_extra_column_to_recorders",
            sql: r"ALTER TABLE recorders ADD COLUMN extra TEXT;",
            kind: MigrationKind::Up,
        },
        // add indexes
        Migration {
            version: 7,
            description: "add_indexes",
            sql: r"
                CREATE INDEX idx_records_live_id ON records (room_id, live_id);
                CREATE INDEX idx_records_created_at ON records (room_id, created_at);
                CREATE INDEX idx_videos_room_id ON videos (room_id);
                CREATE INDEX idx_videos_created_at ON videos (created_at);
            ",
            kind: MigrationKind::Up,
        },
        // add note column for video
        Migration {
            version: 8,
            description: "add_note_column_for_video",
            sql: r"ALTER TABLE videos ADD COLUMN note TEXT;",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 9,
            description: "add_parent_id_column_for_record",
            sql: r"ALTER TABLE records ADD COLUMN parent_id TEXT;",
            kind: MigrationKind::Up,
        },
        // change records table primary key to (parent_id, live_id)
        Migration {
            version: 10,
            description: "change_records_primary_key",
            sql: r"
                CREATE TABLE records_new (
                    parent_id TEXT NOT NULL DEFAULT '',
                    live_id TEXT NOT NULL,
                    platform TEXT NOT NULL DEFAULT 'bilibili',
                    room_id TEXT,
                    title TEXT,
                    length INTEGER,
                    size INTEGER,
                    cover BLOB,
                    created_at TEXT,
                    PRIMARY KEY (parent_id, live_id)
                );
                INSERT INTO records_new (
                    parent_id,
                    live_id,
                    platform,
                    room_id,
                    title,
                    length,
                    size,
                    cover,
                    created_at
                )
                SELECT
                    COALESCE(parent_id, live_id) AS parent_id,
                    live_id,
                    platform,
                    CAST(room_id AS TEXT),
                    title,
                    length,
                    size,
                    cover,
                    created_at
                FROM records;
                DROP TABLE records;
                ALTER TABLE records_new RENAME TO records;
                CREATE INDEX IF NOT EXISTS idx_records_live_id ON records (room_id, live_id);
                CREATE INDEX IF NOT EXISTS idx_records_created_at ON records (room_id, created_at);
            ",
            kind: MigrationKind::Up,
        },
        // convert room_id columns to TEXT across tables
        Migration {
            version: 11,
            description: "convert_room_id_to_text",
            sql: r"
                CREATE TABLE recorders_new (
                    room_id TEXT PRIMARY KEY,
                    platform TEXT NOT NULL DEFAULT 'bilibili',
                    created_at TEXT,
                    auto_start INTEGER NOT NULL DEFAULT 1,
                    extra TEXT
                );
                INSERT INTO recorders_new (
                    room_id,
                    platform,
                    created_at,
                    auto_start,
                    extra
                )
                SELECT
                    CAST(room_id AS TEXT),
                    platform,
                    created_at,
                    COALESCE(auto_start, 1),
                    extra
                FROM recorders;
                DROP TABLE recorders;
                ALTER TABLE recorders_new RENAME TO recorders;

                CREATE TABLE danmu_statistics_new (
                    live_id TEXT PRIMARY KEY,
                    room_id TEXT,
                    value INTEGER,
                    time_point TEXT
                );
                INSERT INTO danmu_statistics_new (
                    live_id,
                    room_id,
                    value,
                    time_point
                )
                SELECT
                    live_id,
                    CAST(room_id AS TEXT),
                    value,
                    time_point
                FROM danmu_statistics;
                DROP TABLE danmu_statistics;
                ALTER TABLE danmu_statistics_new RENAME TO danmu_statistics;

                CREATE TABLE videos_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    room_id TEXT,
                    cover TEXT,
                    file TEXT,
                    length INTEGER,
                    size INTEGER,
                    status INTEGER,
                    bvid TEXT,
                    title TEXT,
                    desc TEXT,
                    tags TEXT,
                    area INTEGER,
                    created_at TEXT,
                    platform TEXT,
                    note TEXT
                );
                INSERT INTO videos_new (
                    id,
                    room_id,
                    cover,
                    file,
                    length,
                    size,
                    status,
                    bvid,
                    title,
                    desc,
                    tags,
                    area,
                    created_at,
                    platform,
                    note
                )
                SELECT
                    id,
                    CAST(room_id AS TEXT),
                    cover,
                    file,
                    length,
                    size,
                    status,
                    bvid,
                    title,
                    desc,
                    tags,
                    area,
                    created_at,
                    platform,
                    note
                FROM videos;
                DROP TABLE videos;
                ALTER TABLE videos_new RENAME TO videos;

                CREATE INDEX IF NOT EXISTS idx_videos_room_id ON videos (room_id);
                CREATE INDEX IF NOT EXISTS idx_videos_created_at ON videos (created_at);
            ",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 12,
            description: "convert_account_uid_to_text",
            sql: r"
                CREATE TABLE accounts_new (
                    uid TEXT,
                    platform TEXT NOT NULL DEFAULT 'bilibili',
                    name TEXT,
                    avatar TEXT,
                    csrf TEXT,
                    cookies TEXT,
                    extra TEXT,
                    created_at TEXT,
                    PRIMARY KEY (uid, platform)
                );
                INSERT INTO accounts_new (
                    uid,
                    platform,
                    name,
                    avatar,
                    csrf,
                    cookies,
                    extra,
                    created_at
                )
                SELECT
                    COALESCE(NULLIF(id_str, ''), CAST(uid AS TEXT)),
                    platform,
                    name,
                    avatar,
                    csrf,
                    cookies,
                    COALESCE(extra, ''),
                    created_at
                FROM accounts;
                DROP TABLE accounts;
                ALTER TABLE accounts_new RENAME TO accounts;
            ",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 13,
            description: "change_records_length_to_float",
            sql: r"
            ALTER TABLE records ADD COLUMN length_float FLOAT;
            UPDATE records SET length_float = CAST(length AS FLOAT);
            ALTER TABLE records DROP COLUMN length;
            ALTER TABLE records RENAME COLUMN length_float TO length;
            ",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 14,
            description: "add_cached_room_meta_to_recorders",
            sql: r"
            ALTER TABLE recorders ADD COLUMN room_title TEXT;
            ALTER TABLE recorders ADD COLUMN room_cover TEXT;
            ALTER TABLE recorders ADD COLUMN user_name TEXT;
            ALTER TABLE recorders ADD COLUMN user_avatar TEXT;
            ",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 15,
            description: "add_resolution_column_to_records",
            sql: r"
            ALTER TABLE records ADD COLUMN resolution TEXT;
            ",
            kind: MigrationKind::Up,
        },
        // Migration 16 removed - extra column already exists from Migration 12
        // Migration {
        //     version: 16,
        //     description: "add_extra_column_to_accounts",
        //     sql: r"
        //     ALTER TABLE accounts ADD COLUMN extra TEXT NOT NULL DEFAULT '';
        //     ",
        //     kind: MigrationKind::Up,
        // },
    ]
}

#[cfg(feature = "headless")]
#[derive(Debug)]
struct MigrationList(Vec<Migration>);

#[cfg(feature = "headless")]
impl MigrationSource<'static> for MigrationList {
    fn resolve(self) -> BoxFuture<'static, std::result::Result<Vec<SqlxMigration>, BoxDynError>> {
        Box::pin(async move {
            let mut migrations = Vec::new();
            for migration in self.0 {
                if matches!(migration.kind, MigrationKind::Up) {
                    migrations.push(SqlxMigration::new(
                        migration.version,
                        migration.description.into(),
                        migration.kind.into(),
                        migration.sql.into(),
                        false,
                    ));
                }
            }
            Ok(migrations)
        })
    }
}

#[cfg(feature = "headless")]
async fn setup_server_state(args: Args) -> Result<State, Box<dyn std::error::Error>> {
    use crate::{constants::API_PORT, static_server::StaticServer, task::TaskManager};
    use progress::progress_manager::ProgressManager;
    use progress::progress_reporter::EventEmitter;

    setup_logging(Path::new("./")).await?;
    log::info!("Setting up server state...");
    let config_path = PathBuf::from(&args.config);
    let (cache_path, output_path) = {
        #[cfg(target_os = "windows")]
        {
            let app_dirs = platform_dirs::AppDirs::new(Some(crate::constants::APP_DATA_ID), false).unwrap();
            (
                choose_windows_cache_path(&app_dirs.cache_dir.join("cache")),
                choose_windows_output_path(&app_dirs.data_dir.join("output")),
            )
        }
        #[cfg(not(target_os = "windows"))]
        {
            (PathBuf::from("./cache"), PathBuf::from("./output"))
        }
    };
    let config = match Config::load(&config_path, &cache_path, &output_path) {
        Ok(config) => config,
        Err(e) => {
            log::error!("Failed to load config: {e}");
            return Err(e.into());
        }
    };
    config.apply_network_env();
    config.apply_record_protocol_env();
    config.apply_kuaishou_fallback_env();
    config.apply_douyin_passport_env();
    config.apply_tiktok_feed_env();
    config.apply_kuaishou_ws_env();
    let config = Arc::new(RwLock::new(config));
    let db = Arc::new(Database::new());
    // connect to sqlite database

    let conn_url = format!("sqlite:{}/data_v2.db", args.db);
    // create db folder if not exists
    if !Path::new(&args.db).exists() {
        std::fs::create_dir_all(&args.db)?;
    }

    if !Sqlite::database_exists(&conn_url).await.unwrap_or(false) {
        Sqlite::create_database(&conn_url).await?;
    }
    let db_pool: Pool<Sqlite> = Pool::connect(&conn_url).await?;
    let migrations = get_migrations();

    let migrator = Migrator::new(MigrationList(migrations))
        .await
        .expect("Failed to create migrator");
    migrator
        .run(&db_pool)
        .await
        .expect("Failed to run migrations");

    db.set(db_pool).await;
    db.finish_pending_tasks().await?;
    let guest_from_db = crate::handlers::account::load_guest_accounts_from_db(&db).await;
    if !guest_from_db.is_empty() {
        let mut cfg = config.write().await;
        let guest_empty_or_blank = cfg.guest_accounts.is_empty()
            || cfg
                .guest_accounts
                .iter()
                .all(|entry| entry.cookies.trim().is_empty());
        if cfg.use_guest_accounts && guest_empty_or_blank {
            cfg.guest_accounts = guest_from_db;
            cfg.save();
        }
    }
    let config_snapshot = config.read().await.clone();
    crate::handlers::account::ensure_login_accounts(&db, &config_snapshot).await;
    crate::handlers::account::ensure_guest_accounts(&db, &config_snapshot).await;

    let progress_manager = Arc::new(ProgressManager::new());
    let emitter = EventEmitter::new(progress_manager.get_event_sender());
    let webhook_poster =
        webhook::poster::create_webhook_poster(&config.read().await.webhook_url, None).unwrap();
    let mut task_manager = TaskManager::new();
    task_manager.start();
    let task_manager = Arc::new(task_manager);
    let recorder_manager = Arc::new(RecorderManager::new(
        emitter,
        db.clone(),
        config.clone(),
        task_manager.clone(),
        webhook_poster.clone(),
    ));

    #[cfg(feature = "headless")]
    let static_server = Arc::new(StaticServer::headless(API_PORT));

    #[cfg(not(feature = "headless"))]
    let static_server =
        Arc::new(start_static_server(config.clone(), recorder_manager.clone()).await?);

    let _ = try_rebuild_archives(&db, config.read().await.cache.clone().into()).await;
    let _ = try_convert_live_covers(&db, config.read().await.cache.clone().into()).await;
    let _ = try_convert_clip_covers(&db, config.read().await.output.clone().into()).await;
    let _ = try_add_parent_id_to_records(&db).await;
    let _ = try_convert_entry_to_m3u8(&db, config.read().await.cache.clone().into()).await;

    Ok(State {
        db,
        config,
        webhook_poster,
        recorder_manager,
        task_manager,
        static_server,
        progress_manager,
        readonly: args.readonly,
    })
}

#[cfg(feature = "gui")]
async fn setup_app_state(app: &tauri::App) -> Result<State, Box<dyn std::error::Error>> {
    use platform_dirs::AppDirs;
    use progress::progress_reporter::EventEmitter;

    use crate::{static_server::start_static_server, task::TaskManager};

    let log_dir = app.path().app_log_dir()?;
    setup_logging(&log_dir).await?;

    log::info!("Setting up app state...");
    let app_dirs = AppDirs::new(Some(crate::constants::APP_DATA_ID), false).unwrap();
    let config_path = app_dirs.config_dir.join("Conf.toml");
    #[cfg(target_os = "windows")]
    let cache_path = choose_windows_cache_path(&app_dirs.cache_dir.join("cache"));
    #[cfg(not(target_os = "windows"))]
    let cache_path = app_dirs.cache_dir.join("cache");
    #[cfg(target_os = "windows")]
    let output_path = choose_windows_output_path(&app_dirs.data_dir.join("output"));
    #[cfg(not(target_os = "windows"))]
    let output_path = app_dirs.data_dir.join("output");
    log::info!("Loading config from {config_path:?}");
    let config = match Config::load(&config_path, &cache_path, &output_path) {
        Ok(config) => config,
        Err(e) => {
            log::error!("Failed to load config, exiting: {e}");
            return Err(e.into());
        }
    };
    config.apply_network_env();
    config.apply_record_protocol_env();
    config.apply_kuaishou_fallback_env();
    config.apply_douyin_passport_env();
    config.apply_tiktok_feed_env();
    config.apply_kuaishou_ws_env();

    let config = Arc::new(RwLock::new(config));
    let config_clone = config.clone();
    let dbs = app.state::<tauri_plugin_sql::DbInstances>().inner();
    let db = Arc::new(Database::new());
    let db_clone = db.clone();
    let emitter = EventEmitter::new(app.handle().clone());
    let binding = dbs.0.read().await;
    let dbpool = binding.get("sqlite:data_v2.db").unwrap();
    let sqlite_pool = match dbpool {
        tauri_plugin_sql::DbPool::Sqlite(pool) => pool.clone(),
    };
    db_clone.set(sqlite_pool.clone()).await;
    db_clone.finish_pending_tasks().await?;
    if let Ok(rows) = sqlx::query("PRAGMA database_list")
        .fetch_all(&sqlite_pool)
        .await
    {
        for row in rows {
            let name: String = row.try_get("name").unwrap_or_default();
            let file: String = row.try_get("file").unwrap_or_default();
            log::info!("[DB] database_list name={} file={}", name, file);
        }
    }
    let guest_from_db = crate::handlers::account::load_guest_accounts_from_db(&db).await;
    if !guest_from_db.is_empty() {
        let mut cfg = config.write().await;
        let guest_empty_or_blank = cfg.guest_accounts.is_empty()
            || cfg
                .guest_accounts
                .iter()
                .all(|entry| entry.cookies.trim().is_empty());
        if cfg.use_guest_accounts && guest_empty_or_blank {
            cfg.guest_accounts = guest_from_db;
            cfg.save();
        }
    }
    let config_snapshot = config.read().await.clone();
    crate::handlers::account::ensure_login_accounts(&db, &config_snapshot).await;
    crate::handlers::account::ensure_guest_accounts(&db, &config_snapshot).await;
    let webhook_poster =
        webhook::poster::create_webhook_poster(&config.read().await.webhook_url, None).unwrap();
    let mut task_manager = TaskManager::new();
    task_manager.start();

    let task_manager = Arc::new(task_manager);

    let recorder_manager = Arc::new(RecorderManager::new(
        app.app_handle().clone(),
        emitter,
        db.clone(),
        config.clone(),
        task_manager.clone(),
        webhook_poster.clone(),
    ));

    let static_server =
        Arc::new(start_static_server(config.clone(), recorder_manager.clone()).await?);

    // try to rebuild archive table
    let cache_path = config_clone.read().await.cache.clone();
    let output_path = config_clone.read().await.output.clone();
    if let Err(e) = try_rebuild_archives(&db_clone, cache_path.clone().into()).await {
        log::warn!("Rebuilding archive table failed: {e}");
    }
    let _ = try_convert_live_covers(&db_clone, cache_path.clone().into()).await;
    let _ = try_convert_clip_covers(&db_clone, output_path.clone().into()).await;
    let _ = try_add_parent_id_to_records(&db_clone).await;
    let _ = try_convert_entry_to_m3u8(&db_clone, cache_path.clone().into()).await;

    Ok(State {
        db,
        config,
        recorder_manager,
        task_manager,
        static_server,
        app_handle: app.handle().clone(),
        webhook_poster,
    })
}

#[cfg(feature = "gui")]
fn setup_plugins(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    let migrations = get_migrations();
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app
                .get_webview_window("main")
                .expect("no main window")
                .set_focus();
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations("sqlite:data_v2.db", migrations)
                .build(),
        )
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init());

    println!("Plugins initialized");

    builder
}

#[cfg(feature = "gui")]
fn setup_event_handlers(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder.on_window_event(|window, event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            // main window is not closable
            if window.label() == "main" {
                window.hide().unwrap();
                api.prevent_close();
            }
        }
    })
}

#[cfg(feature = "gui")]
fn setup_invoke_handlers(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder.invoke_handler(tauri::generate_handler![
        crate::handlers::account::get_accounts,
        crate::handlers::account::add_account,
        crate::handlers::account::update_login_account,
        crate::handlers::account::remove_account,
        crate::handlers::account::get_browser_cookies,
        crate::handlers::account::open_tiktok_login_window,
        crate::handlers::account::get_tiktok_webview_cookies,
        crate::handlers::account::open_douyin_login_window,
        crate::handlers::account::get_douyin_webview_cookies,
        crate::handlers::account::open_kuaishou_login_window,
        crate::handlers::account::get_kuaishou_webview_cookies,
        crate::handlers::account::open_huya_login_window,
        crate::handlers::account::get_huya_webview_cookies,
        crate::handlers::account::open_bilibili_login_window,
        crate::handlers::account::get_bilibili_webview_cookies,
        crate::handlers::account::get_account_count,
        crate::handlers::account::get_qr_status,
        crate::handlers::account::get_qr,
        crate::handlers::account::check_tiktok_proxy,
        crate::handlers::account::check_tiktok_cookie,
        crate::handlers::account::list_webview_windows,
        crate::handlers::account::close_webview_window,
        crate::handlers::account::close_webview_window,
        crate::handlers::account::close_all_login_windows,
        crate::handlers::account::refresh_guest_accounts_force,
        crate::handlers::config::get_config,
        crate::handlers::config::get_static_port,
        crate::handlers::config::set_cache_path,
        crate::handlers::config::import_cache_from_path,
        crate::handlers::config::set_output_path,
        crate::handlers::config::update_notify,
        crate::handlers::config::update_whisper_model,
        crate::handlers::config::update_subtitle_setting,
        crate::handlers::config::update_bilibili_post_enabled,
        crate::handlers::config::update_clip_name_format,
        crate::handlers::config::update_whisper_prompt,
        crate::handlers::config::update_subtitle_generator_type,
        crate::handlers::config::update_openai_api_key,
        crate::handlers::config::update_openai_api_endpoint,
        crate::handlers::config::update_network_config,
        crate::handlers::config::update_record_protocol_preference,
        crate::handlers::config::update_auto_generate,
        crate::handlers::config::update_use_login_accounts,
        crate::handlers::config::update_use_guest_accounts,
        crate::handlers::config::update_kuaishou_follow_list_fallback,
        crate::handlers::config::update_kuaishou_public_page_fallback,
        crate::handlers::config::update_status_check_interval,
        crate::handlers::config::update_whisper_language,
        crate::handlers::config::update_webhook_url,
        crate::handlers::config::update_danmu_ass_options,
        crate::handlers::config::clear_webview_data,
        crate::handlers::config::update_powerlive_key,
        crate::handlers::config::get_login_account_platforms,
        crate::handlers::message::get_messages,
        crate::handlers::message::read_message,
        crate::handlers::message::delete_message,
        crate::handlers::recorder::get_recorder_list,
        crate::handlers::recorder::add_recorder,
        crate::handlers::recorder::remove_recorder,
        crate::handlers::recorder::reload_recorder,
        crate::handlers::recorder::get_room_info,
        crate::handlers::recorder::get_archive_disk_usage,
        crate::handlers::recorder::get_archives,
        crate::handlers::recorder::get_archive,
        crate::handlers::recorder::get_archives_by_parent_id,
        crate::handlers::recorder::get_archive_subtitle,
        crate::handlers::recorder::generate_archive_subtitle,
        crate::handlers::recorder::delete_archive,
        crate::handlers::recorder::delete_archives,
        crate::handlers::recorder::delete_zero_size_archives,
        crate::handlers::recorder::delete_small_size_archives,
        crate::handlers::recorder::get_danmu_record,
        crate::handlers::recorder::export_danmu,
        crate::handlers::recorder::send_danmaku,
        crate::handlers::recorder::get_total_length,
        crate::handlers::recorder::get_today_record_count,
        crate::handlers::recorder::get_recent_record,
        crate::handlers::recorder::refresh_archive_room_info,
        crate::handlers::recorder::set_enable,
        crate::handlers::recorder::fetch_hls,
        crate::handlers::recorder::generate_whole_clip,
        crate::handlers::recorder::fix_archive_covers,
        crate::handlers::video::clip_range,
        crate::handlers::video::upload_procedure,
        crate::handlers::video::cancel,
        crate::handlers::video::get_video,
        crate::handlers::video::get_videos,
        crate::handlers::video::get_all_videos,
        crate::handlers::video::get_video_cover,
        crate::handlers::video::delete_video,
        crate::handlers::video::get_video_typelist,
        crate::handlers::video::update_video_cover,
        crate::handlers::video::generate_video_subtitle,
        crate::handlers::video::get_video_subtitle,
        crate::handlers::video::update_video_subtitle,
        crate::handlers::video::update_video_note,
        crate::handlers::video::encode_video_subtitle,
        crate::handlers::video::generic_ffmpeg_command,
        crate::handlers::video::import_external_video,
        crate::handlers::video::batch_import_external_videos,
        crate::handlers::video::clip_video,
        crate::handlers::video::get_file_size,
        crate::handlers::video::get_import_progress,
        crate::handlers::video::generate_audio_sample,
        crate::handlers::task::get_tasks,
        crate::handlers::task::delete_task,
        crate::handlers::utils::show_in_folder,
        crate::handlers::utils::export_to_file,
        crate::handlers::utils::get_disk_info,
        crate::handlers::utils::fetch_url,
        crate::handlers::utils::open_live,
        crate::handlers::utils::open_clip,
        crate::handlers::utils::open_video_folder,
        crate::handlers::utils::open_log_folder,
        crate::handlers::utils::file_exists,
        crate::handlers::utils::console_log,
        crate::handlers::utils::list_folder,
    ])
}

#[cfg(feature = "gui")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = sentry::init((
        "https://fbf803d994a257326ea8f043d79058eb@sentry.vjoi.cn/2",
        sentry::ClientOptions {
            release: sentry::release_name!(),
            // Capture user IPs and potentially sensitive headers when using HTTP server integrations
            // see https://docs.sentry.io/platforms/rust/data-management/data-collected for more info
            send_default_pii: true,
            session_mode: sentry::SessionMode::Application,
            ..Default::default()
        },
    ));

    let _ = fix_path_env::fix();

    let builder = tauri::Builder::default().plugin(tauri_plugin_deep_link::init());
    let builder = setup_plugins(builder);
    let builder = setup_event_handlers(builder);
    let builder = setup_invoke_handlers(builder);

    builder
        .setup(|app| {
            tauri::async_runtime::block_on(async {
                let state = setup_app_state(app).await?;
                let _ = tray::create_tray(app.handle());

                // check ffmpeg status
                match ffmpeg::check_ffmpeg().await {
                    Err(e) => log::error!("Failed to check ffmpeg version: {e}"),
                    Ok(v) => log::info!("Checked ffmpeg version: {v}"),
                }

                app.manage(state.clone());

                if state.config.read().await.use_guest_accounts {
                    let guest_empty_or_blank = {
                        let cfg = state.config.read().await;
                        cfg.guest_accounts.is_empty()
                            || cfg
                                .guest_accounts
                                .iter()
                                .all(|entry| entry.cookies.trim().is_empty())
                    };
                    if guest_empty_or_blank {
                        let state_clone = state.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ =
                                crate::handlers::account::refresh_guest_accounts_on_demand_owned(
                                    state_clone,
                                    "startup".to_string(),
                                )
                                .await;
                        });
                    }
                }

                // Start periodic guest cookie refresh timer
                crate::handlers::account::start_guest_cookie_refresh_timer(state);

                Ok(())
            })
        })
        .build(tauri::generate_context!())?
        .run(|app_handle: &tauri::AppHandle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // stop all recorders
                let recorder_manager = app_handle.state::<State>().recorder_manager.clone();
                log::info!("Stopping all recorders...");
                tauri::async_runtime::block_on(async move {
                    recorder_manager.stop_all().await;
                });
                log::info!("All recorders stopped successfully.");
            }
        });

    Ok(())
}

#[cfg(feature = "headless")]
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the config file
    #[arg(short, long, default_value_t = String::from("config.toml"))]
    config: String,

    /// Path to the database folder
    #[arg(short, long, default_value_t = String::from("./data"))]
    db: String,

    /// ReadOnly mode
    #[arg(short, long, default_value_t = false)]
    readonly: bool,
}

#[cfg(feature = "headless")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = sentry::init((
        "https://fbf803d994a257326ea8f043d79058eb@sentry.vjoi.cn/2",
        sentry::ClientOptions {
            release: sentry::release_name!(),
            // Capture user IPs and potentially sensitive headers when using HTTP server integrations
            // see https://docs.sentry.io/platforms/rust/data-management/data-collected for more info
            send_default_pii: true,
            session_mode: sentry::SessionMode::Application,
            ..Default::default()
        },
    ));
    // get params from command line
    let args = Args::parse();
    let state = setup_server_state(args)
        .await
        .expect("Failed to setup server state");

    // check ffmpeg status
    match ffmpeg::check_ffmpeg().await {
        Err(e) => log::error!("Failed to check ffmpeg version: {e}"),
        Ok(v) => log::info!("Checked ffmpeg version: {v}"),
    }

    http_server::api_server::start_api_server(state).await;
    Ok(())
}
