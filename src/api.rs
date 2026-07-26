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
const RRC_UI_JS: &str = include_str!("../web/rrc-ui.js");
const STYLE_CSS: &str = include_str!("../web/style.css");

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/rrc-ui.js", get(rrc_ui_js))
        .route("/style.css", get(style_css))
        .route("/api/v1/health", get(health))
        .route("/api/v1/state", get(snapshot))
        .route(
            "/api/v1/identity",
            get(identity_settings).put(update_identity),
        )
        .route("/api/v1/conversations", get(conversations))
        .route("/api/v1/directory", get(directory))
        .route(
            "/api/v1/conversations/{destination_hash}",
            get(messages).delete(clear_conversation),
        )
        .route(
            "/api/v1/conversations/{destination_hash}/read",
            post(mark_conversation_read),
        )
        .route(
            "/api/v1/drafts/{scope}/{target}",
            get(get_draft).put(save_draft),
        )
        .route("/api/v1/messages", post(send_message))
        .route("/api/v1/browser/fetch", post(fetch_page))
        .route("/api/v1/browser/download", post(download_file))
        .route(
            "/api/v1/browser/cache",
            get(browser_cache).delete(clear_browser_cache),
        )
        .route(
            "/api/v1/browser/bookmarks",
            get(browser_bookmarks)
                .post(save_browser_bookmark)
                .delete(remove_browser_bookmark),
        )
        .route("/api/v1/rrc/connect", post(rrc_connect))
        .route("/api/v1/rrc/disconnect", post(rrc_disconnect))
        .route("/api/v1/rrc/nick", post(rrc_nick))
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

#[derive(serde::Deserialize)]
struct RrcNickRequest {
    destination_hash: String,
    nick: String,
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

async fn rrc_nick(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcNickRequest>,
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
        .send(crate::rrc::RrcCommand::SetNick {
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
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response(),
        Err(_) => unavailable("RRC manager stopped"),
    }
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
                (header::CONTENT_TYPE, file.content_type),
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

async fn browser_cache(State(state): State<Arc<AppState>>) -> Response {
    match state.database.browser_cache_entries(unix_seconds()) {
        Ok(entries) => Json(json!(entries)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn clear_browser_cache(State(state): State<Arc<AppState>>) -> Response {
    match state.database.clear_browser_cache() {
        Ok(deleted) => Json(json!({"deleted": deleted})).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn browser_bookmarks(State(state): State<Arc<AppState>>) -> Response {
    match state.database.browser_bookmarks() {
        Ok(bookmarks) => Json(bookmarks).into_response(),
        Err(error) => internal_error(error),
    }
}

#[derive(serde::Deserialize)]
struct BrowserBookmarkRequest {
    url: String,
    name: Option<String>,
}

fn bookmark_name(value: Option<&str>, destination_hash: &[u8; 16]) -> String {
    let name = value
        .unwrap_or("")
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect::<String>()
        .trim()
        .to_string();
    if name.is_empty() {
        hex::encode(destination_hash)
    } else {
        name
    }
}

async fn save_browser_bookmark(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BrowserBookmarkRequest>,
) -> Response {
    let url = match NomadUrl::parse(&request.url) {
        Ok(url) if url.is_page() => url,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "bookmark requires a valid NomadNet page URL"})),
            )
                .into_response();
        }
    };
    let canonical = url.canonical();
    let name = bookmark_name(request.name.as_deref(), &url.destination_hash);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    match state.database.save_browser_bookmark(&canonical, &name, now) {
        Ok(()) => Json(json!({"url": canonical, "name": name, "created_at": now})).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn remove_browser_bookmark(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BrowserBookmarkRequest>,
) -> Response {
    let url = match NomadUrl::parse(&request.url) {
        Ok(url) if url.is_page() => url.canonical(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid bookmark URL"})),
            )
                .into_response();
        }
    };
    match state.database.remove_browser_bookmark(&url) {
        Ok(removed) => Json(json!({"removed": removed})).into_response(),
        Err(error) => internal_error(error),
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

async fn rrc_ui_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        RRC_UI_JS,
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
            "messaging": "available",
            "browser": "available",
            "rrc": "available",
            "interface_statistics": "available"
        }
    }))
}

async fn identity_settings(State(state): State<Arc<AppState>>) -> Response {
    let network = state.network.read().await;
    let name = match state.database.setting("announce_name") {
        Ok(Some(name)) => name,
        Ok(None) => "rsNomadNet".into(),
        Err(error) => return internal_error(error),
    };
    Json(json!({
        "destination_hash": network.destination_hash,
        "name": name,
        "online": matches!(network.state, crate::models::NetworkState::Online),
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct IdentitySettingsRequest {
    name: String,
    #[serde(default)]
    announce_now: bool,
}

async fn update_identity(
    State(state): State<Arc<AppState>>,
    Json(request): Json<IdentitySettingsRequest>,
) -> Response {
    let name = request
        .name
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect::<String>()
        .trim()
        .to_string();
    if let Err(error) = state.database.set_setting("announce_name", &name) {
        return internal_error(error);
    }
    let online = matches!(
        state.network.read().await.state,
        crate::models::NetworkState::Online
    );
    if !online {
        if request.announce_now {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Reticulum is not online", "name": name})),
            )
                .into_response();
        }
        return Json(json!({"name": name, "announced": false})).into_response();
    }
    let (response_tx, response_rx) = oneshot::channel();
    if state
        .network_commands
        .send(crate::network::NetworkCommand::SetAnnounceName {
            name: (!name.is_empty()).then_some(name.clone()),
            announce_now: request.announce_now,
            response: response_tx,
        })
        .await
        .is_err()
    {
        return unavailable("network service is unavailable");
    }
    match response_rx.await {
        Ok(Ok(())) => {
            Json(json!({"name": name, "announced": request.announce_now})).into_response()
        }
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
        Err(_) => unavailable("network service stopped"),
    }
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
    Query(query): Query<MessageQuery>,
) -> Response {
    if parse_hash(&destination_hash).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "destination hash must contain 32 hexadecimal characters"})),
        )
            .into_response();
    }
    let destination_hash = destination_hash.to_lowercase();
    let result = match query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(query) => state.database.search_messages(&destination_hash, query),
        None => state.database.messages(&destination_hash),
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

