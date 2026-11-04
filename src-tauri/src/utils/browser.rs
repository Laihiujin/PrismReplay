use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;
#[cfg(target_os = "windows")]
use base64::{engine::general_purpose, Engine as _};
#[cfg(target_os = "windows")]
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::{fs, io};

#[cfg(target_os = "windows")]
use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

#[cfg(target_os = "windows")]
type DataBlob = CRYPT_INTEGER_BLOB;

#[derive(Debug, serde::Serialize)]
#[cfg_attr(feature = "headless", allow(dead_code))]
pub struct BrowserCookie {
    pub host: String,
    pub name: String,
    pub value: String,
    pub path: String,
    pub expires: i64,
}

#[cfg_attr(feature = "headless", allow(dead_code))]
pub struct BrowserCookieCollector {
    pub profile_path: PathBuf,
}

impl BrowserCookieCollector {
    #[cfg_attr(feature = "headless", allow(dead_code))]
    pub fn new_chrome() -> Option<Self> {
        let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
        let path = Path::new(&local_app_data).join("Google/Chrome/User Data");
        if path.exists() {
            Some(Self { profile_path: path })
        } else {
            None
        }
    }

    #[cfg_attr(feature = "headless", allow(dead_code))]
    pub fn new_edge() -> Option<Self> {
        let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
        let path = Path::new(&local_app_data).join("Microsoft/Edge/User Data");
        if path.exists() {
            Some(Self { profile_path: path })
        } else {
            None
        }
    }

    #[cfg(target_os = "windows")]
    #[cfg_attr(feature = "headless", allow(dead_code))]
    fn get_master_key(&self) -> anyhow::Result<Vec<u8>> {
        let local_state_path = self.profile_path.join("Local State");
        let content = fs::read_to_string(local_state_path)?;
        let json: Value = serde_json::from_str(&content)?;

        let encrypted_key_b64 = json["os_crypt"]["encrypted_key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No encrypted_key found in Local State"))?;

        let encrypted_key_raw = general_purpose::STANDARD.decode(encrypted_key_b64)?;

        if encrypted_key_raw.len() < 5 || &encrypted_key_raw[0..5] != b"DPAPI" {
            return Err(anyhow::anyhow!("Invalid key prefix"));
        }

        let encrypted_key = &encrypted_key_raw[5..];

