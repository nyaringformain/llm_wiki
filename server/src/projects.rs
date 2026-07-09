use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::config::{ServerPaths, SERVER_HOME_DIR};
use crate::db::{ConfigDb, NewProjectRecord, ProjectRecord};

const PROJECT_DIRS: &[&str] = &[
    "raw/sources",
    "raw/assets",
    "wiki/entities",
    "wiki/concepts",
    "wiki/sources",
    "wiki/queries",
    "wiki/comparisons",
    "wiki/synthesis",
    ".obsidian",
];

#[derive(Clone)]
pub struct ProjectService {
    paths: ServerPaths,
    db: ConfigDb,
}

impl ProjectService {
    pub fn new(paths: ServerPaths, db: ConfigDb) -> Self {
        Self { paths, db }
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectResponse>, ProjectServiceError> {
        Ok(self
            .db
            .list_projects()
            .await?
            .into_iter()
            .map(ProjectResponse::from)
            .collect())
    }

    pub async fn create_project(&self, name: &str) -> Result<ProjectResponse, ProjectServiceError> {
        let destination = self.destination_for_name(name)?;
        self.ensure_destination_available(&destination).await?;
        fs::create_dir(&destination.absolute_path).context("failed to create project directory")?;

        let result = (|| {
            write_project_skeleton(&destination.absolute_path)?;
            let identity = ensure_project_identity(&destination.absolute_path)?;
            Ok(identity)
        })();

        let identity = match result {
            Ok(identity) => identity,
            Err(err) => {
                let _ = fs::remove_dir_all(&destination.absolute_path);
                return Err(err);
            }
        };

        match self
            .register_project(
                identity.id,
                destination.name,
                destination.relative_path,
                "created",
            )
            .await
        {
            Ok(project) => Ok(project),
            Err(err) => {
                let _ = fs::remove_dir_all(&destination.absolute_path);
                Err(err)
            }
        }
    }

    pub async fn import_project(
        &self,
        source_path: &str,
        name: Option<&str>,
    ) -> Result<ImportProjectResponse, ProjectServiceError> {
        let source = validate_import_source(source_path)?;
        let display_name = match name.map(str::trim).filter(|value| !value.is_empty()) {
            Some(name) => name.to_string(),
            None => source
                .file_name()
                .and_then(OsStr::to_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ProjectServiceError::invalid_input("Project name could not be inferred")
                })?
                .to_string(),
        };
        let destination = self.destination_for_name(&display_name)?;
        self.ensure_destination_available(&destination).await?;
        fs::create_dir(&destination.absolute_path).context("failed to create project directory")?;

        let mut skipped_symlinks = Vec::new();
        let result = (|| {
            copy_project_tree(
                &source,
                &destination.absolute_path,
                Path::new(""),
                &mut skipped_symlinks,
            )?;
            validate_project_root(&destination.absolute_path)?;
            let identity = ensure_project_identity(&destination.absolute_path)?;
            Ok(identity)
        })();

        let identity = match result {
            Ok(identity) => identity,
            Err(err) => {
                let _ = fs::remove_dir_all(&destination.absolute_path);
                return Err(err);
            }
        };

        let project = match self
            .register_project(
                identity.id,
                destination.name,
                destination.relative_path,
                "imported",
            )
            .await
        {
            Ok(project) => project,
            Err(err) => {
                let _ = fs::remove_dir_all(&destination.absolute_path);
                return Err(err);
            }
        };

        Ok(ImportProjectResponse {
            project,
            skipped_symlinks,
        })
    }

    async fn ensure_destination_available(
        &self,
        destination: &ProjectDestination,
    ) -> Result<(), ProjectServiceError> {
        if fs::symlink_metadata(&destination.absolute_path).is_ok() {
            return Err(ProjectServiceError::conflict(
                "Project directory already exists under the Data Root",
            ));
        }
        if self
            .db
            .project_relative_path_exists(&destination.relative_path)
            .await?
        {
            return Err(ProjectServiceError::conflict(
                "Project is already registered under that Data Root path",
            ));
        }

        Ok(())
    }

    async fn register_project(
        &self,
        id: String,
        name: String,
        relative_path: String,
        source: &str,
    ) -> Result<ProjectResponse, ProjectServiceError> {
        if self.db.project_id_exists(&id).await? {
            return Err(ProjectServiceError::conflict(
                "Project is already registered",
            ));
        }
        if self.db.project_relative_path_exists(&relative_path).await? {
            return Err(ProjectServiceError::conflict(
                "Project is already registered under that Data Root path",
            ));
        }

        let record = self
            .db
            .register_project(NewProjectRecord {
                id,
                name,
                relative_path,
                source: source.to_string(),
            })
            .await?;
        Ok(ProjectResponse::from(record))
    }

