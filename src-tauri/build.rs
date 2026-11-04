fn main() {
    #[cfg(feature = "isolated-build")]
    {
        // Check before tauri-build creates any generated app configuration.
        let config = std::env::var("TAURI_CONFIG").unwrap_or_default();
        if !config.contains("cn.live.localbuild") {
            panic!("isolated-build requires --config src-tauri/tauri.local.conf.json");
        }
    }
    // The CUDA installer does not bundle cublas; subtitle_generator::cuda_runtime
    // downloads it on first use, so the app must be able to start without it.
    if std::env::var_os("CARGO_FEATURE_CUDA").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    {
        println!("cargo:rustc-link-arg=/DELAYLOAD:cublas64_12.dll");
        println!("cargo:rustc-link-arg=/DELAYLOAD:cublasLt64_12.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }
    #[cfg(feature = "gui")]
    tauri_build::build();
}
