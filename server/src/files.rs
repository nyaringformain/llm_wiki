use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::ServerPaths;
use crate::db::ConfigDb;

const DEFAULT_TREE_DEPTH: usize = 30;
const MAX_TREE_DEPTH: usize = 30;
const DEFAULT_UPLOAD_DIR: &str = "raw/sources";
const TRASH_DIR: &str = ".llm-wiki/trash";
const UPLOAD_LIMIT_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct FileService {
    paths: ServerPaths,
    db: ConfigDb,
}

impl FileService {
    pub fn new(paths: ServerPaths, db: ConfigDb) -> Self {
        Self { paths, db }
    }

    pub async fn tree(
        &self,
        project_id: &str,
        request: FileTreeRequest,
    ) -> Result<FileTreeResponse, FileServiceError> {
        let root = self.project_root(project_id).await?;
        let relative = clean_relative_path(request.path.as_deref().unwrap_or(""), true)?;
        let metadata = inspect_project_path(&root, &relative, true)?
            .ok_or_else(|| FileServiceError::not_found("File path was not found"))?;
        if !metadata.is_dir() {
            return Err(FileServiceError::invalid_input(
                "File tree path must be a directory",
            ));
        }

        let mut skipped_symlinks = Vec::new();
        let max_depth = request
            .max_depth
            .unwrap_or(DEFAULT_TREE_DEPTH)
            .clamp(1, MAX_TREE_DEPTH);
        let nodes = build_tree(
            &root,
            &root.join(&relative),
            0,
            max_depth,
            request.include_hidden.unwrap_or(false),
            &mut skipped_symlinks,
        )?;

        Ok(FileTreeResponse {
            ok: true,
            path: browser_relative_path(&relative),
            nodes,
            skipped_symlinks,
        })
    }

    pub async fn read(
        &self,
        project_id: &str,
        path: &str,
    ) -> Result<FileReadResponse, FileServiceError> {
        let root = self.project_root(project_id).await?;
        let relative = clean_relative_path(path, false)?;
        let metadata = inspect_project_path(&root, &relative, true)?
            .ok_or_else(|| FileServiceError::not_found("File path was not found"))?;
        if metadata.is_dir() {
            return Err(FileServiceError::invalid_input(
                "File read path must be a file",
            ));
        }

        let bytes = fs::read(root.join(&relative)).context("failed to read project file")?;
        let contents = String::from_utf8(bytes).map_err(|_| {
            FileServiceError::invalid_input("File is not valid UTF-8; use the preview route")
        })?;

        Ok(FileReadResponse {
            ok: true,
            file: file_info(&root, &relative, &metadata),
            contents,
        })
    }

    pub async fn write(
        &self,
        project_id: &str,
        request: WriteFileRequest,
    ) -> Result<FileWriteResponse, FileServiceError> {
        let root = self.project_root(project_id).await?;
        let relative = clean_relative_path(&request.path, false)?;

        ensure_parent_directory(&root, &relative)?;
        let existing = inspect_project_path(&root, &relative, false)?;
        if let Some(metadata) = existing.as_ref() {
            if metadata.is_dir() {
                return Err(FileServiceError::invalid_input(
                    "File write path must be a file",
                ));
            }
            let expected = normalized_expected_version(request.expected_version.as_deref())
                .ok_or_else(|| {
                    FileServiceError::precondition_required(
                        "expectedVersion is required when overwriting an existing file",
                    )
                })?;
            let current = metadata_version(metadata);
            if expected != current {
                return Err(FileServiceError::precondition_failed(
                    "File changed since it was read",
                ));
            }
        } else if normalized_expected_version(request.expected_version.as_deref()).is_some() {
            return Err(FileServiceError::precondition_failed(
                "File does not exist for the supplied expectedVersion",
            ));
        }

        write_text_atomic(&root, &relative, &request.contents)?;
        let metadata = inspect_project_path(&root, &relative, true)?
            .ok_or_else(|| FileServiceError::not_found("File path was not found"))?;

        Ok(FileWriteResponse {
            ok: true,
            file: file_info(&root, &relative, &metadata),
        })
    }

