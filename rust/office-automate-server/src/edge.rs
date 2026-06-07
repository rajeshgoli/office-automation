use std::{
    collections::{HashMap, VecDeque},
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Component, Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        ConnectInfo, OriginalUri, Path, Query, Request, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, get_service, post},
};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as TungsteniteMessage, client::IntoClientRequest},
};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};

use crate::{
    auth::{AuthManager, HttpAuthMode, OAUTH_CSRF_HEADER, OAuthCredentialSource, WebSocketAuth},
    config::OrchestratorConfig,
    http::CONTROLLER_IPC_TOKEN_HEADER,
};

const CONTROL_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const CONTROL_RATE_LIMIT_MAX_REQUESTS: usize = 60;
const MAX_EDGE_POST_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicEdgeConfig {
    pub orchestrator: OrchestratorConfig,
    pub controller: ControllerIpcConfig,
    pub runtime: PublicEdgeRuntimeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerIpcConfig {
    pub base_url: String,
    pub token: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicEdgeRuntimeConfig {
    pub frontend_dist: PathBuf,
    pub public_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicEdgeFileConfig {
    #[serde(default)]
    orchestrator: OrchestratorConfig,
    controller: ControllerIpcFileConfig,
    #[serde(default)]
    runtime: PublicEdgeRuntimeFileConfig,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ControllerIpcFileConfig {
    base_url: String,
    token: String,
    timeout_seconds: u64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct PublicEdgeRuntimeFileConfig {
    frontend_dist: Option<PathBuf>,
    public_url: Option<String>,
}

#[derive(Clone)]
struct EdgeState {
    auth: AuthManager,
    controller: ControllerClient,
    frontend_dist: PathBuf,
    public_url: Option<String>,
    rate_limiter: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
}

#[derive(Clone)]
struct ControllerClient {
    base_url: String,
    websocket_url: String,
    token: String,
    client: Client,
}

impl PublicEdgeConfig {
    pub fn load(path: impl AsRef<FsPath>) -> Result<Self> {
        Self::load_with_env(path, |key| env::var(key).ok())
    }

    pub fn load_with_env(
        path: impl AsRef<FsPath>,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let config_path = path.as_ref();
        let contents = fs::read_to_string(config_path)
            .with_context(|| format!("failed to read edge config {}", config_path.display()))?;
        let mut file_config: PublicEdgeFileConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse edge config {}", config_path.display()))?;

        if let Some(host) = env_lookup("OFFICE_AUTOMATE_EDGE_HOST") {
            file_config.orchestrator.host = host;
        }
        if let Some(port) = env_lookup("OFFICE_AUTOMATE_EDGE_PORT") {
            file_config.orchestrator.port = port
                .parse()
                .with_context(|| format!("invalid OFFICE_AUTOMATE_EDGE_PORT value {port:?}"))?;
        }
        if let Some(url) = env_lookup("OFFICE_AUTOMATE_EDGE_CONTROLLER_URL") {
            file_config.controller.base_url = url;
        }
        if let Some(token) = env_lookup("OFFICE_AUTOMATE_EDGE_CONTROLLER_TOKEN") {
            file_config.controller.token = token;
        }
        if let Some(seconds) = env_lookup("OFFICE_AUTOMATE_EDGE_CONTROLLER_TIMEOUT_SECONDS") {
            file_config.controller.timeout_seconds = seconds.parse().with_context(|| {
                format!("invalid OFFICE_AUTOMATE_EDGE_CONTROLLER_TIMEOUT_SECONDS value {seconds:?}")
            })?;
        }

        let frontend_dist = env_lookup("OFFICE_AUTOMATE_EDGE_FRONTEND_DIST")
            .map(PathBuf::from)
            .or(file_config.runtime.frontend_dist)
            .or_else(|| {
                env_lookup("OFFICE_AUTOMATE_ROOT")
                    .map(PathBuf::from)
                    .map(|root| root.join("frontend").join("dist"))
            })
            .context(
                "edge config requires runtime.frontend_dist, OFFICE_AUTOMATE_EDGE_FRONTEND_DIST, or OFFICE_AUTOMATE_ROOT",
            )?;
        let public_url =
            env_lookup("OFFICE_AUTOMATE_PUBLIC_URL").or(file_config.runtime.public_url);

        let config = Self {
            orchestrator: file_config.orchestrator,
            controller: ControllerIpcConfig {
                base_url: file_config.controller.base_url,
                token: file_config.controller.token,
                timeout_seconds: if file_config.controller.timeout_seconds == 0 {
                    5
                } else {
                    file_config.controller.timeout_seconds
                },
            },
            runtime: PublicEdgeRuntimeConfig {
                frontend_dist,
                public_url,
            },
        };
        validate_public_edge_config(&config)?;
        Ok(config)
    }
}

pub fn app(config: PublicEdgeConfig) -> Result<Router> {
    let state = EdgeState {
        auth: AuthManager::new(&config.orchestrator)?,
        controller: ControllerClient::new(&config.controller)?,
        frontend_dist: config.runtime.frontend_dist.clone(),
        public_url: config.runtime.public_url.clone(),
        rate_limiter: Arc::new(Mutex::new(HashMap::new())),
    };
    Ok(router_from_state(state))
}

pub async fn serve(config: PublicEdgeConfig) -> Result<()> {
    let bind_address = format!("{}:{}", config.orchestrator.host, config.orchestrator.port);
    let listener = TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("failed to bind public edge listener at {bind_address}"))?;
    let app = app(config)?;

    tracing::info!("office-automate public edge listening on {}", bind_address);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("public edge HTTP server failed")
}

fn validate_public_edge_config(config: &PublicEdgeConfig) -> Result<()> {
    let host = config.orchestrator.host.trim();
    if !is_loopback_bind_host(host) {
        anyhow::bail!("public edge must bind HTTP to loopback; got orchestrator.host={host:?}");
    }
    if config.orchestrator.google_oauth.is_none() {
        anyhow::bail!(
            "public edge requires Google OAuth/JWT; Open and Basic modes are not allowed"
        );
    }
    if config
        .orchestrator
        .google_oauth
        .as_ref()
        .is_some_and(|oauth| !oauth.trusted_networks.is_empty())
    {
        anyhow::bail!("public edge must not configure google_oauth.trusted_networks");
    }
    if config.controller.base_url.trim().is_empty() {
        anyhow::bail!("public edge requires controller.base_url");
    }
    validate_loopback_controller_base_url(&config.controller.base_url)?;
    if config.controller.token.trim().is_empty() {
        anyhow::bail!("public edge requires controller.token");
    }
    Ok(())
}

fn router_from_state(state: EdgeState) -> Router {
    let cors = cors_layer(state.public_url.as_deref());
    let assets_dir = state.frontend_dist.join("assets");

    Router::new()
        .route("/status", get(forward_get))
        .route("/ws", get(edge_websocket))
        .route("/occupancy", post(forward_post))
        .route("/presence", post(forward_post))
        .route("/erv", post(forward_post))
        .route("/hvac", post(forward_post))
        .route(
            "/hvac/temperature-bands",
            get(forward_get).post(forward_post),
        )
        .route("/qingping/interval", post(forward_post))
        .route("/history", get(forward_get))
        .route("/history/sessions", get(forward_get))
        .route("/history/co2-ohlc", get(forward_get))
        .route("/history/temperature", get(forward_get))
        .route("/history/daily-stats", get(forward_get))
        .route("/history/openings", get(forward_get))
        .route("/history/orchestration", get(forward_get))
        .route("/history/project-focus", get(forward_get))
        .route("/history/leverage", get(forward_get))
        .route("/history/project-leverage", get(forward_get))
        .route("/apps/{app}/latest.apk", get(forward_get))
        .route("/apps/{app}/{artifact_file}", get(forward_get))
        .route("/apps/{app}/meta.json", get(forward_get))
        .route("/apk", get(forward_get))
        .route("/auth/login", get(auth_login))
        .route("/auth/callback", get(auth_callback))
        .route("/auth/logout", post(auth_logout))
        .route("/auth/device/start", post(auth_device_start))
        .route("/auth/device/poll", post(auth_device_poll))
        .route("/localtunnel/password", get(localtunnel_gone))
        .nest_service("/assets", get_service(ServeDir::new(assets_dir)))
        .route("/", get(index))
        .route("/{*path}", get(spa_fallback))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            edge_auth_middleware,
        ))
        .with_state(state)
        .layer(middleware::from_fn(security_headers_middleware))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

fn cors_layer(public_url: Option<&str>) -> CorsLayer {
    let mut layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static(OAUTH_CSRF_HEADER),
        ]);
    if let Some(origin) = public_origin_header(public_url) {
        layer = layer.allow_origin(origin).allow_credentials(true);
    }
    layer
}

fn public_origin_header(public_url: Option<&str>) -> Option<HeaderValue> {
    let url = Url::parse(public_url?).ok()?;
    let host = url.host_str()?;
    let origin = match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    };
    HeaderValue::from_str(&origin).ok()
}

