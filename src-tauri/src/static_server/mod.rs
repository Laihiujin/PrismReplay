use crate::config::Config;
use crate::recorder_manager::RecorderManager;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

pub struct StaticServer {
    #[allow(dead_code)]
    pub handle: JoinHandle<()>,
    pub port: u16,
}

impl StaticServer {
    #[cfg(feature = "headless")]
    pub fn headless(port: u16) -> Self {
        Self {
            handle: tokio::spawn(async {}),
            port,
        }
    }
}

async fn handler_hls(
    State(recorder_manager): State<Arc<RecorderManager>>,
    Path(uri): Path<String>,
    query: Option<Query<HashMap<String, String>>>,
) -> Result<impl IntoResponse, StatusCode> {
    let path_segs: Vec<&str> = uri.split('/').collect();
    if path_segs.len() < 4 {
        return Err(StatusCode::NOT_FOUND);
    }

    let filename = path_segs[3];
    let query_str = query
        .map(|q| {
            q.0.iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<String>>()
                .join("&")
        })
        .unwrap_or_default();
    let uri_with_query = if query_str.is_empty() {
        uri.clone()
    } else {
        format!("{}?{}", uri, query_str)
    };

    let hls = recorder_manager
        .handle_hls_request(&uri_with_query)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let content_type = match filename.split('.').next_back() {
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("ts") => "video/mp2t",
        Some("aac") => "audio/aac",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("m4s") => "video/iso.segment",
        _ => "application/octet-stream",
    };

    let mut response =
        axum::response::Response::<axum::body::Body>::new(axum::body::Body::from(hls));
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        content_type.parse().unwrap(),
    );
    if filename.ends_with(".m3u8") {
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            "no-cache, no-store, must-revalidate".parse().unwrap(),
        );
        headers.insert(axum::http::header::PRAGMA, "no-cache".parse().unwrap());
        headers.insert(axum::http::header::EXPIRES, "0".parse().unwrap());
    }

    Ok(response)
}

pub async fn start_static_server(
    config: Arc<RwLock<Config>>,
    recorder_manager: Arc<RecorderManager>,
) -> Result<StaticServer, Box<dyn std::error::Error>> {
    let interface = if cfg!(feature = "isolated-build") { [127, 0, 0, 1] } else { [0, 0, 0, 0] };
    let bind_addr = SocketAddr::from((interface, 0));
    log::info!("Starting static server binding to {}", bind_addr);

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(listener) => {
            match listener.local_addr() {
                Ok(addr) => log::info!("Static server listening on http://{}", addr),
                Err(e) => log::warn!("Unable to determine listening address: {}", e),
            }
            listener
        }
        Err(e) => {
            log::error!("Failed to bind static server: {}", e);
            log::error!("Please check if the port is already in use or try a different port");
            return Err(e.into());
        }
    };

    let port = listener.local_addr().unwrap().port();

    let output_path = config.read().await.output.clone();
    let cache_path = config.read().await.cache.clone();

    let handle = tokio::spawn(async move {
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
        let router = Router::new()
            .route("/hls/*uri", get(handler_hls))
            .with_state(recorder_manager)
            .nest_service("/output", ServeDir::new(output_path))
            .nest_service("/cache", ServeDir::new(cache_path))
            .layer(cors);

        if let Err(e) = axum::serve(listener, router).await {
            log::error!("Server error: {}", e);
        }
    });

    Ok(StaticServer { handle, port })
}
