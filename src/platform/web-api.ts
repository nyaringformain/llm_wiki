export interface WebHealth {
  ok: boolean
  status: "ok" | "setup_required"
  version: string
  dataRootConfigured: boolean
  serverHomeReady: boolean
  databaseReady: boolean
  ownerConfigured: boolean
  setupRequired: boolean
  staticAssetsReady: boolean
}

export interface WebSession {
  ok: boolean
  authenticated: boolean
  setupRequired: boolean
  expiresAt: string | null
}

export interface WebProject {
  id: string
  name: string
  relativePath: string
  source: "created" | "imported"
  createdAt: string
  updatedAt: string
}

export interface WebFileNode {
  name: string
  path: string
  isDir: boolean
  size?: number
  modifiedAtMs: number
  version: string
  children?: WebFileNode[]
}

export interface WebFileInfo {
  path: string
  isDir: boolean
  size?: number
  modifiedAtMs: number
  version: string
  mimeType?: string
}

export interface WebTextFile {
  file: WebFileInfo
  contents: string
}

export interface WebDeleteResult {
  path: string
  trashPath: string
}

export interface WebTreeOptions {
  includeHidden?: boolean
  maxDepth?: number
}

export interface WebSearchImageRef {
  url: string
  alt: string
}

export interface WebSearchResult {
  path: string
  title: string
  snippet: string
  titleMatch: boolean
  score: number
  vectorScore?: number
  images: WebSearchImageRef[]
  content?: string
}

export interface WebSearchResponse {
  ok: boolean
  mode: "keyword" | "graph" | "vector" | "hybrid"
  results: WebSearchResult[]
  tokenHits: number
  graphHits: number
  vectorHits: number
}

export interface WebSearchOptions {
  topK?: number
  includeContent?: boolean
  queryEmbedding?: number[]
  expandGraph?: boolean
}

export interface WebGraphNode {
  id: string
  label: string
  type: string
  path: string
  linkCount: number
  community: number
}

export interface WebGraphEdge {
  source: string
  target: string
  weight: number
}

export interface WebGraphCommunity {
  id: number
  nodeCount: number
  cohesion: number
  topNodes: string[]
}

export interface WebSurprisingConnection {
  source: WebGraphNode
  target: WebGraphNode
  score: number
  reasons: string[]
  key: string
}

export interface WebKnowledgeGap {
  type: "isolated-node" | "sparse-community" | "bridge-node"
  title: string
  description: string
  nodeIds: string[]
  suggestion: string
}

export interface WebGraphResponse {
  ok: boolean
  nodes: WebGraphNode[]
  edges: WebGraphEdge[]
  communities: WebGraphCommunity[]
  insights: {
    surprisingConnections: WebSurprisingConnection[]
    knowledgeGaps: WebKnowledgeGap[]
  }
}

export interface WebVectorChunkResult {
  chunkId: string
  pageId: string
  chunkIndex: number
  chunkText: string
  headingPath: string
  score: number
}

export interface WebVectorStatus {
  ok: boolean
  available: boolean
  chunkCount: number
  legacyPageCount: number
}

export class WebApiError extends Error {
  readonly status: number
  readonly statusText: string
  readonly requestUrl: string
  readonly body: unknown
  readonly cause: unknown

  constructor(
    message: string,
    options: {
      status: number
      statusText?: string
      requestUrl: string
      body?: unknown
      cause?: unknown
    },
  ) {
    super(message)
    this.name = "WebApiError"
    this.status = options.status
    this.statusText = options.statusText ?? ""
    this.requestUrl = options.requestUrl
    this.body = options.body
    this.cause = options.cause
  }
}