    pub async fn delete(
        &self,
        project_id: &str,
        path: &str,
        expected_version: Option<&str>,
    ) -> Result<FileDeleteResponse, FileServiceError> {
        let root = self.project_root(project_id).await?;
        let relative = clean_relative_path(path, false)?;
        if is_trash_path(&relative) {
            return Err(FileServiceError::invalid_input(
                "Project trash paths cannot be deleted through this endpoint",
            ));
        }

        let metadata = inspect_project_path(&root, &relative, true)?
            .ok_or_else(|| FileServiceError::not_found("File path was not found"))?;
        if metadata.is_dir() {
            if let Some(path) = first_symlink_in_tree(&root, &relative)? {
                return Err(FileServiceError::invalid_input(format!(
                    "Project content symlinks are not allowed: {path}"
                )));
            }
        }
        if let Some(expected) = normalized_expected_version(expected_version) {
            let current = metadata_version(&metadata);
            if expected != current {
                return Err(FileServiceError::precondition_failed(
                    "File changed since it was read",
                ));
            }
        }

        let trash_relative = unique_trash_path(&root, &relative)?;
        ensure_parent_directory(&root, &trash_relative)?;
        fs::rename(root.join(&relative), root.join(&trash_relative))
            .context("failed to move file to project trash")?;

        Ok(FileDeleteResponse {
            ok: true,
            path: browser_relative_path(&relative),
            trash_path: browser_relative_path(&trash_relative),
        })
    }

    pub async fn upload(
        &self,
        project_id: &str,
        request: UploadFilesRequest,
    ) -> Result<UploadFilesResponse, FileServiceError> {
        if request.files.is_empty() {
            return Err(FileServiceError::invalid_input(
                "At least one file is required",
            ));
        }

        let root = self.project_root(project_id).await?;
        let upload_dir = upload_directory(request.directory.as_deref())?;
        ensure_directory_exists(&root, &upload_dir)?;

        let mut files = Vec::with_capacity(request.files.len());
        for file in request.files {
            files.push(write_uploaded_file(&root, &upload_dir, file)?);
        }

        Ok(UploadFilesResponse { ok: true, files })
    }

    pub async fn preview(
        &self,
        project_id: &str,
        path: &str,
    ) -> Result<FilePreviewResponse, FileServiceError> {
        let root = self.project_root(project_id).await?;
        let relative = clean_relative_path(path, false)?;
        let metadata = inspect_project_path(&root, &relative, true)?
            .ok_or_else(|| FileServiceError::not_found("File path was not found"))?;
        if metadata.is_dir() {
            return Err(FileServiceError::invalid_input(
                "File preview path must be a file",
            ));
        }

        let bytes = fs::read(root.join(&relative)).context("failed to read project file")?;
        Ok(FilePreviewResponse {
            bytes,
            mime_type: mime_type_for_path(&relative),
            version: metadata_version(&metadata),
            size: metadata.len(),
        })
    }

