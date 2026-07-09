use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

use crate::config::{default_static_dir, server_paths_from_env, ServerPaths};
use crate::db::ConfigDb;
use crate::projects::{
    ImportProjectResponse, ProjectResponse, ProjectService, ProjectServiceError,
};

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

    fn project_service(&self) -> Result<ProjectService, ProjectServiceError> {
        let Some(paths) = self.server_paths.clone() else {
            return Err(ProjectServiceError::SetupRequired);
        };
        let Some(db) = self.config_db.clone() else {
            return Err(ProjectServiceError::SetupRequired);
        };

        Ok(ProjectService::new(paths, db))
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
    error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListProjectsResponse {
    ok: bool,
    projects: Vec<ProjectResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectEnvelope {
    ok: bool,
    project: ProjectResponse,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProjectEnvelope {
    ok: bool,
    project: ProjectResponse,
    skipped_symlinks: Vec<String>,
}

impl From<ImportProjectResponse> for ImportProjectEnvelope {
    fn from(response: ImportProjectResponse) -> Self {
        Self {
            ok: true,
            project: response.project,
            skipped_symlinks: response.skipped_symlinks,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportProjectRequest {
    source_path: String,
    name: Option<String>,
}

pub fn router(state: AppState) -> Router {
    let static_dir = state.static_dir.clone();
    let static_service =
        ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html")));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/setup/status", get(setup_status))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/import", post(import_project))
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

async fn list_projects(State(state): State<AppState>) -> axum::response::Response {
    let result = async {
        let service = state.project_service()?;
        let projects = service.list_projects().await?;
        Ok::<_, ProjectServiceError>(ListProjectsResponse { ok: true, projects })
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(err) => project_error_response(err),
    }
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> axum::response::Response {
    let result = async {
        let service = state.project_service()?;
        let project = service.create_project(&request.name).await?;
        Ok::<_, ProjectServiceError>(ProjectEnvelope { ok: true, project })
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::CREATED, Json(payload)).into_response(),
        Err(err) => project_error_response(err),
    }
}

async fn import_project(
    State(state): State<AppState>,
    Json(request): Json<ImportProjectRequest>,
) -> axum::response::Response {
    let result = async {
        let service = state.project_service()?;
        let imported = service
            .import_project(&request.source_path, request.name.as_deref())
            .await?;
        Ok::<_, ProjectServiceError>(ImportProjectEnvelope::from(imported))
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::CREATED, Json(payload)).into_response(),
        Err(err) => project_error_response(err),
    }
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            ok: false,
            error: "Not found".to_string(),
        }),
    )
}

fn project_error_response(err: ProjectServiceError) -> axum::response::Response {
    match err {
        ProjectServiceError::SetupRequired => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                ok: false,
                error: "Server setup is required before projects are available".to_string(),
            }),
        )
            .into_response(),
        ProjectServiceError::InvalidInput(message) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        ProjectServiceError::InvalidProject(message) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        ProjectServiceError::Conflict(message) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        ProjectServiceError::Internal(err) => internal_server_error(err),
    }
}

fn internal_server_error(err: anyhow::Error) -> axum::response::Response {
    tracing::error!(error = %err, "server API request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            ok: false,
            error: "Internal server error".to_string(),
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
    use axum::body::{to_bytes, Body};
    use axum::http::{header::CONTENT_TYPE, Method, Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::{router, AppState};
    use crate::config::prepare_server_paths;
    use crate::db::ConfigDb;

    async fn request_json(
        state: AppState,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(body) => {
                builder = builder.header(CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = router(state)
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        let payload: Value = serde_json::from_str(&body).unwrap();
        (status, body, payload)
    }

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
    async fn project_routes_require_configured_server() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::new(None, temp.path().join("dist"));
        let (status, body, payload) = request_json(state, Method::GET, "/api/projects", None).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(payload["ok"], false);
        assert!(!body.contains(temp.path().to_str().unwrap()));
    }

    #[tokio::test]
    async fn create_project_registers_data_root_child_without_leaking_paths() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let project_root = paths.data_root().join("my-research-project");
        let db = ConfigDb::open(&paths).await.unwrap();
        let state = AppState::with_config_db(Some(paths), Some(db), temp.path().join("dist"));

        let (status, body, payload) = request_json(
            state.clone(),
            Method::POST,
            "/api/projects",
            Some(json!({ "name": "My Research Project" })),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["project"]["name"], "My Research Project");
        assert_eq!(payload["project"]["relativePath"], "my-research-project");
        assert_eq!(payload["project"]["source"], "created");
        assert!(payload["project"]["id"].as_str().unwrap().contains('-'));
        assert!(project_root.join("schema.md").is_file());
        assert!(project_root.join("wiki/index.md").is_file());
        assert!(project_root.join(".llm-wiki/project.json").is_file());
        assert!(!body.contains(&data_root));

        let (status, body, payload) = request_json(state, Method::GET, "/api/projects", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["projects"].as_array().unwrap().len(), 1);
        assert_eq!(
            payload["projects"][0]["relativePath"],
            "my-research-project"
        );
        assert!(!body.contains(&data_root));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn import_project_copies_into_data_root_and_skips_symlinks_without_leaking_paths() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let external = temp.path().join("external-project");
        std::fs::create_dir_all(external.join("wiki")).unwrap();
        std::fs::write(external.join("schema.md"), "# Schema").unwrap();
        std::fs::write(external.join("wiki/page.md"), "# Page").unwrap();
        let outside = temp.path().join("outside.md");
        std::fs::write(&outside, "# Outside").unwrap();
        symlink(&outside, external.join("wiki/linked.md")).unwrap();

        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let project_root = paths.data_root().join("imported-project");
        let db = ConfigDb::open(&paths).await.unwrap();
        let state = AppState::with_config_db(Some(paths), Some(db), temp.path().join("dist"));
        let source_path = external.to_string_lossy().into_owned();

        let (status, body, payload) = request_json(
            state,
            Method::POST,
            "/api/projects/import",
            Some(json!({
                "sourcePath": source_path,
                "name": "Imported Project"
            })),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(payload["project"]["relativePath"], "imported-project");
        assert_eq!(payload["project"]["source"], "imported");
        assert_eq!(payload["skippedSymlinks"], json!(["wiki/linked.md"]));
        assert!(project_root.join("schema.md").is_file());
        assert!(project_root.join("wiki/page.md").is_file());
        assert!(!project_root.join("wiki/linked.md").exists());
        assert!(project_root.join(".llm-wiki/project.json").is_file());
        assert!(!body.contains(&data_root));
        assert!(!body.contains(&source_path));
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
