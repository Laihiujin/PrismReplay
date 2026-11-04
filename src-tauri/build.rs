fn main() {
    #[cfg(feature = "isolated-build")]
    {
        // Check before tauri-build creates any generated app configuration.
        let config = std::env::var("TAURI_CONFIG").unwrap_or_default();
        if !config.contains("cn.live.localbuild") {
            panic!("isolated-build requires --config src-tauri/tauri.local.conf.json");
        }
    }
    #[cfg(feature = "gui")]
    tauri_build::build();
}
