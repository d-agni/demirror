//! REST API, WebSocket streams and the embedded browser client.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{
        ConnectInfo, Path, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use vmon_proto::{
    MonitorInfo, MonitorSpec, Size, SourcesInfo,
    input::{ClientMessage, InputEvent, TouchPhase},
};

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
        can_place: source::can_place(),
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

async fn stream(
    State(app): State<App>,
    Path(id): Path<u32>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    match app.manager.subscribe(id) {
        Some((mut info, rx)) => {
            // A viewer on this computer only watches: its pointer over the picture would move
            // this computer's pointer onto the monitor, and keep it there.
            if info.input && is_this_computer(peer.ip()) {
                tracing::info!(id, "a viewer on this computer: it only watches");
                info.input = false;
                info.touch = false;
            }
            ws.on_upgrade(move |socket| send_frames(socket, app, info, rx))
        }
        None => not_found(id).into_response(),
    }
}

/// Whether `ip` is one of this computer's addresses (only those can be bound to).
fn is_this_computer(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    ip.is_loopback() || UdpSocket::bind((ip, 0)).is_ok()
}

/// Sends the monitor info as a text message, then every new frame as a
/// binary JPEG. A slow client simply skips frames: the channel keeps only the latest.
/// Text messages from the viewer are its input (`ClientMessage`).
async fn send_frames(mut socket: WebSocket, app: App, info: MonitorInfo, mut rx: FrameRx) {
    let (id, input) = (info.id, info.input);
    let Ok(info) = serde_json::to_string(&info) else {
        return;
    };
    if socket.send(Message::Text(info.into())).await.is_err() {
        return;
    }
    rx.mark_changed();
    let mut fingers = Fingers { app: &app, id, down: HashMap::new() };
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
                Some(Ok(Message::Text(text))) if input => {
                    if let Ok(message) = serde_json::from_str::<ClientMessage>(&text) {
                        fingers.track(&message.input);
                        app.manager.input(id, message.input);
                    }
                }
                _ => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

/// A viewer's fingers that are down: lifted for it if it goes without lifting them (a lost
/// connection), or they'd stay pressed on the host.
struct Fingers<'a> {
    app: &'a App,
    id: u32,
    down: HashMap<u8, (f32, f32)>,
}

impl Fingers<'_> {
    fn track(&mut self, event: &InputEvent) {
        if let InputEvent::Touch { id, phase, x, y } = *event {
            match phase {
                TouchPhase::Down | TouchPhase::Move => self.down.insert(id, (x, y)),
                TouchPhase::Up | TouchPhase::Cancel => self.down.remove(&id),
            };
        }
    }
}

impl Drop for Fingers<'_> {
    fn drop(&mut self) {
        for (finger, (x, y)) in self.down.drain() {
            let lift = InputEvent::Touch { id: finger, phase: TouchPhase::Cancel, x, y };
            self.app.manager.input(self.id, lift);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computer() {
        assert!(is_this_computer("127.0.0.1".parse().unwrap()));
        assert!(is_this_computer("::1".parse().unwrap()));
        assert!(is_this_computer("::ffff:127.0.0.1".parse().unwrap()));
        // TEST-NET-3: nobody's address.
        assert!(!is_this_computer("203.0.113.7".parse().unwrap()));
    }
}