    async fn project_root(&self, project_id: &str) -> Result<PathBuf, FileServiceError> {
        let record = self
            .db
            .project_by_id(project_id)
            .await?
            .ok_or(FileServiceError::ProjectNotFound)?;
        let relative = clean_relative_path(&record.relative_path, false)?;
        let root = self.paths.data_root().join(relative);
        let metadata =
            fs::symlink_metadata(&root).map_err(|_| FileServiceError::ProjectNotFound)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(FileServiceError::invalid_input(
                "Project registry entry does not point to a valid project directory",
            ));
        }
        let canonical = root
            .canonicalize()
            .context("failed to resolve project directory")?;
        if !canonical.starts_with(self.paths.data_root()) {
            return Err(FileServiceError::invalid_input(
                "Project path escapes the configured Data Root",
            ));
        }
        Ok(root)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeRequest {
    pub path: Option<String>,
    pub include_hidden: Option<bool>,
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteFileRequest {
    pub path: String,
    pub contents: String,
    pub expected_version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFilesRequest {
    pub directory: Option<String>,
    pub files: Vec<UploadFileRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFileRequest {
    pub file_name: String,
    pub content_base64: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeResponse {
    pub ok: bool,
    pub path: String,
    pub nodes: Vec<FileNode>,
    pub skipped_symlinks: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReadResponse {
    pub ok: bool,
    pub file: FileInfo,
    pub contents: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileWriteResponse {
    pub ok: bool,
    pub file: FileInfo,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDeleteResponse {
    pub ok: bool,
    pub path: String,
    pub trash_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFilesResponse {
    pub ok: bool,
    pub files: Vec<UploadFileResponse>,
}

pub struct FilePreviewResponse {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub version: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub modified_at_ms: u64,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<FileNode>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub path: String,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub modified_at_ms: u64,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFileResponse {
    pub file_name: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug)]
pub enum FileServiceError {
    SetupRequired,
    ProjectNotFound,
    InvalidInput(String),
    NotFound(String),
    Conflict(String),
    PreconditionRequired(String),
    PreconditionFailed(String),
    PayloadTooLarge(String),
    Internal(anyhow::Error),
}

impl FileServiceError {
    fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    fn precondition_required(message: impl Into<String>) -> Self {
        Self::PreconditionRequired(message.into())
    }

    fn precondition_failed(message: impl Into<String>) -> Self {
        Self::PreconditionFailed(message.into())
    }
}

impl From<anyhow::Error> for FileServiceError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

fn build_tree(
    root: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    include_hidden: bool,
    skipped_symlinks: &mut Vec<String>,
) -> Result<Vec<FileNode>, FileServiceError> {
    let mut nodes = Vec::new();
    for entry in fs::read_dir(dir).context("failed to read project directory")? {
        let entry = entry.context("failed to read project directory entry")?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !entry_is_visible(&name, include_hidden) {
            continue;
        }

        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from(&name));
        let metadata = fs::symlink_metadata(&path).context("failed to inspect project file")?;
        if metadata.file_type().is_symlink() {
            skipped_symlinks.push(browser_relative_path(&relative));
            continue;
        }

        let is_dir = metadata.is_dir();
        let children = if is_dir && depth + 1 < max_depth {
            let children = build_tree(
                root,
                &path,
                depth + 1,
                max_depth,
                include_hidden,
                skipped_symlinks,
            )?;
            if children.is_empty() {
                None
            } else {
                Some(children)
            }
        } else {
            None
        };

        nodes.push(FileNode {
            name,
            path: browser_relative_path(&relative),
            is_dir,
            size: if is_dir { None } else { Some(metadata.len()) },
            modified_at_ms: modified_at_ms(&metadata),
            version: metadata_version(&metadata),
            children,
        });
    }

    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(nodes)
}

fn inspect_project_path(
    root: &Path,
    relative: &Path,
    require_exists: bool,
) -> Result<Option<fs::Metadata>, FileServiceError> {
    if relative.as_os_str().is_empty() {
        return fs::symlink_metadata(root)
            .map(Some)
            .context("failed to inspect project root")
            .map_err(FileServiceError::from);
    }

    let components: Vec<_> = relative.components().collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(FileServiceError::invalid_input(
                        "Project content symlinks are not allowed",
                    ));
                }
                if index + 1 < components.len() && !metadata.is_dir() {
                    return Err(FileServiceError::invalid_input(
                        "Path parent is not a directory",
                    ));
                }
                if index + 1 == components.len() {
                    return Ok(Some(metadata));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if require_exists {
                    return Err(FileServiceError::not_found("File path was not found"));
                }
                return Ok(None);
            }
            Err(err) => return Err(anyhow::Error::from(err).into()),
        }
    }

    Ok(None)
}

fn ensure_parent_directory(root: &Path, relative: &Path) -> Result<(), FileServiceError> {
    let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
    inspect_project_path(root, parent_relative, false)?;
    fs::create_dir_all(root.join(parent_relative)).context("failed to create parent directory")?;
    let metadata = inspect_project_path(root, parent_relative, true)?
        .ok_or_else(|| FileServiceError::not_found("Parent directory was not found"))?;
    if !metadata.is_dir() {
        return Err(FileServiceError::invalid_input(
            "Path parent is not a directory",
        ));
    }
    Ok(())
}

fn ensure_directory_exists(root: &Path, relative: &Path) -> Result<(), FileServiceError> {
    inspect_project_path(root, relative, false)?;
    fs::create_dir_all(root.join(relative)).context("failed to create project directory")?;
    let metadata = inspect_project_path(root, relative, true)?
        .ok_or_else(|| FileServiceError::not_found("Directory was not found"))?;
    if !metadata.is_dir() {
        return Err(FileServiceError::invalid_input(
            "Upload directory path must be a directory",
        ));
    }
    Ok(())
}

fn clean_relative_path(raw: &str, allow_empty: bool) -> Result<PathBuf, FileServiceError> {
    let normalized = raw.trim().replace('\\', "/");
    if normalized.contains('\0') {
        return Err(FileServiceError::invalid_input(
            "Project-relative path is invalid",
        ));
    }
    if normalized.starts_with('/') {
        return Err(FileServiceError::invalid_input(
            "Project-relative path must not be absolute",
        ));
    }
    if normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .any(|part| matches!(part.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic()))
    {
        return Err(FileServiceError::invalid_input(
            "Project-relative path must not be absolute",
        ));
    }

    let mut path = PathBuf::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(FileServiceError::invalid_input(
                    "Project-relative path must not contain parent traversal",
                ));
            }
            value => path.push(value),
        }
    }

    if path.as_os_str().is_empty() && !allow_empty {
        return Err(FileServiceError::invalid_input(
            "Project-relative path is required",
        ));
    }

    Ok(path)
}

fn upload_directory(directory: Option<&str>) -> Result<PathBuf, FileServiceError> {
    let relative = clean_relative_path(directory.unwrap_or(DEFAULT_UPLOAD_DIR), false)?;
    let path = browser_relative_path(&relative);
    if path != DEFAULT_UPLOAD_DIR && !path.starts_with(&format!("{DEFAULT_UPLOAD_DIR}/")) {
        return Err(FileServiceError::invalid_input(
            "Uploads must land under raw/sources",
        ));
    }
    Ok(relative)
}

fn write_uploaded_file(
    root: &Path,
    upload_dir: &Path,
    file: UploadFileRequest,
) -> Result<UploadFileResponse, FileServiceError> {
    let file_name = safe_upload_file_name(&file.file_name)?;
    let bytes = B64
        .decode(file.content_base64.trim())
        .map_err(|_| FileServiceError::invalid_input("Upload contentBase64 is not valid base64"))?;
    if bytes.len() > UPLOAD_LIMIT_BYTES {
        return Err(FileServiceError::PayloadTooLarge(
            "Upload exceeds the 1 GB limit".to_string(),
        ));
    }

    let sha256 = sha256_hex(&bytes);
    let initial_relative = upload_dir.join(&file_name);
    if let Some(metadata) = inspect_project_path(root, &initial_relative, false)? {
        if metadata.is_file() {
            let existing_hash = sha256_file(&root.join(&initial_relative))?;
            if existing_hash == sha256 {
                return Ok(UploadFileResponse {
                    file_name,
                    path: browser_relative_path(&initial_relative),
                    size: metadata.len(),
                    sha256,
                    skipped: true,
                    reason: Some("same_hash".to_string()),
                });
            }
        }
    }

    let target_relative = unique_upload_path(root, upload_dir, &file_name)?;
    write_bytes_create_new(&root.join(&target_relative), &bytes)?;
    let metadata = inspect_project_path(root, &target_relative, true)?
        .ok_or_else(|| FileServiceError::not_found("Uploaded file was not found"))?;

    Ok(UploadFileResponse {
        file_name,
        path: browser_relative_path(&target_relative),
        size: metadata.len(),
        sha256,
        skipped: false,
        reason: None,
    })
}

fn unique_upload_path(
    root: &Path,
    upload_dir: &Path,
    file_name: &str,
) -> Result<PathBuf, FileServiceError> {
    let initial = upload_dir.join(file_name);
    if inspect_project_path(root, &initial, false)?.is_none() {
        return Ok(initial);
    }

    let file_path = Path::new(file_name);
    let stem = file_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(file_name);
    let extension = file_path.extension().and_then(|value| value.to_str());

    for index in 1..10_000 {
        let candidate_name = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem}-{index}.{extension}"),
            _ => format!("{stem}-{index}"),
        };
        let candidate = upload_dir.join(candidate_name);
        if inspect_project_path(root, &candidate, false)?.is_none() {
            return Ok(candidate);
        }
    }

