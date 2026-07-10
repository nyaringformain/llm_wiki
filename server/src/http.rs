use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, SET_COOKIE, USER_AGENT,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

use crate::auth::{
    expired_session_cookie, generate_session_token, hash_password, hash_session_token,
    owner_password_is_acceptable, session_cookie, verify_password, LOGIN_FAILURE_DELAY,
    SESSION_COOKIE_NAME,
};
use crate::config::{default_static_dir, server_paths_from_env, ServerPaths};
use crate::db::ConfigDb;
use crate::files::{
    FileService, FileServiceError, FileTreeRequest, UploadFilesRequest, WriteFileRequest,
};
use crate::projects::{
    ImportProjectResponse, ProjectResponse, ProjectService, ProjectServiceError,
};
use crate::search::{ContentServiceError, ProjectSearchRequest};
use crate::vectorstore::VectorSearchRequest;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordRequest {
    password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RotatePasswordRequest {
    current_password: String,
    new_password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthSessionResponse {
    ok: bool,
    authenticated: bool,
    setup_required: bool,
    expires_at: Option<String>,
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

    let protected_routes = Router::new()
        .route("/api/auth/password", put(auth_password))
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
        .route("/api/projects/{project_id}/search", post(search_project))
        .route("/api/projects/{project_id}/graph", get(project_graph))
        .route(
            "/api/projects/{project_id}/vectors/status",
            get(vector_status),
        )
        .route(
            "/api/projects/{project_id}/vectors/search",
            post(vector_search),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_owner_session,
        ));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/setup/status", get(setup_status))
        .route("/api/auth/setup", post(auth_setup))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/auth/session", get(auth_session))
        .merge(protected_routes)
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

async fn auth_setup(
    State(state): State<AppState>,
    Json(payload): Json<PasswordRequest>,
) -> Response {
    let Some(db) = state.config_db.clone() else {
        return error_response(
            StatusCode::CONFLICT,
            "Data Root and config database must be ready before owner setup",
        );
    };

    if !owner_password_is_acceptable(&payload.password) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Password must be at least 8 characters",
        );
    }

    match state.setup_status().await {
        Ok(status) if status.owner_configured => {
            return error_response(StatusCode::CONFLICT, "Owner is already configured");
        }
        Ok(_) => {}
        Err(err) => return internal_server_error(err),
    }

    let password_hash = match hash_password(payload.password).await {
        Ok(password_hash) => password_hash,
        Err(err) => return internal_server_error(err),
    };

    match db.create_owner(&password_hash).await {
        Ok(true) => create_authenticated_session(&state, &db).await,
        Ok(false) => error_response(StatusCode::CONFLICT, "Owner is already configured"),
        Err(err) => internal_server_error(err),
    }
}

async fn auth_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<PasswordRequest>,
) -> Response {
    let Some(db) = state.config_db.clone() else {
        tokio::time::sleep(LOGIN_FAILURE_DELAY).await;
        return unauthorized_credentials();
    };

    let Some(password_hash) = (match db.owner_password_hash().await {
        Ok(password_hash) => password_hash,
        Err(err) => return internal_server_error(err),
    }) else {
        return failed_login(&db, &headers, "owner_not_configured").await;
    };

    match verify_password(payload.password, password_hash).await {
        Ok(true) => create_authenticated_session(&state, &db).await,
        Ok(false) => failed_login(&db, &headers, "invalid_password").await,
        Err(err) => internal_server_error(err),
    }
}

async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let (Some(db), Some(token)) = (
        state.config_db.clone(),
        session_token_from_headers(&headers),
    ) {
        let token_hash = hash_session_token(&token);
        if let Err(err) = db.revoke_session(&token_hash).await {
            return internal_server_error(err);
        }
    }

    let setup_required = match state.setup_status().await {
        Ok(setup) => setup.setup_required,
        Err(err) => return internal_server_error(err),
    };
    auth_json_response(
        AuthSessionResponse {
            ok: true,
            authenticated: false,
            setup_required,
            expires_at: None,
        },
        Some(expired_session_cookie()),
    )
}

async fn auth_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match session_response(&state, &headers).await {
        Ok((payload, clear_cookie)) => {
            auth_json_response(payload, clear_cookie.then(expired_session_cookie))
        }
        Err(err) => internal_server_error(err),
    }
}