    fn destination_for_name(&self, name: &str) -> Result<ProjectDestination, ProjectServiceError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectServiceError::invalid_input(
                "Project name is required",
            ));
        }
        if name == SERVER_HOME_DIR {
            return Err(ProjectServiceError::invalid_input(
                "Project name is reserved",
            ));
        }

        let relative_path = sanitize_project_dir_name(name)?;
        Ok(ProjectDestination {
            name: name.to_string(),
            absolute_path: self.paths.data_root().join(&relative_path),
            relative_path,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectResponse {
    pub id: String,
    pub name: String,
    pub relative_path: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<ProjectRecord> for ProjectResponse {
    fn from(record: ProjectRecord) -> Self {
        Self {
            id: record.id,
            name: record.name,
            relative_path: record.relative_path,
            source: record.source,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProjectResponse {
    pub project: ProjectResponse,
    pub skipped_symlinks: Vec<String>,
}

#[derive(Debug)]
pub enum ProjectServiceError {
    SetupRequired,
    InvalidInput(String),
    InvalidProject(String),
    Conflict(String),
    Internal(anyhow::Error),
}

impl ProjectServiceError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    fn invalid_project(message: impl Into<String>) -> Self {
        Self::InvalidProject(message.into())
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }
}

impl From<anyhow::Error> for ProjectServiceError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

struct ProjectDestination {
    name: String,
    absolute_path: PathBuf,
    relative_path: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectIdentity {
    id: String,
    created_at: u64,
}

fn sanitize_project_dir_name(name: &str) -> Result<String, ProjectServiceError> {
    let mut slug = String::new();
    let mut previous_was_separator = false;

    for ch in name.trim().chars() {
        if ch.is_alphanumeric() {
            for lowered in ch.to_lowercase() {
                slug.push(lowered);
            }
            previous_was_separator = false;
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            if !previous_was_separator && !slug.is_empty() {
                slug.push('-');
                previous_was_separator = true;
            }
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        return Err(ProjectServiceError::invalid_input(
            "Project name must contain at least one letter or number",
        ));
    }

    Ok(slug)
}

fn validate_import_source(source_path: &str) -> Result<PathBuf, ProjectServiceError> {
    let source = PathBuf::from(source_path.trim());
    if !source.is_absolute() {
        return Err(ProjectServiceError::invalid_input(
            "Import source must be an absolute server path",
        ));
    }

    let metadata = fs::symlink_metadata(&source).map_err(|_| {
        ProjectServiceError::invalid_project("Import source is not a valid project")
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ProjectServiceError::invalid_project(
            "Import source must not be a symlink",
        ));
    }
    if !metadata.is_dir() {
        return Err(ProjectServiceError::invalid_project(
            "Import source must be a directory",
        ));
    }

    validate_project_root(&source)?;
    source
        .canonicalize()
        .context("failed to resolve import source")
        .map_err(ProjectServiceError::from)
}

fn validate_project_root(root: &Path) -> Result<(), ProjectServiceError> {
    if !is_regular_file_without_symlink(&root.join("schema.md")) {
        return Err(ProjectServiceError::invalid_project(
            "Not a valid project: missing schema.md",
        ));
    }
    if !is_directory_without_symlink(&root.join("wiki")) {
        return Err(ProjectServiceError::invalid_project(
            "Not a valid project: missing wiki directory",
        ));
    }
    Ok(())
}

fn is_regular_file_without_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn is_directory_without_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn write_project_skeleton(root: &Path) -> Result<(), ProjectServiceError> {
    for dir in PROJECT_DIRS {
        fs::create_dir_all(root.join(dir)).context("failed to create project directory")?;
    }

    write_text(root.join("schema.md"), SCHEMA_MD)?;
    write_text(root.join("purpose.md"), PURPOSE_MD)?;
    write_text(root.join("wiki/index.md"), WIKI_INDEX_MD)?;
    write_text(root.join("wiki/log.md"), WIKI_LOG_MD)?;
    write_text(root.join("wiki/overview.md"), WIKI_OVERVIEW_MD)?;
    write_text(root.join(".obsidian/app.json"), OBSIDIAN_APP_JSON)?;
    write_text(
        root.join(".obsidian/appearance.json"),
        OBSIDIAN_APPEARANCE_JSON,
    )?;
    write_text(
        root.join(".obsidian/core-plugins.json"),
        OBSIDIAN_CORE_PLUGINS_JSON,
    )?;

    Ok(())
}

fn write_text(path: PathBuf, contents: &str) -> Result<(), ProjectServiceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create project directory")?;
    }
    fs::write(path, contents).context("failed to write project file")?;
    Ok(())
}

fn copy_project_tree(
    source: &Path,
    destination: &Path,
    relative: &Path,
    skipped_symlinks: &mut Vec<String>,
) -> Result<(), ProjectServiceError> {
    for entry in fs::read_dir(source).context("failed to read import source")? {
        let entry = entry.context("failed to read import source entry")?;
        let source_path = entry.path();
        let relative_path = relative.join(entry.file_name());
        let destination_path = destination.join(entry.file_name());
        let metadata =
            fs::symlink_metadata(&source_path).context("failed to inspect import source entry")?;

        if metadata.file_type().is_symlink() {
            skipped_symlinks.push(path_to_browser_relative(&relative_path));
            continue;
        }

        if metadata.is_dir() {
            fs::create_dir(&destination_path).context("failed to create imported directory")?;
            copy_project_tree(
                &source_path,
                &destination_path,
                &relative_path,
                skipped_symlinks,
            )?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path).context("failed to copy imported file")?;
        }
    }

    Ok(())
}

fn ensure_project_identity(root: &Path) -> Result<ProjectIdentity, ProjectServiceError> {
    let identity_path = root.join(".llm-wiki/project.json");
    if let Ok(raw) = fs::read_to_string(&identity_path) {
        if let Ok(identity) = serde_json::from_str::<ProjectIdentity>(&raw) {
            if !identity.id.trim().is_empty() {
                return Ok(identity);
            }
        }
    }

    let identity = ProjectIdentity {
        id: generate_uuid_v4(),
        created_at: current_time_millis(),
    };
    write_text(
        identity_path,
        &serde_json::to_string_pretty(&identity).context("failed to serialize project identity")?,
    )?;
    Ok(identity)
}

fn generate_uuid_v4() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn current_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn path_to_browser_relative(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

const SCHEMA_MD: &str = r#"# Wiki Schema

## Page Types

| Type | Directory | Purpose |
|------|-----------|---------|
| entity | wiki/entities/ | Named things |
| concept | wiki/concepts/ | Ideas and techniques |
| source | wiki/sources/ | Papers, articles, talks, and notes |
| query | wiki/queries/ | Open questions |
| comparison | wiki/comparisons/ | Side-by-side analysis |
| synthesis | wiki/synthesis/ | Cross-cutting summaries |

## Naming

- Files use kebab-case Markdown names.
- Wiki links use `[[page-slug]]`.
"#;

const PURPOSE_MD: &str = r#"# Project Purpose

## Goal

## Key Questions

1.
2.
3.

## Scope

**In scope:**
-

**Out of scope:**
-
"#;

const WIKI_INDEX_MD: &str = r#"# Wiki Index

## Entities

## Concepts

## Sources

## Queries

## Comparisons

## Synthesis
"#;

const WIKI_LOG_MD: &str = r#"# Research Log

## Created

- Project created
"#;

const WIKI_OVERVIEW_MD: &str = r#"---
type: overview
title: Project Overview
tags: []
related: []
---

# Overview
"#;

const OBSIDIAN_APP_JSON: &str = r#"{
  "attachmentFolderPath": "raw/assets",
  "userIgnoreFilters": [
    ".cache",
    ".llm-wiki",
    ".superpowers"
  ],
  "useMarkdownLinks": false,
  "newLinkFormat": "shortest",
  "showUnsupportedFiles": false
}"#;

const OBSIDIAN_APPEARANCE_JSON: &str = r#"{
  "baseFontSize": 16,
  "theme": "obsidian"
}"#;

const OBSIDIAN_CORE_PLUGINS_JSON: &str = r#"{
  "file-explorer": true,
  "global-search": true,
  "graph": true,
  "backlink": true,
  "tag-pane": true,
  "page-preview": true,
  "outgoing-link": true,
  "starred": true
}"#;

#[cfg(test)]
mod tests {
    use super::sanitize_project_dir_name;

    #[test]
    fn project_names_are_sanitized_for_data_root_children() {
        assert_eq!(
            sanitize_project_dir_name(" My Research Project ").unwrap(),
            "my-research-project"
        );
        assert_eq!(sanitize_project_dir_name("../escape").unwrap(), "escape");
        assert!(sanitize_project_dir_name("...").is_err());
    }
}
