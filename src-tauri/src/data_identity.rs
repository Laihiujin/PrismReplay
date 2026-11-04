//! LIVE storage relocation. Old names exist only as explicit migration inputs.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Move whole app-owned roots before SQLite/WebView initialization. Never merge
/// or overwrite an existing destination; fail visibly before moving any root.
pub fn migrate_directories(pairs: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    let mut pending = Vec::new();
    for (old, new) in pairs {
        match fs::symlink_metadata(old) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "LIVE migration source is not a directory: {}",
                        old.display()
                    ),
                ));
            }
            Ok(_) => {}
        }
        match fs::symlink_metadata(new) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "LIVE migration will not merge existing folders: {} -> {}",
                        old.display(),
                        new.display()
                    ),
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if !pending.iter().any(|(source, _)| source == old) {
            pending.push((old.clone(), new.clone()));
        }
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (old, new) in pending {
        let result = (|| {
            if let Some(parent) = new.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&old, &new)
        })();
        if let Err(error) = result {
            for (source, destination) in moved.iter().rev() {
                if let Err(rollback) = fs::rename(destination, source) {
                    return Err(io::Error::other(format!(
                        "LIVE migration failed: {error}; restoring {} failed: {rollback}",
                        source.display()
                    )));
                }
            }
            return Err(error);
        }
        moved.push((old, new));
    }
    Ok(())
}

/// Rewrite only descendants of known app roots, preserving custom user paths.
pub fn remap_path(value: &str, pairs: &[(PathBuf, PathBuf)]) -> String {
    for (old, new) in pairs {
        if let Ok(suffix) = Path::new(value).strip_prefix(old) {
            return new.join(suffix).to_string_lossy().into_owned();
        }
    }
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn fixture() -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "live-migration-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        (root.join("old"), root.join("cn.live"), root)
    }
    #[test]
    fn preserves_bytes_and_removes_old_folder() {
        let (old, new, root) = fixture();
        fs::create_dir_all(old.join("cache")).unwrap();
        fs::write(old.join("data_v2.db"), b"fixture sqlite bytes\0").unwrap();
        fs::write(old.join("accounts.toml"), b"fixture account bytes").unwrap();
        migrate_directories(&[(old.clone(), new.clone())]).unwrap();
        assert!(!old.exists());
        assert_eq!(
            fs::read(new.join("data_v2.db")).unwrap(),
            b"fixture sqlite bytes\0"
        );
        assert_eq!(
            fs::read(new.join("accounts.toml")).unwrap(),
            b"fixture account bytes"
        );
        assert!(new.join("cache").is_dir());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn idempotent_and_duplicate_roots() {
        let (old, new, root) = fixture();
        fs::create_dir_all(&old).unwrap();
        let pairs = vec![(old.clone(), new.clone()), (old, new)];
        migrate_directories(&pairs).unwrap();
        migrate_directories(&pairs).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn conflict_preflight_moves_nothing() {
        let (old, new, root) = fixture();
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("data_v2.db"), b"keep existing").unwrap();
        let another = root.join("another");
        fs::create_dir_all(&another).unwrap();
        let err = migrate_directories(&[
            (another.clone(), root.join("next")),
            (old.clone(), new.clone()),
        ])
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(old.exists() && another.exists());
        assert_eq!(fs::read(new.join("data_v2.db")).unwrap(), b"keep existing");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rolls_back_prior_move_on_error() {
        let (old, new, root) = fixture();
        fs::create_dir_all(&old).unwrap();
        let another = root.join("another");
        fs::create_dir_all(&another).unwrap();
        let blocker = root.join("file");
        fs::write(&blocker, b"not a directory").unwrap();
        assert!(migrate_directories(&[
            (old.clone(), new.clone()),
            (another.clone(), blocker.join("dest"))
        ])
        .is_err());
        assert!(old.exists() && another.exists());
        assert!(!new.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn maps_only_app_owned_prefixes() {
        let pairs = vec![(PathBuf::from("/app/old"), PathBuf::from("/app/cn.live"))];
        assert_eq!(
            remap_path("/app/old/cache/live", &pairs),
            "/app/cn.live/cache/live"
        );
        assert_eq!(
            remap_path("/app/old-other/cache", &pairs),
            "/app/old-other/cache"
        );
        assert_eq!(remap_path("/custom/output", &pairs), "/custom/output");
    }
    #[test]
    fn missing_source_is_noop() {
        let (old, new, root) = fixture();
        migrate_directories(&[(old, new.clone())]).unwrap();
        assert!(!new.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_sources() {
        let (old, new, root) = fixture();
        std::os::unix::fs::symlink(&root, &old).unwrap();
        assert!(migrate_directories(&[(old, new)]).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
