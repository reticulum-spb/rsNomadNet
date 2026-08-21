use std::sync::Arc;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::extract::Request;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::app::AppState;
use crate::models::{SendMessageRequest, ServerEvent};
use crate::service::{AppError, AppService, FetchPage, SendMessage};

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
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            secure_request,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn secure_request(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path.starts_with("/api/") && path != "/api/v1/health" {
        if let Some(expected) = state.config.auth_token_hash {
            let supplied = bearer_token(&request).or_else(|| {
                (path == "/api/v1/events")
                    .then(|| websocket_token(&request))
                    .flatten()
            });
            if !supplied.is_some_and(|token| token_matches(token, &expected)) {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, "Bearer realm=\"rsNomadNet\"")],
                    Json(json!({"error": "authentication required"})),
                )
                    .into_response();
            }
        }
        if mutates_state(request.method()) && !same_origin(&request) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "cross-origin state change rejected"})),
            )
                .into_response();
        }
    }

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; connect-src 'self' ws: wss:; img-src 'self' data:; \
             style-src 'self'; script-src 'self'; base-uri 'none'; frame-ancestors 'none'; \
             form-action 'self'",
        ),
    );
    response
}

fn bearer_token(request: &Request<Body>) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn websocket_token(request: &Request<Body>) -> Option<&str> {
    request
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)?
        .to_str()
        .ok()?
        .split(',')
        .map(str::trim)
        .find_map(|protocol| protocol.strip_prefix("bearer."))
}

fn token_matches(token: &str, expected: &[u8; 32]) -> bool {
    let supplied = rns_crypto::sha::full_hash(token.as_bytes());
    supplied
        .iter()
        .zip(expected)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn mutates_state(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn same_origin(request: &Request<Body>) -> bool {
    if request
        .headers()
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "cross-site")
    {
        return false;
    }
    let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    let Some(host) = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(origin_host) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    origin_host.trim_end_matches('/') == host
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
    service_response(
        AppService::new(state)
            .rrc_connect(&request.destination_hash, request.nick)
            .await,
    )
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
    service_unit_response(
        AppService::new(state)
            .rrc_join(&request.destination_hash, request.room, request.key)
            .await,
    )
}

async fn rrc_part(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcJoinRequest>,
) -> Response {
    service_unit_response(
        AppService::new(state)
            .rrc_part(&request.destination_hash, request.room)
            .await,
    )
}

async fn rrc_clear(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcJoinRequest>,
) -> Response {
    match AppService::new(state).clear_rrc_history(&request.destination_hash, &request.room) {
        Ok(deleted) => Json(json!({"deleted": deleted})).into_response(),
        Err(error) => service_error(error),
    }
}

async fn rrc_ping(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcHubRequest>,
) -> Response {
    match AppService::new(state)
        .rrc_ping(&request.destination_hash)
        .await
    {
        Ok(milliseconds) => Json(json!({"milliseconds": milliseconds})).into_response(),
        Err(error) => service_error(error),
    }
}

async fn rrc_disconnect(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcHubRequest>,
) -> Response {
    service_unit_response(
        AppService::new(state)
            .rrc_disconnect(&request.destination_hash)
            .await,
    )
}

async fn rrc_nick(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcNickRequest>,
) -> Response {
    service_response(
        AppService::new(state)
            .rrc_set_nick(&request.destination_hash, request.nick)
            .await,
    )
}

async fn rrc_list(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcHubRequest>,
) -> Response {
    service_response(
        AppService::new(state)
            .rrc_list_rooms(&request.destination_hash)
            .await,
    )
}

async fn rrc_who(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RrcJoinRequest>,
) -> Response {
    service_response(
        AppService::new(state)
            .rrc_list_users(&request.destination_hash, request.room)
            .await,
    )
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
    service_unit_response(
        AppService::new(state)
            .rrc_send(
                &request.destination_hash,
                request.room,
                request.body,
                request.action,
            )
            .await,
    )
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
    service_response(AppService::new(state).rrc_history(&destination_hash, query.room.as_deref()))
}

async fn download_file(
    State(state): State<Arc<AppState>>,
    Json(request): Json<FetchPageRequest>,
) -> Response {
    match AppService::new(state).download_file(&request.url).await {
        Ok(file) => (
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
        Err(error) => service_error(error),
    }
}

async fn browser_cache(State(state): State<Arc<AppState>>) -> Response {
    service_response(AppService::new(state).browser_cache())
}

async fn clear_browser_cache(State(state): State<Arc<AppState>>) -> Response {
    match AppService::new(state).clear_browser_cache() {
        Ok(deleted) => Json(json!({"deleted": deleted})).into_response(),
        Err(error) => service_error(error),
    }
}

async fn browser_bookmarks(State(state): State<Arc<AppState>>) -> Response {
    service_response(AppService::new(state).browser_bookmarks())
}

#[derive(serde::Deserialize)]
struct BrowserBookmarkRequest {
    url: String,
    name: Option<String>,
}

async fn save_browser_bookmark(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BrowserBookmarkRequest>,
) -> Response {
    service_response(
        AppService::new(state).save_browser_bookmark(&request.url, request.name.as_deref()),
    )
}

async fn remove_browser_bookmark(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BrowserBookmarkRequest>,
) -> Response {
    match AppService::new(state).remove_browser_bookmark(&request.url) {
        Ok(removed) => Json(json!({"removed": removed})).into_response(),
        Err(error) => service_error(error),
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
    let network = AppService::new(state).network_snapshot().await;
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
    service_response(AppService::new(state).identity_settings().await)
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
    service_response(
        AppService::new(state)
            .update_identity(request.name, request.announce_now)
            .await,
    )
}

async fn conversations(State(state): State<Arc<AppState>>) -> Response {
    service_response(AppService::new(state).conversations())
}

async fn directory(State(state): State<Arc<AppState>>) -> Response {
    service_response(AppService::new(state).directory())
}

async fn messages(
    State(state): State<Arc<AppState>>,
    Path(destination_hash): Path<String>,
    Query(query): Query<MessageQuery>,
) -> Response {
    service_response(AppService::new(state).messages(&destination_hash, query.q.as_deref()))
}

#[derive(Default, serde::Deserialize)]
struct MessageQuery {
    q: Option<String>,
}

async fn mark_conversation_read(
    State(state): State<Arc<AppState>>,
    Path(destination_hash): Path<String>,
) -> Response {
    match AppService::new(state).mark_conversation_read(&destination_hash) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => service_error(error),
    }
}

async fn clear_conversation(
    State(state): State<Arc<AppState>>,
    Path(destination_hash): Path<String>,
) -> Response {
    match AppService::new(state).clear_conversation(&destination_hash) {
        Ok(deleted) => Json(json!({"deleted": deleted})).into_response(),
        Err(error) => service_error(error),
    }
}

#[derive(serde::Deserialize)]
struct DraftRequest {
    content: String,
}

async fn get_draft(
    State(state): State<Arc<AppState>>,
    Path((scope, target)): Path<(String, String)>,
) -> Response {
    match AppService::new(state).draft(&scope, &target) {
        Ok(content) => Json(json!({"content": content})).into_response(),
        Err(error) => service_error(error),
    }
}

async fn save_draft(
    State(state): State<Arc<AppState>>,
    Path((scope, target)): Path<(String, String)>,
    Json(request): Json<DraftRequest>,
) -> Response {
    match AppService::new(state).save_draft(&scope, &target, &request.content) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => service_error(error),
    }
}

async fn send_message(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SendMessageRequest>,
) -> Response {
    let result = AppService::new(state)
        .send_message(SendMessage {
            destination_hash: request.destination_hash,
            title: request.title,
            content: request.content,
            delivery_method: request.delivery_method,
            propagation_node: request.propagation_node,
        })
        .await;
    match result {
        Ok(message) => (StatusCode::CREATED, Json(message)).into_response(),
        Err(error) => service_error(error),
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
    service_response(
        AppService::new(state)
            .fetch_page(FetchPage {
                url: request.url,
                reload: request.reload,
                fields: request.fields,
            })
            .await,
    )
}

async fn events(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.protocols(["rsnomadnet"])
        .on_upgrade(move |socket| event_socket(socket, state))
}

async fn event_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let service = AppService::new(state);
    let snapshot = ServerEvent::Snapshot(service.network_snapshot().await);
    if send_event(&mut socket, &snapshot).await.is_err() {
        return;
    }
    let mut receiver = service.subscribe();
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

#[cfg(test)]
fn parse_hash(value: &str) -> Result<[u8; 16], hex::FromHexError> {
    let mut output = [0u8; 16];
    hex::decode_to_slice(value, &mut output)?;
    Ok(output)
}

fn service_response<T: serde::Serialize>(result: Result<T, AppError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error),
    }
}

