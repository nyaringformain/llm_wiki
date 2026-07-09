use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{FromRow, SqlitePool};

use crate::config::ServerPaths;

#[derive(Clone)]
pub struct ConfigDb {
    pool: SqlitePool,
}

impl ConfigDb {
    pub async fn open(paths: &ServerPaths) -> anyhow::Result<Self> {
        let db_path = paths.config_db_path();
        reject_config_db_symlinks(&db_path)?;

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .with_context(|| format!("failed to open config database {}", db_path.display()))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply config database migrations")?;

        restrict_config_db_permissions(&db_path)?;

        Ok(Self { pool })
    }

    pub async fn record_data_root(&self, paths: &ServerPaths) -> anyhow::Result<()> {
        let data_root = paths.data_root().to_string_lossy();

        sqlx::query(
            r#"
            INSERT INTO server_settings (key, value, updated_at)
            VALUES ('data_root', ?, CURRENT_TIMESTAMP)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(data_root.as_ref())
        .execute(&self.pool)
        .await
        .context("failed to record configured Data Root")?;

        Ok(())
    }

    pub async fn setup_state(&self) -> anyhow::Result<SetupState> {
        let owner_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owner_auth")
            .fetch_one(&self.pool)
            .await
            .context("failed to read owner setup state")?;

        Ok(SetupState {
            owner_configured: owner_count > 0,
        })
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<ProjectRecord>> {
        sqlx::query_as::<_, ProjectRecord>(
            r#"
            SELECT id, name, relative_path, source, created_at, updated_at
            FROM project_registry
            ORDER BY lower(name), name
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list project registry")
    }

    pub async fn project_id_exists(&self, id: &str) -> anyhow::Result<bool> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_registry WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .context("failed to check project id")?;

        Ok(count > 0)
    }

    pub async fn project_relative_path_exists(&self, relative_path: &str) -> anyhow::Result<bool> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM project_registry WHERE relative_path = ?")
                .bind(relative_path)
                .fetch_one(&self.pool)
                .await
                .context("failed to check project relative path")?;

        Ok(count > 0)
    }

    pub async fn register_project(
        &self,
        project: NewProjectRecord,
    ) -> anyhow::Result<ProjectRecord> {
        sqlx::query(
            r#"
            INSERT INTO project_registry (id, name, relative_path, source)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(&project.id)
        .bind(&project.name)
        .bind(&project.relative_path)
        .bind(&project.source)
        .execute(&self.pool)
        .await
        .context("failed to register project")?;

        self.project_by_id(&project.id)
            .await?
            .context("registered project could not be read back")
    }

    async fn project_by_id(&self, id: &str) -> anyhow::Result<Option<ProjectRecord>> {
        sqlx::query_as::<_, ProjectRecord>(
            r#"
            SELECT id, name, relative_path, source, created_at, updated_at
            FROM project_registry
            WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to read project registry entry")
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[derive(Debug, Clone)]
pub struct NewProjectRecord {
    pub id: String,
    pub name: String,
    pub relative_path: String,
    pub source: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub relative_path: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupState {
    pub owner_configured: bool,
}

impl SetupState {
    pub fn setup_required(&self) -> bool {
        !self.owner_configured
    }
}

#[cfg(unix)]
fn restrict_config_db_permissions(db_path: &Path) -> anyhow::Result<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    for path in sqlite_file_set(db_path) {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions)
            .with_context(|| format!("failed to restrict config database {}", path.display()))?;
    }

    Ok(())
}

#[cfg(not(unix))]
fn restrict_config_db_permissions(_db_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn reject_config_db_symlinks(db_path: &Path) -> anyhow::Result<()> {
    for path in sqlite_file_set(db_path) {
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };

        if metadata.file_type().is_symlink() {
            anyhow::bail!("config database {} must not be a symlink", path.display());
        }
    }

    Ok(())
}

fn sqlite_file_set(db_path: &Path) -> [PathBuf; 3] {
    [
        db_path.to_path_buf(),
        sqlite_sidecar_path(db_path, "-wal"),
        sqlite_sidecar_path(db_path, "-shm"),
    ]
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::ConfigDb;
    use crate::config::prepare_server_paths;

    #[tokio::test]
    async fn opens_config_database_and_reports_setup_required_without_owner() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();

        let setup = db.setup_state().await.unwrap();

        assert!(paths.config_db_path().is_file());
        assert!(!setup.owner_configured);
        assert!(setup.setup_required());
    }

    #[tokio::test]
    async fn setup_state_reports_ready_after_owner_exists() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();

        sqlx::query("INSERT INTO owner_auth (id, password_hash) VALUES (1, ?)")
            .bind("argon2-placeholder")
            .execute(db.pool())
            .await
            .unwrap();

        let setup = db.setup_state().await.unwrap();

        assert!(setup.owner_configured);
        assert!(!setup.setup_required());
    }

    #[tokio::test]
    async fn records_data_root_in_server_settings() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();

        db.record_data_root(&paths).await.unwrap();

        let stored: String =
            sqlx::query_scalar("SELECT value FROM server_settings WHERE key = 'data_root'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(stored, paths.data_root().to_string_lossy());
    }

    #[tokio::test]
    async fn registers_and_lists_projects() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let db = ConfigDb::open(&paths).await.unwrap();

        let registered = db
            .register_project(super::NewProjectRecord {
                id: "project-id".to_string(),
                name: "Project Name".to_string(),
                relative_path: "project-name".to_string(),
                source: "created".to_string(),
            })
            .await
            .unwrap();
        let projects = db.list_projects().await.unwrap();

        assert_eq!(registered.id, "project-id");
        assert_eq!(registered.relative_path, "project-name");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Project Name");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn config_database_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        ConfigDb::open(&paths).await.unwrap();

        let mode = std::fs::metadata(paths.config_db_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn config_database_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let paths = prepare_server_paths(&temp.path().join("data-root")).unwrap();
        let outside = temp.path().join("outside.sqlite");
        std::fs::write(&outside, "").unwrap();
        symlink(&outside, paths.config_db_path()).unwrap();

        let err = match ConfigDb::open(&paths).await {
            Ok(_) => panic!("config database symlink should be rejected"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("must not be a symlink"));
    }
}
