use recorder::account::Account;
use recorder::platforms::kuaishou::api;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KuaishouProtocol {
    Hls,
    Flv,
    Rtmp,
}

impl KuaishouProtocol {
    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hls" | "m3u8" => Some(Self::Hls),
            "flv" => Some(Self::Flv),
            "rtmp" => Some(Self::Rtmp),
            _ => None,
        }
    }

    fn matches_url(self, url: &str) -> bool {
        match self {
            Self::Hls => url.contains(".m3u8"),
            Self::Flv => url.contains(".flv"),
            Self::Rtmp => url.starts_with("rtmp://") || url.starts_with("rtmps://"),
        }
    }
}

fn parse_bool_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn prefer_flv_env() -> Option<bool> {
    std::env::var("LIVE_KUAISHOU_PREFER_FLV")
        .ok()
        .and_then(|v| parse_bool_env(&v))
}

fn prefer_protocol() -> KuaishouProtocol {
    if let Ok(value) = std::env::var("LIVE_KUAISHOU_PREFER_PROTOCOL") {
        if let Some(protocol) = KuaishouProtocol::from_str(&value) {
            return protocol;
        }
    }
    if let Some(prefer_flv) = prefer_flv_env() {
        if prefer_flv {
            return KuaishouProtocol::Flv;
        }
    }
    KuaishouProtocol::Hls
}

fn protocol_preference_overridden() -> bool {
    std::env::var("LIVE_KUAISHOU_PREFER_PROTOCOL")
        .ok()
        .and_then(|value| KuaishouProtocol::from_str(&value))
        .is_some()
        || prefer_flv_env().is_some()
}

fn select_stream_url(
    streams: &[api::StreamInfo],
    prefer: KuaishouProtocol,
    prefer_login_direct: bool,
) -> Option<String> {
    let is_direct = |url: &str| url.contains("pull.yximgs.com");

    if prefer_login_direct {
        if let Some(selected) = streams
            .iter()
            .find(|stream| is_direct(&stream.url) && prefer.matches_url(&stream.url))
            .map(|stream| stream.url.clone())
        {
            return Some(selected);
        }
        if let Some(selected) = streams
            .iter()
            .find(|stream| is_direct(&stream.url) && KuaishouProtocol::Flv.matches_url(&stream.url))
            .map(|stream| stream.url.clone())
        {
            return Some(selected);
        }
        if let Some(selected) = streams
            .iter()
            .find(|stream| is_direct(&stream.url))
            .map(|stream| stream.url.clone())
        {
            return Some(selected);
        }
    }

    let mut selected = streams
        .iter()
        .find(|stream| prefer.matches_url(&stream.url))
        .map(|stream| stream.url.clone());

    if selected.is_none() {
        selected = streams
            .iter()
            .find(|stream| KuaishouProtocol::Hls.matches_url(&stream.url))
            .map(|stream| stream.url.clone());
    }

    if selected.is_none() {
        selected = streams
            .iter()
            .find(|stream| KuaishouProtocol::Flv.matches_url(&stream.url))
            .map(|stream| stream.url.clone());
    }

    if selected.is_none() {
        selected = streams
            .iter()
            .find(|stream| KuaishouProtocol::Rtmp.matches_url(&stream.url))
            .map(|stream| stream.url.clone());
    }

    if selected.is_none() {
        selected = streams.first().map(|stream| stream.url.clone());
    }

    selected
}

fn build_account() -> Account {
    let cookies = std::env::var("LIVE_PROBE_COOKIE").unwrap_or_default();
    let id = std::env::var("LIVE_PROBE_ACCOUNT_ID").unwrap_or_else(|_| "guest:probe".to_string());
    Account {
        platform: "kuaishou".to_string(),
        id,
        name: "probe".to_string(),
        avatar: String::new(),
        csrf: String::new(),
        cookies,
    }
}

fn print_streams(tag: &str, streams: &[api::StreamInfo]) {
    println!("{tag}_stream_count={}", streams.len());
    for (idx, stream) in streams.iter().take(10).enumerate() {
        println!(
            "{tag}_stream[{idx}] quality={} bitrate={:?} url={}",
            stream.quality, stream.bitrate, stream.url
        );
    }
}

#[tokio::main]
async fn main() {
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,recorder::platforms::kuaishou::api=debug"),
    )
    .is_test(false)
    .try_init();

    let mut urls: Vec<String> = std::env::args().skip(1).collect();
    if urls.is_empty() {
        urls = vec![
            "https://live.kuaishou.com/u/3xvw39dtcbnya89".to_string(),
            "https://live.kuaishou.com/u/menmen987".to_string(),
        ];
    }

    let account = build_account();
    let mut client_builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .no_proxy();
    if let Some(proxy_url) = std::env::var("LIVE_PROBE_PROXY")
        .ok()
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .or_else(|| std::env::var("HTTP_PROXY").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let proxy = reqwest::Proxy::all(&proxy_url).expect("invalid probe proxy URL");
        client_builder = client_builder.proxy(proxy);
    }
    let client = client_builder
        .cookie_store(true)
        .build()
        .expect("failed to build reqwest client");
    let prefer = prefer_protocol();
    let prefer_login_direct = !account.is_guest() && !protocol_preference_overridden();

    println!(
        "probe_account_id={} guest={} cookie_len={} prefer={:?} prefer_login_direct={}",
        account.id,
        account.is_guest(),
        account.cookies.len(),
        prefer,
        prefer_login_direct
    );

    for url in urls {
        println!("\n=== {url} ===");
        match api::get_room_info(&client, &account, &url).await {
            Ok(room) => {
                println!(
                    "get_room_info ok: live_status={} user_id={} user_name={} room_title={}",
                    room.live_status, room.user_id, room.user_name, room.room_title
                );
                print_streams("room_info", &room.streams);

                let selected = select_stream_url(&room.streams, prefer, prefer_login_direct);
                println!("selected_from_room_info={selected:?}");
            }
            Err(err) => {
                println!("get_room_info err: {err}");
            }
        }

        match api::get_stream_urls(&client, &account, &url).await {
            Ok(streams) => {
                print_streams("stream_urls", &streams);
                let selected = select_stream_url(&streams, prefer, prefer_login_direct);
                println!("selected_from_stream_urls={selected:?}");
            }
            Err(err) => {
                println!("get_stream_urls err: {err}");
            }
        }
    }
}
