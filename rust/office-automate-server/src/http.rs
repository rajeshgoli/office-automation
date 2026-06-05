use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::Component,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        ConnectInfo, DefaultBodyLimit, Extension, Multipart, Path, Query, Request, State,
        WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, get_service, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::broadcast, time::timeout};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};

use crate::{
    artifacts::ARTIFACT_MAX_SIZE_BYTES,
    artifacts::{ArtifactStore, is_valid_artifact_hash},
    auth::{AuthManager, AuthenticatedUser, HttpAuthMode, WebSocketAuth, bearer_token},
    config::AppConfig,
    db,
    state::{StateMachine, StateTransition},
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
    state_machine: Arc<RwLock<StateMachine>>,
    status_broadcast: broadcast::Sender<()>,
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
    let state_machine = StateMachine::from_thresholds(&config.thresholds, unix_timestamp_now());
    let (status_broadcast, _) = broadcast::channel(32);
    let state = AppState {
        config,
        auth,
        artifacts,
        temperature_bands: Arc::new(RwLock::new(temperature_bands)),
        temperature_band_defaults,
        state_machine: Arc::new(RwLock::new(state_machine)),
        status_broadcast,
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
        .route(
            "/deploy/{app}",
            post(deploy_app).layer(DefaultBodyLimit::max(artifact_upload_body_limit())),
        )
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
    let auth_mode = state.auth.mode();
    if should_skip_auth(request.method(), request.uri().path(), auth_mode) {
        return next.run(request).await;
    }

    let remote_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(remote_addr)| *remote_addr);
    let headers = auth_headers_with_peer(request.headers(), remote_addr);

    if request.uri().path() == "/ws"
        && is_websocket_upgrade(&headers)
        && matches!(auth_mode, HttpAuthMode::OAuth | HttpAuthMode::Basic)
    {
        return next.run(request).await;
    }

    match auth_mode {
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
                let mut response = next.run(request).await;
                if let Some(cookie) = state.auth.issue_basic_websocket_cookie() {
                    if let Ok(cookie) = cookie.parse() {
                        response.headers_mut().insert(header::SET_COOKIE, cookie);
                    }
                }
                return response;
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

fn artifact_upload_body_limit() -> usize {
    (ARTIFACT_MAX_SIZE_BYTES + 1024 * 1024) as usize
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

async fn status(State(state): State<AppState>) -> Json<Status> {
    Json(status_for_state(&state))
}

async fn websocket(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let auth_headers = auth_headers_with_peer(&headers, Some(remote_addr));
    let mode = state.auth.websocket_auth(&auth_headers);
    ws.on_upgrade(move |socket| websocket_session(socket, state, mode))
}

async fn websocket_session(mut socket: WebSocket, state: AppState, auth_mode: WebSocketAuth) {
    if auth_mode == WebSocketAuth::Reject {
        close_ws(&mut socket, "Authentication required").await;
        return;
    }

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

    let mut status_updates = state.status_broadcast.subscribe();

    if send_status(&mut socket, &state).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) if text == "ping" => {
                        let _ = socket.send(Message::Text("pong".into())).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        tracing::debug!("websocket receive error: {error}");
                        break;
                    }
                }
            }
            update = status_updates.recv() => {
                match update {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        if send_status(&mut socket, &state).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
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

fn broadcast_status(state: &AppState) {
    let _ = state.status_broadcast.send(());
}

fn unix_timestamp_now() -> f64 {
    chrono::Local::now().timestamp_millis() as f64 / 1_000.0
}

#[derive(Debug, Deserialize)]
struct OccupancyRequest {
    last_active_timestamp: f64,
    #[serde(default)]
    external_monitor: bool,
}

async fn occupancy(
    State(state): State<AppState>,
    Json(payload): Json<OccupancyRequest>,
) -> Response {
    let now = unix_timestamp_now();
    let (transition, state_name, erv_should_run) = {
        let mut machine = state
            .state_machine
            .write()
            .expect("state machine lock poisoned");
        let transition = machine.update_mac_occupancy(
            payload.last_active_timestamp,
            payload.external_monitor,
            now,
        );
        let status = machine.status_at(now);
        (transition, status.state, status.erv_should_run)
    };

    if let Err(error) = log_state_transition(&state, transition, "mac_activity") {
        tracing::error!("failed to log occupancy transition: {error:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": "Failed to persist occupancy update"})),
        )
            .into_response();
    }

    broadcast_status(&state);
    Json(json!({"ok": true, "state": state_name, "erv_should_run": erv_should_run})).into_response()
}

