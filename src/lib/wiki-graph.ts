import { webApi } from "@/platform/web-api"
import type {
  KnowledgeGap,
  SurprisingConnection,
} from "@/lib/graph-insights"

export interface GraphNode {
  id: string
  label: string
  type: string
  path: string
  linkCount: number
  community: number
}

export interface GraphEdge {
  source: string
  target: string
  weight: number
}

export interface CommunityInfo {
  id: number
  nodeCount: number
  cohesion: number
  topNodes: string[]
}

export interface WikiGraph {
  nodes: GraphNode[]
  edges: GraphEdge[]
  communities: CommunityInfo[]
  surprisingConnections: SurprisingConnection[]
  knowledgeGaps: KnowledgeGap[]
}

/**
 * Load the server-owned graph bundle for a registered Project.
 *
 * The browser renders and filters this response, but filesystem scanning,
 * wikilink parsing, relevance weights, community assignment, and structural
 * insight detection all stay on the Personal Server.
 */
export async function buildWikiGraph(projectId: string): Promise<WikiGraph> {
  const response = await webApi.graph(projectId)
  return {
    nodes: response.nodes,
    edges: response.edges,
    communities: response.communities,
    surprisingConnections: response.insights.surprisingConnections,
    knowledgeGaps: response.insights.knowledgeGaps,
  }
}