    Err(FileServiceError::conflict(
        "Could not find an available upload filename",
    ))
}

fn safe_upload_file_name(file_name: &str) -> Result<String, FileServiceError> {
    let file_name = file_name.trim();
    if file_name.is_empty()
        || file_name == "."
        || file_name == ".."
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.contains('\0')
    {
        return Err(FileServiceError::invalid_input(
            "Upload fileName must be a single filename",
        ));
    }
    Ok(file_name.to_string())
}

fn write_text_atomic(root: &Path, relative: &Path, contents: &str) -> Result<(), FileServiceError> {
    let target = root.join(relative);
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("llm-wiki-file");
    let tmp_relative = relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(format!(".{file_name}.{}.tmp", current_time_millis()));
    if inspect_project_path(root, &tmp_relative, false)?.is_some() {
        return Err(FileServiceError::conflict(
            "Could not find an available temporary filename",
        ));
    }
    let tmp_path = root.join(&tmp_relative);

    write_bytes_create_new(&tmp_path, contents.as_bytes())?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target).context("failed to replace existing project file")?;
    }
    fs::rename(&tmp_path, &target).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        anyhow::Error::new(err).context("failed to replace project file")
    })?;
    Ok(())
}

fn write_bytes_create_new(path: &Path, bytes: &[u8]) -> Result<(), FileServiceError> {
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .context("failed to create project file")?;
        file.write_all(bytes)
            .context("failed to write project file")?;
        Ok::<(), anyhow::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result.map_err(FileServiceError::from)
}