async fn security_headers_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

async fn edge_auth_middleware(
    State(state): State<EdgeState>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth_mode = state.auth.mode();
    if should_skip_auth(request.method(), request.uri().path(), auth_mode) {
        return next.run(request).await;
    }

    let remote_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(remote_addr)| *remote_addr);
    let headers = auth_headers_with_peer(request.headers(), remote_addr);

    if request.uri().path() == "/ws" && is_websocket_upgrade(&headers) {
        return next.run(request).await;
    }

    let Some(verified) = state.auth.verify_oauth_request(&headers) else {
        let missing = state.auth.bearer_or_session_token(&headers).is_none();
        let message = if missing {
            "Authentication required"
        } else {
            "Invalid or expired token"
        };
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": message, "login_url": "/auth/login"})),
        )
            .into_response();
    };

    if verified.source == OAuthCredentialSource::SessionCookie
        && requires_csrf(request.method())
        && !state.auth.verify_oauth_csrf(&headers)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "CSRF token required"})),
        )
            .into_response();
    }

    let user = verified.user;
    if is_control_route(request.method(), request.uri().path()) {
        let actor = rate_limit_actor(remote_addr, &user.email, request.uri().path());
        if !allow_control_request(&state, actor) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"ok": false, "error": "Rate limit exceeded"})),
            )
                .into_response();
        }
    }

    request.extensions_mut().insert(user);
    next.run(request).await
}

