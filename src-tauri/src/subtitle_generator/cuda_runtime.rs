//! Fetches the CUDA math libraries the first time GPU transcription runs.
//!
//! cublas and cublasLt add over 400 MB to the installer, so they are delay-loaded
//! (see build.rs) and kept out of the bundle. They must be loaded here before
//! anything calls into whisper: a delay-load miss raises an exception that aborts
//! the process.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

use crate::constants::APP_DATA_ID;
use crate::progress::progress_reporter::ProgressReporterTrait;

const VERSION: &str = "12.8.4.1";
// From NVIDIA's manifest: compute/cuda/redist/redistrib_12.8.1.json
const ARCHIVE_URL: &str = "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.8.4.1-archive.zip";
const ARCHIVE_SHA256: &str = "57a470112cec7e112c95253dde8b3c7184d795dbd92b0bde77a4cb7f8c94c8aa";
const ARCHIVE_SIZE: u64 = 563_660_944;
// cublas imports cublasLt, so cublasLt has to be loaded first.
const LIBRARIES: [&str; 2] = ["cublasLt64_12.dll", "cublas64_12.dll"];

static LOADED: AtomicBool = AtomicBool::new(false);

fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Makes the CUDA libraries available, downloading them if needed. Call this
/// before creating a whisper context.
pub async fn ensure_loaded(
    reporter: Option<&(impl ProgressReporterTrait + 'static)>,
) -> Result<(), String> {
    let _guard = install_lock().lock().await;
    if LOADED.load(Ordering::Acquire) {
        return Ok(());
    }

    // Without NVIDIA's driver the download could never be used.
    if !load_library(Path::new("nvcuda.dll")) {
        return Err("未检测到 NVIDIA 显卡驱动，无法使用 CUDA 版生成字幕，请改用 CPU 版".to_string());
    }

    let directory = runtime_directory()?;
    if !is_installed(&directory) {
        install(&directory, reporter).await?;
    }

    for library in LIBRARIES {
        let path = directory.join(library);
        if !load_library(&path) {
            return Err(format!("加载 CUDA 运行库失败: {}", path.display()));
        }
    }
    LOADED.store(true, Ordering::Release);
    Ok(())
}

fn load_library(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // The handle is kept on purpose: these stay loaded for the life of the process.
    unsafe { LoadLibraryW(wide.as_ptr()) != 0 }
}

fn runtime_directory() -> Result<PathBuf, String> {
    let dirs = platform_dirs::AppDirs::new(Some(APP_DATA_ID), false)
        .ok_or_else(|| "无法确定应用数据目录".to_string())?;
    Ok(dirs.data_dir.join(format!("cuda-runtime-{VERSION}")))
}

fn is_installed(directory: &Path) -> bool {
    LIBRARIES
        .iter()
        .all(|library| directory.join(library).is_file())
}

async fn install(
    directory: &Path,
    reporter: Option<&(impl ProgressReporterTrait + 'static)>,
) -> Result<(), String> {
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|e| format!("创建 CUDA 运行库目录失败: {e}"))?;

    let archive = directory.join("libcublas.zip.part");
    download(&archive, reporter).await?;

    if let Some(reporter) = reporter {
        reporter.update("校验 CUDA 运行库").await;
    }
    let target = directory.to_path_buf();
    let source = archive.clone();
    tokio::task::spawn_blocking(move || {
        verify(&source)?;
        extract(&source, &target)
    })
    .await
    .map_err(|e| format!("安装 CUDA 运行库失败: {e}"))??;

    let _ = tokio::fs::remove_file(&archive).await;
    Ok(())
}

/// Resumes a partial download when the server allows it.
async fn download(
    archive: &Path,
    reporter: Option<&(impl ProgressReporterTrait + 'static)>,
) -> Result<(), String> {
    let mut downloaded = tokio::fs::metadata(archive)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if downloaded == ARCHIVE_SIZE {
        return Ok(());
    }
    if downloaded > ARCHIVE_SIZE {
        tokio::fs::remove_file(archive)
            .await
            .map_err(|e| format!("清理 CUDA 运行库缓存失败: {e}"))?;
        downloaded = 0;
    }

    let mut response = reqwest::Client::new()
        .get(ARCHIVE_URL)
        .header(reqwest::header::RANGE, format!("bytes={downloaded}-"))
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|e| format!("下载 CUDA 运行库失败: {e}"))?;

    let resuming = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !resuming {
        downloaded = 0;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(archive)
        .await
        .map_err(|e| format!("写入 CUDA 运行库失败: {e}"))?;

    let mut last_percent = None;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("下载 CUDA 运行库中断，再次生成字幕会继续下载: {e}"))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入 CUDA 运行库失败: {e}"))?;
        downloaded += chunk.len() as u64;

        let percent = downloaded * 100 / ARCHIVE_SIZE;
        if last_percent != Some(percent) {
            last_percent = Some(percent);
            if let Some(reporter) = reporter {
                reporter
                    .update(&format!("首次使用，下载 CUDA 运行库 {percent}%"))
                    .await;
            }
        }
    }
    file.flush()
        .await
        .map_err(|e| format!("写入 CUDA 运行库失败: {e}"))
}

fn verify(archive: &Path) -> Result<(), String> {
    let mut file =
        std::fs::File::open(archive).map_err(|e| format!("读取 CUDA 运行库失败: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("读取 CUDA 运行库失败: {e}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    drop(file);

    let actual: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != ARCHIVE_SHA256 {
        let _ = std::fs::remove_file(archive);
        return Err("CUDA 运行库校验失败，已删除，请重试".to_string());
    }
    Ok(())
}

/// Each library is renamed into place only once complete, so `is_installed` never
/// sees a partial file.
fn extract(archive: &Path, directory: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("读取 CUDA 运行库失败: {e}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("解析 CUDA 运行库失败: {e}"))?;

    for library in LIBRARIES {
        let suffix = format!("/{library}");
        let entry_name = zip
            .file_names()
            .find(|name| name.ends_with(&suffix))
            .map(str::to_string)
            .ok_or_else(|| format!("CUDA 运行库中缺少 {library}"))?;
        let mut entry = zip
            .by_name(&entry_name)
            .map_err(|e| format!("解压 CUDA 运行库失败: {e}"))?;

        let temporary = directory.join(format!("{library}.tmp"));
        let mut output = std::fs::File::create(&temporary)
            .map_err(|e| format!("解压 CUDA 运行库失败: {e}"))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|e| format!("解压 CUDA 运行库失败: {e}"))?;
        drop(output);
        std::fs::rename(&temporary, directory.join(library))
            .map_err(|e| format!("解压 CUDA 运行库失败: {e}"))?;
    }
    Ok(())
}
