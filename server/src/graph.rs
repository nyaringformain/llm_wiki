use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::files::FileService;
use crate::search::{browser_path, wiki_markdown_files, ContentServiceError};

const MAX_COMMUNITY_PASSES: usize = 20;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
    pub link_count: usize,
    pub community: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityInfo {
    pub id: usize,
    pub node_count: usize,
    pub cohesion: f64,
    pub top_nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurprisingConnection {
    pub source: GraphNode,
    pub target: GraphNode,
    pub score: f64,
    pub reasons: Vec<String>,
    pub key: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeGap {
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub description: String,
    pub node_ids: Vec<String>,
    pub suggestion: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphInsights {
    pub surprising_connections: Vec<SurprisingConnection>,
    pub knowledge_gaps: Vec<KnowledgeGap>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGraphResponse {
    pub ok: bool,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub communities: Vec<CommunityInfo>,
    pub insights: GraphInsights,
}

#[derive(Debug, Clone)]
struct RawNode {
    id: String,
    label: String,
    kind: String,
    path: String,
    sources: BTreeSet<String>,
    raw_links: Vec<String>,
}

#[derive(Debug, Clone)]
struct RetrievalNode {
    raw: RawNode,
    out_links: BTreeSet<String>,
    in_links: BTreeSet<String>,
}

pub async fn project_graph(
    files: &FileService,
    project_id: &str,
) -> Result<ProjectGraphResponse, ContentServiceError> {
    let root = files.project_root(project_id).await?;
    build_graph_at_root(&root)
}

pub(crate) fn build_graph_at_root(
    root: &Path,
) -> Result<ProjectGraphResponse, ContentServiceError> {
    let mut raw_nodes = BTreeMap::new();
    for relative in wiki_markdown_files(root)? {
        let content = match fs::read_to_string(root.join(&relative)) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(error) => return Err(anyhow::Error::from(error).into()),
        };
        let Some(file_name) = relative.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let id = file_name.trim_end_matches(".md").to_string();
        let metadata = extract_metadata(&content, file_name);
        if metadata.kind == "query" {
            continue;
        }
        raw_nodes.insert(
            id.clone(),
            RawNode {
                id,
                label: metadata.title,
                kind: metadata.kind,
                path: browser_path(&relative),
                sources: metadata.sources,
                raw_links: extract_wikilinks(&content),
            },
        );
    }

    let node_ids = raw_nodes.keys().cloned().collect::<BTreeSet<_>>();
    let mut retrieval = raw_nodes
        .into_iter()
        .map(|(id, raw)| {
            (
                id,
                RetrievalNode {
                    raw,
                    out_links: BTreeSet::new(),
                    in_links: BTreeSet::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut link_counts = node_ids
        .iter()
        .map(|id| (id.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut raw_edges = Vec::new();

    let links = retrieval
        .values()
        .map(|node| (node.raw.id.clone(), node.raw.raw_links.clone()))
        .collect::<Vec<_>>();
    for (source, targets) in links {
        for target in targets {
            let Some(target) = resolve_target(&target, &node_ids) else {
                continue;
            };
            if source == target {
                continue;
            }
            retrieval
                .get_mut(&source)
                .expect("source node exists")
                .out_links
                .insert(target.clone());
            retrieval
                .get_mut(&target)
                .expect("target node exists")
                .in_links
                .insert(source.clone());
            *link_counts.entry(source.clone()).or_default() += 1;
            *link_counts.entry(target.clone()).or_default() += 1;
            raw_edges.push((source.clone(), target));
        }
    }

    let mut seen = BTreeSet::new();
    let mut edges = Vec::new();
    for (source, target) in raw_edges {
        let key = canonical_edge_key(&source, &target);
        if !seen.insert(key) {
            continue;
        }
        let weight = calculate_relevance(
            retrieval.get(&source).expect("source node exists"),
            retrieval.get(&target).expect("target node exists"),
            &retrieval,
        );
        edges.push(GraphEdge {
            source,
            target,
            weight,
        });
    }
    edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.target.cmp(&right.target))
    });

    let assignments = detect_communities(&node_ids, &edges);
    let mut nodes = retrieval
        .values()
        .map(|node| GraphNode {
            id: node.raw.id.clone(),
            label: node.raw.label.clone(),
            kind: node.raw.kind.clone(),
            path: node.raw.path.clone(),
            link_count: *link_counts.get(&node.raw.id).unwrap_or(&0),
            community: *assignments.get(&node.raw.id).unwrap_or(&0),
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let communities = community_info(&nodes, &edges);
    let insights = GraphInsights {
        surprising_connections: surprising_connections(&nodes, &edges, 5),
        knowledge_gaps: knowledge_gaps(&nodes, &edges, &communities, 8),
    };

    Ok(ProjectGraphResponse {
        ok: true,
        nodes,
        edges,
        communities,
        insights,
    })
}

pub(crate) fn neighbor_paths(
    graph: &ProjectGraphResponse,
    seed_paths: &BTreeSet<String>,
) -> BTreeSet<String> {
    let ids_by_path = graph
        .nodes
        .iter()
        .map(|node| (node.path.clone(), node.id.clone()))
        .collect::<BTreeMap<_, _>>();
    let paths_by_id = graph
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.path.clone()))
        .collect::<BTreeMap<_, _>>();
    let seed_ids = seed_paths
        .iter()
        .filter_map(|path| ids_by_path.get(path).cloned())
        .collect::<BTreeSet<_>>();
    let mut output = BTreeSet::new();
    for edge in &graph.edges {
        if seed_ids.contains(&edge.source) && !seed_ids.contains(&edge.target) {
            if let Some(path) = paths_by_id.get(&edge.target) {
                output.insert(path.clone());
            }
        }
        if seed_ids.contains(&edge.target) && !seed_ids.contains(&edge.source) {
            if let Some(path) = paths_by_id.get(&edge.source) {
                output.insert(path.clone());
            }
        }
    }
    output
}

struct PageMetadata {
    title: String,
    kind: String,
    sources: BTreeSet<String>,
}

fn extract_metadata(content: &str, file_name: &str) -> PageMetadata {
    let mut title = String::new();
    let mut kind = "other".to_string();
    let mut sources = BTreeSet::new();
    let mut in_frontmatter = content.starts_with("---");
    let mut frontmatter_closed = !in_frontmatter;
    let mut in_sources = false;

    for line in content.lines().skip(usize::from(in_frontmatter)) {
        let trimmed = line.trim();
        if in_frontmatter && trimmed == "---" {
            in_frontmatter = false;
            frontmatter_closed = true;
            in_sources = false;
            continue;
        }
        if in_frontmatter {
            if let Some(value) = trimmed.strip_prefix("title:") {
                title = unquote(value);
                in_sources = false;
            } else if let Some(value) = trimmed.strip_prefix("type:") {
                kind = unquote(value).to_lowercase();
                in_sources = false;
            } else if let Some(value) = trimmed.strip_prefix("sources:") {
                in_sources = true;
                parse_inline_sources(value, &mut sources);
            } else if in_sources {
                if let Some(value) = trimmed.strip_prefix('-') {
                    let source = unquote(value);
                    if !source.is_empty() {
                        sources.insert(source);
                    }
                } else if !trimmed.is_empty() {
                    in_sources = false;
                }
            }
            continue;
        }
        if frontmatter_closed && title.is_empty() {
            if let Some(value) = trimmed.strip_prefix("# ") {
                title = value.trim().to_string();
            }
        }
    }
    if title.is_empty() {
        title = file_name.trim_end_matches(".md").replace('-', " ");
    }
    PageMetadata {
        title,
        kind,
        sources,
    }
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn parse_inline_sources(value: &str, sources: &mut BTreeSet<String>) {
    let value = value.trim();
    let Some(value) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return;
    };
    for source in value
        .split(',')
        .map(unquote)
        .filter(|value| !value.is_empty())
    {
        sources.insert(source);
    }
}

fn extract_wikilinks(content: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[[") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("]]") else {
            break;
        };
        let target = rest[..end].split('|').next().unwrap_or("").trim();
        if !target.is_empty() {
            links.push(target.to_string());
        }
        rest = &rest[end + 2..];
    }
    links
}

fn resolve_target(raw: &str, ids: &BTreeSet<String>) -> Option<String> {
    if ids.contains(raw) {
        return Some(raw.to_string());
    }
    let raw_lower = raw.to_lowercase();
    let normalized = raw_lower.split_whitespace().collect::<Vec<_>>().join("-");
    ids.iter()
        .find(|id| {
            let id_lower = id.to_lowercase();
            id_lower == raw_lower
                || id_lower == normalized
                || id_lower.split_whitespace().collect::<Vec<_>>().join("-") == normalized
        })
        .cloned()
}

fn calculate_relevance(
    left: &RetrievalNode,
    right: &RetrievalNode,
    graph: &BTreeMap<String, RetrievalNode>,
) -> f64 {
    let direct = (usize::from(left.out_links.contains(&right.raw.id))
        + usize::from(right.out_links.contains(&left.raw.id))) as f64
        * 3.0;
    let source_overlap = left.raw.sources.intersection(&right.raw.sources).count() as f64 * 4.0;
    let left_neighbors = neighbors(left);
    let right_neighbors = neighbors(right);
    let adamic_adar = left_neighbors
        .intersection(&right_neighbors)
        .filter_map(|id| graph.get(id))
        .map(|node| {
            let degree = (node.out_links.len() + node.in_links.len()).max(2) as f64;
            1.0 / degree.ln()
        })
        .sum::<f64>()
        * 1.5;
    direct + source_overlap + adamic_adar + type_affinity(&left.raw.kind, &right.raw.kind)
}

fn neighbors(node: &RetrievalNode) -> BTreeSet<String> {
    node.out_links.union(&node.in_links).cloned().collect()
}

fn type_affinity(left: &str, right: &str) -> f64 {
    match (left, right) {
        ("entity", "concept") | ("concept", "entity") => 1.2,
        ("concept", "synthesis") | ("synthesis", "concept") => 1.2,
        ("entity", "entity") | ("concept", "concept") | ("synthesis", "synthesis") => 0.8,
        ("source", "source") => 0.5,
        ("source", _) | (_, "source") => 1.0,
        _ => 0.5,
    }
}

fn detect_communities(node_ids: &BTreeSet<String>, edges: &[GraphEdge]) -> BTreeMap<String, usize> {
    let mut labels = node_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = node_ids
        .iter()
        .map(|id| (id.clone(), Vec::<(String, f64)>::new()))
        .collect::<BTreeMap<_, _>>();
    for edge in edges {
        adjacency
            .entry(edge.source.clone())
            .or_default()
            .push((edge.target.clone(), edge.weight));
        adjacency
            .entry(edge.target.clone())
            .or_default()
            .push((edge.source.clone(), edge.weight));
    }

    for _ in 0..MAX_COMMUNITY_PASSES {
        let mut changed = false;
        for id in node_ids {
            let neighbors = &adjacency[id];
            if neighbors.is_empty() {
                continue;
            }
            let mut scores = BTreeMap::<usize, f64>::new();
            for (neighbor, weight) in neighbors {
                *scores.entry(labels[neighbor]).or_default() += weight.max(0.000_001);
            }
            let best = scores.into_iter().max_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| right.0.cmp(&left.0))
            });
            if let Some((best_label, _)) = best {
                if labels[id] != best_label {
                    labels.insert(id.clone(), best_label);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut groups = BTreeMap::<usize, Vec<String>>::new();
    for (id, label) in &labels {
        groups.entry(*label).or_default().push(id.clone());
    }
    let mut ordered = groups.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| left.1[0].cmp(&right.1[0]))
    });
    let remap = ordered
        .iter()
        .enumerate()
        .map(|(new_id, (old_id, _))| (*old_id, new_id))
        .collect::<BTreeMap<_, _>>();
    labels
        .into_iter()
        .map(|(id, old_id)| (id, remap[&old_id]))
        .collect()
}

fn community_info(nodes: &[GraphNode], edges: &[GraphEdge]) -> Vec<CommunityInfo> {
    let mut grouped = BTreeMap::<usize, Vec<&GraphNode>>::new();
    for node in nodes {
        grouped.entry(node.community).or_default().push(node);
    }
    let edge_set = edges
        .iter()
        .map(|edge| canonical_edge_key(&edge.source, &edge.target))
        .collect::<BTreeSet<_>>();
    let mut communities = Vec::new();
    for (id, mut members) in grouped {
        members.sort_by(|left, right| {
            right
                .link_count
                .cmp(&left.link_count)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut intra_edges = 0usize;
        for left in 0..members.len() {
            for right in (left + 1)..members.len() {
                if edge_set.contains(&canonical_edge_key(&members[left].id, &members[right].id)) {
                    intra_edges += 1;
                }
            }
        }
        let possible = if members.len() > 1 {
            members.len() * (members.len() - 1) / 2
        } else {
            1
        };
        communities.push(CommunityInfo {
            id,
            node_count: members.len(),
            cohesion: intra_edges as f64 / possible as f64,
            top_nodes: members
                .iter()
                .take(5)
                .map(|node| node.label.clone())
                .collect(),
        });
    }
    communities.sort_by_key(|community| community.id);
    communities
}

fn surprising_connections(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    limit: usize,
) -> Vec<SurprisingConnection> {
    let node_map = nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let max_degree = nodes
        .iter()
        .map(|node| node.link_count)
        .max()
        .unwrap_or(1)
        .max(1);
    let structural = ["index", "log", "overview"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let distant_pairs = [
        ("source", "concept"),
        ("concept", "source"),
        ("source", "synthesis"),
        ("synthesis", "source"),
        ("query", "entity"),
        ("entity", "query"),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let mut output = Vec::new();

    for edge in edges {
        let (Some(source), Some(target)) = (node_map.get(&edge.source), node_map.get(&edge.target))
        else {
            continue;
        };
        if structural.contains(source.id.as_str()) || structural.contains(target.id.as_str()) {
            continue;
        }
        let mut score = 0.0;
        let mut reasons = Vec::new();
        if source.community != target.community {
            score += 3.0;
            reasons.push("crosses community boundary".to_string());
        }
        if source.kind != target.kind {
            if distant_pairs.contains(&(source.kind.as_str(), target.kind.as_str())) {
                score += 2.0;
                reasons.push(format!("connects {} to {}", source.kind, target.kind));
            } else {
                score += 1.0;
                reasons.push("different types".to_string());
            }
        }
        let minimum = source.link_count.min(target.link_count);
        let maximum = source.link_count.max(target.link_count);
        if minimum <= 2 && maximum as f64 >= max_degree as f64 * 0.5 {
            score += 2.0;
            reasons.push("peripheral node links to hub".to_string());
        }
        if edge.weight > 0.0 && edge.weight < 2.0 {
            score += 1.0;
            reasons.push("weak but present connection".to_string());
        }
        if score >= 3.0 {
            let mut ids = [source.id.clone(), target.id.clone()];
            ids.sort();
            output.push(SurprisingConnection {
                source: (*source).clone(),
                target: (*target).clone(),
                score,
                reasons,
                key: ids.join(":::"),
            });
        }
    }
    output.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.key.cmp(&right.key))
    });
    output.truncate(limit);
    output
}

fn knowledge_gaps(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    communities: &[CommunityInfo],
    limit: usize,
) -> Vec<KnowledgeGap> {
    let mut output = Vec::new();
    let isolated = nodes
        .iter()
        .filter(|node| {
            node.link_count <= 1
                && node.kind != "overview"
                && node.id != "index"
                && node.id != "log"
        })
        .collect::<Vec<_>>();
    if !isolated.is_empty() {
        let mut description = isolated
            .iter()
            .take(5)
            .map(|node| node.label.clone())
            .collect::<Vec<_>>()
            .join(", ");
        if isolated.len() > 5 {
            description.push_str(&format!(" and {} more", isolated.len() - 5));
        }
        output.push(KnowledgeGap {
            kind: "isolated-node".to_string(),
            title: format!(
                "{} isolated page{}",
                isolated.len(),
                if isolated.len() == 1 { "" } else { "s" }
            ),
            description,
            node_ids: isolated.iter().map(|node| node.id.clone()).collect(),
            suggestion: "These pages have few or no connections. Consider adding [[wikilinks]] to related pages, or research to expand their content.".to_string(),
        });
    }
    for community in communities {
        if community.cohesion < 0.15 && community.node_count >= 3 {
            output.push(KnowledgeGap {
                kind: "sparse-community".to_string(),
                title: format!(
                    "Sparse cluster: {}",
                    community
                        .top_nodes
                        .first()
                        .cloned()
                        .unwrap_or_else(|| format!("Community {}", community.id))
                ),
                description: format!(
                    "{} pages with cohesion {:.2} — internal connections are weak.",
                    community.node_count, community.cohesion
                ),
                node_ids: nodes
                    .iter()
                    .filter(|node| node.community == community.id)
                    .map(|node| node.id.clone())
                    .collect(),
                suggestion: "This knowledge area lacks internal cross-references. Consider adding links between these pages or researching to fill gaps.".to_string(),
            });
        }
    }

    let node_map = nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut neighbor_communities = nodes
        .iter()
        .map(|node| (node.id.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, BTreeSet<usize>>>();
    for edge in edges {
        if let (Some(source), Some(target)) =
            (node_map.get(&edge.source), node_map.get(&edge.target))
        {
            neighbor_communities
                .entry(source.id.clone())
                .or_default()
                .insert(target.community);
            neighbor_communities
                .entry(target.id.clone())
                .or_default()
                .insert(source.community);
        }
    }
    let mut bridges = nodes
        .iter()
        .filter(|node| {
            node.id != "index"
                && node.id != "log"
                && node.id != "overview"
                && neighbor_communities[&node.id].len() >= 3
        })
        .collect::<Vec<_>>();
    bridges.sort_by(|left, right| {
        neighbor_communities[&right.id]
            .len()
            .cmp(&neighbor_communities[&left.id].len())
            .then_with(|| left.id.cmp(&right.id))
    });
    for bridge in bridges.into_iter().take(3) {
        let community_count = neighbor_communities[&bridge.id].len();
        output.push(KnowledgeGap {
            kind: "bridge-node".to_string(),
            title: format!("Key bridge: {}", bridge.label),
            description: format!(
                "Connects {community_count} different knowledge clusters. This is a critical junction in your wiki."
            ),
            node_ids: vec![bridge.id.clone()],
            suggestion: "This page bridges multiple knowledge areas. Ensure it's well-maintained — if it's thin, expanding it will strengthen your entire wiki.".to_string(),
        });
    }
    output.truncate(limit);
    output
}

fn canonical_edge_key(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_string(), right.to_string())
    } else {
        (right.to_string(), left.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build_graph_at_root;

    #[test]
    fn builds_relative_graph_and_structural_insights() {
        let temp = tempfile::tempdir().unwrap();
        let wiki = temp.path().join("wiki/concepts");
        fs::create_dir_all(&wiki).unwrap();
        fs::write(
            wiki.join("alpha.md"),
            "---\ntitle: Alpha\ntype: concept\nsources: [paper.pdf]\n---\n[[beta]]",
        )
        .unwrap();
        fs::write(
            wiki.join("beta.md"),
            "---\ntitle: Beta\ntype: entity\nsources: [paper.pdf]\n---\n[[alpha]]",
        )
        .unwrap();

        let graph = build_graph_at_root(temp.path()).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.nodes[0].path, "wiki/concepts/alpha.md");
        assert!(graph.edges[0].weight > 4.0);
        assert!(!graph.communities.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn graph_scan_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("wiki")).unwrap();
        fs::write(temp.path().join("outside.md"), "# Outside").unwrap();
        symlink(
            temp.path().join("outside.md"),
            temp.path().join("wiki/leak.md"),
        )
        .unwrap();
        assert!(build_graph_at_root(temp.path()).is_err());
    }
}
