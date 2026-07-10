use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Context};
use arrow_array::{Float32Array, RecordBatch, StringArray, UInt32Array};
use futures::TryStreamExt;
use lancedb::connect;
use lancedb::query::{ExecutableQuery, QueryBase};
use serde::{Deserialize, Serialize};

use crate::files::FileService;
use crate::search::ContentServiceError;

const TABLE_V1: &str = "wiki_vectors";
const TABLE_V2: &str = "wiki_chunks_v2";
const MAX_VECTOR_RESULTS: usize = 200;
const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;

static VECTORSTORE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::RwLock<()>>>>> =
    OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSearchRequest {
    pub query_embedding: Vec<f32>,
    pub top_k: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkSearchResult {
    pub chunk_id: String,
    pub page_id: String,
    pub chunk_index: u32,
    pub chunk_text: String,
    pub heading_path: String,
    pub score: f32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSearchResponse {
    pub ok: bool,
    pub results: Vec<ChunkSearchResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorStatusResponse {
    pub ok: bool,
    pub available: bool,
    pub chunk_count: usize,
    pub legacy_page_count: usize,
}

pub async fn search_project_vectors(
    files: &FileService,
    project_id: &str,
    request: VectorSearchRequest,
) -> Result<VectorSearchResponse, ContentServiceError> {
    validate_embedding(&request.query_embedding)?;
    let root = files.project_root(project_id).await?;
    let top_k = request.top_k.unwrap_or(20).clamp(1, MAX_VECTOR_RESULTS);
    let results = search_chunks_at_root(&root, request.query_embedding, top_k).await?;
    Ok(VectorSearchResponse { ok: true, results })
}

pub async fn project_vector_status(
    files: &FileService,
    project_id: &str,
) -> Result<VectorStatusResponse, ContentServiceError> {
    let root = files.project_root(project_id).await?;
    let Some(database_path) = checked_database_path(&root)? else {
        return Ok(VectorStatusResponse {
            ok: true,
            available: false,
            chunk_count: 0,
            legacy_page_count: 0,
        });
    };
    let lock = vectorstore_lock(&database_path);
    let _guard = lock.read().await;
    let database = connect_database(&database_path).await?;
    let tables = database
        .table_names()
        .execute()
        .await
        .map_err(|error| anyhow!("failed to list LanceDB tables: {error}"))?;
    let chunk_count = table_count(&database, &tables, TABLE_V2).await?;
    let legacy_page_count = table_count(&database, &tables, TABLE_V1).await?;

    Ok(VectorStatusResponse {
        ok: true,
        available: chunk_count > 0 || legacy_page_count > 0,
        chunk_count,
        legacy_page_count,
    })
}

pub(crate) async fn search_chunks_at_root(
    project_root: &Path,
    query_embedding: Vec<f32>,
    top_k: usize,
) -> Result<Vec<ChunkSearchResult>, ContentServiceError> {
    validate_embedding(&query_embedding)?;
    let Some(database_path) = checked_database_path(project_root)? else {
        return Ok(Vec::new());
    };
    let lock = vectorstore_lock(&database_path);
    let _guard = lock.read().await;
    let database = connect_database(&database_path).await?;
    let tables = database
        .table_names()
        .execute()
        .await
        .map_err(|error| anyhow!("failed to list LanceDB tables: {error}"))?;
    if !tables.iter().any(|table| table == TABLE_V2) {
        return Ok(Vec::new());
    }

    let table = database
        .open_table(TABLE_V2)
        .execute()
        .await
        .map_err(|error| anyhow!("failed to open LanceDB chunk table: {error}"))?;
    let stream = table
        .vector_search(query_embedding)
        .map_err(|error| {
            tracing::debug!(error = %error, "vector query was incompatible with the project index");
            ContentServiceError::invalid_input(
                "queryEmbedding is incompatible with the project vector index",
            )
        })?
        .limit(top_k.clamp(1, MAX_VECTOR_RESULTS))
        .execute()
        .await
        .map_err(|error| anyhow!("failed to execute LanceDB search: {error}"))?;
    let batches: Vec<RecordBatch> = stream
        .try_collect()
        .await
        .map_err(|error| anyhow!("failed to collect LanceDB search results: {error}"))?;

    let mut output = Vec::new();
    for batch in batches {
        append_batch_results(&batch, &mut output)?;
    }
    Ok(output)
}

fn validate_embedding(embedding: &[f32]) -> Result<(), ContentServiceError> {
    if embedding.is_empty() {
        return Err(ContentServiceError::invalid_input(
            "queryEmbedding must not be empty",
        ));
    }
    if embedding.len() > MAX_EMBEDDING_DIMENSIONS {
        return Err(ContentServiceError::invalid_input(
            "queryEmbedding has too many dimensions",
        ));
    }
    if embedding.iter().any(|value| !value.is_finite()) {
        return Err(ContentServiceError::invalid_input(
            "queryEmbedding must contain only finite numbers",
        ));
    }
    Ok(())
}

fn checked_database_path(project_root: &Path) -> Result<Option<PathBuf>, ContentServiceError> {
    let metadata_dir = project_root.join(".llm-wiki");
    let database_dir = metadata_dir.join("lancedb");
    for path in [&metadata_dir, &database_dir] {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(anyhow::Error::from(error).into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(ContentServiceError::invalid_project(
                "Project content symlinks are not allowed",
            ));
        }
        if !metadata.is_dir() {
            return Err(ContentServiceError::invalid_project(
                "Project vector index path must be a directory",
            ));
        }
    }
    let canonical_project = project_root
        .canonicalize()
        .context("failed to resolve project directory")?;
    let canonical_database = database_dir
        .canonicalize()
        .context("failed to resolve project vector index")?;
    if !canonical_database.starts_with(&canonical_project) {
        return Err(ContentServiceError::invalid_project(
            "Project vector index escapes the project root",
        ));
    }
    Ok(Some(database_dir))
}

fn vectorstore_lock(path: &Path) -> Arc<tokio::sync::RwLock<()>> {
    let locks = VECTORSTORE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("vectorstore lock map poisoned");
    locks
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
        .clone()
}

async fn connect_database(path: &Path) -> Result<lancedb::Connection, ContentServiceError> {
    connect(&path.to_string_lossy())
        .execute()
        .await
        .map_err(|error| anyhow!("failed to connect to project LanceDB: {error}").into())
}

async fn table_count(
    database: &lancedb::Connection,
    tables: &[String],
    name: &str,
) -> Result<usize, ContentServiceError> {
    if !tables.iter().any(|table| table == name) {
        return Ok(0);
    }
    database
        .open_table(name)
        .execute()
        .await
        .map_err(|error| anyhow!("failed to open LanceDB table: {error}"))?
        .count_rows(None)
        .await
        .map_err(|error| anyhow!("failed to count LanceDB rows: {error}").into())
}

fn append_batch_results(
    batch: &RecordBatch,
    output: &mut Vec<ChunkSearchResult>,
) -> Result<(), ContentServiceError> {
    let chunk_ids = string_column(batch, "chunk_id")?;
    let page_ids = string_column(batch, "page_id")?;
    let chunk_indexes = batch
        .column_by_name("chunk_index")
        .and_then(|column| column.as_any().downcast_ref::<UInt32Array>())
        .ok_or_else(|| anyhow!("LanceDB chunk table is missing chunk_index"))?;
    let chunk_texts = string_column(batch, "chunk_text")?;
    let heading_paths = string_column(batch, "heading_path")?;
    let distances = batch
        .column_by_name("_distance")
        .and_then(|column| column.as_any().downcast_ref::<Float32Array>())
        .ok_or_else(|| anyhow!("LanceDB search response is missing _distance"))?;

    for index in 0..batch.num_rows() {
        output.push(ChunkSearchResult {
            chunk_id: chunk_ids.value(index).to_string(),
            page_id: page_ids.value(index).to_string(),
            chunk_index: chunk_indexes.value(index),
            chunk_text: chunk_texts.value(index).to_string(),
            heading_path: heading_paths.value(index).to_string(),
            score: 1.0 / (1.0 + distances.value(index)),
        });
    }
    Ok(())
}

fn string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, ContentServiceError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| anyhow!("LanceDB chunk table is missing {name}").into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{
        ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, StringArray, UInt32Array,
    };
    use arrow_schema::{DataType, Field, Schema};
    use lancedb::connect;

    use super::search_chunks_at_root;

    #[tokio::test]
    async fn reads_the_existing_chunk_table_contract() {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join(".llm-wiki/lancedb");
        std::fs::create_dir_all(&database_path).unwrap();
        let schema = Arc::new(Schema::new(vec![
            Field::new("chunk_id", DataType::Utf8, false),
            Field::new("page_id", DataType::Utf8, false),
            Field::new("chunk_index", DataType::UInt32, false),
            Field::new("chunk_text", DataType::Utf8, false),
            Field::new("heading_path", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), 3),
                false,
            ),
        ]));
        let vectors: ArrayRef = Arc::new(FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            3,
            Arc::new(Float32Array::from(vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0])),
            None,
        ));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["alpha#0", "beta#0"])) as ArrayRef,
                Arc::new(StringArray::from(vec!["alpha", "beta"])) as ArrayRef,
                Arc::new(UInt32Array::from(vec![0, 0])) as ArrayRef,
                Arc::new(StringArray::from(vec!["alpha text", "beta text"])) as ArrayRef,
                Arc::new(StringArray::from(vec!["# Alpha", "# Beta"])) as ArrayRef,
                vectors,
            ],
        )
        .unwrap();
        connect(&database_path.to_string_lossy())
            .execute()
            .await
            .unwrap()
            .create_table("wiki_chunks_v2", vec![batch])
            .execute()
            .await
            .unwrap();

        let results = search_chunks_at_root(temp.path(), vec![1.0, 0.0, 0.0], 2)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].page_id, "alpha");
        assert_eq!(results[0].chunk_text, "alpha text");
        assert!(results[0].score > results[1].score);
    }
}
