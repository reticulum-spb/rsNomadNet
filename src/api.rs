use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::oneshot;
use tower_http::trace::TraceLayer;

use crate::app::AppState;
use crate::browser::NomadUrl;
use crate::models::{SendMessageRequest, ServerEvent};

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/api/v1/health", get(health))
        .route("/api/v1/state", get(snapshot))
        .route("/api/v1/conversations", get(conversations))
        .route("/api/v1/directory", get(directory))
        .route("/api/v1/conversations/{destination_hash}", get(messages))
        .route("/api/v1/messages", post(send_message))
        .route("/api/v1/browser/fetch", post(fetch_page))
        .route("/api/v1/browser/download", post(download_file))
        .route("/api/v1/rrc/connect", post(rrc_connect))
        .route("/api/v1/rrc/disconnect", post(rrc_disconnect))
        .route("/api/v1/rrc/join", post(rrc_join))
        .route("/api/v1/rrc/part", post(rrc_part))
        .route("/api/v1/rrc/list", post(rrc_list))
        .route("/api/v1/rrc/who", post(rrc_who))
        .route("/api/v1/rrc/send", post(rrc_send))
        .route("/api/v1/rrc/ping", post(rrc_ping))
        .route("/api/v1/rrc/clear", post(rrc_clear))
        .route("/api/v1/rrc/history/{destination_hash}", get(rrc_history))
        .route("/api/v1/events", get(events))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(serde::Deserialize)]
struct RrcConnectRequest {
    destination_hash: String,
    nick: Option<String>,
}

async fn rrc_connect(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcConnectRequest>,
) -> Response {
    if !matches!(
        state.network.read().await.state,
        crate::models::NetworkState::Online
    ) {
        return unavailable("Reticulum is not online");
    }
    let destination_hash = match parse_hash(&request.destination_hash) {
        Ok(hash) => hash,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid hub destination hash"})),
            )
                .into_response();
        }
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::Connect {
            destination_hash,
            nick: request.nick,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    match rx.await {
        Ok(Ok(hub)) => Json(json!(hub)).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => unavailable("RRC manager stopped"),
    }
}

#[derive(serde::Deserialize)]
struct RrcJoinRequest {
    destination_hash: String,
    room: String,
    key: Option<String>,
}

#[derive(serde::Deserialize)]
struct RrcHubRequest {
    destination_hash: String,
}

async fn rrc_join(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcJoinRequest>,
) -> Response {
    if !matches!(
        state.network.read().await.state,
        crate::models::NetworkState::Online
    ) {
        return unavailable("Reticulum is not online");
    }
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::Join {
            destination_hash,
            room: request.room,
            key: request.key,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    rrc_unit_response(rx).await
}

async fn rrc_part(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcJoinRequest>,
) -> Response {
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::Part {
            destination_hash,
            room: request.room,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    rrc_unit_response(rx).await
}

async fn rrc_clear(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcJoinRequest>,
) -> Response {
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let room = request
        .room
        .trim()
        .trim_start_matches('#')
        .to_ascii_lowercase();
    if room.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "room is required"})),
        )
            .into_response();
    }
    match state
        .database
        .clear_rrc_messages(&hex::encode(destination_hash), &room)
    {
        Ok(deleted) => Json(json!({"deleted": deleted})).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn rrc_ping(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcHubRequest>,
) -> Response {
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::Ping {
            destination_hash,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    match rx.await {
        Ok(Ok(milliseconds)) => Json(json!({"milliseconds": milliseconds})).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => unavailable("RRC manager stopped"),
    }
}

async fn rrc_disconnect(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcHubRequest>,
) -> Response {
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::Disconnect {
            destination_hash,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    rrc_unit_response(rx).await
}

async fn rrc_list(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcHubRequest>,
) -> Response {
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::ListRooms {
            destination_hash,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    match rx.await {
        Ok(Ok(rooms)) => Json(json!(rooms)).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => unavailable("RRC manager stopped"),
    }
}

async fn rrc_who(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcJoinRequest>,
) -> Response {
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::ListUsers {
            destination_hash,
            room: request.room,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    match rx.await {
        Ok(Ok(users)) => Json(json!(users)).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => unavailable("RRC manager stopped"),
    }
}

#[derive(serde::Deserialize)]
struct RrcSendRequest {
    destination_hash: String,
    room: Option<String>,
    body: String,
    #[serde(default)]
    action: bool,
}

async fn rrc_send(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcSendRequest>,
) -> Response {
    if !matches!(
        state.network.read().await.state,
        crate::models::NetworkState::Online
    ) {
        return unavailable("Reticulum is not online");
    }
    let Ok(destination_hash) = parse_hash(&request.destination_hash) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .rrc_commands
        .send(crate::rrc::RrcCommand::Send {
            destination_hash,
            room: request.room,
            body: request.body,
            action: request.action,
            response: tx,
        })
        .await
        .is_err()
    {
        return unavailable("RRC manager is unavailable");
    }
    rrc_unit_response(rx).await
}

async fn rrc_unit_response(receiver: oneshot::Receiver<Result<(), String>>) -> Response {
    match receiver.await {
        Ok(Ok(())) => Json(json!({"status": "ok"})).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => unavailable("RRC manager stopped"),
    }
}

#[derive(serde::Deserialize)]
struct RrcHistoryQuery {
    room: Option<String>,
}

async fn rrc_history(
    State(state): State<Arc<AppState>>,
    Path(destination_hash): Path<String>,
    Query(query): Query<RrcHistoryQuery>,
) -> Response {
    if parse_hash(&destination_hash).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid hub hash"})),
        )
            .into_response();
    }
    let room = query
        .room
        .as_deref()
        .map(|room| room.trim().trim_start_matches('#').to_ascii_lowercase());
    match state
        .database
        .rrc_messages(&destination_hash.to_ascii_lowercase(), room.as_deref())
    {
        Ok(messages) => Json(json!(messages)).into_response(),
        Err(error) => internal_error(error),
    }
}

fn unavailable(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": message})),
    )
        .into_response()
}

async fn download_file(
    State(state): State<Arc<AppState>>,
    Json(request): Json<FetchPageRequest>,
) -> Response {
    let url = match NomadUrl::parse(&request.url) {
        Ok(url) if url.is_file() => url,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "download requires a /file/ URL"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if !matches!(
        state.network.read().await.state,
        crate::models::NetworkState::Online
    ) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Reticulum is not online"})),
        )
            .into_response();
    }
    let (response_tx, response_rx) = oneshot::channel();
    if state
        .network_commands
        .send(crate::network::NetworkCommand::FetchFile {
            url,
            response: response_tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "network service is unavailable"})),
        )
            .into_response();
    }
    match response_rx.await {
        Ok(Ok(file)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", file.filename),
                ),
            ],
            file.bytes,
        )
            .into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "network service stopped"})),
        )
            .into_response(),
    }
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn style_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

