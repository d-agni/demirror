//! REST API, WebSocket streams and the embedded browser client.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        Path, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use vmon_proto::{MonitorInfo, MonitorSpec, Size, SourcesInfo};

use crate::{
    monitor::{FrameRx, MonitorManager},
    source,
};

#[derive(Clone)]
pub struct App {
    pub manager: Arc<MonitorManager>,
    pub token: Option<String>,
}

pub fn router(app: App) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../static/index.html")) }),
        )
        .route(
            "/view/{id}",
            get(|| async { Html(include_str!("../static/view.html")) }),
        )
        .route("/api/sources", get(sources))
        .route("/api/monitors", get(list).post(create))
        .route("/api/monitors/{id}", get(info).delete(close))
        .route("/api/monitors/{id}/size", post(resize))
        .route("/ws/monitors/{id}", get(stream))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .with_state(app)
}

async fn auth(State(app): State<App>, request: Request, next: Next) -> Response {
    let Some(token) = &app.token else {
        return next.run(request).await;
    };
    let from_query = request
        .uri()
        .query()
        .into_iter()
        .flat_map(|q| q.split('&'))
        .any(|pair| pair.strip_prefix("token=") == Some(token.as_str()));
    let from_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        == Some(token.as_str());
    if from_query || from_header {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "missing or wrong token (add ?token=...)",
        )
            .into_response()
    }
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<serde_json::Value>)>;

fn api_error(status: StatusCode, message: impl ToString) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({ "error": message.to_string() })),
    )
}

async fn sources(State(app): State<App>) -> Json<SourcesInfo> {
    let probe =
        tokio::task::spawn_blocking(|| (source::list_outputs(), source::virtual_support())).await;
    let (outputs, (virtual_available, virtual_detail)) =
        probe.unwrap_or_else(|e| (Err(e.into()), (false, "probe failed".into())));
    let outputs = outputs.unwrap_or_else(|e| {
        tracing::debug!("no screens to capture: {e:#}");
        Vec::new()
    });
    Json(SourcesInfo {
        screen_backend: source::screen_backend().map(Into::into),
        outputs,
        can_create_outputs: app.manager.can_create_outputs(),
        virtual_available,
        virtual_detail,
    })
}

async fn list(State(app): State<App>) -> Json<Vec<MonitorInfo>> {
    Json(app.manager.list())
}

async fn info(State(app): State<App>, Path(id): Path<u32>) -> ApiResult<MonitorInfo> {
    app.manager
        .list()
        .into_iter()
        .find(|m| m.id == id)
        .map(Json)
        .ok_or_else(|| not_found(id))
}

async fn create(State(app): State<App>, Json(spec): Json<MonitorSpec>) -> ApiResult<MonitorInfo> {
    let manager = app.manager.clone();
    match tokio::task::spawn_blocking(move || manager.create(spec)).await {
        Ok(Ok(info)) => Ok(Json(info)),
        Ok(Err(e)) => Err(api_error(StatusCode::BAD_REQUEST, format!("{e:#}"))),
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

async fn resize(
    State(app): State<App>,
    Path(id): Path<u32>,
    Json(size): Json<Size>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    match app.manager.resize(id, size.width, size.height) {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(not_found(id)),
        Err(e) => Err(api_error(StatusCode::BAD_REQUEST, format!("{e:#}"))),
    }
}

async fn close(
    State(app): State<App>,
    Path(id): Path<u32>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let manager = app.manager.clone();
    if tokio::task::spawn_blocking(move || manager.close(id))
        .await
        .unwrap_or(false)
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found(id))
    }
}

fn not_found(id: u32) -> (StatusCode, Json<serde_json::Value>) {
    api_error(StatusCode::NOT_FOUND, format!("no monitor {id}"))
}

async fn stream(State(app): State<App>, Path(id): Path<u32>, ws: WebSocketUpgrade) -> Response {
    match app.manager.subscribe(id) {
        Some((info, rx)) => ws.on_upgrade(move |socket| send_frames(socket, info, rx)),
        None => not_found(id).into_response(),
    }
}

/// Sends the monitor info as a text message, then every new frame as a
/// binary JPEG. A slow client simply skips frames: the channel keeps only the latest.
async fn send_frames(mut socket: WebSocket, info: MonitorInfo, mut rx: FrameRx) {
    let Ok(info) = serde_json::to_string(&info) else {
        return;
    };
    if socket.send(Message::Text(info.into())).await.is_err() {
        return;
    }
    rx.mark_changed();
    loop {
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    break; // monitor closed
                }
                let frame = rx.borrow_and_update().clone();
                if let Some(jpeg) = frame
                    && socket.send(Message::Binary(jpeg)).await.is_err() {
                        return;
                    }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                _ => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}
