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
use chrono::{NaiveTime, Timelike};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::broadcast,
    task::JoinHandle,
    time::{self, timeout},
};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};

use crate::{
    artifacts::ARTIFACT_MAX_SIZE_BYTES,
    artifacts::{ArtifactStore, is_valid_artifact_hash},
    auth::{AuthManager, AuthenticatedUser, HttpAuthMode, WebSocketAuth, bearer_token},
    automation::ErvPolicyCoordinator,
    config::{AppConfig, ThresholdsConfig},
    db,
    erv::{
        ERV_MANUAL_OVERRIDE_SECONDS, ErvFanSpeed, ErvSpeedWriter, ErvState, RustuyaErvSpeedWriter,
    },
    hvac::{
        HvacControlMode, HvacModeCommand, HvacModeWriter, HvacRuntimeSnapshot, HvacState,
        KumoHvacModeWriter,
    },
    mqtt,
    policy::{ErvPolicyState, HvacBandAction, HvacMode, get_hvac_band_action},
    presence::{MacOsPresenceReader, PresenceSnapshot},
    qingping::QingpingState,
    state::{OccupancyState, StateMachine, StateTransition},
    status::{Status, TemperatureBands},
    yolink::{self, YoLinkState},
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
    qingping: QingpingState,
    erv: ErvState,
    hvac: HvacState,
    erv_automation: ErvPolicyCoordinator,
    hvac_writer: Arc<dyn HvacModeWriter>,
}

pub fn app(config: AppConfig) -> Router {
    try_app(config).expect("failed to build HTTP app")
}

pub fn try_app(config: AppConfig) -> Result<Router> {
    try_app_with_qingping(config, QingpingState::default())
}

fn try_app_with_qingping(config: AppConfig, qingping: QingpingState) -> Result<Router> {
    let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
        &config.thresholds,
        unix_timestamp_now(),
    )));
    let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
    let erv_state = ErvState::new(config.runtime.database_path.clone());
    let hvac_state = HvacState::new(config.runtime.database_path.clone());
    try_app_with_state(
        config,
        qingping,
        state_machine,
        yolink,
        erv_state,
        hvac_state,
    )
}

fn try_app_with_state(
    config: AppConfig,
    qingping: QingpingState,
    state_machine: Arc<RwLock<StateMachine>>,
    yolink: YoLinkState,
    erv_state: ErvState,
    hvac_state: HvacState,
) -> Result<Router> {
    try_app_with_erv_writer(
        config,
        qingping,
        state_machine,
        yolink,
        erv_state,
        hvac_state,
        Arc::new(RustuyaErvSpeedWriter),
        Arc::new(KumoHvacModeWriter),
    )
}

fn try_app_with_erv_writer(
    config: AppConfig,
    qingping: QingpingState,
    state_machine: Arc<RwLock<StateMachine>>,
    yolink: YoLinkState,
    erv_state: ErvState,
    hvac_state: HvacState,
    erv_writer: Arc<dyn ErvSpeedWriter>,
    hvac_writer: Arc<dyn HvacModeWriter>,
) -> Result<Router> {
    let (router, _) = try_app_with_erv_writer_and_coordinator(
        config,
        qingping,
        state_machine,
        yolink,
        erv_state,
        hvac_state,
        erv_writer,
        hvac_writer,
    )?;
    Ok(router)
}

fn try_app_with_erv_writer_and_coordinator(
    config: AppConfig,
    qingping: QingpingState,
    state_machine: Arc<RwLock<StateMachine>>,
    yolink: YoLinkState,
    erv_state: ErvState,
    hvac_state: HvacState,
    erv_writer: Arc<dyn ErvSpeedWriter>,
    hvac_writer: Arc<dyn HvacModeWriter>,
) -> Result<(Router, AppState)> {
    let (state, _) = build_app_state(
        config,
        qingping,
        state_machine,
        yolink,
        erv_state,
        hvac_state,
        erv_writer,
        hvac_writer,
    )?;
    let router = router_from_state(state.clone());
    Ok((router, state))
}

fn build_app_state(
    config: AppConfig,
    qingping: QingpingState,
    state_machine: Arc<RwLock<StateMachine>>,
    yolink: YoLinkState,
    erv_state: ErvState,
    hvac_state: HvacState,
    erv_writer: Arc<dyn ErvSpeedWriter>,
    hvac_writer: Arc<dyn HvacModeWriter>,
) -> Result<(AppState, ErvPolicyCoordinator)> {
    db::migrate_database(&config.runtime.database_path)?;
    let auth = AuthManager::new(&config.orchestrator)?;
    let artifacts = ArtifactStore::new(
        config.runtime.artifacts_dir.clone(),
        config.runtime.legacy_apk_path.clone(),
    );
    let temperature_band_defaults = TemperatureBands::from_config(&config);
    let temperature_bands = load_hvac_temperature_bands(&config, temperature_band_defaults);
    let (status_broadcast, _) = broadcast::channel(32);
    yolink.set_status_broadcast(status_broadcast.clone());
    erv_state.set_status_broadcast(status_broadcast.clone());
    hvac_state.set_status_broadcast(status_broadcast.clone());
    yolink.restore_from_database(unix_timestamp_now())?;
    let erv_policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
    let erv_automation = ErvPolicyCoordinator::new(
        config.clone(),
        state_machine.clone(),
        qingping.clone(),
        erv_state.clone(),
        erv_policy,
        erv_writer,
        status_broadcast.clone(),
    );
    let state = AppState {
        config,
        auth,
        artifacts,
        temperature_bands: Arc::new(RwLock::new(temperature_bands)),
        temperature_band_defaults,
        state_machine,
        status_broadcast,
        qingping,
        erv: erv_state,
        hvac: hvac_state,
        erv_automation: erv_automation.clone(),
        hvac_writer,
    };

    Ok((state, erv_automation))
}

fn router_from_state(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    let frontend_dist = state.config.runtime.frontend_dist.clone();
    let assets_dir = frontend_dist.join("assets");

    Router::new()
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
        .with_state(state.clone())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

pub async fn serve(config: AppConfig) -> Result<()> {
    let bind_address = format!("{}:{}", config.orchestrator.host, config.orchestrator.port);
    let listener = TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("failed to bind HTTP listener at {bind_address}"))?;
    let qingping = QingpingState::default();
    let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
        &config.thresholds,
        unix_timestamp_now(),
    )));
    let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
    let erv_state = ErvState::new(config.runtime.database_path.clone());
    let hvac_state = HvacState::new(config.runtime.database_path.clone());
    let (app_state, erv_automation) = build_app_state(
        config.clone(),
        qingping.clone(),
        state_machine,
        yolink.clone(),
        erv_state.clone(),
        hvac_state.clone(),
        Arc::new(RustuyaErvSpeedWriter),
        Arc::new(KumoHvacModeWriter),
    )
    .context("failed to build HTTP app state")?;
    let app = router_from_state(app_state.clone());
    let runtime_handle = tokio::runtime::Handle::current();
    let qingping_policy_trigger: mqtt::SensorIngressHook = Arc::new({
        let erv_automation = erv_automation.clone();
        let app_state = app_state.clone();
        let runtime_handle = runtime_handle.clone();
        move || {
            let erv_automation = erv_automation.clone();
            let app_state = app_state.clone();
            runtime_handle.spawn(async move {
                evaluate_sensor_policies(erv_automation, app_state, false, "Qingping").await;
            });
        }
    });
    let _mqtt_runtime =
        mqtt::start_qingping_ingress(&config, qingping, Some(qingping_policy_trigger))
            .context("failed to start MQTT ingress")?;
    let yolink_hvac_trigger: yolink::DeviceIngressHook = Arc::new({
        let app_state = app_state.clone();
        let runtime_handle = runtime_handle.clone();
        move |transition| {
            let app_state = app_state.clone();
            runtime_handle.spawn(async move {
                evaluate_yolink_hvac_update(app_state, transition).await;
            });
        }
    });
    let _yolink_task = yolink::start_yolink_client(
        &config,
        yolink,
        Some(erv_automation.clone()),
        Some(yolink_hvac_trigger),
    );
    let _erv_task = crate::erv::start_erv_status_poll(&config, erv_state);
    let _hvac_task = crate::hvac::start_hvac_status_poll(&config, hvac_state);
    let _presence_task = start_presence_poll(app_state);

    tracing::info!("office-automate-server listening on {}", bind_address);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("HTTP server failed")
}

fn start_presence_poll(state: AppState) -> Option<JoinHandle<()>> {
    if !state.config.presence.enabled {
        tracing::info!("internal macOS presence polling disabled");
        return None;
    }

    let poll_interval = Duration::from_secs(state.config.presence.poll_interval_seconds.max(1));
    let reader = MacOsPresenceReader::from_config(&state.config.presence);
    Some(tokio::spawn(async move {
        let mut interval = time::interval(poll_interval);
        loop {
            interval.tick().await;
            match reader.read_snapshot().await {
                Ok(snapshot) => {
                    if let Err(error) = apply_presence_snapshot(&state, snapshot).await {
                        tracing::warn!("internal presence update failed: {error:#}");
                    }
                }
                Err(error) => {
                    tracing::warn!("internal presence signal read failed: {error:#}");
                }
            }
        }
    }))
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

async fn evaluate_sensor_policies(
    erv_automation: ErvPolicyCoordinator,
    state: AppState,
    bypass_dwell: bool,
    source: &'static str,
) {
    if let Err(error) = erv_automation.evaluate_erv_policy(bypass_dwell).await {
        tracing::warn!("ERV automated policy apply failed after {source} update: {error:#}");
    }
    if let Err(error) = evaluate_and_apply_hvac_policy(&state).await {
        tracing::warn!("HVAC automated policy apply failed after {source} update: {error:#}");
    }
    erv_automation.broadcast_status();
}

async fn evaluate_yolink_hvac_update(state: AppState, transition: Option<StateTransition>) {
    let cleared_override = clear_hvac_manual_override_on_transition(&state, transition);
    if let Err(error) = evaluate_and_apply_hvac_policy(&state).await {
        tracing::warn!("HVAC automated policy apply failed after YoLink update: {error:#}");
    }
    if cleared_override {
        broadcast_status(&state);
    }
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
    match apply_occupancy_signal(
        &state,
        payload.last_active_timestamp,
        payload.external_monitor,
        "mac_activity",
    )
    .await
    {
        Ok((state_name, erv_should_run)) => {
            Json(json!({"ok": true, "state": state_name, "erv_should_run": erv_should_run}))
                .into_response()
        }
        Err(error) => {
            tracing::error!("failed to apply occupancy update: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": "Failed to persist occupancy update"})),
            )
                .into_response()
        }
    }
}