export interface WebApi {
  health(): Promise<WebHealth>
  session(): Promise<WebSession>
  setupOwner(password: string): Promise<WebSession>
  login(password: string): Promise<WebSession>
  logout(): Promise<WebSession>
  rotatePassword(
    currentPassword: string,
    newPassword: string,
  ): Promise<WebSession>
  listProjects(): Promise<WebProject[]>
  createProject(name: string): Promise<WebProject>
  tree(
    projectId: string,
    path: string,
    options?: WebTreeOptions,
  ): Promise<WebFileNode[]>
  readText(projectId: string, path: string): Promise<WebTextFile>
  writeText(
    projectId: string,
    path: string,
    contents: string,
    expectedVersion?: string,
  ): Promise<WebFileInfo>
  deletePath(
    projectId: string,
    path: string,
    expectedVersion?: string,
  ): Promise<WebDeleteResult>
  previewUrl(projectId: string, path: string): string
  search(
    projectId: string,
    query: string,
    options?: WebSearchOptions,
  ): Promise<WebSearchResponse>
  graph(projectId: string): Promise<WebGraphResponse>
  vectorStatus(projectId: string): Promise<WebVectorStatus>
  vectorSearch(
    projectId: string,
    queryEmbedding: number[],
    topK?: number,
  ): Promise<WebVectorChunkResult[]>
}

interface ProjectListEnvelope {
  ok: boolean
  projects: WebProject[]
}

interface ProjectEnvelope {
  ok: boolean
  project: WebProject
}

interface FileTreeEnvelope {
  ok: boolean
  path: string
  nodes: WebFileNode[]
  skippedSymlinks: string[]
}

interface FileReadEnvelope extends WebTextFile {
  ok: boolean
}

interface FileWriteEnvelope {
  ok: boolean
  file: WebFileInfo
}

interface FileDeleteEnvelope extends WebDeleteResult {
  ok: boolean
}

interface VectorSearchEnvelope {
  ok: boolean
  results: WebVectorChunkResult[]
}

type Fetch = typeof globalThis.fetch

const authEvents = new EventTarget()
const AUTH_REQUIRED_EVENT = "auth-required"

export function subscribeAuthRequired(listener: () => void): () => void {
  const eventListener = () => listener()
  authEvents.addEventListener(AUTH_REQUIRED_EVENT, eventListener)
  return () => authEvents.removeEventListener(AUTH_REQUIRED_EVENT, eventListener)
}

function notifyAuthRequired(requestUrl: string, status: number): void {
  if (status === 401 && !requestUrl.startsWith("/api/auth/")) {
    authEvents.dispatchEvent(new Event(AUTH_REQUIRED_EVENT))
  }
}

function projectEndpoint(projectId: string, suffix: string): string {
  const normalizedId = projectId.trim()
  if (!normalizedId) {
    throw new TypeError("Project id is required")
  }
  return `/api/projects/${encodeURIComponent(normalizedId)}/${suffix}`
}

function projectRelativePath(path: string, allowEmpty: boolean): string {
  const normalized = path.trim()
  if (normalized.includes("\0")) {
    throw new TypeError("Project-relative path is invalid")
  }
  if (normalized.includes("\\")) {
    throw new TypeError("Project-relative path must use forward slashes")
  }
  if (
    normalized.startsWith("/") ||
    /^[a-z][a-z\d+.-]*:\/\//i.test(normalized)
  ) {
    throw new TypeError("Project-relative path must not be absolute")
  }

  const parts: string[] = []
  for (const part of normalized.split("/")) {
    if (!part) continue
    if (part === "." || part === "..") {
      throw new TypeError("Project-relative path must not contain traversal")
    }
    if (/^[a-z]:/i.test(part)) {
      throw new TypeError("Project-relative path must not be absolute")
    }
    parts.push(part)
  }

  const result = parts.join("/")
  if (!allowEmpty && !result) {
    throw new TypeError("Project-relative path is required")
  }
  return result
}

function treeQuery(path: string, options?: WebTreeOptions): URLSearchParams {
  const query = new URLSearchParams()
  query.set("path", projectRelativePath(path, true))
  if (options?.includeHidden !== undefined) {
    query.set("includeHidden", String(options.includeHidden))
  }
  if (options?.maxDepth !== undefined) {
    if (!Number.isInteger(options.maxDepth) || options.maxDepth < 1) {
      throw new RangeError("maxDepth must be a positive integer")
    }
    query.set("maxDepth", String(options.maxDepth))
  }
  return query
}

