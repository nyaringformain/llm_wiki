use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

use crate::config::{default_static_dir, server_paths_from_env, ServerPaths};
use crate::db::ConfigDb;
use crate::files::{
    FileService, FileServiceError, FileTreeRequest, UploadFilesRequest, WriteFileRequest,
};
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

    fn file_service(&self) -> Result<FileService, FileServiceError> {
        let Some(paths) = self.server_paths.clone() else {
            return Err(FileServiceError::SetupRequired);
        };
        let Some(db) = self.config_db.clone() else {
            return Err(FileServiceError::SetupRequired);
        };

        Ok(FileService::new(paths, db))
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

#[derive(Debug, Deserialize)]
struct ProjectFilePath {
    project_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilePathQuery {
    path: Option<String>,
    expected_version: Option<String>,
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
        .route("/api/projects/{project_id}/files/tree", get(file_tree))
        .route("/api/projects/{project_id}/files/read", get(read_file))
        .route("/api/projects/{project_id}/files/write", put(write_file))
        .route("/api/projects/{project_id}/files", delete(delete_file))
        .route(
            "/api/projects/{project_id}/files/upload",
            post(upload_files),
        )
        .route(
            "/api/projects/{project_id}/files/preview",
            get(preview_file),
        )
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

async fn file_tree(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Query(request): Query<FileTreeRequest>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        service.tree(&path.project_id, request).await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(err) => file_error_response(err),
    }
}

async fn read_file(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Query(request): Query<FilePathQuery>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        service
            .read(&path.project_id, request.path.as_deref().unwrap_or(""))
            .await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(err) => file_error_response(err),
    }
}

async fn write_file(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Json(request): Json<WriteFileRequest>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        service.write(&path.project_id, request).await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(err) => file_error_response(err),
    }
}

async fn delete_file(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Query(request): Query<FilePathQuery>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        service
            .delete(
                &path.project_id,
                request.path.as_deref().unwrap_or(""),
                request.expected_version.as_deref(),
            )
            .await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(err) => file_error_response(err),
    }
}

async fn upload_files(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Json(request): Json<UploadFilesRequest>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        service.upload(&path.project_id, request).await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::CREATED, Json(payload)).into_response(),
        Err(err) => file_error_response(err),
    }
}