#[derive(Debug, Deserialize)]
struct PresenceRequest {
    state: String,
}

async fn presence(State(state): State<AppState>, Json(payload): Json<PresenceRequest>) -> Response {
    match payload.state.as_str() {
        "present" | "away" => {
            let requested_state = payload.state.as_str();
            let now = unix_timestamp_now();
            let present = requested_state == "present";
            let (transition, state_name, is_present) = {
                let mut machine = state
                    .state_machine
                    .write()
                    .expect("state machine lock poisoned");
                let transition = machine.set_manual_presence(present, now);
                let status = machine.status_at(now);
                (transition, status.state, status.is_present)
            };

            if let Err(error) = db::log_device_event(
                &state.config.runtime.database_path,
                "presence",
                &format!("manual_{requested_state}"),
                Some("Dashboard"),
                None,
            )
            .and_then(|_| log_state_transition(&state, transition, "manual"))
            {
                tracing::error!("failed to persist manual presence update: {error:#}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": "Failed to persist presence update"})),
                )
                    .into_response();
            }

            broadcast_status(&state);
            Json(json!({"ok": true, "state": state_name, "is_present": is_present})).into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "state must be present or away"})),
        )
            .into_response(),
    }
}

fn log_state_transition(
    state: &AppState,
    transition: Option<StateTransition>,
    trigger: &str,
) -> Result<()> {
    let Some(transition) = transition else {
        return Ok(());
    };

    let co2_ppm = state
        .state_machine
        .read()
        .expect("state machine lock poisoned")
        .sensors
        .co2_ppm;
    db::log_occupancy_change(
        &state.config.runtime.database_path,
        transition.new_state.as_str(),
        Some(trigger),
        Some(co2_ppm),
        Some(&json!({"old_state": transition.old_state.as_str()})),
    )
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

    broadcast_status(&state);
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
    let mut status =
        Status::read_only_with_temperature_bands(&state.config, active_temperature_bands(state));
    let now = unix_timestamp_now();
    let state_status = {
        let mut machine = state
            .state_machine
            .write()
            .expect("state machine lock poisoned");
        machine.advance_timers(now);
        machine.status_at(now)
    };

    status.state = state_status.state;
    status.is_present = state_status.is_present;
    status.presence_signal_active = state_status.presence_signal_active;
    status.safety_interlock = state_status.safety_interlock;
    status.erv_should_run = state_status.erv_should_run;
    status.verifying_departure = state_status.verifying_departure;
    status.in_door_open_mode = state_status.in_door_open_mode;
    status.sensors.mac_last_active = state_status.sensors.mac_last_active;
    status.sensors.mac_active =
        state_status.sensors.external_monitor && state_status.sensors.mac_last_active > 0.0;
    status.sensors.external_monitor = state_status.sensors.external_monitor;
    status.sensors.motion_detected = state_status.sensors.motion_detected;
    status.sensors.door_open = state_status.sensors.door_open;
    status.sensors.window_open = state_status.sensors.window_open;
    status.sensors.co2_ppm = state_status.sensors.co2_ppm;
    status
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
    limit: Option<i64>,
}

fn clamp(value: Option<i64>, default: i64, min: i64, max: i64) -> i64 {
    value.unwrap_or(default).clamp(min, max)
}