function fileQuery(path: string, expectedVersion?: string): URLSearchParams {
  const query = new URLSearchParams()
  query.set("path", projectRelativePath(path, false))
  if (expectedVersion !== undefined) {
    query.set("expectedVersion", expectedVersion)
  }
  return query
}

function positiveInteger(value: number, name: string): number {
  if (!Number.isInteger(value) || value < 1) {
    throw new RangeError(`${name} must be a positive integer`)
  }
  return value
}

function validEmbedding(values: number[]): number[] {
  if (values.length === 0) {
    throw new TypeError("queryEmbedding must not be empty")
  }
  if (!values.every(Number.isFinite)) {
    throw new TypeError("queryEmbedding must contain only finite numbers")
  }
  return values
}

function errorMessage(
  body: unknown,
  rawBody: string,
  status: number,
  statusText: string,
): string {
  if (body && typeof body === "object") {
    const record = body as Record<string, unknown>
    if (typeof record.error === "string" && record.error.trim()) {
      return record.error
    }
    if (typeof record.message === "string" && record.message.trim()) {
      return record.message
    }
  }
  if (rawBody.trim()) return rawBody.trim()
  return statusText
    ? `Request failed: ${status} ${statusText}`
    : `Request failed with status ${status}`
}

function parseBody(rawBody: string): unknown {
  if (!rawBody.trim()) return undefined
  try {
    return JSON.parse(rawBody) as unknown
  } catch {
    return rawBody
  }
}

async function requestJson<T>(
  fetchImpl: Fetch,
  requestUrl: string,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set("Accept", "application/json")
  if (init.body !== undefined && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json")
  }

  let response: Response
  try {
    response = await fetchImpl(requestUrl, {
      ...init,
      credentials: "include",
      headers,
    })
  } catch (cause) {
    const detail = cause instanceof Error && cause.message
      ? `: ${cause.message}`
      : ""
    throw new WebApiError(`Network request failed${detail}`, {
      status: 0,
      requestUrl,
      cause,
    })
  }

  let rawBody: string
  try {
    rawBody = await response.text()
  } catch (cause) {
    throw new WebApiError("Failed to read the server response", {
      status: response.status,
      statusText: response.statusText,
      requestUrl,
      cause,
    })
  }

  const body = parseBody(rawBody)
  if (!response.ok) {
    notifyAuthRequired(requestUrl, response.status)
    throw new WebApiError(
      errorMessage(body, rawBody, response.status, response.statusText),
      {
        status: response.status,
        statusText: response.statusText,
        requestUrl,
        body,
      },
    )
  }
  if (body === undefined || typeof body === "string") {
    throw new WebApiError("Server returned an invalid JSON response", {
      status: response.status,
      statusText: response.statusText,
      requestUrl,
      body,
    })
  }

  return body as T
}

