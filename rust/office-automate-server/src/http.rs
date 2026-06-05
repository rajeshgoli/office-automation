use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        ConnectInfo, Extension, Multipart, Path, Query, Request, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, get_service, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};

use crate::{
    artifacts::{ArtifactStore, is_valid_artifact_hash},
    auth::{AuthManager, AuthenticatedUser, HttpAuthMode, WebSocketAuth, bearer_token},
    config::AppConfig,
    db,
    status::{Status, TemperatureBands},
};

const HVAC_TEMPERATURE_BANDS_SETTING: &str = "hvac_temperature_bands";

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    auth: AuthManager,
    artifacts: ArtifactStore,
    temperature_bands: Arc<RwLock<TemperatureBands>>,
    temperature_band_defaults: TemperatureBands,
}

pub fn app(config: AppConfig) -> Router {
    try_app(config).expect("failed to build HTTP app")
}

pub fn try_app(config: AppConfig) -> Result<Router> {
    db::migrate_database(&config.runtime.database_path)?;
    let auth = AuthManager::new(&config.orchestrator)?;
    let artifacts = ArtifactStore::new(
        config.runtime.artifacts_dir.clone(),
        config.runtime.legacy_apk_path.clone(),
    );
    let temperature_band_defaults = TemperatureBands::from_config(&config);
    let temperature_bands = load_hvac_temperature_bands(&config, temperature_band_defaults);
    let state = AppState {
        config,
        auth,
        artifacts,
        temperature_bands: Arc::new(RwLock::new(temperature_bands)),
        temperature_band_defaults,
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    let frontend_dist = state.config.runtime.frontend_dist.clone();
    let assets_dir = frontend_dist.join("assets");

    Ok(Router::new()
        .route("/status", get(status))
        .route("/ws", get(websocket))
        .route("/occupancy", post(occupancy))
        .route("/presence", post(presence))
        .route("/erv", post(erv))
        .route("/hvac", post(hvac))
        .route(
            "/hvac/temperature-bands",
            get(hvac_temperature_bands).post(update_hvac_temperature_bands),
        )
        .route("/qingping/interval", post(qingping_interval))
        .route("/history", get(history))
        .route("/history/sessions", get(history_sessions))
        .route("/history/co2-ohlc", get(history_co2_ohlc))
        .route("/history/temperature", get(history_temperature))
        .route("/history/daily-stats", get(history_daily_stats))
        .route("/history/openings", get(history_openings))
        .route("/history/orchestration", get(history_orchestration))
        .route("/history/project-focus", get(history_project_focus))
        .route("/history/leverage", get(history_leverage))
        .route("/history/project-leverage", get(history_project_leverage))
        .route("/deploy/{app}", post(deploy_app))
        .route("/apps/{app}/latest.apk", get(latest_app_artifact))
        .route("/apps/{app}/{artifact_file}", get(hashed_app_artifact))
        .route("/apps/{app}/meta.json", get(app_artifact_meta))
        .route("/apk", get(legacy_apk))
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
            auth_middleware,
        ))
        .with_state(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http()))
}

pub async fn serve(config: AppConfig) -> Result<()> {
    let bind_address = format!("{}:{}", config.orchestrator.host, config.orchestrator.port);
    let listener = TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("failed to bind HTTP listener at {bind_address}"))?;

    tracing::info!("office-automate-server listening on {}", bind_address);
    axum::serve(
        listener,
        try_app(config)
            .context("failed to build HTTP app")?
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("HTTP server failed")
}