async fn history(State(state): State<AppState>, Query(query): Query<HistoryQuery>) -> Response {
    let hours = clamp(query.hours, 24, 1, 168);
    let limit = clamp(query.limit, 1000, 10, 10000);
    let history = match db::read_history(&state.config.runtime.database_path, hours, limit) {
        Ok(history) => history,
        Err(error) => {
            tracing::error!("failed to read history: {error:#}");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": error.to_string()})),
            )
                .into_response();
        }
    };

    Json(json!({
        "ok": true,
        "hours": hours,
        "sensor_readings": history.sensor_readings,
        "occupancy_history": history.occupancy_history,
        "device_events": history.device_events,
        "climate_actions": history.climate_actions,
        "limit": limit,
    }))
    .into_response()
}

async fn history_sessions() -> Response {
    history_not_implemented("history sessions")
}

async fn history_co2_ohlc() -> Response {
    history_not_implemented("CO2 OHLC history")
}

async fn history_temperature() -> Response {
    history_not_implemented("temperature history")
}

async fn history_daily_stats() -> Response {
    history_not_implemented("daily stats history")
}

async fn history_openings() -> Response {
    history_not_implemented("opening history")
}

async fn history_orchestration() -> Response {
    history_not_implemented("orchestration history")
}

async fn history_project_focus() -> Response {
    history_not_implemented("project focus history")
}

async fn history_leverage() -> Response {
    history_not_implemented("leverage history")
}

async fn history_project_leverage() -> Response {
    history_not_implemented("project leverage history")
}