async fn forward_get(State(state): State<EdgeState>, OriginalUri(uri): OriginalUri) -> Response {
    state.controller.forward(Method::GET, &uri, None).await
}

async fn forward_post(
    State(state): State<EdgeState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > MAX_EDGE_POST_BODY_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"ok": false, "error": "Request body too large"})),
        )
            .into_response();
    }
    let content_type = headers.get(header::CONTENT_TYPE).cloned();
    state
        .controller
        .forward(Method::POST, &uri, Some((body, content_type)))
        .await
}

async fn edge_websocket(
    State(state): State<EdgeState>,
    ConnectInfo(remote_addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let auth_headers = auth_headers_with_peer(&headers, Some(remote_addr));
    let mode = state.auth.websocket_auth(&auth_headers);
    ws.on_upgrade(move |socket| edge_websocket_session(socket, state, mode))
}

async fn edge_websocket_session(mut socket: WebSocket, state: EdgeState, auth_mode: WebSocketAuth) {
    if !authenticate_edge_websocket(&mut socket, &state.auth, auth_mode).await {
        return;
    }

    let mut request = match state
        .controller
        .websocket_url
        .as_str()
        .into_client_request()
    {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!("failed to build controller websocket request: {error}");
            close_ws(&mut socket, "Controller unavailable").await;
            return;
        }
    };
    let token = match HeaderValue::from_str(&state.controller.token) {
        Ok(token) => token,
        Err(_) => {
            tracing::error!("controller IPC token is not a valid header value");
            close_ws(&mut socket, "Controller unavailable").await;
            return;
        }
    };
    request
        .headers_mut()
        .insert(CONTROLLER_IPC_TOKEN_HEADER, token);

    let controller = timeout(Duration::from_secs(5), connect_async(request)).await;
    let Ok(Ok((mut controller_ws, _))) = controller else {
        close_ws(&mut socket, "Controller unavailable").await;
        return;
    };

    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(message)) => {
                        if let Some(message) = axum_to_tungstenite(message) {
                            if controller_ws.send(message).await.is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::debug!("edge websocket receive error: {error}");
                        break;
                    }
                    None => break,
                }
            }
            message = controller_ws.next() => {
                match message {
                    Some(Ok(message)) => {
                        if let Some(message) = tungstenite_to_axum(message) {
                            if socket.send(message).await.is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::debug!("controller websocket receive error: {error}");
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

async fn authenticate_edge_websocket(
    socket: &mut WebSocket,
    auth: &AuthManager,
    auth_mode: WebSocketAuth,
) -> bool {
    match auth_mode {
        WebSocketAuth::UpgradeBearer | WebSocketAuth::Open => true,
        WebSocketAuth::FirstMessage => {
            match timeout(Duration::from_secs(10), socket.recv()).await {
                Ok(Some(Ok(Message::Text(message)))) => {
                    let Ok(value) = serde_json::from_str::<Value>(&message) else {
                        close_ws(socket, "Authentication required").await;
                        return false;
                    };
                    if value.get("type").and_then(Value::as_str) != Some("auth") {
                        close_ws(socket, "Authentication required").await;
                        return false;
                    }
                    let Some(token) = value.get("token").and_then(Value::as_str) else {
                        close_ws(socket, "Authentication required").await;
                        return false;
                    };
                    if auth.verify_jwt(token).is_none() {
                        close_ws(socket, "Invalid token").await;
                        return false;
                    }
                    true
                }
                _ => {
                    close_ws(socket, "Authentication failed").await;
                    false
                }
            }
        }
        WebSocketAuth::Reject | WebSocketAuth::TrustedNetwork | WebSocketAuth::UpgradeBasic => {
            close_ws(socket, "Authentication required").await;
            false
        }
    }
}

async fn close_ws(socket: &mut WebSocket, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 4001,
            reason: reason.into(),
        })))
        .await;
}