async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if should_skip_auth(request.method(), request.uri().path(), request.headers()) {
        return next.run(request).await;
    }

    let mut headers = request.headers().clone();
    if !headers.contains_key("x-forwarded-for") {
        if let Some(ConnectInfo(remote_addr)) = request
            .extensions()
            .get::<ConnectInfo<std::net::SocketAddr>>()
        {
            if let Ok(value) = remote_addr.ip().to_string().parse() {
                headers.insert("x-forwarded-for", value);
            }
        }
    }

    match state.auth.mode() {
        HttpAuthMode::Open => next.run(request).await,
        HttpAuthMode::OAuth => {
            if state.auth.is_trusted_request(&headers) {
                request.extensions_mut().insert(AuthenticatedUser {
                    email: "trusted_network".to_string(),
                });
                return next.run(request).await;
            }

            let Some(user) = state.auth.verify_bearer_header(&headers) else {
                let missing = bearer_token(&headers).is_none();
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

            request.extensions_mut().insert(user);
            next.run(request).await
        }
        HttpAuthMode::Basic => {
            if state.auth.verify_basic_header(&headers) {
                return next.run(request).await;
            }

            let mut response =
                (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                "Basic realm=\"Office Climate\"".parse().expect("header"),
            );
            response
        }
    }
}

fn should_skip_auth(method: &Method, path: &str, headers: &HeaderMap) -> bool {
    if *method == Method::OPTIONS {
        return true;
    }

    if is_websocket_upgrade(headers) {
        return true;
    }

    if path == "/apk" || path.starts_with("/apps/") {
        return true;
    }

    if path == "/auth/login"
        || path == "/auth/callback"
        || path == "/auth/device/start"
        || path == "/auth/device/poll"
    {
        return true;
    }

    path == "/"
        || path == "/index.html"
        || path.starts_with("/assets/")
        || path.ends_with(".png")
        || path.ends_with(".json")
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

async fn status(State(state): State<AppState>) -> Json<Status> {
    Json(status_for_state(&state))
}

async fn websocket(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let mut auth_headers = headers.clone();
    if !auth_headers.contains_key("x-forwarded-for") {
        if let Ok(value) = remote_addr.ip().to_string().parse() {
            auth_headers.insert("x-forwarded-for", value);
        }
    }
    let mode = state.auth.websocket_auth(&auth_headers);
    ws.on_upgrade(move |socket| websocket_session(socket, state, mode))
}

async fn websocket_session(mut socket: WebSocket, state: AppState, auth_mode: WebSocketAuth) {
    if auth_mode == WebSocketAuth::FirstMessage {
        match timeout(Duration::from_secs(10), socket.recv()).await {
            Ok(Some(Ok(Message::Text(message)))) => {
                let Ok(value) = serde_json::from_str::<Value>(&message) else {
                    close_ws(&mut socket, "Authentication required").await;
                    return;
                };
                if value.get("type").and_then(Value::as_str) != Some("auth") {
                    close_ws(&mut socket, "Authentication required").await;
                    return;
                }
                let Some(token) = value.get("token").and_then(Value::as_str) else {
                    close_ws(&mut socket, "Authentication required").await;
                    return;
                };
                if state.auth.verify_jwt(token).is_none() {
                    close_ws(&mut socket, "Invalid token").await;
                    return;
                }
            }
            _ => {
                close_ws(&mut socket, "Authentication failed").await;
                return;
            }
        }
    }

    if send_status(&mut socket, &state).await.is_err() {
        return;
    }

    while let Some(message) = socket.recv().await {
        match message {
            Ok(Message::Text(text)) if text == "ping" => {
                let _ = socket.send(Message::Text("pong".into())).await;
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!("websocket receive error: {error}");
                break;
            }
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

async fn send_status(socket: &mut WebSocket, state: &AppState) -> Result<(), axum::Error> {
    let status = serde_json::to_string(&status_for_state(state)).expect("status serializes");
    socket.send(Message::Text(status.into())).await
}

#[derive(Debug, Deserialize)]
struct OccupancyRequest {
    last_active_timestamp: f64,
    #[serde(default)]
    external_monitor: bool,
}

async fn occupancy(Json(payload): Json<OccupancyRequest>) -> Json<Value> {
    let _ = (payload.last_active_timestamp, payload.external_monitor);
    Json(json!({"ok": true, "state": "away", "erv_should_run": false}))
}

#[derive(Debug, Deserialize)]
struct PresenceRequest {
    state: String,
}

async fn presence(Json(payload): Json<PresenceRequest>) -> Response {
    match payload.state.as_str() {
        "present" => {
            Json(json!({"ok": true, "state": "present", "is_present": true})).into_response()
        }
        "away" => Json(json!({"ok": true, "state": "away", "is_present": false})).into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "state must be present or away"})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct ErvRequest {
    speed: String,
}

async fn erv(Json(payload): Json<ErvRequest>) -> Response {
    if !matches!(payload.speed.as_str(), "off" | "quiet" | "medium" | "turbo") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Invalid ERV speed"})),
        )
            .into_response();
    }

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            json!({"ok": false, "error": "ERV control is not enabled in Rust compatibility mode"}),
        ),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct HvacRequest {
    mode: String,
    setpoint_f: Option<f64>,
}

async fn hvac(Json(payload): Json<HvacRequest>) -> Response {
    if !matches!(payload.mode.as_str(), "off" | "heat" | "cool") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Invalid HVAC mode"})),
        )
            .into_response();
    }
    let _ = payload.setpoint_f;

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            json!({"ok": false, "error": "HVAC control is not enabled in Rust compatibility mode"}),
        ),
    )
        .into_response()
}