async fn preview_file(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Query(request): Query<FilePathQuery>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        service
            .preview(&path.project_id, request.path.as_deref().unwrap_or(""))
            .await
    }
    .await;

    match result {
        Ok(payload) => {
            let mut response = Body::from(payload.bytes).into_response();
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_str(&payload.mime_type)
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );
            response.headers_mut().insert(
                CONTENT_LENGTH,
                HeaderValue::from_str(&payload.size.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );
            response.headers_mut().insert(
                "x-llm-wiki-file-version",
                HeaderValue::from_str(&payload.version)
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            response
        }
        Err(err) => file_error_response(err),
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

fn file_error_response(err: FileServiceError) -> axum::response::Response {
    match err {
        FileServiceError::SetupRequired => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                ok: false,
                error: "Server setup is required before files are available".to_string(),
            }),
        )
            .into_response(),
        FileServiceError::ProjectNotFound => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                ok: false,
                error: "Project was not found".to_string(),
            }),
        )
            .into_response(),
        FileServiceError::InvalidInput(message) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        FileServiceError::NotFound(message) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        FileServiceError::Conflict(message) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        FileServiceError::PreconditionRequired(message) => (
            StatusCode::PRECONDITION_REQUIRED,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        FileServiceError::PreconditionFailed(message) => (
            StatusCode::PRECONDITION_FAILED,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        FileServiceError::PayloadTooLarge(message) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(ErrorResponse {
                ok: false,
                error: message,
            }),
        )
            .into_response(),
        FileServiceError::Internal(err) => internal_server_error(err),
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
    use axum::http::{header::CONTENT_TYPE, HeaderMap, Method, Request, StatusCode};
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
        let payload: Value = serde_json::from_str(&body).unwrap_or_else(|err| {
            panic!("expected JSON response, status={status}, body={body:?}, error={err}")
        });
        (status, body, payload)
    }

    async fn request_bytes(
        state: AppState,
        method: Method,
        uri: &str,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, body.to_vec())
    }

    async fn configured_state(temp: &tempfile::TempDir) -> (AppState, crate::config::ServerPaths) {
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state =
            AppState::with_config_db(Some(paths.clone()), Some(db), temp.path().join("dist"));
        (state, paths)
    }

    async fn create_test_project(state: AppState, name: &str) -> (String, String) {
        let (status, _body, payload) = request_json(
            state,
            Method::POST,
            "/api/projects",
            Some(json!({ "name": name })),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        (
            payload["project"]["id"].as_str().unwrap().to_string(),
            payload["project"]["relativePath"]
                .as_str()
                .unwrap()
                .to_string(),
        )
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
    async fn file_routes_write_read_tree_preview_and_trash_without_leaking_paths() {
        let temp = tempfile::tempdir().unwrap();
        let (state, paths) = configured_state(&temp).await;
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let (project_id, relative_project_path) =
            create_test_project(state.clone(), "Files Project").await;
        let project_root = paths.data_root().join(relative_project_path);

        let write_uri = format!("/api/projects/{project_id}/files/write");
        let (status, body, payload) = request_json(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Notes\n"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["file"]["path"], "wiki/notes.md");
        assert_eq!(payload["file"]["mimeType"], "text/markdown");
        assert!(!body.contains(&data_root));
        let version = payload["file"]["version"].as_str().unwrap().to_string();

        let read_uri = format!("/api/projects/{project_id}/files/read?path=wiki/notes.md");
        let (status, body, payload) =
            request_json(state.clone(), Method::GET, &read_uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["contents"], "# Notes\n");
        assert_eq!(payload["file"]["version"], version);
        assert!(!body.contains(&data_root));

        let (status, _body, payload) = request_json(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Missing precondition\n"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(payload["ok"], false);

        let (status, _body, payload) = request_json(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Stale\n",
                "expectedVersion": "stale"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(payload["ok"], false);

        let (status, _body, payload) = request_json(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Updated Notes\n",
                "expectedVersion": version
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["file"]["size"], 16);

        let tree_uri = format!("/api/projects/{project_id}/files/tree?path=wiki&maxDepth=1");
        let (status, body, payload) =
            request_json(state.clone(), Method::GET, &tree_uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let nodes = payload["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|node| node["path"] == "wiki/notes.md"));
        assert!(!body.contains(&data_root));

        let preview_uri = format!("/api/projects/{project_id}/files/preview?path=wiki/notes.md");
        let (status, headers, body) = request_bytes(state.clone(), Method::GET, &preview_uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(CONTENT_TYPE).unwrap().to_str().unwrap(),
            "text/markdown"
        );
        assert_eq!(String::from_utf8(body).unwrap(), "# Updated Notes\n");

        let delete_uri = format!("/api/projects/{project_id}/files?path=wiki/notes.md");
        let (status, body, payload) = request_json(state, Method::DELETE, &delete_uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["path"], "wiki/notes.md");
        let trash_path = payload["trashPath"].as_str().unwrap();
        assert!(trash_path.starts_with(".llm-wiki/trash/"));
        assert!(!project_root.join("wiki/notes.md").exists());
        assert!(project_root.join(trash_path).is_file());
        assert!(!body.contains(&data_root));
    }

    #[tokio::test]
    async fn file_routes_reject_project_relative_escape_without_leaking_paths() {
        let temp = tempfile::tempdir().unwrap();
        let (state, paths) = configured_state(&temp).await;
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let (project_id, _relative_project_path) =
            create_test_project(state.clone(), "Escape Project").await;

        let uri =
            format!("/api/projects/{project_id}/files/read?path=../.llm-wiki-server/config.sqlite");
        let (status, body, payload) = request_json(state, Method::GET, &uri, None).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
        assert!(payload["error"]
            .as_str()
            .unwrap()
            .contains("parent traversal"));
        assert!(!body.contains(&data_root));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_routes_reject_and_report_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let (state, paths) = configured_state(&temp).await;
        let (project_id, relative_project_path) =
            create_test_project(state.clone(), "Symlink Project").await;
        let project_root = paths.data_root().join(relative_project_path);
        let outside = temp.path().join("outside-secret.md");
        std::fs::write(&outside, "# Secret").unwrap();
        symlink(&outside, project_root.join("wiki/linked.md")).unwrap();

        let read_uri = format!("/api/projects/{project_id}/files/read?path=wiki/linked.md");
        let (status, body, payload) =
            request_json(state.clone(), Method::GET, &read_uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
        assert!(!body.contains(outside.to_str().unwrap()));

        let tree_uri =
            format!("/api/projects/{project_id}/files/tree?path=wiki&includeHidden=true");
        let (status, _body, payload) =
            request_json(state.clone(), Method::GET, &tree_uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["skippedSymlinks"], json!(["wiki/linked.md"]));
        assert!(!payload["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["path"] == "wiki/linked.md"));

        let delete_uri = format!("/api/projects/{project_id}/files?path=wiki");
        let (status, body, payload) = request_json(state, Method::DELETE, &delete_uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
        assert!(payload["error"]
            .as_str()
            .unwrap()
            .contains("wiki/linked.md"));
        assert!(project_root.join("wiki/linked.md").exists());
        assert!(!body.contains(outside.to_str().unwrap()));
    }

    #[tokio::test]
    async fn upload_files_skip_same_hash_and_rename_different_content() {
        let temp = tempfile::tempdir().unwrap();
        let (state, paths) = configured_state(&temp).await;
        let (project_id, relative_project_path) =
            create_test_project(state.clone(), "Upload Project").await;
        let project_root = paths.data_root().join(relative_project_path);
        let upload_uri = format!("/api/projects/{project_id}/files/upload");

        let (status, _body, payload) = request_json(
            state.clone(),
            Method::POST,
            &upload_uri,
            Some(json!({
                "files": [{
                    "fileName": "paper.txt",
                    "contentBase64": "YWxwaGE="
                }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(payload["files"][0]["path"], "raw/sources/paper.txt");
        assert_eq!(payload["files"][0]["skipped"], false);

        let (status, _body, payload) = request_json(
            state.clone(),
            Method::POST,
            &upload_uri,
            Some(json!({
                "files": [{
                    "fileName": "paper.txt",
                    "contentBase64": "YWxwaGE="
                }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(payload["files"][0]["path"], "raw/sources/paper.txt");
        assert_eq!(payload["files"][0]["skipped"], true);
        assert_eq!(payload["files"][0]["reason"], "same_hash");

        let (status, _body, payload) = request_json(
            state,
            Method::POST,
            &upload_uri,
            Some(json!({
                "files": [{
                    "fileName": "paper.txt",
                    "contentBase64": "YmV0YQ=="
                }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(payload["files"][0]["path"], "raw/sources/paper-1.txt");
        assert_eq!(payload["files"][0]["skipped"], false);
        assert_eq!(
            std::fs::read_to_string(project_root.join("raw/sources/paper.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            std::fs::read_to_string(project_root.join("raw/sources/paper-1.txt")).unwrap(),
            "beta"
        );
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