fn axum_to_tungstenite(message: Message) -> Option<TungsteniteMessage> {
    match message {
        Message::Text(text) => Some(TungsteniteMessage::Text(text.to_string().into())),
        Message::Binary(bytes) => Some(TungsteniteMessage::Binary(bytes)),
        Message::Ping(bytes) => Some(TungsteniteMessage::Ping(bytes)),
        Message::Pong(bytes) => Some(TungsteniteMessage::Pong(bytes)),
        Message::Close(frame) => Some(TungsteniteMessage::Close(frame.map(|frame| {
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.to_string().into(),
            }
        }))),
    }
}

fn tungstenite_to_axum(message: TungsteniteMessage) -> Option<Message> {
    match message {
        TungsteniteMessage::Text(text) => Some(Message::Text(text.to_string().into())),
        TungsteniteMessage::Binary(bytes) => Some(Message::Binary(bytes)),
        TungsteniteMessage::Ping(bytes) => Some(Message::Ping(bytes)),
        TungsteniteMessage::Pong(bytes) => Some(Message::Pong(bytes)),
        TungsteniteMessage::Close(frame) => Some(Message::Close(frame.map(|frame| CloseFrame {
            code: frame.code.into(),
            reason: frame.reason.to_string().into(),
        }))),
        TungsteniteMessage::Frame(_) => None,
    }
}

impl ControllerClient {
    fn new(config: &ControllerIpcConfig) -> Result<Self> {
        validate_loopback_controller_base_url(&config.base_url)?;
        let timeout = Duration::from_secs(config.timeout_seconds.max(1));
        let client = Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .build()
            .context("failed to build controller IPC client")?;
        Ok(Self {
            base_url: config.base_url.trim_end_matches('/').to_string(),
            websocket_url: websocket_url(&config.base_url)?,
            token: config.token.clone(),
            client,
        })
    }

    async fn forward(
        &self,
        method: Method,
        uri: &Uri,
        body: Option<(Bytes, Option<HeaderValue>)>,
    ) -> Response {
        if !is_allowed_controller_route(&method, uri.path()) {
            return StatusCode::NOT_FOUND.into_response();
        }

        let Some(url) = self.controller_url(uri) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let mut request = match method {
            Method::GET => self.client.get(url),
            Method::POST => self.client.post(url),
            _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
        }
        .header(CONTROLLER_IPC_TOKEN_HEADER, self.token.as_str());

        if let Some((body, content_type)) = body {
            if let Some(content_type) = content_type {
                request = request.header(header::CONTENT_TYPE.as_str(), content_type.as_bytes());
            }
            request = request.body(body.to_vec());
        }

        match request.send().await {
            Ok(response) => controller_response_to_axum(response).await,
            Err(error) => {
                tracing::warn!("controller IPC request failed: {error:#}");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"ok": false, "error": "Controller unavailable"})),
                )
                    .into_response()
            }
        }
    }

    fn controller_url(&self, uri: &Uri) -> Option<String> {
        let path_and_query = uri.path_and_query()?.as_str();
        Some(format!("{}{}", self.base_url, path_and_query))
    }
}