async fn hvac_temperature_bands(State(state): State<AppState>) -> Json<Value> {
    temperature_bands_response(active_temperature_bands(&state), &state)
}

#[derive(Debug, Deserialize)]
struct TemperatureBandsPayload {
    temperature_bands: Option<TemperatureBands>,
    heat_on_temp_f: Option<i64>,
    heat_off_temp_f: Option<i64>,
    cool_off_temp_f: Option<i64>,
    cool_on_temp_f: Option<i64>,
}

async fn update_hvac_temperature_bands(
    State(state): State<AppState>,
    Json(payload): Json<TemperatureBandsPayload>,
) -> Response {
    let bands = payload.temperature_bands.unwrap_or(TemperatureBands {
        heat_on_temp_f: payload.heat_on_temp_f.unwrap_or_default(),
        heat_off_temp_f: payload.heat_off_temp_f.unwrap_or_default(),
        cool_off_temp_f: payload.cool_off_temp_f.unwrap_or_default(),
        cool_on_temp_f: payload.cool_on_temp_f.unwrap_or_default(),
    });

    if let Err(error) = validate_temperature_bands(bands) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": error})),
        )
            .into_response();
    }

    if let Err(error) = db::set_setting(
        &state.config.runtime.database_path,
        HVAC_TEMPERATURE_BANDS_SETTING,
        &bands,
    ) {
        tracing::error!("failed to persist HVAC temperature bands: {error:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": "Failed to persist temperature bands"})),
        )
            .into_response();
    }

    *state
        .temperature_bands
        .write()
        .expect("temperature band lock poisoned") = bands;

    temperature_bands_response(active_temperature_bands(&state), &state).into_response()
}

fn temperature_bands_response(bands: TemperatureBands, state: &AppState) -> Json<Value> {
    Json(json!({
        "ok": true,
        "temperature_bands": bands,
        "temperature_band_defaults": state.temperature_band_defaults,
    }))
}

fn load_hvac_temperature_bands(config: &AppConfig, defaults: TemperatureBands) -> TemperatureBands {
    match db::get_setting::<TemperatureBands>(
        &config.runtime.database_path,
        HVAC_TEMPERATURE_BANDS_SETTING,
    ) {
        Ok(Some(bands)) if validate_temperature_bands(bands).is_ok() => bands,
        Ok(Some(bands)) => {
            tracing::warn!("ignoring invalid stored HVAC temperature bands: {bands:?}");
            defaults
        }
        Ok(None) => defaults,
        Err(error) => {
            tracing::warn!("failed to load HVAC temperature bands: {error:#}");
            defaults
        }
    }
}

fn active_temperature_bands(state: &AppState) -> TemperatureBands {
    *state
        .temperature_bands
        .read()
        .expect("temperature band lock poisoned")
}

fn status_for_state(state: &AppState) -> Status {
    Status::read_only_with_temperature_bands(&state.config, active_temperature_bands(state))
}