fn service_unit_response(result: Result<(), AppError>) -> Response {
    match result {
        Ok(()) => Json(json!({"status": "ok"})).into_response(),
        Err(error) => service_error(error),
    }
}

fn service_error(error: AppError) -> Response {
    let status = match &error {
        AppError::Invalid(_) => StatusCode::BAD_REQUEST,
        AppError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        AppError::Conflict(_) => StatusCode::CONFLICT,
        AppError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        AppError::Remote(_) => StatusCode::BAD_GATEWAY,
        AppError::Internal(source) => {
            tracing::error!(%source, "application service request failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "internal server error"})),
            )
                .into_response();
        }
    };
    (status, Json(json!({"error": error.to_string()}))).into_response()
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

    #[test]
    fn bearer_tokens_are_compared_in_constant_time() {
        let expected = rns_crypto::sha::full_hash(b"0123456789abcdef0123456789abcdef");
        assert!(token_matches("0123456789abcdef0123456789abcdef", &expected));
        assert!(!token_matches(
            "0123456789abcdef0123456789abcdeg",
            &expected
        ));
        let request = Request::builder()
            .header(
                header::SEC_WEBSOCKET_PROTOCOL,
                "rsnomadnet, bearer.0123456789abcdef0123456789abcdef",
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            websocket_token(&request),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn rejects_cross_origin_mutations() {
        let same = Request::builder()
            .method(Method::POST)
            .header(header::HOST, "nomad.example")
            .header(header::ORIGIN, "https://nomad.example")
            .body(Body::empty())
            .unwrap();
        assert!(same_origin(&same));

        let cross = Request::builder()
            .method(Method::POST)
            .header(header::HOST, "nomad.example")
            .header(header::ORIGIN, "https://attacker.example")
            .body(Body::empty())
            .unwrap();
        assert!(!same_origin(&cross));
    }
}