async fn controller_response_to_axum(response: reqwest::Response) -> Response {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = response.headers().clone();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!("failed to read controller IPC response body: {error:#}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        if is_forwarded_response_header(name.as_str()) {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

fn is_forwarded_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "content-type"
            | "cache-control"
            | "content-disposition"
            | "location"
            | "etag"
            | "last-modified"
    )
}

fn is_allowed_controller_route(method: &Method, path: &str) -> bool {
    match (method, path) {
        (&Method::GET, "/status")
        | (&Method::GET, "/history")
        | (&Method::GET, "/history/sessions")
        | (&Method::GET, "/history/co2-ohlc")
        | (&Method::GET, "/history/temperature")
        | (&Method::GET, "/history/daily-stats")
        | (&Method::GET, "/history/openings")
        | (&Method::GET, "/history/orchestration")
        | (&Method::GET, "/history/project-focus")
        | (&Method::GET, "/history/leverage")
        | (&Method::GET, "/history/project-leverage")
        | (&Method::GET, "/hvac/temperature-bands")
        | (&Method::GET, "/apk") => true,
        (&Method::GET, path) if path.starts_with("/apps/") => true,
        (&Method::POST, "/occupancy")
        | (&Method::POST, "/presence")
        | (&Method::POST, "/erv")
        | (&Method::POST, "/hvac")
        | (&Method::POST, "/hvac/temperature-bands")
        | (&Method::POST, "/qingping/interval") => true,
        _ => false,
    }
}

fn is_control_route(method: &Method, path: &str) -> bool {
    *method == Method::POST
        && matches!(
            path,
            "/occupancy"
                | "/presence"
                | "/erv"
                | "/hvac"
                | "/hvac/temperature-bands"
                | "/qingping/interval"
        )
}

fn rate_limit_actor(remote_addr: Option<SocketAddr>, email: &str, path: &str) -> String {
    let remote = remote_addr
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!("{remote}:{email}:{path}")
}

fn allow_control_request(state: &EdgeState, actor: String) -> bool {
    let now = Instant::now();
    let cutoff = now - CONTROL_RATE_LIMIT_WINDOW;
    let mut limiter = state.rate_limiter.lock().expect("rate limiter lock");
    let entries = limiter.entry(actor).or_default();
    while entries.front().is_some_and(|instant| *instant < cutoff) {
        entries.pop_front();
    }
    if entries.len() >= CONTROL_RATE_LIMIT_MAX_REQUESTS {
        return false;
    }
    entries.push_back(now);
    true
}

fn websocket_url(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("http://") {
        Ok(format!("ws://{rest}/ws"))
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        Ok(format!("wss://{rest}/ws"))
    } else {
        anyhow::bail!("controller.base_url must start with http:// or https://")
    }
}

fn validate_loopback_controller_base_url(base_url: &str) -> Result<()> {
    let url = Url::parse(base_url.trim())
        .with_context(|| format!("invalid controller.base_url {base_url:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("controller.base_url must start with http:// or https://");
    }
    let Some(host) = url.host_str() else {
        anyhow::bail!("controller.base_url must include a hostname");
    };
    if !is_loopback_bind_host(host) {
        anyhow::bail!("public edge controller.base_url must stay on loopback; got host={host:?}");
    }
    Ok(())
}

fn should_skip_auth(method: &Method, path: &str, auth_mode: HttpAuthMode) -> bool {
    if *method == Method::OPTIONS {
        return true;
    }

    if path == "/apk" || path.starts_with("/apps/") {
        return true;
    }

    matches!(auth_mode, HttpAuthMode::OAuth)
        && (path == "/auth/login"
            || path == "/auth/callback"
            || path == "/auth/device/start"
            || path == "/auth/device/poll"
            || path == "/"
            || path == "/index.html"
            || path.starts_with("/assets/")
            || path.ends_with(".png")
            || path.ends_with(".json"))
}

fn requires_csrf(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

fn auth_headers_with_peer(headers: &HeaderMap, remote_addr: Option<SocketAddr>) -> HeaderMap {
    let mut headers = headers.clone();
    let forwarded_for = forwarded_for_ip(&headers);
    headers.remove("x-forwarded-for");

    let client_ip = match remote_addr {
        Some(remote_addr) if remote_addr.ip().is_loopback() => forwarded_for,
        Some(remote_addr) => Some(remote_addr.ip()),
        None => None,
    };

    if let Some(client_ip) = client_ip {
        if let Ok(value) = client_ip.to_string().parse() {
            headers.insert("x-forwarded-for", value);
        }
    }
    headers
}

fn forwarded_for_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-forwarded-for")?
        .to_str()
        .ok()?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

async fn auth_login(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Missing Host header"})),
        )
            .into_response();
    };
    let platform = query
        .get("platform")
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let forwarded_proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok());

    match state.auth.begin_login(host, forwarded_proto, platform) {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::error!("failed to start OAuth login: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to start OAuth"})),
            )
                .into_response()
        }
    }
}