export function createWebApi(fetchImpl: Fetch = globalThis.fetch.bind(globalThis)): WebApi {
  return {
    async health() {
      return requestJson<WebHealth>(fetchImpl, "/api/health")
    },

    async session() {
      return requestJson<WebSession>(fetchImpl, "/api/auth/session")
    },

    async setupOwner(password) {
      return requestJson<WebSession>(fetchImpl, "/api/auth/setup", {
        method: "POST",
        body: JSON.stringify({ password }),
      })
    },

    async login(password) {
      return requestJson<WebSession>(fetchImpl, "/api/auth/login", {
        method: "POST",
        body: JSON.stringify({ password }),
      })
    },

    async logout() {
      return requestJson<WebSession>(fetchImpl, "/api/auth/logout", {
        method: "POST",
      })
    },

    async rotatePassword(currentPassword, newPassword) {
      return requestJson<WebSession>(fetchImpl, "/api/auth/password", {
        method: "PUT",
        body: JSON.stringify({ currentPassword, newPassword }),
      })
    },

    async listProjects() {
      const response = await requestJson<ProjectListEnvelope>(
        fetchImpl,
        "/api/projects",
      )
      return response.projects
    },

    async createProject(name) {
      const response = await requestJson<ProjectEnvelope>(
        fetchImpl,
        "/api/projects",
        {
          method: "POST",
          body: JSON.stringify({ name }),
        },
      )
      return response.project
    },

    async tree(projectId, path, options) {
      const endpoint = projectEndpoint(projectId, "files/tree")
      const query = treeQuery(path, options)
      const response = await requestJson<FileTreeEnvelope>(
        fetchImpl,
        `${endpoint}?${query.toString()}`,
      )
      return response.nodes
    },

    async readText(projectId, path) {
      const endpoint = projectEndpoint(projectId, "files/read")
      const query = fileQuery(path)
      const response = await requestJson<FileReadEnvelope>(
        fetchImpl,
        `${endpoint}?${query.toString()}`,
      )
      return { file: response.file, contents: response.contents }
    },

    async writeText(projectId, path, contents, expectedVersion) {
      const endpoint = projectEndpoint(projectId, "files/write")
      const body: {
        path: string
        contents: string
        expectedVersion?: string
      } = {
        path: projectRelativePath(path, false),
        contents,
      }
      if (expectedVersion !== undefined) {
        body.expectedVersion = expectedVersion
      }
      const response = await requestJson<FileWriteEnvelope>(fetchImpl, endpoint, {
        method: "PUT",
        body: JSON.stringify(body),
      })
      return response.file
    },

    async deletePath(projectId, path, expectedVersion) {
      const endpoint = projectEndpoint(projectId, "files")
      const query = fileQuery(path, expectedVersion)
      const response = await requestJson<FileDeleteEnvelope>(
        fetchImpl,
        `${endpoint}?${query.toString()}`,
        { method: "DELETE" },
      )
      return { path: response.path, trashPath: response.trashPath }
    },

    previewUrl(projectId, path) {
      const endpoint = projectEndpoint(projectId, "files/preview")
      const query = fileQuery(path)
      return `${endpoint}?${query.toString()}`
    },

    async search(projectId, query, options) {
      const normalizedQuery = query.trim()
      if (!normalizedQuery) throw new TypeError("Search query is required")
      const endpoint = projectEndpoint(projectId, "search")
      const body: Record<string, unknown> = {
        query: normalizedQuery,
        expandGraph: options?.expandGraph ?? true,
      }
      if (options?.topK !== undefined) {
        body.topK = positiveInteger(options.topK, "topK")
      }
      if (options?.includeContent !== undefined) {
        body.includeContent = options.includeContent
      }
      if (options?.queryEmbedding !== undefined) {
        body.queryEmbedding = validEmbedding(options.queryEmbedding)
      }
      return requestJson<WebSearchResponse>(fetchImpl, endpoint, {
        method: "POST",
        body: JSON.stringify(body),
      })
    },

    async graph(projectId) {
      return requestJson<WebGraphResponse>(
        fetchImpl,
        projectEndpoint(projectId, "graph"),
      )
    },

    async vectorStatus(projectId) {
      return requestJson<WebVectorStatus>(
        fetchImpl,
        projectEndpoint(projectId, "vectors/status"),
      )
    },

    async vectorSearch(projectId, queryEmbedding, topK) {
      const body: { queryEmbedding: number[]; topK?: number } = {
        queryEmbedding: validEmbedding(queryEmbedding),
      }
      if (topK !== undefined) body.topK = positiveInteger(topK, "topK")
      const response = await requestJson<VectorSearchEnvelope>(
        fetchImpl,
        projectEndpoint(projectId, "vectors/search"),
        { method: "POST", body: JSON.stringify(body) },
      )
      return response.results
    },
  }
}

export const webApi = createWebApi()