fn first_symlink_in_tree(root: &Path, relative: &Path) -> Result<Option<String>, FileServiceError> {
    for entry in fs::read_dir(root.join(relative)).context("failed to read project directory")? {
        let entry = entry.context("failed to read project directory entry")?;
        let path = entry.path();
        let entry_relative = path
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| relative.join(entry.file_name()));
        let metadata = fs::symlink_metadata(&path).context("failed to inspect project file")?;
        if metadata.file_type().is_symlink() {
            return Ok(Some(browser_relative_path(&entry_relative)));
        }
        if metadata.is_dir() {
            if let Some(path) = first_symlink_in_tree(root, &entry_relative)? {
                return Ok(Some(path));
            }
        }
    }

    Ok(None)
}

fn unique_trash_path(root: &Path, relative: &Path) -> Result<PathBuf, FileServiceError> {
    let stamp = current_time_millis();
    for index in 0..10_000 {
        let container = if index == 0 {
            PathBuf::from(TRASH_DIR).join(stamp.to_string())
        } else {
            PathBuf::from(TRASH_DIR).join(format!("{stamp}-{index}"))
        };
        let candidate = container.join(relative);
        if inspect_project_path(root, &candidate, false)?.is_none() {
            return Ok(candidate);
        }
    }
    Err(FileServiceError::conflict(
        "Could not find an available project trash path",
    ))
}

fn is_trash_path(relative: &Path) -> bool {
    browser_relative_path(relative) == TRASH_DIR
        || browser_relative_path(relative).starts_with(&format!("{TRASH_DIR}/"))
}

fn file_info(root: &Path, relative: &Path, metadata: &fs::Metadata) -> FileInfo {
    let is_dir = metadata.is_dir();
    FileInfo {
        path: browser_relative_path(relative),
        is_dir,
        size: if is_dir { None } else { Some(metadata.len()) },
        modified_at_ms: modified_at_ms(metadata),
        version: metadata_version(metadata),
        mime_type: if is_dir {
            None
        } else {
            Some(mime_type_for_path(&root.join(relative)))
        },
    }
}

fn metadata_version(metadata: &fs::Metadata) -> String {
    format!(
        "{}:{}",
        modified_at_ms(metadata),
        if metadata.is_dir() { 0 } else { metadata.len() }
    )
}

fn modified_at_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn current_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn normalized_expected_version(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn entry_is_visible(name: &str, include_hidden: bool) -> bool {
    include_hidden || !name.starts_with('.')
}

fn browser_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn mime_type_for_path(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn sha256_file(path: &Path) -> Result<String, FileServiceError> {
    let bytes = fs::read(path).context("failed to read existing file for hashing")?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::{clean_relative_path, upload_directory};

    #[test]
    fn project_relative_paths_reject_escape_attempts() {
        assert_eq!(
            clean_relative_path("wiki/index.md", false).unwrap(),
            std::path::PathBuf::from("wiki/index.md")
        );
        assert!(clean_relative_path("../outside.md", false).is_err());
        assert!(clean_relative_path("/etc/passwd", false).is_err());
        assert!(clean_relative_path("C:/Users/example", false).is_err());
        assert!(clean_relative_path("wiki/C:/Users/example", false).is_err());
    }

    #[test]
    fn uploads_are_limited_to_raw_sources() {
        assert_eq!(
            upload_directory(None).unwrap(),
            std::path::PathBuf::from("raw/sources")
        );
        assert!(upload_directory(Some("wiki")).is_err());
        assert!(upload_directory(Some("raw/sources/papers")).is_ok());
    }
}