async fn auth_callback(
    State(state): State<EdgeState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(error) = query.get("error") {
        return (
            StatusCode::BAD_REQUEST,
            Html(format!(
                "<html><body><h1>Login Failed</h1><p>{}</p></body></html>",
                escape_html_text(error)
            )),
        )
            .into_response();
    }

    let (Some(code), Some(oauth_state)) = (query.get("code"), query.get("state")) else {
        return (StatusCode::BAD_REQUEST, "Missing code or state").into_response();
    };

    match state.auth.finish_callback(code, oauth_state).await {
        Ok(Some(login)) if login.platform.as_deref() == Some("android") => Response::builder()
            .status(StatusCode::FOUND)
            .header(
                header::LOCATION,
                format!(
                    "officeclimate://auth?token={}&email={}",
                    urlencoding::encode(&login.jwt),
                    urlencoding::encode(&login.email)
                ),
            )
            .body(Body::empty())
            .expect("valid android redirect response"),
        Ok(Some(login)) => browser_login_response(&state.auth, &login.jwt),
        Ok(None) => (
            StatusCode::FORBIDDEN,
            Html("<html><body><h1>Login Failed</h1><p>Email not authorized</p></body></html>"),
        )
            .into_response(),
        Err(error) if error.to_string() == "Invalid state" => {
            (StatusCode::BAD_REQUEST, "Invalid state").into_response()
        }
        Err(error) => {
            tracing::error!("OAuth callback failed: {error:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, "OAuth callback failed").into_response()
        }
    }
}

async fn auth_logout(State(state): State<EdgeState>, headers: HeaderMap) -> Response {
    let Some(token) = state.auth.bearer_or_session_token(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "No token provided"})),
        )
            .into_response();
    };

    state.auth.invalidate_token(token);
    let mut response = Json(json!({"ok": true, "message": "Logged out"})).into_response();
    append_clear_oauth_cookies(&mut response);
    response
}

async fn auth_device_start(State(state): State<EdgeState>) -> Response {
    match state.auth.start_device_flow().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::error!("device flow start failed: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct DevicePollRequest {
    device_code: Option<String>,
}

async fn auth_device_poll(
    State(state): State<EdgeState>,
    Json(payload): Json<DevicePollRequest>,
) -> Response {
    let Some(device_code) = payload.device_code.filter(|code| !code.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Missing device_code"})),
        )
            .into_response();
    };

    match state.auth.poll_device_flow(&device_code).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::error!("device flow poll failed: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"status": "error", "message": error.to_string()})),
            )
                .into_response()
        }
    }
}

async fn localtunnel_gone() -> impl IntoResponse {
    (
        StatusCode::GONE,
        Json(json!({
            "ok": false,
            "error": "LocalTunnel is not supported; use Cloudflare Tunnel",
        })),
    )
}

async fn index(State(state): State<EdgeState>) -> Response {
    serve_static_or_index(&state, "index.html").await
}

async fn spa_fallback(State(state): State<EdgeState>, Path(path): Path<String>) -> Response {
    serve_static_or_index(&state, &path).await
}

async fn serve_static_or_index(state: &EdgeState, path: &str) -> Response {
    let requested = state.frontend_dist.join(path);
    let target = if is_safe_spa_path(path) && requested.exists() && requested.is_file() {
        requested
    } else {
        state.frontend_dist.join("index.html")
    };

    match tokio::fs::read(&target).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(bytes))
            .expect("static response"),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn is_safe_spa_path(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn escape_html_text(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            _ => character.to_string(),
        })
        .collect()
}