async fn auth_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RotatePasswordRequest>,
) -> Response {
    if !owner_password_is_acceptable(&payload.new_password) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Password must be at least 8 characters",
        );
    }

    let Some(db) = state.config_db.clone() else {
        return authentication_required(false);
    };
    let Some(password_hash) = (match db.owner_password_hash().await {
        Ok(password_hash) => password_hash,
        Err(err) => return internal_server_error(err),
    }) else {
        return authentication_required(false);
    };

    match verify_password(payload.current_password, password_hash).await {
        Ok(true) => {}
        Ok(false) => {
            return failed_login(&db, &headers, "invalid_current_password").await;
        }
        Err(err) => return internal_server_error(err),
    }

    let new_password_hash = match hash_password(payload.new_password).await {
        Ok(password_hash) => password_hash,
        Err(err) => return internal_server_error(err),
    };
    match db.rotate_owner_password(&new_password_hash).await {
        Ok(true) => create_authenticated_session(&state, &db).await,
        Ok(false) => authentication_required(false),
        Err(err) => internal_server_error(err),
    }
}

async fn require_owner_session(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(db) = state.config_db.clone() else {
        return authentication_required(false);
    };
    let Some(token) = session_token_from_headers(request.headers()) else {
        return authentication_required(false);
    };

    match db.valid_session(&hash_session_token(&token)).await {
        Ok(Some(_)) => {
            let mut response = next.run(request).await;
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
            response
        }
        Ok(None) => authentication_required(true),
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

async fn search_project(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Json(request): Json<ProjectSearchRequest>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        crate::search::search_project(&service, &path.project_id, request).await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(error) => content_error_response(error),
    }
}

async fn project_graph(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        crate::graph::project_graph(&service, &path.project_id).await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(error) => content_error_response(error),
    }
}

async fn vector_status(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        crate::vectorstore::project_vector_status(&service, &path.project_id).await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(error) => content_error_response(error),
    }
}

async fn vector_search(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<ProjectFilePath>,
    Json(request): Json<VectorSearchRequest>,
) -> axum::response::Response {
    let result = async {
        let service = state.file_service()?;
        crate::vectorstore::search_project_vectors(&service, &path.project_id, request).await
    }
    .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(error) => content_error_response(error),
    }
}

async fn create_authenticated_session(state: &AppState, db: &ConfigDb) -> Response {
    let token = generate_session_token();
    let token_hash = hash_session_token(&token);

    match db.create_session(&token_hash).await {
        Ok(session) => {
            let setup_required = match state.setup_status().await {
                Ok(setup) => setup.setup_required,
                Err(err) => return internal_server_error(err),
            };
            auth_json_response(
                AuthSessionResponse {
                    ok: true,
                    authenticated: true,
                    setup_required,
                    expires_at: Some(session.expires_at),
                },
                Some(session_cookie(&token)),
            )
        }
        Err(err) => internal_server_error(err),
    }
}

async fn session_response(
    state: &AppState,
    headers: &HeaderMap,
) -> anyhow::Result<(AuthSessionResponse, bool)> {
    let setup = state.setup_status().await?;
    let Some(db) = state.config_db.clone() else {
        return Ok((unauthenticated_session(setup.setup_required), false));
    };
    let Some(token) = session_token_from_headers(headers) else {
        return Ok((unauthenticated_session(setup.setup_required), false));
    };

    let token_hash = hash_session_token(&token);
    let Some(session) = db.valid_session(&token_hash).await? else {
        return Ok((unauthenticated_session(setup.setup_required), true));
    };

    Ok((
        AuthSessionResponse {
            ok: true,
            authenticated: true,
            setup_required: setup.setup_required,
            expires_at: Some(session.expires_at),
        },
        false,
    ))
}

fn unauthenticated_session(setup_required: bool) -> AuthSessionResponse {
    AuthSessionResponse {
        ok: true,
        authenticated: false,
        setup_required,
        expires_at: None,
    }
}

async fn failed_login(db: &ConfigDb, headers: &HeaderMap, reason: &'static str) -> Response {
    let user_agent = header_to_str(headers, USER_AGENT);
    if let Err(err) = db.record_login_failure(None, user_agent, reason).await {
        return internal_server_error(err);
    }

    tokio::time::sleep(LOGIN_FAILURE_DELAY).await;
    unauthorized_credentials()
}

fn session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(COOKIE)?.to_str().ok()?;
    cookie_header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE_NAME)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty())
}

