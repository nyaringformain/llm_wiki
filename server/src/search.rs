use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::files::{FileService, FileServiceError};
use crate::vectorstore;

const DEFAULT_RESULTS: usize = 20;
const MAX_RESULTS: usize = 50;
const MAX_SEARCH_FILES: usize = 10_000;
const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;
const RRF_K: f64 = 60.0;
const FILENAME_EXACT_BONUS: f64 = 200.0;
const PHRASE_IN_TITLE_BONUS: f64 = 50.0;
const PHRASE_IN_CONTENT_PER_OCC: f64 = 20.0;
const MAX_PHRASE_OCC_COUNTED: usize = 10;
const TITLE_TOKEN_WEIGHT: f64 = 5.0;
const CONTENT_TOKEN_WEIGHT: f64 = 1.0;
const SNIPPET_CONTEXT: usize = 80;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchRequest {
    pub query: String,
    pub top_k: Option<usize>,
    pub include_content: Option<bool>,
    pub query_embedding: Option<Vec<f32>>,
    pub expand_graph: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchImageRef {
    pub url: String,
    pub alt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchResult {
    pub path: String,
    pub title: String,
    pub snippet: String,
    pub title_match: bool,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_score: Option<f32>,
    pub images: Vec<SearchImageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchResponse {
    pub ok: bool,
    pub mode: String,
    pub results: Vec<ProjectSearchResult>,
    pub token_hits: usize,
    pub graph_hits: usize,
    pub vector_hits: usize,
}

#[derive(Debug)]
pub enum ContentServiceError {
    Project(FileServiceError),
    InvalidInput(String),
    InvalidProject(String),
    Internal(anyhow::Error),
}

impl ContentServiceError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    pub fn invalid_project(message: impl Into<String>) -> Self {
        Self::InvalidProject(message.into())
    }
}

impl From<FileServiceError> for ContentServiceError {
    fn from(error: FileServiceError) -> Self {
        Self::Project(error)
    }
}

impl From<anyhow::Error> for ContentServiceError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

#[derive(Debug, Clone)]
struct PageVectorResult {
    id: String,
    score: f32,
    chunk_text: String,
    heading_path: String,
}

pub async fn search_project(
    files: &FileService,
    project_id: &str,
    request: ProjectSearchRequest,
) -> Result<ProjectSearchResponse, ContentServiceError> {
    let query = request.query.trim();
    if query.is_empty() {
        return Err(ContentServiceError::invalid_input("query is required"));
    }

    if let Some(embedding) = request.query_embedding.as_ref() {
        validate_embedding(embedding)?;
    }

    let root = files.project_root(project_id).await?;
    let limit = request
        .top_k
        .unwrap_or(DEFAULT_RESULTS)
        .clamp(1, MAX_RESULTS);
    let include_content = request.include_content.unwrap_or(false);
    let tokens = tokenize_query(query);
    let effective_tokens = if tokens.is_empty() {
        vec![query.to_lowercase()]
    } else {
        tokens
    };
    let query_phrase = trim_query_punctuation(&query.to_lowercase());
    let mut results = Vec::new();
    let mut page_paths_by_stem = BTreeMap::new();

    for relative in wiki_markdown_files(&root)? {
        let absolute = root.join(&relative);
        let content = match fs::read_to_string(&absolute) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(error) => return Err(anyhow::Error::from(error).into()),
        };
        if let Some(stem) = absolute.file_stem().and_then(|value| value.to_str()) {
            page_paths_by_stem.insert(stem.to_string(), browser_path(&relative));
        }
        if let Some(result) = score_file(
            &relative,
            &content,
            &effective_tokens,
            &query_phrase,
            query,
            include_content,
        ) {
            results.push(result);
        }
    }

    let mut token_sorted = (0..results.len()).collect::<Vec<_>>();
    token_sorted.sort_by(|left, right| {
        results[*right]
            .score
            .partial_cmp(&results[*left].score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| results[*left].path.cmp(&results[*right].path))
    });
    let token_rank = token_sorted
        .iter()
        .enumerate()
        .map(|(index, result_index)| (results[*result_index].path.clone(), index + 1))
        .collect::<BTreeMap<_, _>>();

    let mut graph_hits = 0;
    if request.expand_graph.unwrap_or(false) && !token_rank.is_empty() {
        let graph = crate::graph::build_graph_at_root(&root)?;
        let seed_paths = token_rank.keys().cloned().collect::<BTreeSet<_>>();
        let neighbor_paths = crate::graph::neighbor_paths(&graph, &seed_paths);
        graph_hits =
            materialize_graph_results(&neighbor_paths, &root, &mut results, include_content);
    }

    let mut vector_rank = BTreeMap::new();
    let mut vector_score = BTreeMap::new();
    let mut vector_hits = 0;
    if let Some(embedding) = request.query_embedding {
        let vector_results = aggregate_vector_results(
            vectorstore::search_chunks_at_root(&root, embedding, (limit * 3).max(30)).await?,
            limit.max(10),
        );
        vector_hits = vector_results.len();
        for (index, result) in vector_results.iter().enumerate() {
            vector_rank.insert(result.id.clone(), index + 1);
            vector_score.insert(result.id.clone(), result.score);
        }
        materialize_vector_only_results(
            &vector_results,
            &page_paths_by_stem,
            &root,
            &mut results,
            include_content,
        );
    }

    if vector_hits == 0 {
        results.sort_by(result_order);
        results.truncate(limit);
        return Ok(ProjectSearchResponse {
            ok: true,
            mode: if graph_hits > 0 { "graph" } else { "keyword" }.to_string(),
            token_hits: token_rank.len(),
            graph_hits,
            vector_hits,
            results,
        });
    }

    for result in &mut results {
        let token = token_rank.get(&result.path).copied();
        let vector = vector_rank.get(&file_stem(&result.path)).copied();
        if token.is_none() && vector.is_none() {
            continue;
        }
        let mut score = 0.0;
        if let Some(rank) = token {
            score += 1.0 / (RRF_K + rank as f64);
        }
        if let Some(rank) = vector {
            score += 1.0 / (RRF_K + rank as f64);
        }
        result.vector_score = vector_score.get(&file_stem(&result.path)).copied();
        result.score = score;
    }
    results.sort_by(result_order);
    results.truncate(limit);

    Ok(ProjectSearchResponse {
        ok: true,
        mode: if token_rank.is_empty() {
            "vector".to_string()
        } else {
            "hybrid".to_string()
        },
        token_hits: token_rank.len(),
        graph_hits,
        vector_hits,
        results,
    })
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

pub(crate) fn wiki_markdown_files(root: &Path) -> Result<Vec<PathBuf>, ContentServiceError> {
    let wiki = root.join("wiki");
    let metadata = match fs::symlink_metadata(&wiki) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(anyhow::Error::from(error).into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(ContentServiceError::invalid_project(
            "Project content symlinks are not allowed",
        ));
    }
    if !metadata.is_dir() {
        return Err(ContentServiceError::invalid_project(
            "Project wiki path must be a directory",
        ));
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(&wiki).follow_links(false) {
        let entry = entry.context("failed to scan project wiki")?;
        if entry.file_type().is_symlink() {
            return Err(ContentServiceError::invalid_project(
                "Project content symlinks are not allowed",
            ));
        }
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("md")
        {
            let relative = entry
                .path()
                .strip_prefix(root)
                .context("wiki file escaped project root")?
                .to_path_buf();
            files.push(relative);
            if files.len() > MAX_SEARCH_FILES {
                return Err(ContentServiceError::invalid_project(format!(
                    "Project wiki exceeds the {MAX_SEARCH_FILES} file search limit"
                )));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn aggregate_vector_results(
    raw: Vec<vectorstore::ChunkSearchResult>,
    top_k: usize,
) -> Vec<PageVectorResult> {
    let mut by_page = BTreeMap::<String, Vec<vectorstore::ChunkSearchResult>>::new();
    for chunk in raw {
        by_page
            .entry(chunk.page_id.clone())
            .or_default()
            .push(chunk);
    }

    let mut ranked = Vec::new();
    for (id, mut chunks) in by_page {
        chunks.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
        });
        let top_chunk = &chunks[0];
        let tail = chunks.iter().skip(1).map(|chunk| chunk.score).sum::<f32>();
        let score = top_chunk.score + (tail * 0.3).min((1.0 - top_chunk.score).max(0.0));
        ranked.push(PageVectorResult {
            id,
            score,
            chunk_text: top_chunk.chunk_text.clone(),
            heading_path: top_chunk.heading_path.clone(),
        });
    }
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    ranked.truncate(top_k);
    ranked
}

fn materialize_vector_only_results(
    vector_results: &[PageVectorResult],
    paths_by_stem: &BTreeMap<String, String>,
    root: &Path,
    results: &mut Vec<ProjectSearchResult>,
    include_content: bool,
) {
    let mut known = results
        .iter()
        .map(|result| file_stem(&result.path))
        .collect::<BTreeSet<_>>();
    for vector in vector_results {
        if known.contains(&vector.id) {
            continue;
        }
        let Some(relative) = paths_by_stem.get(&vector.id) else {
            continue;
        };
        let Ok(content) = fs::read_to_string(root.join(relative)) else {
            continue;
        };
        let file_name = Path::new(relative)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        results.push(ProjectSearchResult {
            path: relative.clone(),
            title: extract_title(&content, file_name),
            snippet: vector_snippet(vector),
            title_match: false,
            score: 0.0,
            vector_score: Some(vector.score),
            images: extract_image_refs(&content),
            content: include_content.then_some(content),
        });
        known.insert(vector.id.clone());
    }
}

fn materialize_graph_results(
    graph_paths: &BTreeSet<String>,
    root: &Path,
    results: &mut Vec<ProjectSearchResult>,
    include_content: bool,
) -> usize {
    let mut known = results
        .iter()
        .map(|result| result.path.clone())
        .collect::<BTreeSet<_>>();
    let graph_score = results
        .iter()
        .map(|result| result.score)
        .fold(0.0_f64, f64::max)
        * 0.15;
    let mut added = 0;
    for relative in graph_paths {
        if !known.insert(relative.clone()) {
            continue;
        }
        let Ok(content) = fs::read_to_string(root.join(relative)) else {
            continue;
        };
        let file_name = Path::new(relative)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        results.push(ProjectSearchResult {
            path: relative.clone(),
            title: extract_title(&content, file_name),
            snippet: first_text_snippet(&content),
            title_match: false,
            score: graph_score,
            vector_score: None,
            images: extract_image_refs(&content),
            content: include_content.then_some(content),
        });
        added += 1;
    }
    added
}

fn vector_snippet(result: &PageVectorResult) -> String {
    let mut text = result.chunk_text.trim().replace('\n', " ");
    if text.chars().count() > SNIPPET_CONTEXT * 2 {
        text = text.chars().take(SNIPPET_CONTEXT * 2).collect();
        text.push_str("...");
    }
    if result.heading_path.trim().is_empty() || text.is_empty() {
        text
    } else {
        format!("{}: {text}", result.heading_path.trim())
    }
}

fn score_file(
    relative: &Path,
    content: &str,
    tokens: &[String],
    query_phrase: &str,
    query: &str,
    include_content: bool,
) -> Option<ProjectSearchResult> {
    let file_name = relative
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let title = extract_title(content, file_name);
    let title_text = format!("{title} {file_name}");
    let title_lower = title_text.to_lowercase();
    let content_lower = content.to_lowercase();
    let stem = file_name.trim_end_matches(".md").to_lowercase();
    let filename_exact = !query_phrase.is_empty() && stem == query_phrase;
    let title_has_phrase = !query_phrase.is_empty() && title_lower.contains(query_phrase);
    let content_phrase_occ =
        count_occurrences(&content_lower, query_phrase).min(MAX_PHRASE_OCC_COUNTED);
    let title_token_score = token_match_score(&title_text, tokens);
    let content_token_score = token_match_score(content, tokens);

    if !filename_exact
        && !title_has_phrase
        && content_phrase_occ == 0
        && title_token_score == 0
        && content_token_score == 0
    {
        return None;
    }

    let score = (if filename_exact {
        FILENAME_EXACT_BONUS
    } else {
        0.0
    }) + (if title_has_phrase {
        PHRASE_IN_TITLE_BONUS
    } else {
        0.0
    }) + content_phrase_occ as f64 * PHRASE_IN_CONTENT_PER_OCC
        + title_token_score as f64 * TITLE_TOKEN_WEIGHT
        + content_token_score as f64 * CONTENT_TOKEN_WEIGHT;
    let snippet_anchor = if content_phrase_occ > 0 {
        query_phrase.to_string()
    } else {
        tokens
            .iter()
            .find(|token| content_lower.contains(token.as_str()))
            .cloned()
            .unwrap_or_else(|| query.to_string())
    };

    Some(ProjectSearchResult {
        path: browser_path(relative),
        title,
        snippet: build_snippet(content, &snippet_anchor),
        title_match: title_token_score > 0 || title_has_phrase,
        score,
        vector_score: None,
        images: extract_image_refs(content),
        content: include_content.then_some(content.to_string()),
    })
}

fn result_order(left: &ProjectSearchResult, right: &ProjectSearchResult) -> std::cmp::Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| left.path.cmp(&right.path))
}

pub fn tokenize_query(query: &str) -> Vec<String> {
    let raw = query
        .to_lowercase()
        .split(is_query_separator)
        .filter(|token| token.chars().count() > 1)
        .filter(|token| !is_stop_word(token))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    for token in raw {
        let chars = token.chars().collect::<Vec<_>>();
        let has_cjk = chars
            .iter()
            .any(|character| ('\u{3400}'..='\u{9fff}').contains(character));
        if has_cjk && chars.len() > 2 {
            for pair in chars.windows(2) {
                output.push(pair.iter().collect());
            }
            for character in &chars {
                let value = character.to_string();
                if !is_stop_word(&value) {
                    output.push(value);
                }
            }
            output.push(token);
        } else {
            output.push(token);
        }
    }
    output
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn is_query_separator(character: char) -> bool {
    character.is_whitespace()
        || character.is_ascii_punctuation()
        || matches!(
            character,
            '，' | '。'
                | '！'
                | '？'
                | '、'
                | '；'
                | '：'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '（'
                | '）'
                | '·'
                | '～'
                | '…'
        )
}

fn is_stop_word(token: &str) -> bool {
    matches!(
        token,
        "的" | "是"
            | "了"
            | "什么"
            | "在"
            | "有"
            | "和"
            | "与"
            | "对"
            | "从"
            | "the"
            | "is"
            | "a"
            | "an"
            | "what"
            | "how"
            | "are"
            | "was"
            | "were"
            | "do"
            | "does"
            | "did"
            | "be"
            | "been"
            | "being"
            | "have"
            | "has"
            | "had"
            | "it"
            | "its"
            | "in"
            | "on"
            | "at"
            | "to"
            | "for"
            | "of"
            | "with"
            | "by"
            | "this"
            | "that"
            | "these"
            | "those"
    )
}

fn trim_query_punctuation(value: &str) -> String {
    value.trim_matches(is_query_separator).to_string()
}

fn token_match_score(text: &str, tokens: &[String]) -> usize {
    let lower = text.to_lowercase();
    tokens
        .iter()
        .filter(|token| lower.contains(token.as_str()))
        .count()
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        haystack.match_indices(needle).count()
    }
}

pub fn extract_title(content: &str, file_name: &str) -> String {
    let has_frontmatter = content.starts_with("---");
    let mut in_frontmatter = has_frontmatter;
    let mut frontmatter_closed = false;
    for line in content.lines().skip(usize::from(has_frontmatter)) {
        let trimmed = line.trim();
        if in_frontmatter && trimmed == "---" {
            in_frontmatter = false;
            frontmatter_closed = true;
            continue;
        }
        if in_frontmatter && trimmed.starts_with("title:") {
            return trimmed
                .trim_start_matches("title:")
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
        }
        if has_frontmatter && !frontmatter_closed {
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("# ") {
            return title.trim().to_string();
        }
    }
    file_name.trim_end_matches(".md").replace('-', " ")
}

pub fn extract_image_refs(content: &str) -> Vec<SearchImageRef> {
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let mut rest = content;
    while let Some(start) = rest.find("![") {
        rest = &rest[start + 2..];
        let Some(alt_end) = rest.find("](") else {
            break;
        };
        let alt = &rest[..alt_end];
        rest = &rest[alt_end + 2..];
        let Some(url_end) = rest.find(')') else {
            break;
        };
        let url = &rest[..url_end];
        if !url.trim().is_empty()
            && !url.contains(char::is_whitespace)
            && seen.insert(url.to_string())
        {
            output.push(SearchImageRef {
                url: url.to_string(),
                alt: alt.to_string(),
            });
        }
        rest = &rest[url_end + 1..];
    }
    output
}

fn build_snippet(content: &str, anchor: &str) -> String {
    let lower = content.to_lowercase();
    let anchor_lower = anchor.to_lowercase();
    let byte_position = lower.find(&anchor_lower).unwrap_or(0);
    let char_position = content[..byte_position].chars().count();
    let chars = content.chars().collect::<Vec<_>>();
    let start = char_position.saturating_sub(SNIPPET_CONTEXT);
    let end = (char_position + anchor.chars().count() + SNIPPET_CONTEXT).min(chars.len());
    let mut snippet = chars[start..end]
        .iter()
        .collect::<String>()
        .replace('\n', " ");
    if start > 0 {
        snippet.insert_str(0, "...");
    }
    if end < chars.len() {
        snippet.push_str("...");
    }
    snippet
}

fn first_text_snippet(content: &str) -> String {
    let text = content
        .lines()
        .filter(|line| !line.trim().is_empty() && line.trim() != "---")
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let mut snippet = text.chars().take(SNIPPET_CONTEXT * 2).collect::<String>();
    if text.chars().count() > SNIPPET_CONTEXT * 2 {
        snippet.push_str("...");
    }
    snippet
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(path)
        .to_string()
}

pub(crate) fn browser_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::{extract_image_refs, extract_title, tokenize_query};

    #[test]
    fn search_helpers_preserve_existing_desktop_contract() {
        assert!(tokenize_query("What is vector search?").contains(&"vector".to_string()));
        assert_eq!(
            extract_title("---\ntitle: 'Vector Search'\n---\n# Ignored", "vector.md"),
            "Vector Search"
        );
        assert_eq!(
            extract_image_refs("![diagram](media/vector.png)")[0].url,
            "media/vector.png"
        );
    }
}