fn browser_login_response(auth: &AuthManager, token: &str) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, "/")
        .body(Body::empty())
        .expect("valid browser login response");
    if let Some(cookies) = auth.issue_oauth_session_cookies(token) {
        for cookie in cookies {
            response.headers_mut().append(
                header::SET_COOKIE,
                cookie.parse().expect("valid OAuth session cookie"),
            );
        }
    }
    response
}

fn append_clear_oauth_cookies(response: &mut Response) {
    for cookie in AuthManager::clear_oauth_session_cookies() {
        response.headers_mut().append(
            header::SET_COOKIE,
            cookie.parse().expect("valid clear cookie"),
        );
    }
}

fn is_loopback_bind_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::Request as HttpRequest};
    use tower::ServiceExt;

    fn edge_config(controller_base_url: String) -> PublicEdgeConfig {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.keep();
        PublicEdgeConfig {
            orchestrator: OrchestratorConfig {
                google_oauth: Some(crate::config::GoogleOAuthConfig {
                    client_id: "client".to_string(),
                    client_secret: "secret".to_string(),
                    allowed_emails: vec!["engineer@example.test".to_string()],
                    jwt_secret: Some("test-secret".to_string()),
                    trusted_networks: vec![],
                    ..crate::config::GoogleOAuthConfig::default()
                }),
                ..OrchestratorConfig::default()
            },
            controller: ControllerIpcConfig {
                base_url: controller_base_url,
                token: "controller-token".to_string(),
                timeout_seconds: 5,
            },
            runtime: PublicEdgeRuntimeConfig {
                frontend_dist: root.join("frontend/dist"),
                public_url: Some("https://office.example.test".to_string()),
            },
        }
    }

    fn oauth_cookie_header(auth: &AuthManager, token: &str) -> (String, String) {
        let cookies = auth
            .issue_oauth_session_cookies(token)
            .expect("oauth cookies");
        let cookie_header = cookies
            .iter()
            .map(|cookie| cookie.split(';').next().expect("cookie pair"))
            .collect::<Vec<_>>()
            .join("; ");
        let csrf_token = cookie_header
            .split("; ")
            .find_map(|cookie| cookie.strip_prefix("office_csrf="))
            .expect("csrf cookie")
            .to_string();
        (cookie_header, csrf_token)
    }

    #[test]
    fn edge_config_rejects_device_sections() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config_path = temp_dir.path().join("edge.yaml");
        fs::write(
            &config_path,
            r#"
orchestrator:
  host: "127.0.0.1"
controller:
  base_url: "http://127.0.0.1:9001"
  token: "controller-token"
runtime:
  frontend_dist: "/tmp/frontend"
qingping:
  mqtt_broker: "192.168.1.10"
"#,
        )
        .expect("write config");

        let error = PublicEdgeConfig::load_with_env(&config_path, |_| None)
            .expect_err("edge config must reject device sections");

        assert!(error.to_string().contains("failed to parse edge config"));
    }

    #[test]
    fn edge_config_requires_loopback_oauth_and_controller_token() {
        let mut config = edge_config("http://127.0.0.1:9001".to_string());
        config.orchestrator.host = "0.0.0.0".to_string();
        let error =
            validate_public_edge_config(&config).expect_err("non-loopback edge should fail");
        assert!(error.to_string().contains("must bind HTTP to loopback"));

        let mut config = edge_config("http://127.0.0.1:9001".to_string());
        config.orchestrator.google_oauth = None;
        let error = validate_public_edge_config(&config).expect_err("open edge should fail");
        assert!(error.to_string().contains("requires Google OAuth/JWT"));

        let mut config = edge_config("http://127.0.0.1:9001".to_string());
        config.controller.token.clear();
        let error =
            validate_public_edge_config(&config).expect_err("empty controller token should fail");
        assert!(error.to_string().contains("requires controller.token"));

        let config = edge_config("http://192.168.1.10:9001".to_string());
        let error = validate_public_edge_config(&config)
            .expect_err("non-loopback controller base url should fail");
        assert!(
            error
                .to_string()
                .contains("public edge controller.base_url must stay on loopback")
        );
    }

    #[test]
    fn controller_allowlist_excludes_deploy_and_unknown_routes() {
        assert!(is_allowed_controller_route(&Method::GET, "/status"));
        assert!(is_allowed_controller_route(&Method::POST, "/hvac"));
        assert!(is_allowed_controller_route(
            &Method::GET,
            "/apps/office-climate/meta.json"
        ));
        assert!(!is_allowed_controller_route(
            &Method::POST,
            "/deploy/office-climate"
        ));
        assert!(!is_allowed_controller_route(&Method::GET, "/admin/secrets"));
    }

    #[tokio::test]
    async fn deploy_route_is_not_available_on_public_edge() {
        let config = edge_config("http://127.0.0.1:9".to_string());
        let token = AuthManager::new(&config.orchestrator)
            .expect("auth")
            .generate_jwt("engineer@example.test")
            .expect("jwt");
        let app = app(config).expect("edge app");
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/deploy/office-climate")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "unexpected status: {}",
            response.status()
        );
    }

    #[tokio::test]
    async fn status_route_forwards_with_controller_token() {
        let controller = Router::new().route(
            "/status",
            get(|headers: HeaderMap| async move {
                let token = headers
                    .get(CONTROLLER_IPC_TOKEN_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("");
                if token == "controller-token" {
                    Json(json!({"ok": true, "source": "controller"})).into_response()
                } else {
                    StatusCode::UNAUTHORIZED.into_response()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind controller");
        let address = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, controller).await.expect("controller");
        });

        let config = edge_config(format!("http://{address}"));
        let token = AuthManager::new(&config.orchestrator)
            .expect("auth")
            .generate_jwt("engineer@example.test")
            .expect("jwt");
        let app = app(config).expect("edge app");
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let payload: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(payload["source"], "controller");
    }

    #[tokio::test]
    async fn edge_sets_security_headers_and_exact_cors_origin() {
        let app = app(edge_config("http://127.0.0.1:9".to_string())).expect("edge app");
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/auth/login")
                    .header(header::HOST, "office.example.test")
                    .header(header::ORIGIN, "https://office.example.test")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .expect("cors origin"),
            "https://office.example.test"
        );
        assert!(
            response
                .headers()
                .contains_key(header::CONTENT_SECURITY_POLICY)
        );
        assert!(
            response
                .headers()
                .contains_key(header::STRICT_TRANSPORT_SECURITY)
        );
        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .expect("nosniff"),
            "nosniff"
        );
    }

    #[tokio::test]
    async fn edge_session_cookie_requires_csrf_before_forwarding_post() {
        let controller = Router::new().route(
            "/presence",
            axum::routing::post(|headers: HeaderMap| async move {
                let token = headers
                    .get(CONTROLLER_IPC_TOKEN_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("");
                if token == "controller-token" {
                    Json(json!({"ok": true})).into_response()
                } else {
                    StatusCode::UNAUTHORIZED.into_response()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind controller");
        let address = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, controller).await.expect("controller");
        });

        let config = edge_config(format!("http://{address}"));
        let auth = AuthManager::new(&config.orchestrator).expect("auth");
        let token = auth.generate_jwt("engineer@example.test").expect("token");
        let (cookie_header, csrf_token) = oauth_cookie_header(&auth, &token);
        let app = app(config).expect("edge app");
        let payload = json!({"state": "away"}).to_string();

        let response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/presence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie_header.as_str())
                    .body(Body::from(payload.clone()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/presence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie_header)
                    .header(OAUTH_CSRF_HEADER, csrf_token)
                    .body(Body::from(payload))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn latest_apk_redirect_is_preserved_by_public_edge() {
        let controller = Router::new()
            .route(
                "/apps/demo/latest.apk",
                get(|| async move {
                    (
                        StatusCode::FOUND,
                        [
                            (header::LOCATION, "/apps/demo/abc123.apk"),
                            (header::CACHE_CONTROL, "no-cache"),
                        ],
                    )
                        .into_response()
                }),
            )
            .route(
                "/apps/demo/abc123.apk",
                get(|| async move { (StatusCode::OK, "hashed-apk").into_response() }),
            );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind controller");
        let address = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, controller).await.expect("controller");
        });

        let config = edge_config(format!("http://{address}"));
        let app = app(config).expect("edge app");
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/apps/demo/latest.apk")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/apps/demo/abc123.apk")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
    }
}