fn header_to_str(headers: &HeaderMap, name: axum::http::header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn auth_json_response(payload: AuthSessionResponse, cookie: Option<String>) -> Response {
    let mut response = (StatusCode::OK, Json(payload)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(cookie) = cookie {
        response.headers_mut().insert(
            SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("auth cookie only contains ASCII-safe data"),
        );
    }
    response
}

fn unauthorized_credentials() -> Response {
    let mut response = error_response(StatusCode::UNAUTHORIZED, "Invalid credentials");
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn authentication_required(clear_cookie: bool) -> Response {
    let mut response = error_response(StatusCode::UNAUTHORIZED, "Authentication required");
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if clear_cookie {
        response.headers_mut().insert(
            SET_COOKIE,
            HeaderValue::from_str(&expired_session_cookie())
                .expect("expired auth cookie only contains ASCII-safe data"),
        );
    }
    response
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

fn error_response(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            ok: false,
            error: error.into(),
        }),
    )
        .into_response()
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

fn content_error_response(error: ContentServiceError) -> axum::response::Response {
    match error {
        ContentServiceError::Project(error) => file_error_response(error),
        ContentServiceError::InvalidInput(message)
        | ContentServiceError::InvalidProject(message) => {
            error_response(StatusCode::BAD_REQUEST, message)
        }
        ContentServiceError::Internal(error) => internal_server_error(error),
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
    use std::time::Instant;

    use axum::body::{to_bytes, Body};
    use axum::http::{
        header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, SET_COOKIE, USER_AGENT},
        HeaderMap, Method, Request, StatusCode,
    };
    use axum::response::Response;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::{router, AppState};
    use crate::auth::{
        generate_session_token, hash_session_token, LOGIN_FAILURE_DELAY, SESSION_COOKIE_NAME,
    };
    use crate::config::prepare_server_paths;
    use crate::db::ConfigDb;

    async fn request_json_as_owner(
        state: AppState,
        method: Method,
        uri: &str,
        body: Option<Value>,
        cookie: &str,
    ) -> (StatusCode, String, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(COOKIE, cookie);
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
        cookie: &str,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(COOKIE, cookie)
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

    async fn configured_state(
        temp: &tempfile::TempDir,
    ) -> (AppState, crate::config::ServerPaths, String) {
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        db.create_owner("test-only-password-hash").await.unwrap();
        let token = generate_session_token();
        db.create_session(&hash_session_token(&token))
            .await
            .unwrap();
        let cookie = format!("{SESSION_COOKIE_NAME}={token}");
        let state =
            AppState::with_config_db(Some(paths.clone()), Some(db), temp.path().join("dist"));
        (state, paths, cookie)
    }

    async fn create_test_project(state: AppState, name: &str, cookie: &str) -> (String, String) {
        let (status, _body, payload) = request_json_as_owner(
            state,
            Method::POST,
            "/api/projects",
            Some(json!({ "name": name })),
            cookie,
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

    async fn post_password(state: AppState, uri: &str, password: &str) -> Response {
        router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "password": password }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn get_with_cookie(state: AppState, uri: &str, cookie: &str) -> Response {
        router(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn session_cookie_from_response(response: &Response) -> String {
        let set_cookie = response
            .headers()
            .get(SET_COOKIE)
            .expect("authenticated response must set a session cookie")
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        assert!(set_cookie.contains("Max-Age=86400"));
        assert!(response
            .headers()
            .get(CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap()
            .split(',')
            .any(|directive| directive.trim() == "no-store"));
        set_cookie.split(';').next().unwrap().to_owned()
    }

    async fn response_json(response: Response) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
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
    async fn owner_setup_hashes_password_and_starts_fixed_24_hour_session() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state =
            AppState::with_config_db(Some(paths), Some(db.clone()), temp.path().join("dist"));

        let too_short = post_password(state.clone(), "/api/auth/setup", "short").await;
        assert_eq!(too_short.status(), StatusCode::BAD_REQUEST);

        let response = post_password(state.clone(), "/api/auth/setup", "correct horse").await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = session_cookie_from_response(&response);
        let token = cookie.split_once('=').unwrap().1;
        let payload = response_json(response).await;
        assert_eq!(payload["authenticated"], true);
        assert_eq!(payload["setupRequired"], false);
        assert!(payload["expiresAt"].as_str().unwrap().ends_with('Z'));

        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM owner_auth WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_ne!(stored_hash, "correct horse");
        assert!(stored_hash.starts_with("$argon2"));

        let stored_token_hash: String =
            sqlx::query_scalar("SELECT token_hash FROM owner_sessions LIMIT 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_ne!(stored_token_hash, token);
        assert_eq!(stored_token_hash, hash_session_token(token));
        assert_eq!(stored_token_hash.len(), 64);

        let ttl_seconds: f64 = sqlx::query_scalar(
            r#"
            SELECT (julianday(expires_at) - julianday(created_at)) * 86400.0
            FROM owner_sessions
            "#,
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!((86_300.0..=86_500.0).contains(&ttl_seconds));

        let expiry_before: String =
            sqlx::query_scalar("SELECT expires_at FROM owner_sessions LIMIT 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let session = get_with_cookie(state.clone(), "/api/auth/session", &cookie).await;
        assert_eq!(session.status(), StatusCode::OK);
        assert_eq!(response_json(session).await["authenticated"], true);
        let expiry_after: String =
            sqlx::query_scalar("SELECT expires_at FROM owner_sessions LIMIT 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(expiry_before, expiry_after);

        let second = post_password(state, "/api/auth/setup", "another password").await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn login_failure_is_delayed_logged_and_does_not_echo_password() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state =
            AppState::with_config_db(Some(paths), Some(db.clone()), temp.path().join("dist"));
        assert_eq!(
            post_password(state.clone(), "/api/auth/setup", "correct horse")
                .await
                .status(),
            StatusCode::OK
        );

        let started = Instant::now();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/login")
                    .header(CONTENT_TYPE, "application/json")
                    .header(USER_AGENT, "llm-wiki-auth-test")
                    .body(Body::from(
                        json!({ "password": "wrong password" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(started.elapsed() >= LOGIN_FAILURE_DELAY);
        assert!(response.headers().get(SET_COOKIE).is_none());
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "Invalid credentials");
        assert!(!payload.to_string().contains("wrong password"));

        let failure: (String, String) = sqlx::query_as(
            "SELECT reason, user_agent FROM login_failures ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(failure.0, "invalid_password");
        assert_eq!(failure.1, "llm-wiki-auth-test");
    }

    #[tokio::test]
    async fn login_logout_and_session_status_use_revocable_cookie_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state = AppState::with_config_db(Some(paths), Some(db), temp.path().join("dist"));
        assert_eq!(
            post_password(state.clone(), "/api/auth/setup", "correct horse")
                .await
                .status(),
            StatusCode::OK
        );

        let login = post_password(state.clone(), "/api/auth/login", "correct horse").await;
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = session_cookie_from_response(&login);

        let session = get_with_cookie(state.clone(), "/api/auth/session", &cookie).await;
        assert_eq!(response_json(session).await["authenticated"], true);

        let logout = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/logout")
                    .header(COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::OK);
        let clear_cookie = logout.headers().get(SET_COOKIE).unwrap().to_str().unwrap();
        assert!(clear_cookie.contains("Max-Age=0"));
        assert!(clear_cookie.contains("HttpOnly"));
        assert!(clear_cookie.contains("SameSite=Strict"));

        let session = get_with_cookie(state, "/api/auth/session", &cookie).await;
        assert_eq!(response_json(session).await["authenticated"], false);
    }

    #[tokio::test]
    async fn password_rotation_revokes_old_sessions_and_accepts_only_new_password() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state =
            AppState::with_config_db(Some(paths), Some(db.clone()), temp.path().join("dist"));
        let setup = post_password(state.clone(), "/api/auth/setup", "correct horse").await;
        let setup_cookie = session_cookie_from_response(&setup);
        let login = post_password(state.clone(), "/api/auth/login", "correct horse").await;
        let login_cookie = session_cookie_from_response(&login);

        let rotated = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/auth/password")
                    .header(COOKIE, &setup_cookie)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "currentPassword": "correct horse",
                            "newPassword": "new correct horse"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rotated.status(), StatusCode::OK);
        let rotated_cookie = session_cookie_from_response(&rotated);

        for old_cookie in [&setup_cookie, &login_cookie] {
            let session = get_with_cookie(state.clone(), "/api/auth/session", old_cookie).await;
            assert_eq!(response_json(session).await["authenticated"], false);
        }
        let session = get_with_cookie(state.clone(), "/api/auth/session", &rotated_cookie).await;
        assert_eq!(response_json(session).await["authenticated"], true);

        let old_login = post_password(state.clone(), "/api/auth/login", "correct horse").await;
        assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);
        let new_login = post_password(state, "/api/auth/login", "new correct horse").await;
        assert_eq!(new_login.status(), StatusCode::OK);

        let active_sessions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM owner_sessions WHERE revoked_at IS NULL")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(active_sessions, 2);
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
    async fn every_project_and_file_route_requires_owner_session() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _paths, _cookie) = configured_state(&temp).await;
        let routes = [
            (Method::GET, "/api/projects"),
            (Method::POST, "/api/projects"),
            (Method::POST, "/api/projects/import"),
            (Method::GET, "/api/projects/project-id/files/tree"),
            (Method::GET, "/api/projects/project-id/files/read"),
            (Method::PUT, "/api/projects/project-id/files/write"),
            (Method::DELETE, "/api/projects/project-id/files"),
            (Method::POST, "/api/projects/project-id/files/upload"),
            (Method::GET, "/api/projects/project-id/files/preview"),
            (Method::POST, "/api/projects/project-id/search"),
            (Method::GET, "/api/projects/project-id/graph"),
            (Method::GET, "/api/projects/project-id/vectors/status"),
            (Method::POST, "/api/projects/project-id/vectors/search"),
        ];

        for (method, uri) in routes {
            let response = router(state.clone())
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} must require an owner session"
            );
            let payload = response_json(response).await;
            assert_eq!(payload["error"], "Authentication required");
            assert!(!payload.to_string().contains(temp.path().to_str().unwrap()));
        }
    }

    #[tokio::test]
    async fn invalid_expired_and_revoked_sessions_cannot_reach_protected_routes() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _paths, valid_cookie) = configured_state(&temp).await;
        let db = state.config_db.clone().unwrap();

        let invalid = get_with_cookie(
            state.clone(),
            "/api/projects",
            &format!("{SESSION_COOKIE_NAME}=not-a-session"),
        )
        .await;
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
        assert!(invalid
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));

        let token = valid_cookie.split_once('=').unwrap().1;
        sqlx::query(
            "UPDATE owner_sessions SET expires_at = datetime(CURRENT_TIMESTAMP, '-1 second') WHERE token_hash = ?",
        )
        .bind(hash_session_token(token))
        .execute(db.pool())
        .await
        .unwrap();
        let expired = get_with_cookie(state.clone(), "/api/projects", &valid_cookie).await;
        assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
        let expired_status =
            get_with_cookie(state.clone(), "/api/auth/session", &valid_cookie).await;
        assert_eq!(response_json(expired_status).await["authenticated"], false);

        let revoked_token = generate_session_token();
        let revoked_hash = hash_session_token(&revoked_token);
        db.create_session(&revoked_hash).await.unwrap();
        db.revoke_session(&revoked_hash).await.unwrap();
        let revoked_cookie = format!("{SESSION_COOKIE_NAME}={revoked_token}");
        let revoked = get_with_cookie(state, "/api/projects", &revoked_cookie).await;
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_project_registers_data_root_child_without_leaking_paths() {
        let temp = tempfile::tempdir().unwrap();
        let (state, paths, cookie) = configured_state(&temp).await;
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let project_root = paths.data_root().join("my-research-project");

        let (status, body, payload) = request_json_as_owner(
            state.clone(),
            Method::POST,
            "/api/projects",
            Some(json!({ "name": "My Research Project" })),
            &cookie,
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

        let (status, body, payload) =
            request_json_as_owner(state, Method::GET, "/api/projects", None, &cookie).await;
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

        let (state, paths, cookie) = configured_state(&temp).await;
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let project_root = paths.data_root().join("imported-project");
        let source_path = external.to_string_lossy().into_owned();

        let (status, body, payload) = request_json_as_owner(
            state,
            Method::POST,
            "/api/projects/import",
            Some(json!({
                "sourcePath": source_path,
                "name": "Imported Project"
            })),
            &cookie,
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
        let (state, paths, cookie) = configured_state(&temp).await;
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let (project_id, relative_project_path) =
            create_test_project(state.clone(), "Files Project", &cookie).await;
        let project_root = paths.data_root().join(relative_project_path);

        let write_uri = format!("/api/projects/{project_id}/files/write");
        let (status, body, payload) = request_json_as_owner(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Notes\n"
            })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["file"]["path"], "wiki/notes.md");
        assert_eq!(payload["file"]["mimeType"], "text/markdown");
        assert!(!body.contains(&data_root));
        let version = payload["file"]["version"].as_str().unwrap().to_string();

        let read_uri = format!("/api/projects/{project_id}/files/read?path=wiki/notes.md");
        let (status, body, payload) =
            request_json_as_owner(state.clone(), Method::GET, &read_uri, None, &cookie).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["contents"], "# Notes\n");
        assert_eq!(payload["file"]["version"], version);
        assert!(!body.contains(&data_root));

        let (status, _body, payload) = request_json_as_owner(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Missing precondition\n"
            })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(payload["ok"], false);

        let (status, _body, payload) = request_json_as_owner(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Stale\n",
                "expectedVersion": "stale"
            })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(payload["ok"], false);

        let (status, _body, payload) = request_json_as_owner(
            state.clone(),
            Method::PUT,
            &write_uri,
            Some(json!({
                "path": "wiki/notes.md",
                "contents": "# Updated Notes\n",
                "expectedVersion": version
            })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["file"]["size"], 16);

        let tree_uri = format!("/api/projects/{project_id}/files/tree?path=wiki&maxDepth=1");
        let (status, body, payload) =
            request_json_as_owner(state.clone(), Method::GET, &tree_uri, None, &cookie).await;
        assert_eq!(status, StatusCode::OK);
        let nodes = payload["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|node| node["path"] == "wiki/notes.md"));
        assert!(!body.contains(&data_root));

        let preview_uri = format!("/api/projects/{project_id}/files/preview?path=wiki/notes.md");
        let (status, headers, body) =
            request_bytes(state.clone(), Method::GET, &preview_uri, &cookie).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(CONTENT_TYPE).unwrap().to_str().unwrap(),
            "text/markdown"
        );
        assert_eq!(
            headers.get(CACHE_CONTROL).unwrap().to_str().unwrap(),
            "private, no-store"
        );
        assert_eq!(String::from_utf8(body).unwrap(), "# Updated Notes\n");

        let delete_uri = format!("/api/projects/{project_id}/files?path=wiki/notes.md");
        let (status, body, payload) =
            request_json_as_owner(state, Method::DELETE, &delete_uri, None, &cookie).await;
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
        let (state, paths, cookie) = configured_state(&temp).await;
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let (project_id, _relative_project_path) =
            create_test_project(state.clone(), "Escape Project", &cookie).await;

        let uri =
            format!("/api/projects/{project_id}/files/read?path=../.llm-wiki-server/config.sqlite");
        let (status, body, payload) =
            request_json_as_owner(state, Method::GET, &uri, None, &cookie).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
        assert!(payload["error"]
            .as_str()
            .unwrap()
            .contains("parent traversal"));
        assert!(!body.contains(&data_root));
    }

    #[tokio::test]
    async fn search_graph_and_vector_status_are_project_scoped_and_path_private() {
        let temp = tempfile::tempdir().unwrap();
        let (state, paths, cookie) = configured_state(&temp).await;
        let data_root = paths.data_root().to_string_lossy().into_owned();
        let (project_id, relative_project_path) =
            create_test_project(state.clone(), "Knowledge Project", &cookie).await;
        let project_root = paths.data_root().join(relative_project_path);
        std::fs::write(
            project_root.join("wiki/concepts/alpha.md"),
            "---\ntitle: Alpha Concept\ntype: concept\nsources: [paper.pdf]\n---\n# Alpha\nVector retrieval links to [[beta]].\n",
        )
        .unwrap();
        std::fs::write(
            project_root.join("wiki/entities/beta.md"),
            "---\ntitle: Beta Entity\ntype: entity\nsources: [paper.pdf]\n---\n# Beta\nRelated material.\n",
        )
        .unwrap();
        std::fs::write(
            project_root.join("wiki/concepts/isolated.md"),
            "---\ntitle: Isolated\ntype: concept\n---\n# Isolated\n",
        )
        .unwrap();

        let search_uri = format!("/api/projects/{project_id}/search");
        let (status, body, payload) = request_json_as_owner(
            state.clone(),
            Method::POST,
            &search_uri,
            Some(json!({
                "query": "Alpha Concept",
                "topK": 20,
                "expandGraph": true
            })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["mode"], "graph");
        assert_eq!(payload["graphHits"], 1);
        assert!(payload["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|result| result["path"] == "wiki/entities/beta.md"));
        assert!(!body.contains(&data_root));

        let graph_uri = format!("/api/projects/{project_id}/graph");
        let (status, body, payload) =
            request_json_as_owner(state.clone(), Method::GET, &graph_uri, None, &cookie).await;
        assert_eq!(status, StatusCode::OK);
        assert!(payload["nodes"].as_array().unwrap().len() >= 3);
        assert!(!payload["edges"].as_array().unwrap().is_empty());
        assert!(payload["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["path"].as_str().unwrap().starts_with("wiki/")));
        assert!(!payload["insights"]["knowledgeGaps"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!body.contains(&data_root));

        let status_uri = format!("/api/projects/{project_id}/vectors/status");
        let (status, body, payload) =
            request_json_as_owner(state.clone(), Method::GET, &status_uri, None, &cookie).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["available"], false);
        assert_eq!(payload["chunkCount"], 0);
        assert!(!body.contains(&data_root));

        let vector_uri = format!("/api/projects/{project_id}/vectors/search");
        let (status, _body, payload) = request_json_as_owner(
            state,
            Method::POST,
            &vector_uri,
            Some(json!({ "queryEmbedding": [] })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_routes_reject_and_report_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let (state, paths, cookie) = configured_state(&temp).await;
        let (project_id, relative_project_path) =
            create_test_project(state.clone(), "Symlink Project", &cookie).await;
        let project_root = paths.data_root().join(relative_project_path);
        let outside = temp.path().join("outside-secret.md");
        std::fs::write(&outside, "# Secret").unwrap();
        symlink(&outside, project_root.join("wiki/linked.md")).unwrap();

        let read_uri = format!("/api/projects/{project_id}/files/read?path=wiki/linked.md");
        let (status, body, payload) =
            request_json_as_owner(state.clone(), Method::GET, &read_uri, None, &cookie).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
        assert!(!body.contains(outside.to_str().unwrap()));

        let tree_uri =
            format!("/api/projects/{project_id}/files/tree?path=wiki&includeHidden=true");
        let (status, _body, payload) =
            request_json_as_owner(state.clone(), Method::GET, &tree_uri, None, &cookie).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["skippedSymlinks"], json!(["wiki/linked.md"]));
        assert!(!payload["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["path"] == "wiki/linked.md"));

        let search_uri = format!("/api/projects/{project_id}/search");
        let (status, body, payload) = request_json_as_owner(
            state.clone(),
            Method::POST,
            &search_uri,
            Some(json!({ "query": "secret" })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
        assert!(!body.contains(outside.to_str().unwrap()));

        let graph_uri = format!("/api/projects/{project_id}/graph");
        let (status, body, payload) =
            request_json_as_owner(state.clone(), Method::GET, &graph_uri, None, &cookie).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(payload["ok"], false);
        assert!(!body.contains(outside.to_str().unwrap()));

        let delete_uri = format!("/api/projects/{project_id}/files?path=wiki");
        let (status, body, payload) =
            request_json_as_owner(state, Method::DELETE, &delete_uri, None, &cookie).await;
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
        let (state, paths, cookie) = configured_state(&temp).await;
        let (project_id, relative_project_path) =
            create_test_project(state.clone(), "Upload Project", &cookie).await;
        let project_root = paths.data_root().join(relative_project_path);
        let upload_uri = format!("/api/projects/{project_id}/files/upload");

        let (status, _body, payload) = request_json_as_owner(
            state.clone(),
            Method::POST,
            &upload_uri,
            Some(json!({
                "files": [{
                    "fileName": "paper.txt",
                    "contentBase64": "YWxwaGE="
                }]
            })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(payload["files"][0]["path"], "raw/sources/paper.txt");
        assert_eq!(payload["files"][0]["skipped"], false);

        let (status, _body, payload) = request_json_as_owner(
            state.clone(),
            Method::POST,
            &upload_uri,
            Some(json!({
                "files": [{
                    "fileName": "paper.txt",
                    "contentBase64": "YWxwaGE="
                }]
            })),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(payload["files"][0]["path"], "raw/sources/paper.txt");
        assert_eq!(payload["files"][0]["skipped"], true);
        assert_eq!(payload["files"][0]["reason"], "same_hash");

        let (status, _body, payload) = request_json_as_owner(
            state,
            Method::POST,
            &upload_uri,
            Some(json!({
                "files": [{
                    "fileName": "paper.txt",
                    "contentBase64": "YmV0YQ=="
                }]
            })),
            &cookie,
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
