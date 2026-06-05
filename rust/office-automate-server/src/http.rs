use anyhow::{Context, Result};
use axum::{
    Json, Router,
    http::{Method, header},
    routing::get,
};
use tokio::net::TcpListener;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

use crate::{config::AppConfig, status::Status};

#[derive(Debug, Clone)]
struct AppState {
    config: AppConfig,
}

pub fn app(config: AppConfig) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    Router::new()
        .route("/status", get(status))
        .with_state(AppState { config })
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

pub async fn serve(config: AppConfig) -> Result<()> {
    let bind_address = format!("{}:{}", config.orchestrator.host, config.orchestrator.port);
    let listener = TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("failed to bind HTTP listener at {bind_address}"))?;

    tracing::info!("office-automate-server listening on {}", bind_address);
    axum::serve(listener, app(config))
        .await
        .context("HTTP server failed")
}

async fn status(axum::extract::State(state): axum::extract::State<AppState>) -> Json<Status> {
    Json(Status::read_only_default(&state.config))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use super::*;
    use crate::config::{OrchestratorConfig, QingpingConfig, RuntimeConfig, ThresholdsConfig};

    fn test_config() -> AppConfig {
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            qingping: QingpingConfig::default(),
            thresholds: ThresholdsConfig::default(),
            runtime: RuntimeConfig {
                root: PathBuf::from("/tmp/office"),
                config_path: PathBuf::from("/tmp/office/config.yaml"),
                data_dir: PathBuf::from("/tmp/office/data"),
                database_path: PathBuf::from("/tmp/office/data/office_climate.db"),
                base_url: None,
                public_url: None,
                mqtt_host: "127.0.0.1".to_string(),
                mqtt_port: 1883,
            },
        }
    }

    #[tokio::test]
    async fn status_route_returns_compatibility_skeleton() {
        let response = app(test_config())
            .oneshot(
                Request::builder()
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
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");

        assert_eq!(value["state"], "away");
        assert!(value.get("sensors").is_some());
        assert!(value.get("air_quality").is_some());
        assert!(value.get("erv").is_some());
        assert!(value.get("hvac").is_some());
        assert!(value["erv"]["control"].get("last_local_ok_at").is_some());
    }
}