fn validate_temperature_bands(bands: TemperatureBands) -> Result<(), &'static str> {
    if !(45..=85).contains(&bands.heat_on_temp_f) {
        return Err("heat_on_temp_f must be between 45 and 85");
    }
    if !(46..=90).contains(&bands.heat_off_temp_f) {
        return Err("heat_off_temp_f must be between 46 and 90");
    }
    if !(55..=95).contains(&bands.cool_off_temp_f) {
        return Err("cool_off_temp_f must be between 55 and 95");
    }
    if !(56..=100).contains(&bands.cool_on_temp_f) {
        return Err("cool_on_temp_f must be between 56 and 100");
    }
    if bands.heat_on_temp_f >= bands.heat_off_temp_f {
        return Err("heat_on_temp_f must be below heat_off_temp_f");
    }
    if bands.cool_off_temp_f >= bands.cool_on_temp_f {
        return Err("cool_off_temp_f must be below cool_on_temp_f");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct QingpingIntervalRequest {
    interval: i64,
}

async fn qingping_interval(Json(payload): Json<QingpingIntervalRequest>) -> Response {
    if payload.interval < 15 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "interval must be >= 15"})),
        )
            .into_response();
    }

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"ok": false, "error": "MQTT command path is not enabled yet"})),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    hours: Option<i64>,
    days: Option<i64>,
    limit: Option<i64>,
    bucket_minutes: Option<i64>,
}

fn clamp(value: Option<i64>, default: i64, min: i64, max: i64) -> i64 {
    value.unwrap_or(default).clamp(min, max)
}

async fn history(Query(query): Query<HistoryQuery>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "hours": clamp(query.hours, 24, 1, 168),
        "sensor_readings": [],
        "occupancy_history": [],
        "device_events": [],
        "climate_actions": [],
        "limit": clamp(query.limit, 1000, 10, 10000),
    }))
}

async fn history_sessions(Query(query): Query<HistoryQuery>) -> Json<Value> {
    Json(json!({"ok": true, "days": clamp(query.days, 7, 1, 30), "sessions": [], "summary": {}}))
}

async fn history_co2_ohlc(Query(query): Query<HistoryQuery>) -> Json<Value> {
    let hours = clamp(query.hours, 24, 1, 168);
    Json(json!({
        "ok": true,
        "hours": hours,
        "bucket_minutes": query.bucket_minutes.unwrap_or(default_co2_bucket(hours)),
        "candles": [],
    }))
}

async fn history_temperature(Query(query): Query<HistoryQuery>) -> Json<Value> {
    let hours = clamp(query.hours, 24, 1, 168);
    Json(json!({
        "ok": true,
        "hours": hours,
        "bucket_minutes": query.bucket_minutes.unwrap_or(default_temperature_bucket(hours)),
        "points": [],
    }))
}

async fn history_daily_stats(Query(query): Query<HistoryQuery>) -> Json<Value> {
    Json(json!({"ok": true, "days": clamp(query.days, 7, 1, 30), "stats": []}))
}

async fn history_openings(Query(query): Query<HistoryQuery>) -> Json<Value> {
    Json(json!({"ok": true, "days": [], "requested_days": clamp(query.days, 7, 1, 30)}))
}

async fn history_orchestration(Query(query): Query<HistoryQuery>) -> Json<Value> {
    Json(json!({"ok": true, "days": [], "requested_days": clamp(query.days, 7, 1, 30)}))
}

async fn history_project_focus(Query(query): Query<HistoryQuery>) -> Json<Value> {
    Json(json!({"ok": true, "days": [], "requested_days": clamp(query.days, 7, 1, 30)}))
}

async fn history_leverage(Query(query): Query<HistoryQuery>) -> Json<Value> {
    Json(json!({"ok": true, "days": [], "week": {}, "requested_days": clamp(query.days, 7, 1, 30)}))
}