#[derive(Default, serde::Deserialize)]
struct MessageQuery {
    q: Option<String>,
}

async fn mark_conversation_read(
    State(state): State<Arc<AppState>>,
    Path(destination_hash): Path<String>,
) -> Response {
    if parse_hash(&destination_hash).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid destination hash"})),
        )
            .into_response();
    }
    match state
        .database
        .mark_conversation_read(&destination_hash.to_lowercase())
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn clear_conversation(
    State(state): State<Arc<AppState>>,
    Path(destination_hash): Path<String>,
) -> Response {
    if parse_hash(&destination_hash).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid destination hash"})),
        )
            .into_response();
    }
    let destination_hash = destination_hash.to_lowercase();
    match state.database.conversation_has_pending(&destination_hash) {
        Ok(true) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "conversation has messages awaiting delivery"})),
            )
                .into_response();
        }
        Ok(false) => {}
        Err(error) => return internal_error(error),
    }
    match state.database.clear_conversation(&destination_hash) {
        Ok(deleted) => Json(json!({"deleted": deleted})).into_response(),
        Err(error) => internal_error(error),
    }
}

#[derive(serde::Deserialize)]
struct DraftRequest {
    content: String,
}

fn valid_draft_target(scope: &str, target: &str) -> bool {
    matches!(scope, "lxmf" | "rrc")
        && !target.is_empty()
        && target.len() <= 256
        && !target.chars().any(char::is_control)
}

async fn get_draft(
    State(state): State<Arc<AppState>>,
    Path((scope, target)): Path<(String, String)>,
) -> Response {
    if !valid_draft_target(&scope, &target) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid draft target"})),
        )
            .into_response();
    }
    match state.database.draft(&scope, &target) {
        Ok(content) => Json(json!({"content": content.unwrap_or_default()})).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn save_draft(
    State(state): State<Arc<AppState>>,
    Path((scope, target)): Path<(String, String)>,
    Json(request): Json<DraftRequest>,
) -> Response {
    if !valid_draft_target(&scope, &target) || request.content.len() > 1024 * 1024 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid draft"})),
        )
            .into_response();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    match state
        .database
        .save_draft(&scope, &target, &request.content, now)
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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
    let delivery_method = request.delivery_method.trim().to_ascii_lowercase();
    if !matches!(
        delivery_method.as_str(),
        "" | "auto" | "automatic" | "opportunistic" | "direct" | "propagated"
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "unknown LXMF delivery method"})),
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
    let propagation_node = match request.propagation_node.as_deref() {
        Some(value) if !value.trim().is_empty() => match parse_hash(value) {
            Ok(hash) => Some(hash),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "propagation node hash must contain 32 hexadecimal characters"})),
                )
                    .into_response();
            }
        },
        _ => None,
    };
    if propagation_node.is_some() && delivery_method != "propagated" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "propagation_node is only valid for propagated delivery"})),
        )
            .into_response();
    }
    let (response_tx, response_rx) = oneshot::channel();
    if state
        .network_commands
        .send(crate::network::NetworkCommand::SendMessage {
            destination_hash,
            title: request.title,
            content: request.content,
            delivery_method,
            propagation_node,
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

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
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
