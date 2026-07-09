use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};
use serde::Serialize;
use tower_http::services::{ServeDir, ServeFile};

use crate::config::{default_static_dir, server_paths_from_env, ServerPaths};
use crate::db::ConfigDb;

#[derive(Clone)]
pub struct AppState {
    server_paths: Option<ServerPaths>,
    config_db: Option<ConfigDb>,
    static_dir: PathBuf,
    version: &'static str,
}

impl AppState {
    pub async fn from_env() -> anyhow::Result<Self> {
        let server_paths = server_paths_from_env()?;
        let config_db = match &server_paths {
            Some(paths) => {
                let db = ConfigDb::open(paths).await?;
                db.record_data_root(paths).await?;
                Some(db)
            }
            None => None,
        };

        Ok(Self::with_config_db(
            server_paths,
            config_db,
            default_static_dir(),
        ))
    }

    pub fn new(server_paths: Option<ServerPaths>, static_dir: PathBuf) -> Self {
        Self::with_config_db(server_paths, None, static_dir)
    }

    pub fn with_config_db(
        server_paths: Option<ServerPaths>,
        config_db: Option<ConfigDb>,
        static_dir: PathBuf,
    ) -> Self {
        Self {
            server_paths,
            config_db,
            static_dir,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    async fn setup_status(&self) -> anyhow::Result<SetupStatusResponse> {
        let data_root_configured = self.server_paths.is_some();
        let server_home_ready = self
            .server_paths
            .as_ref()
            .map(|paths| paths.server_home().is_dir())
            .unwrap_or(false);
        let owner_configured = match &self.config_db {
            Some(db) => db.setup_state().await?.owner_configured,
            None => false,
        };
        let database_ready = self.config_db.is_some();
        let setup_required = !data_root_configured || !database_ready || !owner_configured;
        let status = if setup_required {
            "setup_required"
        } else {
            "ok"
        };

        Ok(SetupStatusResponse {
            ok: true,
            status,
            data_root_configured,
            server_home_ready,
            database_ready,
            owner_configured,
            setup_required,
        })
    }

    async fn health(&self) -> anyhow::Result<HealthResponse> {
        let setup = self.setup_status().await?;

        Ok(HealthResponse {
            ok: true,
            status: setup.status,
            version: self.version,
            data_root_configured: setup.data_root_configured,
            server_home_ready: setup.server_home_ready,
            database_ready: setup.database_ready,
            owner_configured: setup.owner_configured,
            setup_required: setup.setup_required,
            static_assets_ready: self.static_dir.join("index.html").is_file(),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    status: &'static str,
    version: &'static str,
    data_root_configured: bool,
    server_home_ready: bool,
    database_ready: bool,
    owner_configured: bool,
    setup_required: bool,
    static_assets_ready: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupStatusResponse {
    ok: bool,
    status: &'static str,
    data_root_configured: bool,
    server_home_ready: bool,
    database_ready: bool,
    owner_configured: bool,
    setup_required: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    ok: bool,
    error: &'static str,
}

pub fn router(state: AppState) -> Router {
    let static_dir = state.static_dir.clone();
    let static_service =
        ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html")));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/setup/status", get(setup_status))
        .route("/api/{*path}", any(api_not_found))
        .fallback_service(static_service)
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.health().await {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(err) => internal_server_error(err),
    }
}

async fn setup_status(State(state): State<AppState>) -> impl IntoResponse {
    match state.setup_status().await {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(err) => internal_server_error(err),
    }
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            ok: false,
            error: "Not found",
        }),
    )
}

fn internal_server_error(err: anyhow::Error) -> axum::response::Response {
    tracing::error!(error = %err, "server API request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            ok: false,
            error: "Internal server error",
        }),
    )
        .into_response()
}

pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{router, AppState};
    use crate::config::prepare_server_paths;
    use crate::db::ConfigDb;

    #[tokio::test]
    async fn health_reports_setup_required_without_paths() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::new(None, temp.path().join("dist"));
        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "setup_required");
        assert_eq!(payload["dataRootConfigured"], false);
        assert_eq!(payload["serverHomeReady"], false);
        assert_eq!(payload["databaseReady"], false);
        assert_eq!(payload["ownerConfigured"], false);
        assert_eq!(payload["setupRequired"], true);
        assert!(!String::from_utf8_lossy(&body).contains(temp.path().to_str().unwrap()));
    }

    #[tokio::test]
    async fn health_reports_configured_data_root_without_leaking_paths() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let state = AppState::new(Some(paths), temp.path().join("dist"));
        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "setup_required");
        assert_eq!(payload["dataRootConfigured"], true);
        assert_eq!(payload["serverHomeReady"], true);
        assert_eq!(payload["databaseReady"], false);
        assert_eq!(payload["ownerConfigured"], false);
        assert_eq!(payload["setupRequired"], true);
        assert!(!String::from_utf8_lossy(&body).contains(&data_root));
    }

    #[tokio::test]
    async fn setup_status_reports_owner_setup_required_without_leaking_paths() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state = AppState::with_config_db(Some(paths), Some(db), temp.path().join("dist"));
        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/setup/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "setup_required");
        assert_eq!(payload["dataRootConfigured"], true);
        assert_eq!(payload["databaseReady"], true);
        assert_eq!(payload["ownerConfigured"], false);
        assert_eq!(payload["setupRequired"], true);
        assert!(!String::from_utf8_lossy(&body).contains(&data_root));
    }

    #[tokio::test]
    async fn health_reports_ok_after_owner_exists() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        sqlx::query("INSERT INTO owner_auth (id, password_hash) VALUES (1, ?)")
            .bind("argon2-placeholder")
            .execute(db.pool())
            .await
            .unwrap();
        let state = AppState::with_config_db(Some(paths), Some(db), temp.path().join("dist"));
        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["databaseReady"], true);
        assert_eq!(payload["ownerConfigured"], true);
        assert_eq!(payload["setupRequired"], false);
    }

    #[tokio::test]
    async fn unknown_api_paths_return_json_404() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::new(None, temp.path().join("dist"));
        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/unknown")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["ok"], false);
    }

    #[tokio::test]
    async fn frontend_routes_fall_back_to_index_html() {
        let temp = tempfile::tempdir().unwrap();
        let dist = temp.path().join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("index.html"), "<main>LLM Wiki</main>").unwrap();

        let state = AppState::new(None, dist);
        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/projects/example/wiki")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&body), "<main>LLM Wiki</main>");
    }
}