async fn apply_presence_snapshot(state: &AppState, snapshot: PresenceSnapshot) -> Result<()> {
    apply_occupancy_signal(
        state,
        snapshot.last_active_timestamp,
        snapshot.external_monitor,
        "internal_presence",
    )
    .await?;
    Ok(())
}

async fn apply_occupancy_signal(
    state: &AppState,
    last_active_timestamp: f64,
    external_monitor: bool,
    trigger: &str,
) -> Result<(String, bool)> {
    let now = unix_timestamp_now();
    let applied_transition = Arc::new(std::sync::Mutex::new(None));
    let applied_transition_for_update = Arc::clone(&applied_transition);
    let policy_result = state
        .erv_automation
        .update_state_and_maybe_evaluate(|| {
            let (transition, state_name, erv_should_run) = {
                let mut machine = state
                    .state_machine
                    .write()
                    .expect("state machine lock poisoned");
                let transition =
                    machine.update_mac_occupancy(last_active_timestamp, external_monitor, now);
                let status = machine.status_at(now);
                (transition, status.state, status.erv_should_run)
            };
            *applied_transition_for_update
                .lock()
                .expect("occupancy transition lock poisoned") = transition;
            log_state_transition(state, transition, trigger)
                .context("failed to persist occupancy update")?;
            Ok((
                (transition, state_name, erv_should_run),
                transition,
                now,
                transition.is_some(),
                true,
            ))
        })
        .await;

    let (transition, state_name, erv_should_run) = match policy_result {
        Ok(result) => result,
        Err(error)
            if error
                .to_string()
                .contains("failed to persist occupancy update") =>
        {
            return Err(error);
        }
        Err(error) => {
            tracing::warn!("ERV automated policy apply failed after occupancy update: {error:#}");
            let status = state
                .state_machine
                .read()
                .expect("state machine lock poisoned")
                .status_at(now);
            let transition = *applied_transition
                .lock()
                .expect("occupancy transition lock poisoned");
            (transition, status.state, status.erv_should_run)
        }
    };
    clear_hvac_manual_override_on_transition(state, transition);
    if let Err(error) = evaluate_and_apply_hvac_policy(state).await {
        tracing::warn!("HVAC automated policy apply failed after occupancy update: {error:#}");
    }
    broadcast_status(state);
    Ok((state_name, erv_should_run))
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
            let applied_transition = Arc::new(std::sync::Mutex::new(None));
            let applied_transition_for_update = Arc::clone(&applied_transition);
            let policy_result = state
                .erv_automation
                .update_state_and_maybe_evaluate(|| {
                    let (transition, state_name, is_present) = {
                        let mut machine = state
                            .state_machine
                            .write()
                            .expect("state machine lock poisoned");
                        let transition = machine.set_manual_presence(present, now);
                        let status = machine.status_at(now);
                        (transition, status.state, status.is_present)
                    };
                    *applied_transition_for_update
                        .lock()
                        .expect("presence transition lock poisoned") = transition;
                    db::log_device_event(
                        &state.config.runtime.database_path,
                        "presence",
                        &format!("manual_{requested_state}"),
                        Some("Dashboard"),
                        None,
                    )
                    .and_then(|_| log_state_transition(&state, transition, "manual"))
                    .context("failed to persist presence update")?;
                    Ok((
                        (transition, state_name, is_present),
                        transition,
                        now,
                        transition.is_some(),
                        true,
                    ))
                })
                .await;

            let (transition, state_name, is_present) = match policy_result {
                Ok(result) => result,
                Err(error)
                    if error
                        .to_string()
                        .contains("failed to persist presence update") =>
                {
                    tracing::error!("failed to persist manual presence update: {error:#}");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"ok": false, "error": "Failed to persist presence update"})),
                    )
                        .into_response();
                }
                Err(error) => {
                    tracing::warn!(
                        "ERV automated policy apply failed after presence update: {error:#}"
                    );
                    let status = state
                        .state_machine
                        .read()
                        .expect("state machine lock poisoned")
                        .status_at(now);
                    let transition = *applied_transition
                        .lock()
                        .expect("presence transition lock poisoned");
                    (transition, status.state, status.is_present)
                }
            };
            clear_hvac_manual_override_on_transition(&state, transition);
            if let Err(error) = evaluate_and_apply_hvac_policy(&state).await {
                tracing::warn!(
                    "HVAC automated policy apply failed after presence update: {error:#}"
                );
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

async fn erv(State(state): State<AppState>, Json(payload): Json<ErvRequest>) -> Response {
    let speed = payload.speed.to_ascii_lowercase();
    let Some(speed) = ErvFanSpeed::parse(&speed) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Invalid ERV speed"})),
        )
            .into_response();
    };

    match state
        .erv_automation
        .apply_manual_erv_speed(speed, state.erv_automation.latest_co2_ppm())
        .await
    {
        Ok(status) => {
            broadcast_status(&state);
            Json(json!({
                "ok": true,
                "erv": {
                    "speed": status.fan_speed.map(ErvFanSpeed::as_str).unwrap_or("unknown"),
                    "running": status.power,
                    "manual_override": true,
                    "expires_in": ERV_MANUAL_OVERRIDE_SECONDS,
                }
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn evaluate_and_apply_hvac_policy(state: &AppState) -> Result<()> {
    if !state.config.mitsubishi.active_control_enabled {
        return Ok(());
    }

    let now = unix_timestamp_now();
    let state_status = {
        let machine = state
            .state_machine
            .read()
            .expect("state machine lock poisoned");
        machine.status_at(now)
    };
    let hvac_snapshot = state.hvac.snapshot();
    let temp_f = state
        .qingping
        .latest()
        .and_then(|reading| reading.temp_c)
        .map(celsius_to_fahrenheit);
    let bands = active_temperature_bands(state);
    let hvac_mode = hvac_mode_from_str(&hvac_snapshot.mode).unwrap_or(HvacMode::Off);
    let critical_heat_needed = !state_status.is_present
        && temp_f
            .is_some_and(|temp_f| temp_f < state.config.thresholds.hvac_critical_temp_f as f64)
        && hvac_mode != HvacMode::Heat;

    if !state_status.safety_interlock
        && !critical_heat_needed
        && state.hvac.manual_override_active()
    {
        return Ok(());
    }

    if state_status.safety_interlock {
        let verified_status = state
            .hvac
            .smoke_status_with(&state.config.mitsubishi, state.hvac_writer.as_ref())
            .await?;
        if let Some(previous_mode) = active_hvac_control_mode(&verified_status.mode) {
            let previous_heat_setpoint_c = verified_status.heat_setpoint_c;
            let previous_cool_setpoint_c = verified_status.cool_setpoint_c;
            apply_hvac_mode_after_verified_status(
                state,
                HvacControlMode::Off,
                None,
                "safety_interlock",
            )
            .await?;
            state.hvac.set_suspended_with_setpoints(
                true,
                Some(previous_mode),
                previous_heat_setpoint_c,
                previous_cool_setpoint_c,
            );
            state.hvac.set_temp_band_mode(None);
            state.hvac.clear_manual_override();
            broadcast_status(state);
        }
        return Ok(());
    }

    if critical_heat_needed {
        let temp_label = temp_f.expect("critical temperature checked").round() as i64;
        let setpoint_c = hvac_snapshot.heat_setpoint_c.unwrap_or(22.0);
        apply_hvac_mode(
            state,
            HvacControlMode::Heat,
            Some(setpoint_c),
            &format!("critical_temp_{temp_label}F"),
        )
        .await?;
        state.hvac.set_suspended(false, None);
        state.hvac.set_temp_band_mode(None);
        state.hvac.clear_manual_override();
        broadcast_status(state);
        return Ok(());
    }

    let temp_band_mode = hvac_snapshot
        .temp_band_mode
        .as_deref()
        .and_then(hvac_mode_from_str);
    let erv_snapshot = state.erv.snapshot();
    let within_occupancy_hours = is_within_occupancy_hours(&state.config.thresholds);
    if !state_status.is_present
        && erv_snapshot.running
        && !hvac_snapshot.suspended
        && temp_f.is_some_and(|temp_f| temp_f > state.config.thresholds.hvac_min_temp_f as f64)
    {
        let verified_status = state
            .hvac
            .smoke_status_with(&state.config.mitsubishi, state.hvac_writer.as_ref())
            .await?;
        if let Some(previous_mode) = active_hvac_control_mode(&verified_status.mode) {
            let previous_heat_setpoint_c = verified_status.heat_setpoint_c;
            let previous_cool_setpoint_c = verified_status.cool_setpoint_c;
            let temp_label = temp_f.expect("ERV suspension temperature checked").round() as i64;
            apply_hvac_mode_after_verified_status(
                state,
                HvacControlMode::Off,
                None,
                &format!("erv_running_temp_{temp_label}F"),
            )
            .await?;
            state.hvac.set_suspended_with_setpoints(
                true,
                Some(previous_mode),
                previous_heat_setpoint_c,
                previous_cool_setpoint_c,
            );
            state.hvac.set_temp_band_mode(None);
            broadcast_status(state);
        }
        return Ok(());
    }

    let action = get_hvac_band_action(
        temp_f,
        hvac_mode,
        temp_band_mode,
        if state_status.is_present {
            OccupancyState::Present
        } else {
            OccupancyState::Away
        },
        erv_snapshot.running,
        state.config.thresholds.hvac_min_temp_f as f64,
        within_occupancy_hours,
        bands.heat_off_temp_f as f64,
        bands.heat_on_temp_f as f64,
        bands.cool_on_temp_f as f64,
        bands.cool_off_temp_f as f64,
    );

    let temp_label = temp_f.unwrap_or_default().round() as i64;
    match action {
        None => {
            if let Some(restore_mode) = suspended_restore_mode(
                &hvac_snapshot,
                temp_f,
                bands,
                state_status.is_present,
                erv_snapshot.running,
                within_occupancy_hours,
            ) {
                let command = match restore_mode {
                    HvacControlMode::Heat => HvacModeCommand::new(
                        HvacControlMode::Heat,
                        Some(
                            hvac_snapshot
                                .suspended_heat_setpoint_c
                                .or(hvac_snapshot.heat_setpoint_c)
                                .unwrap_or(22.0),
                        ),
                    ),
                    HvacControlMode::Cool => HvacModeCommand::new(
                        HvacControlMode::Cool,
                        Some(
                            hvac_snapshot
                                .suspended_cool_setpoint_c
                                .or(hvac_snapshot.cool_setpoint_c)
                                .unwrap_or_else(|| {
                                    fahrenheit_to_celsius(
                                        state.config.thresholds.hvac_cool_setpoint_f as f64,
                                    )
                                }),
                        ),
                    ),
                    HvacControlMode::Auto => HvacModeCommand::auto(
                        hvac_snapshot
                            .suspended_heat_setpoint_c
                            .or(hvac_snapshot.heat_setpoint_c)
                            .unwrap_or(22.0),
                        hvac_snapshot
                            .suspended_cool_setpoint_c
                            .or(hvac_snapshot.cool_setpoint_c)
                            .unwrap_or_else(|| {
                                fahrenheit_to_celsius(
                                    state.config.thresholds.hvac_cool_setpoint_f as f64,
                                )
                            }),
                    ),
                    HvacControlMode::Off => unreachable!("restore mode is never off"),
                };
                let reason = if state_status.is_present {
                    "present_restore"
                } else {
                    "erv_stopped_occupancy_hours"
                };
                apply_hvac_mode_command(state, command, reason).await?;
                state.hvac.set_suspended(false, None);
                state.hvac.set_temp_band_mode(None);
                broadcast_status(state);
            }
            return Ok(());
        }
        Some(HvacBandAction::PauseHeat) => {
            apply_hvac_mode(
                state,
                HvacControlMode::Off,
                None,
                &format!("heat_band_pause_{temp_label}F"),
            )
            .await?;
            state.hvac.set_temp_band_mode(Some(HvacControlMode::Heat));
        }
        Some(HvacBandAction::ResumeHeat) => {
            let setpoint_c = hvac_snapshot.heat_setpoint_c.unwrap_or(22.0);
            apply_hvac_mode(
                state,
                HvacControlMode::Heat,
                Some(setpoint_c),
                &format!("heat_band_resume_{temp_label}F"),
            )
            .await?;
            state.hvac.set_suspended(false, None);
            state.hvac.set_temp_band_mode(None);
        }
        Some(HvacBandAction::StopCool) => {
            apply_hvac_mode(
                state,
                HvacControlMode::Off,
                None,
                &format!("cool_band_stop_{temp_label}F"),
            )
            .await?;
            state.hvac.set_temp_band_mode(Some(HvacControlMode::Cool));
        }
        Some(HvacBandAction::StartCool) => {
            let setpoint_c = hvac_snapshot.cool_setpoint_c.unwrap_or_else(|| {
                fahrenheit_to_celsius(state.config.thresholds.hvac_cool_setpoint_f as f64)
            });
            apply_hvac_mode(
                state,
                HvacControlMode::Cool,
                Some(setpoint_c),
                &format!("cool_band_start_{temp_label}F"),
            )
            .await?;
            state.hvac.set_suspended(false, None);
            state.hvac.set_temp_band_mode(None);
        }
    }

    broadcast_status(state);
    Ok(())
}

fn clear_hvac_manual_override_on_transition(
    state: &AppState,
    transition: Option<StateTransition>,
) -> bool {
    if transition.is_some() {
        return state.hvac.clear_manual_override();
    }
    false
}

async fn apply_hvac_mode(
    state: &AppState,
    mode: HvacControlMode,
    setpoint_c: Option<f64>,
    reason: &str,
) -> Result<crate::hvac::HvacDeviceStatus> {
    state
        .hvac
        .set_mode_with(
            &state.config.mitsubishi,
            state.hvac_writer.as_ref(),
            mode,
            setpoint_c,
            reason,
        )
        .await
}

async fn apply_hvac_mode_command(
    state: &AppState,
    command: HvacModeCommand,
    reason: &str,
) -> Result<crate::hvac::HvacDeviceStatus> {
    state
        .hvac
        .set_mode_command_with(
            &state.config.mitsubishi,
            state.hvac_writer.as_ref(),
            command,
            reason,
        )
        .await
}

async fn apply_hvac_mode_after_verified_status(
    state: &AppState,
    mode: HvacControlMode,
    setpoint_c: Option<f64>,
    reason: &str,
) -> Result<crate::hvac::HvacDeviceStatus> {
    state
        .hvac
        .set_mode_after_verified_status_with(
            &state.config.mitsubishi,
            state.hvac_writer.as_ref(),
            mode,
            setpoint_c,
            reason,
        )
        .await
}

fn active_hvac_control_mode(mode: &str) -> Option<String> {
    match HvacControlMode::parse(mode) {
        Some(HvacControlMode::Heat | HvacControlMode::Cool | HvacControlMode::Auto) => {
            Some(mode.to_string())
        }
        Some(HvacControlMode::Off) | None => None,
    }
}

fn fahrenheit_to_celsius(value: f64) -> f64 {
    (value - 32.0) * 5.0 / 9.0
}

fn celsius_to_fahrenheit(value: f64) -> f64 {
    value * 9.0 / 5.0 + 32.0
}

fn round_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn hvac_mode_from_str(value: &str) -> Option<HvacMode> {
    match value {
        "off" => Some(HvacMode::Off),
        "heat" => Some(HvacMode::Heat),
        "cool" => Some(HvacMode::Cool),
        "auto" => Some(HvacMode::Auto),
        _ => None,
    }
}

fn is_within_occupancy_hours(thresholds: &ThresholdsConfig) -> bool {
    is_within_occupancy_hours_at(thresholds, chrono::Local::now().time())
}

fn is_within_occupancy_hours_at(thresholds: &ThresholdsConfig, now: NaiveTime) -> bool {
    let start = NaiveTime::parse_from_str(&thresholds.expected_occupancy_start, "%H:%M");
    let end = NaiveTime::parse_from_str(&thresholds.expected_occupancy_end, "%H:%M");
    match (start, end) {
        (Ok(start), Ok(end)) => start <= now && now <= end,
        _ => {
            tracing::warn!("Invalid occupancy hours config, defaulting to 7AM-10PM");
            (7..22).contains(&now.hour())
        }
    }
}

fn suspended_restore_mode(
    snapshot: &HvacRuntimeSnapshot,
    temp_f: Option<f64>,
    bands: TemperatureBands,
    is_present: bool,
    erv_running: bool,
    within_occupancy_hours: bool,
) -> Option<HvacControlMode> {
    if !snapshot.suspended || (!is_present && (erv_running || !within_occupancy_hours)) {
        return None;
    }

    let mode = snapshot
        .last_mode
        .as_deref()
        .and_then(HvacControlMode::parse)?;
    if should_restore_hvac_mode(mode, temp_f, bands) {
        Some(mode)
    } else {
        None
    }
}

fn should_restore_hvac_mode(
    mode: HvacControlMode,
    temp_f: Option<f64>,
    bands: TemperatureBands,
) -> bool {
    let Some(temp_f) = temp_f else {
        return true;
    };

    match mode {
        HvacControlMode::Heat => temp_f < bands.heat_off_temp_f as f64,
        HvacControlMode::Cool => temp_f > bands.cool_off_temp_f as f64,
        HvacControlMode::Auto => true,
        HvacControlMode::Off => false,
    }
}

#[derive(Debug, Deserialize)]
struct HvacRequest {
    mode: String,
    setpoint_f: Option<f64>,
}

async fn hvac(State(state): State<AppState>, Json(payload): Json<HvacRequest>) -> Response {
    let mode_name = payload.mode.to_ascii_lowercase();
    let Some(mode) = HvacControlMode::parse(&mode_name) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Invalid HVAC mode"})),
        )
            .into_response();
    };
    if matches!(mode, HvacControlMode::Auto) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Invalid HVAC mode"})),
        )
            .into_response();
    }

    let setpoint_f = payload.setpoint_f.unwrap_or(70.0);
    let setpoint_c = fahrenheit_to_celsius(setpoint_f);

    match apply_hvac_mode(&state, mode, Some(setpoint_c), "manual_override").await {
        Ok(status) => {
            state.hvac.record_manual_override(mode, setpoint_f);
            broadcast_status(&state);
            Json(json!({
                "ok": true,
                "hvac": {
                    "mode": mode.as_str(),
                    "setpoint_f": setpoint_f,
                    "setpoint_c": round_tenth(status.setpoint_c),
                    "manual_override": true,
                    "expires_in": crate::hvac::HVAC_MANUAL_OVERRIDE_SECONDS,
                }
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
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
    state.qingping.overlay_status(&mut status);
    state.erv.overlay_status(&mut status);
    state.hvac.overlay_status(&mut status);
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
    days: Option<i64>,
    limit: Option<i64>,
    bucket_minutes: Option<i64>,
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

async fn history_sessions(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = clamp(query.days, 7, 1, 30);
    match db::read_office_sessions(&state.config.runtime.database_path, days) {
        Ok(payload) => Json(json!({"ok": true, "days": days, "sessions": payload["sessions"], "summary": payload["summary"]})).into_response(),
        Err(error) => history_error("history/sessions", error),
    }
}

async fn history_co2_ohlc(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let hours = clamp(query.hours, 24, 1, 168);
    let bucket_minutes = query
        .bucket_minutes
        .unwrap_or(default_co2_bucket(hours))
        .max(1);
    match db::read_co2_ohlc(&state.config.runtime.database_path, hours, bucket_minutes) {
        Ok(payload) => Json(json!({"ok": true, "hours": hours, "bucket_minutes": payload["bucket_minutes"], "candles": payload["candles"]})).into_response(),
        Err(error) => history_error("history/co2-ohlc", error),
    }
}

async fn history_temperature(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let hours = clamp(query.hours, 24, 1, 168);
    let bucket_minutes = query
        .bucket_minutes
        .unwrap_or(default_temperature_bucket(hours))
        .max(1);
    match db::read_temperature_history(&state.config.runtime.database_path, hours, bucket_minutes) {
        Ok(payload) => Json(json!({"ok": true, "hours": hours, "bucket_minutes": payload["bucket_minutes"], "points": payload["points"]})).into_response(),
        Err(error) => history_error("history/temperature", error),
    }
}

async fn history_daily_stats(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = clamp(query.days, 7, 1, 30);
    match db::read_daily_stats(&state.config.runtime.database_path, days) {
        Ok(stats) => Json(json!({"ok": true, "days": days, "stats": stats})).into_response(),
        Err(error) => history_error("history/daily-stats", error),
    }
}

async fn history_openings(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = clamp(query.days, 7, 1, 30);
    match db::read_openings(&state.config.runtime.database_path, days) {
        Ok(days) => Json(json!({"ok": true, "days": days})).into_response(),
        Err(error) => history_error("history/openings", error),
    }
}

async fn history_orchestration(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = clamp(query.days, 7, 1, 30);
    match db::read_orchestration_activity(&state.config.runtime.database_path, days) {
        Ok(days) => Json(json!({"ok": true, "days": days})).into_response(),
        Err(error) => history_error("history/orchestration", error),
    }
}

async fn history_project_focus(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = clamp(query.days, 7, 1, 30);
    match db::read_project_focus(&state.config.runtime.database_path, days) {
        Ok(days) => Json(json!({"ok": true, "days": days})).into_response(),
        Err(error) => history_error("history/project-focus", error),
    }
}

async fn history_leverage(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = clamp(query.days, 7, 1, 30);
    match db::read_leverage_history_with_telemetry(
        &state.config.runtime.database_path,
        &state.config.runtime.telemetry_db_path,
        days,
    ) {
        Ok(payload) => Json(json!({"ok": true, "days": payload["days"], "week": payload["week"]}))
            .into_response(),
        Err(error) => history_error("history/leverage", error),
    }
}

async fn history_project_leverage(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = clamp(query.days, 7, 1, 30);
    match db::read_project_leverage(&state.config.runtime.database_path, days) {
        Ok(payload) => Json(json!({"ok": true, "projects": payload["projects"]})).into_response(),
        Err(error) => history_error("history/project-leverage", error),
    }
}

fn default_co2_bucket(hours: i64) -> i64 {
    if hours <= 1 {
        5
    } else if hours <= 6 {
        15
    } else if hours <= 24 {
        60
    } else {
        240
    }
}

fn default_temperature_bucket(hours: i64) -> i64 {
    if hours <= 1 {
        5
    } else if hours <= 6 {
        15
    } else if hours <= 24 {
        30
    } else {
        120
    }
}

fn history_error(route: &str, error: anyhow::Error) -> Response {
    tracing::error!("failed to read {route}: {error:#}");
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": error.to_string()})),
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
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

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
    use crate::{
        config::{
            ErvConfig, GoogleOAuthConfig, MitsubishiConfig, OrchestratorConfig, QingpingConfig,
            RuntimeConfig, ThresholdsConfig, YoLinkConfig,
        },
        erv::{BoxFutureResult as ErvBoxFutureResult, ErvDeviceStatus, parse_erv_status_payload},
        hvac::{HvacDeviceStatus, parse_kumo_adapter_status},
    };

    #[derive(Default)]
    struct FakeErvWriter {
        smoke_results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
        write_results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
        smoke_calls: AtomicUsize,
        write_speeds: Mutex<Vec<ErvFanSpeed>>,
    }

    impl FakeErvWriter {
        fn new(
            smoke_results: Vec<Result<ErvDeviceStatus>>,
            write_results: Vec<Result<ErvDeviceStatus>>,
        ) -> Self {
            Self {
                smoke_results: Mutex::new(smoke_results.into()),
                write_results: Mutex::new(write_results.into()),
                smoke_calls: AtomicUsize::new(0),
                write_speeds: Mutex::new(Vec::new()),
            }
        }

        fn smoke_calls(&self) -> usize {
            self.smoke_calls.load(Ordering::SeqCst)
        }

        fn write_speeds(&self) -> Vec<ErvFanSpeed> {
            self.write_speeds
                .lock()
                .expect("fake writer speeds lock")
                .clone()
        }
    }

    impl ErvSpeedWriter for FakeErvWriter {
        fn smoke_status<'a>(
            &'a self,
            _config: &'a ErvConfig,
        ) -> ErvBoxFutureResult<'a, ErvDeviceStatus> {
            self.smoke_calls.fetch_add(1, Ordering::SeqCst);
            let result = self
                .smoke_results
                .lock()
                .expect("fake writer smoke lock")
                .pop_front()
                .unwrap_or_else(|| anyhow::bail!("no fake ERV smoke result configured"));
            Box::pin(async move { result })
        }

        fn set_speed<'a>(
            &'a self,
            _config: &'a ErvConfig,
            speed: ErvFanSpeed,
        ) -> ErvBoxFutureResult<'a, ErvDeviceStatus> {
            self.write_speeds
                .lock()
                .expect("fake writer speeds lock")
                .push(speed);
            let result = self
                .write_results
                .lock()
                .expect("fake writer write lock")
                .pop_front()
                .unwrap_or_else(|| anyhow::bail!("no fake ERV write result configured"));
            Box::pin(async move { result })
        }
    }

    #[derive(Default)]
    struct FakeHvacWriter {
        smoke_results: Mutex<VecDeque<Result<HvacDeviceStatus>>>,
        write_results: Mutex<VecDeque<Result<HvacDeviceStatus>>>,
        smoke_calls: AtomicUsize,
        write_commands: Mutex<Vec<HvacModeCommand>>,
    }

    impl FakeHvacWriter {
        fn new(
            smoke_results: Vec<Result<HvacDeviceStatus>>,
            write_results: Vec<Result<HvacDeviceStatus>>,
        ) -> Self {
            Self {
                smoke_results: Mutex::new(smoke_results.into()),
                write_results: Mutex::new(write_results.into()),
                smoke_calls: AtomicUsize::new(0),
                write_commands: Mutex::new(Vec::new()),
            }
        }

        fn smoke_calls(&self) -> usize {
            self.smoke_calls.load(Ordering::SeqCst)
        }

        fn write_modes(&self) -> Vec<(HvacControlMode, Option<f64>)> {
            self.write_commands
                .lock()
                .expect("fake HVAC commands lock")
                .iter()
                .map(|command| (command.mode, command.setpoint_c))
                .collect()
        }

        fn write_commands(&self) -> Vec<HvacModeCommand> {
            self.write_commands
                .lock()
                .expect("fake HVAC commands lock")
                .clone()
        }
    }

    impl HvacModeWriter for FakeHvacWriter {
        fn smoke_status<'a>(
            &'a self,
            _config: &'a MitsubishiConfig,
        ) -> crate::hvac::BoxFutureResult<'a, HvacDeviceStatus> {
            self.smoke_calls.fetch_add(1, Ordering::SeqCst);
            let result = self
                .smoke_results
                .lock()
                .expect("fake HVAC smoke lock")
                .pop_front()
                .unwrap_or_else(|| anyhow::bail!("no fake HVAC smoke result configured"));
            Box::pin(async move { result })
        }

        fn set_mode<'a>(
            &'a self,
            _config: &'a MitsubishiConfig,
            mode: HvacControlMode,
            setpoint_c: Option<f64>,
        ) -> crate::hvac::BoxFutureResult<'a, HvacDeviceStatus> {
            self.set_mode_command(_config, HvacModeCommand::new(mode, setpoint_c))
        }

        fn set_mode_command<'a>(
            &'a self,
            _config: &'a MitsubishiConfig,
            command: HvacModeCommand,
        ) -> crate::hvac::BoxFutureResult<'a, HvacDeviceStatus> {
            self.write_commands
                .lock()
                .expect("fake HVAC commands lock")
                .push(command);
            let result = self
                .write_results
                .lock()
                .expect("fake HVAC write lock")
                .pop_front()
                .unwrap_or_else(|| anyhow::bail!("no fake HVAC write result configured"));
            Box::pin(async move { result })
        }
    }

    fn test_config() -> AppConfig {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.keep();
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            presence: crate::config::PresenceConfig::default(),
            qingping: QingpingConfig::default(),
            yolink: YoLinkConfig::default(),
            erv: ErvConfig::default(),
            mitsubishi: MitsubishiConfig::default(),
            thresholds: ThresholdsConfig::default(),
            telemetry: crate::config::TelemetryConfig::default(),
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
                telemetry_db_path: root.join("data/telemetry.db"),
                tool_usage_db_path: root.join("data/tool_usage.db"),
                engram_db_path: root.join("data/engram_state.db"),
                engram_registry_path: root.join("data/engram_concept_registry.md"),
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

    fn configured_erv_config(active_control_enabled: bool) -> AppConfig {
        let mut config = test_config();
        config.erv = ErvConfig {
            device_type: "tuya".to_string(),
            ip: "192.0.2.10".to_string(),
            device_id: "device-id".to_string(),
            local_key: "local-key".to_string(),
            active_control_enabled,
            verify_delay_seconds: 0,
            ..ErvConfig::default()
        };
        config
    }

    fn configured_hvac_config(active_control_enabled: bool) -> AppConfig {
        let mut config = test_config();
        config.mitsubishi = MitsubishiConfig {
            device_type: "kumo".to_string(),
            username: Some("kumo-user".to_string()),
            password: Some("kumo-pass".to_string()),
            device_serial: Some("kumo-serial".to_string()),
            active_control_enabled,
            ..MitsubishiConfig::default()
        };
        config
    }

    fn set_occupancy_window_excluding_now(config: &mut AppConfig) {
        let current_hour = chrono::Timelike::hour(&chrono::Local::now());
        let (start, end) = if current_hour < 22 {
            ("23:00", "23:30")
        } else {
            ("00:00", "01:00")
        };
        config.thresholds.expected_occupancy_start = start.to_string();
        config.thresholds.expected_occupancy_end = end.to_string();
    }

    fn qingping_with_co2(co2_ppm: i64) -> QingpingState {
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(co2_ppm));
        qingping
    }

    fn qingping_with_temp(temp_c: f64) -> QingpingState {
        let qingping = QingpingState::default();
        qingping.apply_reading(crate::qingping::QingpingReading {
            temp_c: Some(temp_c),
            ..qingping_reading(500)
        });
        qingping
    }

    fn qingping_reading(co2_ppm: i64) -> crate::qingping::QingpingReading {
        crate::qingping::QingpingReading {
            device_name: "Qingping Air Monitor".to_string(),
            mac_hint: "AABBCCDDEEFF".to_string(),
            temp_c: Some(22.0),
            humidity: Some(45.0),
            co2_ppm: Some(co2_ppm),
            pm25: Some(2),
            pm10: Some(3),
            tvoc: Some(25),
            noise_db: Some(35),
            timestamp: "2026-06-05T12:30:00".to_string(),
            raw_data: "{}".to_string(),
        }
    }

    fn hvac_status(mode: HvacControlMode, setpoint_c: f64) -> HvacDeviceStatus {
        match mode {
            HvacControlMode::Off => parse_kumo_adapter_status(&json!({
                "power": 0,
                "operationMode": "off",
                "spHeat": 22.0,
                "spCool": 25.5
            }))
            .expect("HVAC off status"),
            HvacControlMode::Heat => parse_kumo_adapter_status(&json!({
                "power": 1,
                "operationMode": "heat",
                "spHeat": setpoint_c,
                "spCool": 25.5
            }))
            .expect("HVAC heat status"),
            HvacControlMode::Cool => parse_kumo_adapter_status(&json!({
                "power": 1,
                "operationMode": "cool",
                "spHeat": 22.0,
                "spCool": setpoint_c
            }))
            .expect("HVAC cool status"),
            HvacControlMode::Auto => parse_kumo_adapter_status(&json!({
                "power": 1,
                "operationMode": "auto",
                "spHeat": 22.0,
                "spCool": 25.5
            }))
            .expect("HVAC auto status"),
        }
    }

    fn hvac_auto_status(heat_setpoint_c: f64, cool_setpoint_c: f64) -> HvacDeviceStatus {
        parse_kumo_adapter_status(&json!({
            "power": 1,
            "operationMode": "auto",
            "spHeat": heat_setpoint_c,
            "spCool": cool_setpoint_c
        }))
        .expect("HVAC auto status")
    }

    fn office_door_device() -> yolink::YoLinkDevice {
        yolink::YoLinkDevice {
            device_id: "door-1".to_string(),
            name: "Office Door".to_string(),
            token: "door-token".to_string(),
            device_type: yolink::DeviceType::DoorSensor,
            state: json!({"state": "closed", "online": true})
                .as_object()
                .expect("object")
                .clone(),
        }
    }

    fn office_motion_device() -> yolink::YoLinkDevice {
        yolink::YoLinkDevice {
            device_id: "motion-1".to_string(),
            name: "Office Motion".to_string(),
            token: "motion-token".to_string(),
            device_type: yolink::DeviceType::MotionSensor,
            state: json!({"state": "normal", "online": true})
                .as_object()
                .expect("object")
                .clone(),
        }
    }

    fn app_with_erv_writer(
        config: AppConfig,
        qingping: QingpingState,
        writer: Arc<dyn ErvSpeedWriter>,
    ) -> Router {
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        try_app_with_erv_writer(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            HvacState::default(),
            writer,
            Arc::new(FakeHvacWriter::default()),
        )
        .expect("app")
    }

    fn app_with_hvac_writer(config: AppConfig, writer: Arc<dyn HvacModeWriter>) -> Router {
        let qingping = QingpingState::default();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        try_app_with_erv_writer(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer,
        )
        .expect("app")
    }

    fn app_with_hvac_state(config: AppConfig, hvac_state: HvacState) -> Router {
        let qingping = QingpingState::default();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        try_app_with_erv_writer(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            Arc::new(FakeHvacWriter::default()),
        )
        .expect("app")
    }

    fn erv_status(speed: ErvFanSpeed) -> ErvDeviceStatus {
        let payload = match speed {
            ErvFanSpeed::Off => r#"{"dps":{"1":false,"101":1,"102":1}}"#,
            ErvFanSpeed::Quiet => r#"{"dps":{"1":true,"101":1,"102":1}}"#,
            ErvFanSpeed::Medium => r#"{"dps":{"1":true,"101":3,"102":2}}"#,
            ErvFanSpeed::Turbo => r#"{"dps":{"1":true,"101":8,"102":8}}"#,
        };
        parse_erv_status_payload(payload).expect("ERV status")
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
    async fn status_route_reflects_latest_qingping_reading() {
        let qingping = QingpingState::default();
        qingping.apply_reading(crate::qingping::QingpingReading {
            device_name: "Qingping Air Monitor".to_string(),
            mac_hint: "AABBCCDDEEFF".to_string(),
            temp_c: Some(22.5),
            humidity: Some(45.0),
            co2_ppm: Some(640),
            pm25: Some(3),
            pm10: Some(4),
            tvoc: Some(25),
            noise_db: Some(37),
            timestamp: "2026-06-05T12:30:00".to_string(),
            raw_data: "{}".to_string(),
        });

        let response = try_app_with_qingping(test_config(), qingping)
            .expect("app")
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

        assert_eq!(value["sensors"]["co2_ppm"], 640);
        assert_eq!(value["air_quality"]["co2_ppm"], 640);
        assert_eq!(value["air_quality"]["temp_c"], 22.5);
        assert_eq!(value["air_quality"]["humidity"], 45.0);
        assert_eq!(value["air_quality"]["pm25"], 3.0);
        assert_eq!(value["air_quality"]["tvoc"], 25);
        assert_eq!(value["air_quality"]["noise_db"], 37.0);
        assert_eq!(value["air_quality"]["last_update"], "2026-06-05T12:30:00");
    }

    #[tokio::test]
    async fn status_route_reflects_latest_hvac_reading() {
        let hvac_state = HvacState::default();
        hvac_state.record_status(
            parse_kumo_adapter_status(&json!({
                "power": 1,
                "operationMode": "cool",
                "spHeat": 21.0,
                "spCool": 25.5
            }))
            .expect("HVAC status"),
        );

        let response = app_with_hvac_state(test_config(), hvac_state)
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

        assert_eq!(value["hvac"]["mode"], "cool");
        assert_eq!(value["hvac"]["setpoint_c"], 25.5);
        assert_eq!(value["hvac"]["temperature_bands"]["heat_on_temp_f"], 71);
    }

    #[tokio::test]
    async fn status_route_restores_yolink_device_state_from_database() {
        let config = test_config();
        db::migrate_database(&config.runtime.database_path).expect("migration");
        db::log_device_event(
            &config.runtime.database_path,
            "door",
            "open",
            Some("Office Door"),
            None,
        )
        .expect("log door");
        db::log_device_event(
            &config.runtime.database_path,
            "window",
            "closed",
            Some("Office Window"),
            None,
        )
        .expect("log window");
        db::log_device_event(
            &config.runtime.database_path,
            "motion",
            "detected",
            Some("Office Motion"),
            None,
        )
        .expect("log motion");

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

        assert_eq!(value["sensors"]["door_open"], true);
        assert_eq!(value["sensors"]["window_open"], false);
        assert_eq!(value["sensors"]["motion_detected"], true);
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
    async fn occupancy_route_preserves_external_reporter_contract() {
        let service = app(test_config());
        let last_active_timestamp = unix_timestamp_now();
        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/occupancy")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "last_active_timestamp": last_active_timestamp,
                            "external_monitor": true
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["ok"], true);
        assert_eq!(value["state"], "present");
        assert_eq!(value["erv_should_run"], false);

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
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["state"], "present");
        assert_eq!(value["sensors"]["mac_last_active"], last_active_timestamp);
        assert_eq!(value["sensors"]["external_monitor"], true);

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .uri("/history?hours=1")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["occupancy_history"][0]["state"], "present");
        assert_eq!(value["occupancy_history"][0]["trigger"], "mac_activity");
    }

    #[tokio::test]
    async fn stale_occupancy_signal_does_not_undo_manual_away() {
        let service = app(test_config());
        let last_active_timestamp = unix_timestamp_now() - 5.0;
        let occupancy_body = json!({
            "last_active_timestamp": last_active_timestamp,
            "external_monitor": true
        })
        .to_string();

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/occupancy")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(occupancy_body.clone()))
                    .expect("request"),
            )
            .await
            .expect("first occupancy response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/presence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"state":"away"}"#))
                    .expect("request"),
            )
            .await
            .expect("manual away response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/occupancy")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(occupancy_body))
                    .expect("request"),
            )
            .await
            .expect("second occupancy response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["state"], "away");

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("status response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["state"], "away");
        assert_eq!(value["sensors"]["mac_last_active"], last_active_timestamp);
        assert_eq!(value["sensors"]["external_monitor"], true);
    }

    #[tokio::test]
    async fn internal_presence_snapshot_uses_occupancy_update_path() {
        let config = test_config();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let (state, _) = build_app_state(
            config.clone(),
            QingpingState::default(),
            state_machine,
            yolink,
            ErvState::new(config.runtime.database_path.clone()),
            HvacState::new(config.runtime.database_path.clone()),
            Arc::new(FakeErvWriter::default()),
            Arc::new(FakeHvacWriter::default()),
        )
        .expect("app state");
        let last_active_timestamp = unix_timestamp_now();

        apply_presence_snapshot(
            &state,
            PresenceSnapshot {
                last_active_timestamp,
                external_monitor: true,
                idle_seconds: 0.1,
                display_count: 2,
                external_displays: vec!["Studio Display".to_string()],
            },
        )
        .await
        .expect("presence update");

        let status = state
            .state_machine
            .read()
            .expect("state machine lock poisoned")
            .status_at(unix_timestamp_now());
        assert_eq!(status.state, "present");
        assert_eq!(status.sensors.mac_last_active, last_active_timestamp);
        assert!(status.sensors.external_monitor);

        let history = db::read_history(&config.runtime.database_path, 1, 10).expect("history");
        assert_eq!(history.occupancy_history[0]["state"], "present");
        assert_eq!(history.occupancy_history[0]["trigger"], "internal_presence");
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
    async fn manual_erv_write_requires_active_control_gate() {
        let writer = Arc::new(FakeErvWriter::new(
            vec![Ok(erv_status(ErvFanSpeed::Off))],
            vec![Ok(erv_status(ErvFanSpeed::Turbo))],
        ));
        let service = app_with_erv_writer(
            configured_erv_config(false),
            QingpingState::default(),
            writer.clone(),
        );

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/erv")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"speed":"turbo"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert!(
            value["error"]
                .as_str()
                .expect("error")
                .contains("active control is disabled")
        );
        assert_eq!(writer.smoke_calls(), 0);
        assert!(writer.write_speeds().is_empty());
    }

    #[tokio::test]
    async fn manual_erv_write_smokes_writes_logs_and_broadcasts_status() {
        let config = configured_erv_config(true);
        let writer = Arc::new(FakeErvWriter::new(
            vec![Ok(erv_status(ErvFanSpeed::Off))],
            vec![Ok(erv_status(ErvFanSpeed::Turbo))],
        ));
        let service = app_with_erv_writer(config.clone(), qingping_with_co2(2100), writer.clone());
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
        timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("initial timeout")
            .expect("initial message")
            .expect("initial ok");

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/erv")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"speed":"turbo"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["erv"]["speed"], "turbo");
        assert_eq!(value["erv"]["running"], true);
        assert_eq!(value["erv"]["manual_override"], true);
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_speeds(), vec![ErvFanSpeed::Turbo]);

        let update = timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("update timeout")
            .expect("update message")
            .expect("update ok");
        assert!(
            update
                .into_text()
                .expect("text")
                .contains("\"speed\":\"turbo\"")
        );

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
        assert_eq!(value["erv"]["speed"], "turbo");
        assert_eq!(value["erv"]["running"], true);
        assert_eq!(value["manual_override"]["erv"], true);
        assert_eq!(value["manual_override"]["erv_speed"], "turbo");
        let expires_in = value["manual_override"]["erv_expires_in"]
            .as_i64()
            .expect("expires");
        assert!(expires_in > 0);
        assert!(expires_in <= ERV_MANUAL_OVERRIDE_SECONDS);

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
        assert_eq!(value["climate_actions"][0]["system"], "erv");
        assert_eq!(value["climate_actions"][0]["action"], "turbo");
        assert_eq!(value["climate_actions"][0]["reason"], "manual_override");
        assert_eq!(value["climate_actions"][0]["co2_ppm"], 2100);

        server.abort();
    }

    #[tokio::test]
    async fn manual_erv_override_prevents_next_policy_update_from_rewriting_speed() {
        let writer = Arc::new(FakeErvWriter::new(
            vec![Ok(erv_status(ErvFanSpeed::Off))],
            vec![Ok(erv_status(ErvFanSpeed::Turbo))],
        ));
        let service = app_with_erv_writer(
            configured_erv_config(true),
            qingping_with_co2(450),
            writer.clone(),
        );

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/erv")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"speed":"turbo"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

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

        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_speeds(), vec![ErvFanSpeed::Turbo]);

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
        let actions = value["climate_actions"].as_array().expect("actions");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["action"], "turbo");
        assert_eq!(actions[0]["reason"], "manual_override");
    }

    #[tokio::test]
    async fn manual_hvac_write_requires_active_control_gate() {
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Heat, 21.1))],
        ));
        let service = app_with_hvac_writer(configured_hvac_config(false), writer.clone());

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/hvac")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"mode":"heat","setpoint_f":70}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert!(
            value["error"]
                .as_str()
                .expect("error")
                .contains("active control is disabled")
        );
        assert_eq!(writer.smoke_calls(), 0);
        assert!(writer.write_modes().is_empty());
    }

    #[tokio::test]
    async fn manual_hvac_write_rejects_auto_mode() {
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Auto, 22.0))],
        ));
        let service = app_with_hvac_writer(configured_hvac_config(true), writer.clone());

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/hvac")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"mode":"auto","setpoint_f":70}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["error"], "Invalid HVAC mode");
        assert_eq!(writer.smoke_calls(), 0);
        assert!(writer.write_modes().is_empty());
    }

    #[tokio::test]
    async fn manual_hvac_write_surfaces_device_failure() {
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Err(anyhow::anyhow!("fake HVAC write failed"))],
        ));
        let service = app_with_hvac_writer(configured_hvac_config(true), writer.clone());

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/hvac")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"mode":"heat","setpoint_f":70}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert!(
            value["error"]
                .as_str()
                .expect("error")
                .contains("fake HVAC write failed")
        );
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(
            writer.write_modes(),
            vec![(HvacControlMode::Heat, Some(fahrenheit_to_celsius(70.0)))]
        );
    }

    #[tokio::test]
    async fn manual_hvac_write_smokes_writes_logs_and_broadcasts_status() {
        let config = configured_hvac_config(true);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Cool, 21.1111111111))],
        ));
        let service = app_with_hvac_writer(config.clone(), writer.clone());
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
        timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("initial timeout")
            .expect("initial message")
            .expect("initial ok");

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/hvac")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"mode":"cool","setpoint_f":70}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["hvac"]["mode"], "cool");
        assert_eq!(value["hvac"]["setpoint_f"], 70.0);
        assert_eq!(value["hvac"]["setpoint_c"], 21.1);
        assert_eq!(value["hvac"]["manual_override"], true);
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(
            writer.write_modes(),
            vec![(HvacControlMode::Cool, Some(fahrenheit_to_celsius(70.0)))]
        );

        let update = timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("update timeout")
            .expect("update message")
            .expect("update ok");
        assert!(
            update
                .into_text()
                .expect("text")
                .contains("\"manual_override\"")
        );

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
        assert_eq!(value["climate_actions"][0]["system"], "hvac");
        assert_eq!(value["climate_actions"][0]["action"], "cool");
        assert_eq!(value["climate_actions"][0]["reason"], "manual_override");
        assert_eq!(
            value["climate_actions"][0]["setpoint"],
            fahrenheit_to_celsius(70.0)
        );

        server.abort();
    }

    #[tokio::test]
    async fn automated_hvac_policy_turns_off_for_safety_interlock() {
        let config = configured_hvac_config(true);
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .update_door(true, unix_timestamp_now());
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Heat, 22.0));
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
        ));
        let service = try_app_with_erv_writer(
            config.clone(),
            QingpingState::default(),
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

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
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_modes(), vec![(HvacControlMode::Off, None)]);

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
        assert_eq!(value["climate_actions"][0]["system"], "hvac");
        assert_eq!(value["climate_actions"][0]["action"], "off");
        assert_eq!(value["climate_actions"][0]["reason"], "safety_interlock");
    }

    #[tokio::test]
    async fn automated_hvac_policy_safety_interlock_bypasses_manual_override() {
        let config = configured_hvac_config(true);
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        {
            let mut machine = state_machine.write().expect("state machine lock poisoned");
            machine.set_manual_presence(true, now);
            machine.update_door(true, now + 1.0);
        }
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Heat, 22.0));
        hvac_state.record_manual_override(HvacControlMode::Heat, 70.0);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
        ));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            QingpingState::default(),
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies safety off despite manual override");

        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_modes(), vec![(HvacControlMode::Off, None)]);
        assert!(!state.hvac.manual_override_active());
        let snapshot = state.hvac.snapshot();
        assert!(snapshot.suspended);
        assert_eq!(snapshot.last_mode.as_deref(), Some("heat"));
    }

    #[tokio::test]
    async fn automated_hvac_policy_reads_device_before_safety_interlock_off() {
        let config = configured_hvac_config(true);
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .update_door(true, now + 1.0);
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
        ));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            QingpingState::default(),
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies safety off from fresh status");

        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_modes(), vec![(HvacControlMode::Off, None)]);
        let snapshot = state.hvac.snapshot();
        assert!(snapshot.suspended);
        assert_eq!(snapshot.last_mode.as_deref(), Some("heat"));
    }

    #[tokio::test]
    async fn yolink_safety_event_drives_hvac_policy_without_route_update() {
        let config = configured_hvac_config(true);
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, now);
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        yolink.apply_devices(vec![office_door_device()]);
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Heat, 22.0));
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
        ));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            QingpingState::default(),
            state_machine,
            yolink.clone(),
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        yolink
            .apply_event("door-1", json!({"state": "open"}), now + 1.0)
            .expect("event applies");
        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies");

        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_modes(), vec![(HvacControlMode::Off, None)]);
    }

    #[tokio::test]
    async fn yolink_transition_clears_hvac_manual_override_before_policy() {
        let config = configured_hvac_config(true);
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        yolink.apply_devices(vec![office_motion_device()]);
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        hvac_state.record_manual_override(HvacControlMode::Off, 70.0);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Cool, 25.5))],
        ));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping_with_temp(28.0),
            state_machine,
            yolink.clone(),
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        let applied = yolink
            .apply_event("motion-1", json!({"state": "alert"}), now + 1.0)
            .expect("event applies")
            .expect("motion report applies");
        assert!(applied.transition.is_some());

        evaluate_yolink_hvac_update(state.clone(), applied.transition).await;

        assert!(!state.hvac.manual_override_active());
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(
            writer.write_modes(),
            vec![(HvacControlMode::Cool, Some(25.5))]
        );
    }

    #[tokio::test]
    async fn yolink_transition_broadcasts_when_only_hvac_override_clears() {
        let config = configured_hvac_config(true);
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        hvac_state.record_manual_override(HvacControlMode::Off, 70.0);
        let writer = Arc::new(FakeHvacWriter::new(Vec::new(), Vec::new()));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping_with_temp(21.0),
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");
        let mut receiver = state.status_broadcast.subscribe();

        evaluate_yolink_hvac_update(
            state.clone(),
            Some(StateTransition {
                old_state: OccupancyState::Away,
                new_state: OccupancyState::Present,
            }),
        )
        .await;

        assert!(!state.hvac.manual_override_active());
        assert_eq!(writer.smoke_calls(), 0);
        timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("broadcast timeout")
            .expect("broadcast received");
    }

    #[tokio::test]
    async fn automated_hvac_policy_restores_suspended_heat_after_safety_clears() {
        let config = configured_hvac_config(true);
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        {
            let mut machine = state_machine.write().expect("state machine lock poisoned");
            machine.set_manual_presence(true, now);
            machine.update_door(true, now + 1.0);
        }
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Heat, 22.0));
        let writer = Arc::new(FakeHvacWriter::new(
            vec![
                Ok(hvac_status(HvacControlMode::Heat, 22.0)),
                Ok(hvac_status(HvacControlMode::Off, 22.0)),
            ],
            vec![
                Ok(hvac_status(HvacControlMode::Off, 22.0)),
                Ok(hvac_status(HvacControlMode::Heat, 22.0)),
            ],
        ));
        let qingping = qingping_with_temp(fahrenheit_to_celsius(68.0));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping,
            state_machine.clone(),
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies safety off");
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .update_door(false, now + 2.0);
        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy restores heat");

        assert_eq!(writer.smoke_calls(), 2);
        assert_eq!(
            writer.write_modes(),
            vec![
                (HvacControlMode::Off, None),
                (HvacControlMode::Heat, Some(22.0)),
            ]
        );
    }

    #[tokio::test]
    async fn automated_hvac_policy_restores_suspended_auto_after_safety_clears() {
        let config = configured_hvac_config(true);
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        {
            let mut machine = state_machine.write().expect("state machine lock poisoned");
            machine.set_manual_presence(true, now);
            machine.update_door(true, now + 1.0);
        }
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_auto_status(20.0, 26.0));
        let writer = Arc::new(FakeHvacWriter::new(
            vec![
                Ok(hvac_auto_status(20.0, 26.0)),
                Ok(hvac_status(HvacControlMode::Off, 22.0)),
            ],
            vec![
                Ok(hvac_status(HvacControlMode::Off, 22.0)),
                Ok(hvac_auto_status(20.0, 26.0)),
            ],
        ));
        let qingping = qingping_with_temp(fahrenheit_to_celsius(72.0));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping,
            state_machine.clone(),
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies safety off");
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .update_door(false, now + 2.0);
        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy restores auto");

        assert_eq!(writer.smoke_calls(), 2);
        assert_eq!(
            writer.write_commands(),
            vec![
                HvacModeCommand::new(HvacControlMode::Off, None),
                HvacModeCommand::auto(20.0, 26.0),
            ]
        );
        let snapshot = state.hvac.snapshot();
        assert_eq!(snapshot.mode, "auto");
        assert!(!snapshot.suspended);
    }

    #[tokio::test]
    async fn automated_hvac_policy_heats_away_room_below_critical_temp() {
        let config = configured_hvac_config(true);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
        ));
        let qingping = qingping_with_temp(fahrenheit_to_celsius(54.0));
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies");

        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(
            writer.write_modes(),
            vec![(HvacControlMode::Heat, Some(22.0))]
        );
    }

    #[tokio::test]
    async fn automated_hvac_policy_critical_heat_bypasses_manual_override() {
        let config = configured_hvac_config(true);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
        ));
        let qingping = qingping_with_temp(fahrenheit_to_celsius(54.0));
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        hvac_state.record_manual_override(HvacControlMode::Off, 70.0);
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies");

        assert!(!state.hvac.manual_override_active());
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(
            writer.write_modes(),
            vec![(HvacControlMode::Heat, Some(22.0))]
        );
    }

    #[tokio::test]
    async fn automated_hvac_policy_skips_away_heat_band_resume_outside_occupancy_hours() {
        let mut config = configured_hvac_config(true);
        set_occupancy_window_excluding_now(&mut config);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
        ));
        let qingping = qingping_with_temp(fahrenheit_to_celsius(70.0));
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        hvac_state.set_temp_band_mode(Some(HvacControlMode::Heat));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy respects occupancy hours");

        assert_eq!(writer.smoke_calls(), 0);
        assert!(writer.write_modes().is_empty());
    }

    #[tokio::test]
    async fn automated_hvac_policy_suspends_heat_while_away_erv_running() {
        let mut config = configured_hvac_config(true);
        config.erv = configured_erv_config(true).erv;
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now,
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let seed_erv_writer = FakeErvWriter::new(vec![Ok(erv_status(ErvFanSpeed::Turbo))], vec![]);
        erv_state
            .smoke_status_with(&config.erv, &seed_erv_writer)
            .await
            .expect("seed ERV running status");
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Heat, 22.0));
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Heat, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
        ));
        let qingping = qingping_with_temp(fahrenheit_to_celsius(70.0));
        let (service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_and_apply_hvac_policy(&state)
            .await
            .expect("HVAC policy applies ERV suspension");

        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_modes(), vec![(HvacControlMode::Off, None)]);
        let snapshot = state.hvac.snapshot();
        assert!(snapshot.suspended);
        assert_eq!(snapshot.last_mode.as_deref(), Some("heat"));

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
        assert_eq!(value["climate_actions"][0]["system"], "hvac");
        assert_eq!(value["climate_actions"][0]["action"], "off");
        assert_eq!(
            value["climate_actions"][0]["reason"],
            "erv_running_temp_70F"
        );
    }

    #[tokio::test]
    async fn automated_hvac_policy_respects_disabled_active_control_gate() {
        let config = configured_hvac_config(false);
        let writer = Arc::new(FakeHvacWriter::new(Vec::new(), Vec::new()));
        let qingping = qingping_with_temp(28.0);
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        let service = try_app_with_erv_writer(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

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
        assert_eq!(writer.smoke_calls(), 0);
        assert!(writer.write_modes().is_empty());
    }

    #[tokio::test]
    async fn automated_hvac_policy_clears_manual_override_on_state_transition() {
        let config = configured_hvac_config(true);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![
                Ok(hvac_status(HvacControlMode::Off, 22.0)),
                Ok(hvac_status(HvacControlMode::Off, 22.0)),
            ],
            vec![
                Ok(hvac_status(HvacControlMode::Off, 22.0)),
                Ok(hvac_status(HvacControlMode::Cool, 25.5)),
            ],
        ));
        let qingping = qingping_with_temp(28.0);
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        let service = try_app_with_erv_writer(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/hvac")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"mode":"off","setpoint_f":70}"#))
                    .expect("request"),
            )
            .await
            .expect("manual response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = service
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("status response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read status");
        let value: Value = serde_json::from_slice(&body).expect("status json");
        assert_eq!(value["manual_override"]["hvac"], true);

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
            .expect("presence response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(writer.smoke_calls(), 2);
        assert_eq!(
            writer.write_modes(),
            vec![
                (HvacControlMode::Off, Some(fahrenheit_to_celsius(70.0))),
                (HvacControlMode::Cool, Some(25.5)),
            ]
        );

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("status response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read status");
        let value: Value = serde_json::from_slice(&body).expect("status json");
        assert_eq!(value["manual_override"]["hvac"], false);
        assert_eq!(value["hvac"]["mode"], "cool");
    }

    #[tokio::test]
    async fn presence_preserves_transition_when_erv_policy_fails() {
        let mut config = configured_erv_config(true);
        config.mitsubishi = configured_hvac_config(true).mitsubishi;
        let qingping = QingpingState::default();
        qingping.apply_reading(crate::qingping::QingpingReading {
            temp_c: Some(28.0),
            ..qingping_reading(2100)
        });
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        hvac_state.record_manual_override(HvacControlMode::Off, 70.0);
        let erv_writer = Arc::new(FakeErvWriter::new(
            vec![Ok(erv_status(ErvFanSpeed::Off))],
            vec![Err(anyhow::anyhow!("ERV write failed"))],
        ));
        let hvac_writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(HvacControlMode::Cool, 25.5))],
        ));
        let service = try_app_with_erv_writer(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            erv_writer.clone(),
            hvac_writer.clone(),
        )
        .expect("app");

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
            .expect("presence response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(erv_writer.write_speeds(), vec![ErvFanSpeed::Quiet]);
        assert_eq!(
            hvac_writer.write_modes(),
            vec![(HvacControlMode::Cool, Some(25.5))]
        );

        let response = service
            .oneshot(
                HttpRequest::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("status response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read status");
        let value: Value = serde_json::from_slice(&body).expect("status json");
        assert_eq!(value["state"], "present");
        assert_eq!(value["manual_override"]["hvac"], false);
        assert_eq!(value["hvac"]["mode"], "cool");
    }

    #[tokio::test]
    async fn automated_hvac_policy_starts_cooling_for_hot_present_room() {
        let config = configured_hvac_config(true);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(
                HvacControlMode::Cool,
                fahrenheit_to_celsius(78.0),
            ))],
        ));
        let qingping = qingping_with_temp(28.0);
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        let service = try_app_with_erv_writer(
            config.clone(),
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

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
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(
            writer.write_modes(),
            vec![(HvacControlMode::Cool, Some(25.5))]
        );

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
        assert_eq!(value["climate_actions"][0]["system"], "hvac");
        assert_eq!(value["climate_actions"][0]["action"], "cool");
        assert_eq!(value["climate_actions"][0]["reason"], "cool_band_start_82F");
    }

    #[tokio::test]
    async fn qingping_sensor_update_drives_hvac_policy_without_route_update() {
        let config = configured_hvac_config(true);
        let writer = Arc::new(FakeHvacWriter::new(
            vec![Ok(hvac_status(HvacControlMode::Off, 22.0))],
            vec![Ok(hvac_status(
                HvacControlMode::Cool,
                fahrenheit_to_celsius(78.0),
            ))],
        ));
        let qingping = qingping_with_temp(28.0);
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            unix_timestamp_now(),
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, unix_timestamp_now());
        let yolink = YoLinkState::new(state_machine.clone(), config.runtime.database_path.clone());
        let erv_state = ErvState::new(config.runtime.database_path.clone());
        let hvac_state = HvacState::new(config.runtime.database_path.clone());
        hvac_state.record_status(hvac_status(HvacControlMode::Off, 22.0));
        let (_service, state) = try_app_with_erv_writer_and_coordinator(
            config,
            qingping,
            state_machine,
            yolink,
            erv_state,
            hvac_state,
            Arc::new(FakeErvWriter::default()),
            writer.clone(),
        )
        .expect("app");

        evaluate_sensor_policies(
            state.erv_automation.clone(),
            state.clone(),
            false,
            "Qingping",
        )
        .await;

        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(
            writer.write_modes(),
            vec![(HvacControlMode::Cool, Some(25.5))]
        );
    }

    #[tokio::test]
    async fn automated_erv_policy_respects_disabled_active_control_gate() {
        let writer = Arc::new(FakeErvWriter::new(
            vec![Ok(erv_status(ErvFanSpeed::Off))],
            vec![Ok(erv_status(ErvFanSpeed::Quiet))],
        ));
        let service = app_with_erv_writer(
            configured_erv_config(false),
            qingping_with_co2(2100),
            writer.clone(),
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
        assert_eq!(writer.smoke_calls(), 0);
        assert!(writer.write_speeds().is_empty());
    }

    #[tokio::test]
    async fn automated_erv_policy_uses_gated_writer_after_presence_update() {
        let writer = Arc::new(FakeErvWriter::new(
            vec![Ok(erv_status(ErvFanSpeed::Off))],
            vec![Ok(erv_status(ErvFanSpeed::Quiet))],
        ));
        let service = app_with_erv_writer(
            configured_erv_config(true),
            qingping_with_co2(2100),
            writer.clone(),
        );

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
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_speeds(), vec![ErvFanSpeed::Quiet]);

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
        assert_eq!(value["climate_actions"][0]["system"], "erv");
        assert_eq!(value["climate_actions"][0]["action"], "quiet");
        assert_eq!(
            value["climate_actions"][0]["reason"],
            "present_co2_critical_2100ppm"
        );
        assert_eq!(value["climate_actions"][0]["co2_ppm"], 2100);
    }

    #[tokio::test]
    async fn automated_erv_policy_records_away_transition_before_deciding() {
        let qingping = qingping_with_co2(400);
        let writer = Arc::new(FakeErvWriter::new(
            vec![Ok(erv_status(ErvFanSpeed::Off))],
            vec![Ok(erv_status(ErvFanSpeed::Turbo))],
        ));
        let service = app_with_erv_writer(
            configured_erv_config(true),
            qingping.clone(),
            writer.clone(),
        );

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
        assert_eq!(writer.smoke_calls(), 1);

        qingping.apply_reading(qingping_reading(2100));
        let response = service
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/presence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"state":"away"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(writer.smoke_calls(), 1);
        assert!(writer.write_speeds().is_empty());
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
    async fn secondary_history_routes_return_compatible_payloads() {
        let config = test_config();
        db::migrate_database(&config.runtime.database_path).expect("migration");
        let connection =
            rusqlite::Connection::open(&config.runtime.database_path).expect("open database");
        let now = chrono::Local::now().naive_local();
        let today = now.date();
        let timestamp = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let present_at = today
            .and_hms_opt(9, 0, 0)
            .expect("present timestamp")
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let away_at = today
            .and_hms_opt(12, 0, 0)
            .expect("away timestamp")
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let open_at = today
            .and_hms_opt(10, 0, 0)
            .expect("open timestamp")
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let close_at = today
            .and_hms_opt(10, 15, 0)
            .expect("close timestamp")
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        connection
            .execute(
                "INSERT INTO occupancy_log (timestamp, state) VALUES (?, ?), (?, ?)",
                (&present_at, "present", &away_at, "away"),
            )
            .expect("insert occupancy");
        connection
            .execute(
                "INSERT INTO sensor_readings (timestamp, co2_ppm, temp_c, source) VALUES (?, ?, ?, ?)",
                (&timestamp, 900, 22.0, "qingping"),
            )
            .expect("insert sensor");
        connection
            .execute(
                "INSERT INTO device_events (timestamp, device_type, event) VALUES (?, ?, ?), (?, ?, ?)",
                (&open_at, "door", "open", &close_at, "door", "closed"),
            )
            .expect("insert device events");
        connection
            .execute(
                "INSERT INTO climate_actions (timestamp, system, action) VALUES (?, ?, ?), (?, ?, ?)",
                (&present_at, "erv", "quiet", &away_at, "erv", "off"),
            )
            .expect("insert climate");
        connection
            .execute(
                "INSERT INTO orchestration_activity (timestamp, tool, project, session_id) VALUES (?, ?, ?, ?)",
                (&timestamp, "codex", "fractal-1234-work", "session-1"),
            )
            .expect("insert orchestration");
        connection
            .execute(
                "INSERT INTO github_prs (repo, pr_number, title, state, created_at, merged_at) VALUES (?, ?, ?, ?, ?, ?)",
                (
                    "rajeshgoli/office-automation",
                    83,
                    "Port HTTP contracts",
                    "MERGED",
                    &timestamp,
                    &timestamp,
                ),
            )
            .expect("insert pr");
        connection
            .execute(
                "INSERT INTO project_leverage (date, project, metric, value) VALUES (?, ?, ?, ?)",
                (today.to_string(), "session-manager", "sm_dispatches", 2.0),
            )
            .expect("insert project leverage");

        let service = app(config);
        for (path, checks) in [
            (
                "/history/sessions?days=1",
                vec![("sessions", "array"), ("summary", "object")],
            ),
            (
                "/history/co2-ohlc?hours=1&bucket_minutes=60",
                vec![("candles", "array"), ("bucket_minutes", "number")],
            ),
            (
                "/history/temperature?hours=1&bucket_minutes=60",
                vec![("points", "array"), ("bucket_minutes", "number")],
            ),
            (
                "/history/daily-stats?days=1",
                vec![("stats", "array"), ("days", "number")],
            ),
            ("/history/openings?days=1", vec![("days", "array")]),
            ("/history/orchestration?days=1", vec![("days", "array")]),
            ("/history/project-focus?days=1", vec![("days", "array")]),
            (
                "/history/leverage?days=1",
                vec![("days", "array"), ("week", "object")],
            ),
            (
                "/history/project-leverage?days=1",
                vec![("projects", "object")],
            ),
        ] {
            let response = service
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read body");
            let value: Value = serde_json::from_slice(&body).expect("json body");
            assert_eq!(value["ok"], true, "{path}");
            for (field, kind) in checks {
                match kind {
                    "array" => assert!(value[field].is_array(), "{path} {field}"),
                    "object" => assert!(value[field].is_object(), "{path} {field}"),
                    "number" => assert!(value[field].is_number(), "{path} {field}"),
                    _ => unreachable!("unknown kind"),
                }
            }
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
