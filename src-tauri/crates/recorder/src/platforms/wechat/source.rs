//! Parse media URLs supplied by the user or by the opt-in capture in `capture.rs`.
//! Compatible with res-downloader's MediaInfo and video-channel objectDesc JSON.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Clone, Serialize, Deserialize)]
pub struct WechatSource {
    pub url: String,
    pub title: String,
    /// Observed as a live stream by the capture proxy, so its path need not end in an extension.
    #[serde(default)]
    pub captured_stream: bool,
}

impl WechatSource {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() || input.len() > 131_072 {
            return Err("请粘贴视频号媒体直链或单条资源 JSON（不超过 128KB）".into());
        }
        let mut captured_stream = false;
        let (raw_url, title) = if input.starts_with('{') {
            let value: Value = serde_json::from_str(input)
                .map_err(|_| "资源 JSON 无效，请复制单条资源对象".to_string())?;
            captured_stream = value
                .get("captured_stream")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let desc = value
                .pointer("/data/object/objectDesc")
                .or_else(|| value.get("objectDesc"))
                .unwrap_or(&value);
            let media = desc.pointer("/media/0").unwrap_or(desc);
            for obj in [&value, desc, media] {
                for key in ["decodeKey", "DecodeKey", "decode_key"] {
                    if obj.get(key).is_some_and(|v| {
                        !v.is_null()
                            && v.as_str() != Some("")
                            && v.as_str() != Some("0")
                            && v.as_u64() != Some(0)
                    }) {
                        return Err("该资源标记为加密视频，请先用原工具导出已授权的可播放文件；此适配不处理解密".into());
                    }
                }
            }
            let text = |obj: &Value, keys: &[&str]| -> Option<String> {
                keys.iter()
                    .find_map(|key| obj.get(*key).and_then(Value::as_str))
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            };
            let mut url = text(media, &["url", "Url", "liveUrl", "playUrl"])
                .ok_or_else(|| "JSON 中缺少媒体 url/Url 字段".to_string())?;
            if let Some(token) = text(media, &["urlToken"]) {
                // urlToken is an opaque signed suffix: do not decode or reorder it.
                if !url.ends_with(&token) {
                    url.push_str(&token);
                }
            }
            let title = text(desc, &["description", "Description", "title"])
                .or_else(|| text(&value, &["description", "Description", "title"]))
                .unwrap_or_else(|| "视频号录制".into());
            (url, title)
        } else {
            (input.to_string(), "视频号录制".into())
        };
        if raw_url.chars().any(char::is_control) || raw_url.trim() != raw_url {
            return Err("媒体 URL 含有无效空白或控制字符".into());
        }
        let url = Url::parse(&raw_url).map_err(|_| "媒体地址不是有效 URL".to_string())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("只接受不含用户凭据及片段的 HTTP(S) 媒体直链".into());
        }
        let host = url.host_str().unwrap_or("");
        if !["qq.com", "myqcloud.com", "qcloud.com", "qcloudcdn.com", "wxlivecdn.com"]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        {
            return Err(format!("暂不支持域名 {host}：请使用腾讯/微信 CDN 媒体直链，而非分享页或其他站点"));
        }
        let path = url.path().to_ascii_lowercase();
        // wxlivecdn.com only serves live streams, whatever the path looks like.
        let live_cdn = host == "wxlivecdn.com" || host.ends_with(".wxlivecdn.com");
        if !captured_stream
            && !live_cdn
            && !(path.ends_with(".m3u8")
                || path.ends_with(".flv")
                || path.ends_with(".mp4")
                || path.ends_with("/stodownload"))
        {
            return Err("请复制 m3u8/flv/mp4 媒体直链，不支持用视频号分享页直接解析".into());
        }
        Ok(Self {
            url: raw_url,
            title: title.chars().take(160).collect(),
            captured_stream,
        })
    }

    /// Keep signed query strings out of room IDs, paths, logs and notifications.
    pub fn room_id(&self) -> String {
        let base = self.url.split('?').next().unwrap_or(&self.url);
        format!("wx_{:x}", md5::compute(base))
    }

    pub fn to_extra(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|_| "无法保存视频号资源信息".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_stream_url_is_preserved() {
        let url = "https://pull.example.myqcloud.com/live/one.flv?token=A%2FB&txTime=1";
        let source = WechatSource::parse(url).unwrap();
        assert_eq!(source.url, url);
        assert!(source.room_id().starts_with("wx_"));
        assert!(!source.room_id().contains("token"));
        assert_eq!(
            source.room_id(),
            WechatSource::parse(&url.replace("txTime=1", "txTime=2"))
                .unwrap()
                .room_id()
        );
    }

    #[test]
    fn imports_res_downloader_media_info() {
        let source = WechatSource::parse(r#"{"Url":"https://finder.video.qq.com/v.mp4?sig=1","Description":"回放","DecodeKey":""}"#).unwrap();
        assert_eq!(source.title, "回放");
        assert_eq!(
            WechatSource::parse(&source.to_extra().unwrap())
                .unwrap()
                .url,
            source.url
        );
    }

    #[test]
    fn imports_object_desc_and_opaque_token() {
        let source = WechatSource::parse(r#"{"objectDesc":{"description":"直播","media":[{"url":"https://wxapp.tc.qq.com/live/a.m3u8?x=1","urlToken":"&token=A%2FB"}]}}"#).unwrap();
        assert!(source.url.ends_with("?x=1&token=A%2FB"));
        assert_eq!(source.title, "直播");
    }

    #[test]
    fn captured_stream_without_extension_survives_saving() {
        let input = r#"{"url":"https://live.video.qq.com/stream?sig=1","title":"直播","captured_stream":true}"#;
        let source = WechatSource::parse(input).unwrap();
        let saved = WechatSource::parse(&source.to_extra().unwrap()).unwrap();
        assert_eq!(saved.url, "https://live.video.qq.com/stream?sig=1");
        assert!(saved.captured_stream);
        // The host restriction still applies to captured streams.
        assert!(WechatSource::parse(r#"{"url":"https://evil.test/stream","captured_stream":true}"#).is_err());
        // Without the capture flag an extensionless URL is still rejected.
        assert!(WechatSource::parse("https://live.video.qq.com/stream?sig=1").is_err());
    }

    #[test]
    fn live_cdn_is_accepted_with_any_path() {
        let source = WechatSource::parse("https://a.wxlivecdn.com/trtc_1/stream?sign=1").unwrap();
        assert_eq!(source.url, "https://a.wxlivecdn.com/trtc_1/stream?sign=1");
        assert!(WechatSource::parse("https://wxlivecdn.com.evil.test/a.flv").is_err());
    }

    #[test]
    fn rejects_unsafe_unsupported_and_encrypted_inputs() {
        for input in [
            "",
            "file:///tmp/a.mp4",
            "https://localhost/a.mp4",
            "https://qq.com.evil.test/a.flv",
            "https://channels.weixin.qq.com/web/pages/feed",
            "https://user:pass@finder.video.qq.com/a.mp4",
            "https://finder.video.qq.com/a.mp4#fragment",
            "{broken}",
            r#"{"Url":"https://finder.video.qq.com/a.mp4","DecodeKey":"123"}"#,
        ] {
            assert!(
                WechatSource::parse(input).is_err(),
                "accepted invalid fixture"
            );
        }
    }
}