async fn history_project_leverage(Query(query): Query<HistoryQuery>) -> Json<Value> {
    let _ = clamp(query.days, 7, 1, 30);
    Json(json!({"ok": true, "projects": {}}))
}

fn default_co2_bucket(hours: i64) -> i64 {
    if hours <= 6 {
        5
    } else if hours <= 24 {
        15
    } else if hours <= 72 {
        60
    } else {
        240
    }
}

fn default_temperature_bucket(hours: i64) -> i64 {
    if hours <= 6 {
        5
    } else if hours <= 24 {
        15
    } else if hours <= 72 {
        30
    } else {
        120
    }
}

async fn deploy_app(
    State(state): State<AppState>,
    user: Option<Extension<AuthenticatedUser>>,
    Path(app): Path<String>,
    multipart: Multipart,
) -> Response {
    let user_email = user
        .map(|Extension(user)| user.email)
        .unwrap_or_else(|| "unknown".to_string());
    match state
        .artifacts
        .store_upload(&app, multipart, &user_email)
        .await
    {
        Ok(outcome) => Json(json!({
            "ok": true,
            "app": outcome.app,
            "size_bytes": outcome.size_bytes,
            "download_url": outcome.download_url,
        }))
        .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn latest_app_artifact(State(state): State<AppState>, Path(app): Path<String>) -> Response {
    let Ok(Some(metadata)) = state.artifacts.read_metadata(&app).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if !is_valid_artifact_hash(&metadata.artifact_hash) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let mut response = Response::builder()
        .status(StatusCode::FOUND)
        .header(
            header::LOCATION,
            format!("/apps/{app}/{}.apk", metadata.artifact_hash),
        )
        .body(Body::empty())
        .expect("valid redirect response");
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-cache".parse().expect("header"));
    response
}

async fn hashed_app_artifact(
    State(state): State<AppState>,
    Path((app, artifact_file)): Path<(String, String)>,
) -> Response {
    let Some(artifact_hash) = artifact_file.strip_suffix(".apk") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match state
        .artifacts
        .serve_hashed_artifact(&app, artifact_hash)
        .await
    {
        Ok(Some(response)) => response,
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!("artifact read failed: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn app_artifact_meta(State(state): State<AppState>, Path(app): Path<String>) -> Response {
    match state.artifacts.read_metadata(&app).await {
        Ok(Some(metadata)) => Json(metadata).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!("artifact metadata read failed: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn legacy_apk(State(state): State<AppState>) -> Response {
    match state.artifacts.serve_legacy_apk().await {
        Ok(Some(response)) => response,
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!("legacy APK read failed: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn auth_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !state.auth.oauth_enabled() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "OAuth not configured"})),
        )
            .into_response();
    }

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
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !state.auth.oauth_enabled() {
        return (StatusCode::NOT_IMPLEMENTED, "OAuth not configured").into_response();
    }

    if let Some(error) = query.get("error") {
        return (
            StatusCode::BAD_REQUEST,
            Html(format!(
                "<html><body><h1>Login Failed</h1><p>{error}</p></body></html>"
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
        Ok(Some(login)) => Html(format!(
            "<html><head><script>localStorage.setItem('auth_token', '{}');localStorage.setItem('user_email', '{}');window.location.href = '/';</script></head><body><p>Login successful! Redirecting...</p></body></html>",
            login.jwt, login.email
        ))
        .into_response(),
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

async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !state.auth.oauth_enabled() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "OAuth not configured"})),
        )
            .into_response();
    }

    let Some(token) = bearer_token(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "No token provided"})),
        )
            .into_response();
    };

    state.auth.invalidate_token(token);
    Json(json!({"ok": true, "message": "Logged out"})).into_response()
}

async fn auth_device_start(State(state): State<AppState>) -> Response {
    if !state.auth.oauth_enabled() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "OAuth not configured"})),
        )
            .into_response();
    }

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
    State(state): State<AppState>,
    Json(payload): Json<DevicePollRequest>,
) -> Response {
    if !state.auth.oauth_enabled() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "OAuth not configured"})),
        )
            .into_response();
    }

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

