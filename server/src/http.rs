use std::path::PathBuf;

use axum::extract::State;
use axum::http::header::{COOKIE, SET_COOKIE, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordRequest {
    password: String,
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
    error: &'static str,
}

pub fn router(state: AppState) -> Router {
    let static_dir = state.static_dir.clone();
    let static_service =
        ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html")));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/setup/status", get(setup_status))
        .route("/api/auth/setup", post(auth_setup))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/auth/session", get(auth_session))
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
) -> impl IntoResponse {
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
) -> impl IntoResponse {
    let Some(db) = state.config_db.clone() else {
        tokio::time::sleep(LOGIN_FAILURE_DELAY).await;
        return unauthorized();
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

async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
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
    let payload = AuthSessionResponse {
        ok: true,
        authenticated: false,
        setup_required,
        expires_at: None,
    };
    json_with_set_cookie(payload, expired_session_cookie())
}

async fn auth_session(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    match session_response(&state, &headers).await {
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

async fn create_authenticated_session(state: &AppState, db: &ConfigDb) -> axum::response::Response {
    let token = generate_session_token();
    let token_hash = hash_session_token(&token);

    match db.create_session(&token_hash).await {
        Ok(session) => {
            let setup_required = match state.setup_status().await {
                Ok(setup) => setup.setup_required,
                Err(err) => return internal_server_error(err),
            };
            let payload = AuthSessionResponse {
                ok: true,
                authenticated: true,
                setup_required,
                expires_at: Some(session.expires_at),
            };
            json_with_set_cookie(payload, session_cookie(&token))
        }
        Err(err) => internal_server_error(err),
    }
}

async fn session_response(
    state: &AppState,
    headers: &HeaderMap,
) -> anyhow::Result<AuthSessionResponse> {
    let setup = state.setup_status().await?;
    let Some(db) = state.config_db.clone() else {
        return Ok(AuthSessionResponse {
            ok: true,
            authenticated: false,
            setup_required: setup.setup_required,
            expires_at: None,
        });
    };

    let Some(token) = session_token_from_headers(headers) else {
        return Ok(AuthSessionResponse {
            ok: true,
            authenticated: false,
            setup_required: setup.setup_required,
            expires_at: None,
        });
    };

    let token_hash = hash_session_token(&token);
    let Some(session) = db.valid_session(&token_hash).await? else {
        return Ok(AuthSessionResponse {
            ok: true,
            authenticated: false,
            setup_required: setup.setup_required,
            expires_at: None,
        });
    };

    Ok(AuthSessionResponse {
        ok: true,
        authenticated: true,
        setup_required: setup.setup_required,
        expires_at: Some(session.expires_at),
    })
}

async fn failed_login(
    db: &ConfigDb,
    headers: &HeaderMap,
    reason: &'static str,
) -> axum::response::Response {
    let user_agent = header_to_str(headers, USER_AGENT);
    if let Err(err) = db.record_login_failure(None, user_agent, reason).await {
        return internal_server_error(err);
    }

    tokio::time::sleep(LOGIN_FAILURE_DELAY).await;
    unauthorized()
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

fn json_with_set_cookie(payload: AuthSessionResponse, cookie: String) -> axum::response::Response {
    let mut response = (StatusCode::OK, Json(payload)).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("auth cookie only contains ASCII-safe data"),
    );
    response
}

fn unauthorized() -> axum::response::Response {
    error_response(StatusCode::UNAUTHORIZED, "Invalid password")
}

fn error_response(status: StatusCode, error: &'static str) -> axum::response::Response {
    (status, Json(ErrorResponse { ok: false, error })).into_response()
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
    use axum::http::header::{COOKIE, SET_COOKIE, USER_AGENT};
    use axum::http::{Method, StatusCode};
    use axum::response::Response;
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
    async fn owner_setup_hashes_password_and_starts_24_hour_session() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state =
            AppState::with_config_db(Some(paths), Some(db.clone()), temp.path().join("dist"));

        let response = post_password(state.clone(), "/api/auth/setup", "correct horse").await;

        assert_eq!(response.status(), StatusCode::OK);
        let cookie = session_cookie_from_response(&response);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["authenticated"], true);
        assert_eq!(payload["setupRequired"], false);
        assert!(payload["expiresAt"].as_str().is_some());

        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM owner_auth WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_ne!(stored_hash, "correct horse");
        assert!(stored_hash.starts_with("$argon2"));

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

        let response = get_with_cookie(state, "/api/auth/session", &cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["authenticated"], true);
        assert_eq!(payload["setupRequired"], false);
    }

    #[tokio::test]
    async fn owner_setup_rejects_second_owner() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state = AppState::with_config_db(Some(paths), Some(db), temp.path().join("dist"));

        let first = post_password(state.clone(), "/api/auth/setup", "correct horse").await;
        assert_eq!(first.status(), StatusCode::OK);

        let second = post_password(state, "/api/auth/setup", "another password").await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn login_failure_is_logged_and_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();
        let state =
            AppState::with_config_db(Some(paths), Some(db.clone()), temp.path().join("dist"));
        let setup = post_password(state.clone(), "/api/auth/setup", "correct horse").await;
        assert_eq!(setup.status(), StatusCode::OK);

        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .header(USER_AGENT, "llm-wiki-auth-test")
                    .body(axum::body::Body::from(r#"{"password":"wrong password"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(SET_COOKIE).is_none());
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
        let setup = post_password(state.clone(), "/api/auth/setup", "correct horse").await;
        assert_eq!(setup.status(), StatusCode::OK);

        let login = post_password(state.clone(), "/api/auth/login", "correct horse").await;
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = session_cookie_from_response(&login);

        let session = get_with_cookie(state.clone(), "/api/auth/session", &cookie).await;
        assert_eq!(session.status(), StatusCode::OK);
        let body = to_bytes(session.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["authenticated"], true);

        let logout = router(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/logout")
                    .header(COOKIE, &cookie)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::OK);
        let clear_cookie = logout.headers().get(SET_COOKIE).unwrap().to_str().unwrap();
        assert!(clear_cookie.contains("Max-Age=0"));

        let session = get_with_cookie(state, "/api/auth/session", &cookie).await;
        assert_eq!(session.status(), StatusCode::OK);
        let body = to_bytes(session.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["authenticated"], false);
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

    async fn post_password(state: AppState, uri: &str, password: &str) -> Response {
        router(state)
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(format!(
                        r#"{{"password":{}}}"#,
                        serde_json::to_string(password).unwrap()
                    )))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn get_with_cookie(state: AppState, uri: &str, cookie: &str) -> Response {
        router(state)
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(COOKIE, cookie)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn session_cookie_from_response(response: &Response) -> String {
        let set_cookie = response
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Lax"));
        assert!(set_cookie.contains("Max-Age=86400"));
        set_cookie.split(';').next().unwrap().to_owned()
    }
}