async fn snapshot(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let network = state.network.read().await.clone();
    Json(json!({
        "network": network,
        "features": {
            "messaging": "planned",
            "browser": "planned",
            "rrc": "planned",
            "interface_statistics": "available"
        }
    }))
}

async fn conversations(State(state): State<Arc<AppState>>) -> Response {
    match state.database.conversations() {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn directory(State(state): State<Arc<AppState>>) -> Response {
    match state.database.directory() {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn messages(
    State(state): State<Arc<AppState>>,
    Path(destination_hash): Path<String>,
) -> Response {
    if parse_hash(&destination_hash).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "destination hash must contain 32 hexadecimal characters"})),
        )
            .into_response();
    }
    match state.database.messages(&destination_hash.to_lowercase()) {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn send_message(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SendMessageRequest>,
) -> Response {
    if parse_hash(&request.destination_hash).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "destination hash must contain 32 hexadecimal characters"})),
        )
            .into_response();
    }
    if request.content.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "message content cannot be empty"})),
        )
            .into_response();
    }
    if request.title.len() > 1024 || request.content.len() > 1024 * 1024 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": "message exceeds local safety limits"})),
        )
            .into_response();
    }
    let network = state.network.read().await;
    if !matches!(network.state, crate::models::NetworkState::Online) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Reticulum is not online"})),
        )
            .into_response();
    }
    drop(network);
    let destination_hash = parse_hash(&request.destination_hash).expect("validated above");
    let (response_tx, response_rx) = oneshot::channel();
    if state
        .network_commands
        .send(crate::network::NetworkCommand::SendMessage {
            destination_hash,
            title: request.title,
            content: request.content,
            response: response_tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "network service is unavailable"})),
        )
            .into_response();
    }
    match response_rx.await {
        Ok(Ok(message)) => (StatusCode::CREATED, Json(json!(message))).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "network service stopped"})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct FetchPageRequest {
    url: String,
    #[serde(default)]
    reload: bool,
    #[serde(default)]
    fields: std::collections::BTreeMap<String, String>,
}

async fn fetch_page(
    State(state): State<Arc<AppState>>,
    Json(request): Json<FetchPageRequest>,
) -> Response {
    let url = match NomadUrl::parse(&request.url) {
        Ok(url) if url.is_page() => url,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "page fetch requires a /page/ URL"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if !matches!(
        state.network.read().await.state,
        crate::models::NetworkState::Online
    ) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Reticulum is not online"})),
        )
            .into_response();
    }
    let (response_tx, response_rx) = oneshot::channel();
    if state
        .network_commands
        .send(crate::network::NetworkCommand::FetchPage {
            url,
            reload: request.reload,
            fields: request.fields,
            response: response_tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "network service is unavailable"})),
        )
            .into_response();
    }
    match response_rx.await {
        Ok(Ok(page)) => Json(json!(page)).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "network service stopped"})),
        )
            .into_response(),
    }
}

async fn events(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| event_socket(socket, state))
}

async fn event_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let snapshot = ServerEvent::Snapshot(state.network.read().await.clone());
    if send_event(&mut socket, &snapshot).await.is_err() {
        return;
    }
    let mut receiver = state.events.subscribe();
    loop {
        tokio::select! {
            event = receiver.recv() => {
                match event {
                    Ok(event) if send_event(&mut socket, &event).await.is_ok() => {}
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn send_event(socket: &mut WebSocket, event: &ServerEvent) -> Result<(), axum::Error> {
    let payload = serde_json::to_string(event).expect("server events serialize");
    socket.send(Message::Text(payload.into())).await
}

fn parse_hash(value: &str) -> Result<[u8; 16], hex::FromHexError> {
    let mut output = [0u8; 16];
    hex::decode_to_slice(value, &mut output)?;
    Ok(output)
}

fn internal_error(error: anyhow::Error) -> Response {
    tracing::error!(%error, "API request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "internal server error"})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_reticulum_hashes() {
        assert!(parse_hash("0123456789abcdef0123456789abcdef").is_ok());
        assert!(parse_hash("0123").is_err());
        assert!(parse_hash("zz23456789abcdef0123456789abcdef").is_err());
    }
}