async fn index(State(state): State<AppState>) -> Response {
    serve_static_or_index(&state, "index.html").await
}

async fn spa_fallback(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    serve_static_or_index(&state, &path).await
}

async fn serve_static_or_index(state: &AppState, path: &str) -> Response {
    let requested = state.config.runtime.frontend_dist.join(path);
    let target = if requested.exists() && requested.is_file() {
        requested
    } else {
        state.config.runtime.frontend_dist.join("index.html")
    };

    match tokio::fs::read(&target).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(bytes))
            .expect("static response"),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request as HttpRequest,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message as TungsteniteMessage, client::IntoClientRequest},
    };
    use tower::ServiceExt;

    use super::*;
    use crate::config::{
        GoogleOAuthConfig, OrchestratorConfig, QingpingConfig, RuntimeConfig, ThresholdsConfig,
    };

    fn test_config() -> AppConfig {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.keep();
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            qingping: QingpingConfig::default(),
            thresholds: ThresholdsConfig::default(),
            runtime: RuntimeConfig {
                root: root.clone(),
                config_path: root.join("config.yaml"),
                data_dir: root.join("data"),
                database_path: root.join("data/office_climate.db"),
                frontend_dist: root.join("frontend/dist"),
                artifacts_dir: root.join("data/apps"),
                legacy_apk_path: root.join("data/app-debug.apk"),
                base_url: None,
                public_url: None,
                mqtt_host: "127.0.0.1".to_string(),
                mqtt_port: 1883,
            },
        }
    }

    fn oauth_config() -> AppConfig {
        AppConfig {
            orchestrator: OrchestratorConfig {
                google_oauth: Some(GoogleOAuthConfig {
                    client_id: "client".to_string(),
                    client_secret: "secret".to_string(),
                    allowed_emails: vec!["engineer@rajeshgo.li".to_string()],
                    jwt_secret: Some("test-secret".to_string()),
                    trusted_networks: vec!["127.0.0.1/32".to_string()],
                    ..GoogleOAuthConfig::default()
                }),
                ..OrchestratorConfig::default()
            },
            ..test_config()
        }
    }

    fn multipart_body(boundary: &str, bytes: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"version_code\"\r\n\r\n7\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"version_name\"\r\n\r\n1.2.0\r\n").as_bytes());
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"artifact.apk\"\r\nContent-Type: application/vnd.android.package-archive\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }

    #[tokio::test]
    async fn status_route_returns_compatibility_skeleton() {
        let response = app(test_config())
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");

        assert_eq!(value["state"], "away");
        assert!(value.get("sensors").is_some());
        assert!(value.get("air_quality").is_some());
        assert!(value.get("erv").is_some());
        assert!(value.get("hvac").is_some());
        assert!(value["erv"]["control"].get("last_local_ok_at").is_some());
    }

    #[tokio::test]
    async fn hvac_temperature_band_updates_persist_for_get_and_status() {
        let config = test_config();
        let payload = json!({
            "temperature_bands": {
                "heat_on_temp_f": 69,
                "heat_off_temp_f": 73,
                "cool_off_temp_f": 79,
                "cool_on_temp_f": 83,
            }
        });

        let response = app(config.clone())
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/hvac/temperature-bands")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let response = app(config.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/hvac/temperature-bands")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["temperature_bands"]["heat_on_temp_f"], 69);
        assert_eq!(value["temperature_bands"]["cool_on_temp_f"], 83);

        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["hvac"]["temperature_bands"]["heat_on_temp_f"], 69);
        assert_eq!(value["hvac"]["temperature_bands"]["cool_on_temp_f"], 83);
        assert_eq!(
            value["hvac"]["temperature_band_defaults"]["heat_on_temp_f"],
            71
        );
    }

    #[tokio::test]
    async fn oauth_middleware_requires_bearer_for_protected_routes() {
        let response = app(oauth_config())
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["login_url"], "/auth/login");
    }

    #[tokio::test]
    async fn artifact_upload_and_download_preserve_metadata_and_headers() {
        let config = oauth_config();
        let token = AuthManager::new(&config.orchestrator)
            .expect("auth")
            .generate_jwt("engineer@rajeshgo.li")
            .expect("token");
        let boundary = "test-boundary";
        let response = app(config.clone())
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/deploy/office-climate")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(multipart_body(boundary, b"apk-bytes")))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["download_url"], "/apps/office-climate/latest.apk");

        let response = app(config.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/apps/office-climate/meta.json")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let metadata: Value = serde_json::from_slice(&body).expect("json body");
        let artifact_hash = metadata["artifact_hash"].as_str().expect("hash");
        assert_eq!(metadata["uploaded_by"], "engineer@rajeshgo.li");
        assert_eq!(metadata["version_code"], 7);
        assert_eq!(metadata["version_name"], "1.2.0");

        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri(format!("/apps/office-climate/{artifact_hash}.apk"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=office-climate.apk"
        );
    }

    #[tokio::test]
    async fn websocket_first_message_auth_delays_initial_status_until_token() {
        let config = AppConfig {
            orchestrator: OrchestratorConfig {
                google_oauth: Some(GoogleOAuthConfig {
                    client_id: "client".to_string(),
                    client_secret: "secret".to_string(),
                    allowed_emails: vec!["engineer@rajeshgo.li".to_string()],
                    jwt_secret: Some("test-secret".to_string()),
                    trusted_networks: Vec::new(),
                    ..GoogleOAuthConfig::default()
                }),
                ..OrchestratorConfig::default()
            },
            ..test_config()
        };
        let token = AuthManager::new(&config.orchestrator)
            .expect("auth")
            .generate_jwt("engineer@rajeshgo.li")
            .expect("token");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app(config).into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("server");
        });

        let (mut ws, _) = connect_async(format!("ws://{address}/ws"))
            .await
            .expect("connect");
        assert!(
            timeout(Duration::from_millis(100), ws.next())
                .await
                .is_err()
        );

        ws.send(TungsteniteMessage::Text(
            json!({"type": "auth", "token": token}).to_string().into(),
        ))
        .await
        .expect("send auth");
        let message = timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("status timeout")
            .expect("message")
            .expect("message ok");
        assert!(
            message
                .into_text()
                .expect("text")
                .contains("\"state\":\"away\"")
        );

        ws.send(TungsteniteMessage::Text("ping".to_string().into()))
            .await
            .expect("send ping");
        let message = timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("pong timeout")
            .expect("message")
            .expect("message ok");
        assert_eq!(message.into_text().expect("text"), "pong");

        server.abort();
    }

    #[tokio::test]
    async fn websocket_upgrade_bearer_auth_receives_initial_status_immediately() {
        let config = AppConfig {
            orchestrator: OrchestratorConfig {
                google_oauth: Some(GoogleOAuthConfig {
                    client_id: "client".to_string(),
                    client_secret: "secret".to_string(),
                    allowed_emails: vec!["engineer@rajeshgo.li".to_string()],
                    jwt_secret: Some("test-secret".to_string()),
                    trusted_networks: Vec::new(),
                    ..GoogleOAuthConfig::default()
                }),
                ..OrchestratorConfig::default()
            },
            ..test_config()
        };
        let token = AuthManager::new(&config.orchestrator)
            .expect("auth")
            .generate_jwt("engineer@rajeshgo.li")
            .expect("token");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app(config).into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("server");
        });

        let mut request = format!("ws://{address}/ws")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("header"),
        );
        let (mut ws, _) = connect_async(request).await.expect("connect");

        let message = timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("status timeout")
            .expect("message")
            .expect("message ok");
        assert!(
            message
                .into_text()
                .expect("text")
                .contains("\"state\":\"away\"")
        );

        server.abort();
    }

    #[tokio::test]
    async fn localtunnel_route_is_gone_for_cloudflared_target() {
        let response = app(test_config())
            .oneshot(
                HttpRequest::builder()
                    .uri("/localtunnel/password")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::GONE);
    }
}