        unsafe {
            let mut input = DataBlob {
                cbData: encrypted_key.len() as u32,
                pbData: encrypted_key.as_ptr() as *mut u8,
            };
            let mut output = DataBlob {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            };

            if CryptUnprotectData(
                &mut input as *mut _,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output as *mut _,
            ) != 0
            {
                let decrypted =
                    std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
                // Note: We should ideally call LocalFree(output.pbData) here, but for now we prioritize correctness of decryption
                Ok(decrypted)
            } else {
                Err(anyhow::anyhow!("CryptUnprotectData failed"))
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn get_master_key(&self) -> anyhow::Result<Vec<u8>> {
        Err(anyhow::anyhow!("Non-windows platforms not supported yet"))
    }

    #[cfg_attr(feature = "headless", allow(dead_code))]
    pub fn get_cookies(&self, domain_filter: &str) -> anyhow::Result<Vec<BrowserCookie>> {
        let master_key = self.get_master_key()?;
        let mut cookies = Vec::new();

        // Discover profiles dynamically to handle non-standard profile names.
        let mut profiles = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.profile_path) {
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "Default" || name.starts_with("Profile ") {
                    profiles.push(name);
                }
            }
        }
        if profiles.is_empty() {
            profiles = vec![
                "Default".to_string(),
                "Profile 1".to_string(),
                "Profile 2".to_string(),
                "Profile 3".to_string(),
                "Profile 4".to_string(),
                "Profile 5".to_string(),
            ];
        }

        for profile in profiles {
            let profile_dir = self.profile_path.join(&profile);
            let cookie_path = {
                let network_path = profile_dir.join("Network/Cookies");
                if network_path.exists() {
                    network_path
                } else {
                    profile_dir.join("Cookies")
                }
            };
            if !cookie_path.exists() {
                continue;
            }

            // Copy file to avoid lock
            let temp_db =
                std::env::temp_dir().join(format!("live_cookies_{}.db", profile.replace(' ', "_")));
            if let Err(e) = copy_cookie_db(&cookie_path, &temp_db) {
                log::warn!(
                    "Failed to copy cookie database for profile {}: {}",
                    profile,
                    e
                );
                continue;
            }

            match rusqlite::Connection::open(&temp_db) {
                Ok(conn) => {
                    let mut stmt = conn.prepare("SELECT host_key, name, encrypted_value, value, path, expires_utc FROM cookies WHERE host_key LIKE ?")?;
                    let rows = stmt.query_map([format!("%{}%", domain_filter)], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    })?;

                    for row in rows {
                        if let Ok((host, name, encrypted_value, value_plain, path, expires)) = row {
                            let mut value = if !encrypted_value.is_empty() {
                                self.decrypt_cookie(&encrypted_value, &master_key).ok()
                            } else {
                                None
                            };
                            if value.is_none() && !value_plain.is_empty() {
                                value = Some(value_plain);
                            }
                            if let Some(value) = value {
                                cookies.push(BrowserCookie {
                                    host,
                                    name,
                                    value,
                                    path,
                                    expires,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "Failed to open cookie database for profile {}: {}",
                        profile,
                        e
                    );
                }
            }

            let _ = fs::remove_file(temp_db);
        }

        Ok(cookies)
    }

    #[cfg_attr(feature = "headless", allow(dead_code))]
    pub fn get_cookies_as_string(&self, domain_filter: &str) -> anyhow::Result<String> {
        let cookies = self.get_cookies(domain_filter)?;
        let cookie_str = cookies
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        Ok(cookie_str)
    }

    #[cfg_attr(feature = "headless", allow(dead_code))]
    fn decrypt_cookie(&self, encrypted_value: &[u8], master_key: &[u8]) -> anyhow::Result<String> {
        if encrypted_value.is_empty() {
            return Ok(String::new());
        }

        // Chromium v80+ use "v10" or "v11" prefix
        if encrypted_value.len() > 15
            && (&encrypted_value[0..3] == b"v10" || &encrypted_value[0..3] == b"v11")
        {
            let nonce = &encrypted_value[3..15];
            let ciphertext = &encrypted_value[15..];

            let cipher = Aes256Gcm::new_from_slice(master_key)?;
            let decrypted = cipher
                .decrypt(aes_gcm::Nonce::from_slice(nonce), ciphertext)
                .map_err(|e| anyhow::anyhow!("AES decryption failed: {}", e))?;

            Ok(String::from_utf8(decrypted)?)
        } else {
            // Older versions or different format (Direct DPAPI)
            #[cfg(target_os = "windows")]
            {
                unsafe {
                    let mut input = DataBlob {
                        cbData: encrypted_value.len() as u32,
                        pbData: encrypted_value.as_ptr() as *mut u8,
                    };
                    let mut output = DataBlob {
                        cbData: 0,
                        pbData: std::ptr::null_mut(),
                    };

                    if CryptUnprotectData(
                        &mut input as *mut _,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0,
                        &mut output as *mut _,
                    ) != 0
                    {
                        let decrypted =
                            std::slice::from_raw_parts(output.pbData, output.cbData as usize)
                                .to_vec();
                        Ok(String::from_utf8(decrypted)?)
                    } else {
                        Err(anyhow::anyhow!("DPAPI decryption failed"))
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                Err(anyhow::anyhow!(
                    "Legacy decryption only supported on Windows"
                ))
            }
        }
    }
}

#[cfg_attr(feature = "headless", allow(dead_code))]
fn copy_cookie_db(src: &Path, dst: &Path) -> io::Result<()> {
    if let Ok(_) = fs::copy(src, dst) {
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        use std::fs::OpenOptions;
        use std::io::{Read, Write};
        use std::os::windows::fs::OpenOptionsExt;

        let mut src_file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(src)?;
        let mut dst_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(dst)?;

        let mut buf = Vec::new();
        src_file.read_to_end(&mut buf)?;
        dst_file.write_all(&buf)?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "Failed to copy cookie db",
        ))
    }
}
