use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const DATA_ROOT_ENV: &str = "LLM_WIKI_DATA_ROOT";
pub const SERVER_HOME_DIR: &str = ".llm-wiki-server";
pub const CONFIG_DB_FILE: &str = "config.sqlite";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPaths {
    data_root: PathBuf,
    server_home: PathBuf,
}

impl ServerPaths {
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn server_home(&self) -> &Path {
        &self.server_home
    }

    pub fn config_db_path(&self) -> PathBuf {
        self.server_home.join(CONFIG_DB_FILE)
    }
}

#[derive(Debug)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ConfigError {}

pub fn server_paths_from_env() -> Result<Option<ServerPaths>, ConfigError> {
    resolve_server_paths(env::var(DATA_ROOT_ENV).ok().as_deref())
}

pub fn resolve_server_paths(data_root: Option<&str>) -> Result<Option<ServerPaths>, ConfigError> {
    let Some(raw_data_root) = data_root.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let requested = PathBuf::from(raw_data_root);
    prepare_server_paths(&requested).map(Some)
}

pub fn prepare_server_paths(data_root: &Path) -> Result<ServerPaths, ConfigError> {
    if !data_root.is_absolute() {
        return Err(ConfigError::new(format!(
            "{DATA_ROOT_ENV} must be an absolute path"
        )));
    }

    fs::create_dir_all(data_root).map_err(|err| {
        ConfigError::new(format!(
            "failed to create Data Root {}: {err}",
            data_root.display()
        ))
    })?;

    let data_root = data_root.canonicalize().map_err(|err| {
        ConfigError::new(format!(
            "failed to canonicalize Data Root {}: {err}",
            data_root.display()
        ))
    })?;

    if !data_root.is_dir() {
        return Err(ConfigError::new(format!(
            "Data Root {} is not a directory",
            data_root.display()
        )));
    }

    let server_home = data_root.join(SERVER_HOME_DIR);
    reject_server_home_symlink(&server_home)?;
    fs::create_dir_all(&server_home).map_err(|err| {
        ConfigError::new(format!(
            "failed to create Server Home {}: {err}",
            server_home.display()
        ))
    })?;
    reject_server_home_symlink(&server_home)?;
    restrict_server_home_permissions(&server_home)?;

    let server_home = server_home.canonicalize().map_err(|err| {
        ConfigError::new(format!(
            "failed to canonicalize Server Home {}: {err}",
            server_home.display()
        ))
    })?;

    if !server_home.starts_with(&data_root) {
        return Err(ConfigError::new(format!(
            "Server Home must stay under the configured Data Root"
        )));
    }

    Ok(ServerPaths {
        data_root,
        server_home,
    })
}

pub fn default_static_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|repo_root| repo_root.join("dist"))
        .unwrap_or_else(|| PathBuf::from("dist"))
}

fn reject_server_home_symlink(server_home: &Path) -> Result<(), ConfigError> {
    let Ok(metadata) = fs::symlink_metadata(server_home) else {
        return Ok(());
    };

    if metadata.file_type().is_symlink() {
        return Err(ConfigError::new(format!(
            "Server Home {} must not be a symlink",
            server_home.display()
        )));
    }

    Ok(())
}

#[cfg(unix)]
fn restrict_server_home_permissions(server_home: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(server_home)
        .map_err(|err| {
            ConfigError::new(format!(
                "failed to inspect Server Home {}: {err}",
                server_home.display()
            ))
        })?
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(server_home, permissions).map_err(|err| {
        ConfigError::new(format!(
            "failed to restrict Server Home {}: {err}",
            server_home.display()
        ))
    })
}

#[cfg(not(unix))]
fn restrict_server_home_permissions(_server_home: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{prepare_server_paths, resolve_server_paths, CONFIG_DB_FILE, SERVER_HOME_DIR};

    #[test]
    fn missing_or_blank_data_root_is_unconfigured() {
        assert!(resolve_server_paths(None).unwrap().is_none());
        assert!(resolve_server_paths(Some("   ")).unwrap().is_none());
    }

    #[test]
    fn relative_data_root_is_rejected() {
        let err = resolve_server_paths(Some("relative/path")).unwrap_err();
        assert!(err.to_string().contains("absolute path"));
    }

    #[test]
    fn server_home_is_created_under_data_root() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data-root");
        let paths = prepare_server_paths(&data_root).unwrap();

        assert!(paths.data_root().is_dir());
        assert!(paths.server_home().is_dir());
        assert!(paths.server_home().starts_with(paths.data_root()));
        assert_eq!(
            paths
                .server_home()
                .file_name()
                .and_then(|name| name.to_str()),
            Some(SERVER_HOME_DIR)
        );
        assert_eq!(
            paths
                .config_db_path()
                .file_name()
                .and_then(|name| name.to_str()),
            Some(CONFIG_DB_FILE)
        );

        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(paths.server_home())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn server_home_symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data-root");
        let outside = temp.path().join("outside-home");
        std::fs::create_dir_all(&data_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, data_root.join(SERVER_HOME_DIR)).unwrap();

        let err = prepare_server_paths(&data_root).unwrap_err();
        assert!(err.to_string().contains("must not be a symlink"));
    }
}
