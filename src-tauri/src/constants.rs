pub const PREFIX_SUBTITLE: &str = "[subtitle]";
pub const PREFIX_IMPORTED: &str = "[imported]";
pub const PREFIX_DANMAKU: &str = "[danmaku]";
pub const PREFIX_CLIP: &str = "[clip]";

#[allow(dead_code)]
pub const API_PORT: u16 = 3000;

// The experimental executable must never reuse the installed app's storage.
#[cfg(feature = "isolated-build")]
pub const APP_DATA_ID: &str = "cn.prismreplay.localbuild";
#[cfg(not(feature = "isolated-build"))]
pub const APP_DATA_ID: &str = "cn.prismreplay";