fn history_not_implemented(name: &str) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "ok": false,
            "error": format!("{name} is not implemented in the Rust compatibility server yet"),
        })),
    )
        .into_response()
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
    let metadata = match state.artifacts.read_metadata(&app).await {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!("artifact metadata read failed: {error:#}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
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
        Ok(Some(login)) => {
            let token = script_json_string(&login.jwt);
            let email = script_json_string(&login.email);
            Html(format!(
                "<html><head><script>localStorage.setItem('auth_token', {token});localStorage.setItem('user_email', {email});window.location.href = '/';</script></head><body><p>Login successful! Redirecting...</p></body></html>",
            ))
            .into_response()
        }
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

fn script_json_string(value: &str) -> String {
    serde_json::to_string(value)
        .expect("string serializes")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
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
    let target = if is_safe_spa_path(path) && requested.exists() && requested.is_file() {
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

fn is_safe_spa_path(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
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

    fn basic_config() -> AppConfig {
        AppConfig {
            orchestrator: OrchestratorConfig {
                auth_username: Some("user".to_string()),
                auth_password: Some("pass".to_string()),
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
    async fn basic_auth_protects_frontend_static_routes() {
        let config = basic_config();
        tokio::fs::create_dir_all(&config.runtime.frontend_dist)
            .await
            .expect("create dist");
        tokio::fs::create_dir_all(config.runtime.frontend_dist.join("assets"))
            .await
            .expect("create assets");
        tokio::fs::write(config.runtime.frontend_dist.join("index.html"), "dashboard")
            .await
            .expect("write index");
        tokio::fs::write(
            config.runtime.frontend_dist.join("assets/app.js"),
            "console.log('dashboard')",
        )
        .await
        .expect("write asset");

        for path in ["/", "/index.html", "/assets/app.js", "/manifest.json"] {
            let response = app(config.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE),
                Some(&"Basic realm=\"Office Climate\"".parse().expect("header"))
            );
        }

        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn websocket_upgrade_header_does_not_bypass_non_ws_route_auth() {
        let boundary = "deploy-boundary";
        let body = multipart_body(boundary, b"apk-bytes");
        let response = app(oauth_config())
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/deploy/office-climate")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::UPGRADE, "websocket")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn oauth_trusted_network_ignores_client_supplied_forwarded_for() {
        let config = AppConfig {
            orchestrator: OrchestratorConfig {
                google_oauth: Some(GoogleOAuthConfig {
                    client_id: "client".to_string(),
                    client_secret: "secret".to_string(),
                    allowed_emails: vec!["engineer@rajeshgo.li".to_string()],
                    jwt_secret: Some("test-secret".to_string()),
                    trusted_networks: vec!["203.0.113.0/24".to_string()],
                    ..GoogleOAuthConfig::default()
                }),
                ..OrchestratorConfig::default()
            },
            ..test_config()
        };

        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .header("x-forwarded-for", "203.0.113.7")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn oauth_loopback_proxy_requires_forwarded_client_for_trusted_networks() {
        let response = app(oauth_config())
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .header("x-forwarded-for", "198.51.100.24")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 49152))))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app(oauth_config())
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 49152))))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let config = AppConfig {
            orchestrator: OrchestratorConfig {
                google_oauth: Some(GoogleOAuthConfig {
                    client_id: "client".to_string(),
                    client_secret: "secret".to_string(),
                    allowed_emails: vec!["engineer@rajeshgo.li".to_string()],
                    jwt_secret: Some("test-secret".to_string()),
                    trusted_networks: vec!["203.0.113.0/24".to_string()],
                    ..GoogleOAuthConfig::default()
                }),
                ..OrchestratorConfig::default()
            },
            ..test_config()
        };
        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .header("x-forwarded-for", "203.0.113.7")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 49152))))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn presence_route_updates_status_and_history() {
        let service = app(test_config());
        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/presence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"state":"present"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["state"], "present");
        assert_eq!(value["is_present"], true);

        let response = service
            .clone()
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
        assert_eq!(value["state"], "present");
        assert_eq!(value["is_present"], true);
        assert_eq!(value["sensors"]["motion_detected"], true);

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .uri("/history?hours=1")
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
        assert_eq!(value["device_events"][0]["device_type"], "presence");
        assert_eq!(value["device_events"][0]["event"], "manual_present");
        assert_eq!(value["occupancy_history"][0]["state"], "present");
        assert_eq!(value["occupancy_history"][0]["trigger"], "manual");
    }

    #[tokio::test]
    async fn websocket_broadcasts_manual_presence_updates() {
        let service = app(test_config());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server_service = service.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                server_service.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("server");
        });

        let (mut ws, _) = connect_async(format!("ws://{address}/ws"))
            .await
            .expect("connect");
        let initial = timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("initial timeout")
            .expect("initial message")
            .expect("initial ok");
        assert!(
            initial
                .into_text()
                .expect("text")
                .contains("\"state\":\"away\"")
        );

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/presence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"state":"present"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let update = timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("update timeout")
            .expect("update message")
            .expect("update ok");
        assert!(
            update
                .into_text()
                .expect("text")
                .contains("\"state\":\"present\"")
        );

        server.abort();
    }

    #[tokio::test]
    async fn history_route_queries_persisted_rows() {
        let config = test_config();
        db::migrate_database(&config.runtime.database_path).expect("migration");
        let connection =
            rusqlite::Connection::open(&config.runtime.database_path).expect("open database");
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        connection
            .execute(
                r#"
                INSERT INTO sensor_readings (timestamp, co2_ppm, temp_c, source)
                VALUES (?, ?, ?, ?)
                "#,
                (&timestamp, 612, 22.5, "qingping"),
            )
            .expect("insert sensor");
        connection
            .execute(
                r#"
                INSERT INTO climate_actions (timestamp, system, action, setpoint, co2_ppm, reason)
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
                (&timestamp, "erv", "turbo", Option::<f64>::None, 612, "test"),
            )
            .expect("insert climate action");

        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/history?hours=1&limit=10")
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

        assert_eq!(value["sensor_readings"][0]["co2_ppm"], 612);
        assert_eq!(value["sensor_readings"][0]["temp_c"], 22.5);
        assert_eq!(value["sensor_readings"][0]["source"], "qingping");
        assert_eq!(value["climate_actions"][0]["system"], "erv");
        assert_eq!(value["climate_actions"][0]["action"], "turbo");
        assert_eq!(value["climate_actions"][0]["reason"], "test");
    }

    #[tokio::test]
    async fn secondary_history_routes_fail_loudly_until_ported() {
        for path in [
            "/history/sessions?days=7",
            "/history/co2-ohlc?hours=24",
            "/history/temperature?hours=24",
            "/history/daily-stats?days=7",
            "/history/openings?days=7",
            "/history/orchestration?days=7",
            "/history/project-focus?days=7",
            "/history/leverage?days=7",
            "/history/project-leverage?days=7",
        ] {
            let response = app(test_config())
                .oneshot(
                    HttpRequest::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED, "{path}");
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read body");
            let value: Value = serde_json::from_slice(&body).expect("json body");
            assert_eq!(value["ok"], false, "{path}");
            assert!(
                value["error"]
                    .as_str()
                    .expect("error")
                    .contains("not implemented"),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn basic_auth_websocket_requires_header_or_session_cookie() {
        let service = app(basic_config());
        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let websocket_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("basic websocket cookie")
            .to_str()
            .expect("cookie")
            .split(';')
            .next()
            .expect("cookie pair")
            .to_string();

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                service.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("server");
        });

        let (mut unauthenticated, _) = connect_async(format!("ws://{address}/ws"))
            .await
            .expect("unauthenticated websocket should upgrade for close frame");
        let close = timeout(Duration::from_secs(1), unauthenticated.next())
            .await
            .expect("close timeout")
            .expect("close message")
            .expect("close ok");
        assert!(matches!(close, TungsteniteMessage::Close(_)));

        let mut request = format!("ws://{address}/ws")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().expect("header"),
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

        let mut request = format!("ws://{address}/ws")
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert(header::COOKIE, websocket_cookie.parse().expect("cookie"));
        let (mut ws, _) = connect_async(request).await.expect("connect with cookie");
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
    async fn oauth_callback_escapes_error_text() {
        let response = app(oauth_config())
            .oneshot(
                HttpRequest::builder()
                    .uri("/auth/callback?error=%3Cscript%3Ealert(1)%3C%2Fscript%3E%26%22%27")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;&amp;&quot;&#39;"));
        assert!(!body.contains("<script>"));
    }

    #[test]
    fn script_json_string_round_trips_and_escapes_script_delimiters() {
        let value = "quote'\"\\</script><script>&";
        let literal = script_json_string(value);

        assert_eq!(
            serde_json::from_str::<String>(&literal).expect("json string"),
            value
        );
        assert!(literal.contains("\\\""));
        assert!(literal.contains("\\\\"));
        assert!(literal.contains("\\u003c/script\\u003e"));
        assert!(literal.contains("\\u0026"));
        assert!(!literal.contains("</script>"));
        assert!(!literal.contains("<script>"));
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
    async fn latest_artifact_surfaces_malformed_metadata_as_server_error() {
        let config = test_config();
        let app_dir = config.runtime.artifacts_dir.join("office-climate");
        tokio::fs::create_dir_all(&app_dir)
            .await
            .expect("create app dir");
        tokio::fs::write(app_dir.join("meta.json"), "{not json")
            .await
            .expect("write malformed metadata");

        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/apps/office-climate/latest.apk")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn artifact_upload_accepts_apks_larger_than_axum_default_body_limit() {
        let config = oauth_config();
        let token = AuthManager::new(&config.orchestrator)
            .expect("auth")
            .generate_jwt("engineer@rajeshgo.li")
            .expect("token");
        let boundary = "large-upload-boundary";
        let bytes = vec![b'a'; 3 * 1024 * 1024];
        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/deploy/office-climate")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(multipart_body(boundary, &bytes)))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["size_bytes"], bytes.len());
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

    #[tokio::test]
    async fn spa_fallback_rejects_parent_directory_traversal() {
        let config = test_config();
        tokio::fs::create_dir_all(&config.runtime.frontend_dist)
            .await
            .expect("create frontend dist");
        tokio::fs::write(
            config.runtime.frontend_dist.join("index.html"),
            b"spa-index",
        )
        .await
        .expect("write index");
        tokio::fs::write(config.runtime.root.join("secret.txt"), b"secret")
            .await
            .expect("write escaped file");

        let response = app(config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/%2e%2e/secret.txt")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(&body[..], b"spa-index");
    }
}
