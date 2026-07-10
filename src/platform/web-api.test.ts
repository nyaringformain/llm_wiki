import { describe, expect, it, vi } from "vitest"
import {
  createWebApi,
  subscribeAuthRequired,
  WebApiError,
  type WebFileInfo,
  type WebProject,
} from "./web-api"

function jsonResponse(
  body: unknown,
  init: { status?: number; statusText?: string } = {},
): Response {
  return new Response(JSON.stringify(body), {
    status: init.status ?? 200,
    statusText: init.statusText,
    headers: { "Content-Type": "application/json" },
  })
}

function mockApi(response: Response) {
  const fetchMock = vi.fn<typeof fetch>()
  fetchMock.mockResolvedValueOnce(response)
  return { api: createWebApi(fetchMock), fetchMock }
}

function requestUrl(input: RequestInfo | URL): URL {
  return new URL(String(input), "http://example.test")
}

const project: WebProject = {
  id: "project-1",
  name: "Research Notes",
  relativePath: "research-notes",
  source: "created",
  createdAt: "2026-07-10 01:00:00",
  updatedAt: "2026-07-10 01:00:00",
}

const fileInfo: WebFileInfo = {
  path: "wiki/notes.md",
  isDir: false,
  size: 8,
  modifiedAtMs: 1_720_000_000_000,
  version: "8:1720000000000",
  mimeType: "text/markdown",
}

describe("webApi server and project requests", () => {
  it("loads health from the same-origin API with credentials", async () => {
    const payload = {
      ok: true,
      status: "ok",
      version: "0.6.0",
      dataRootConfigured: true,
      serverHomeReady: true,
      databaseReady: true,
      ownerConfigured: true,
      setupRequired: false,
      staticAssetsReady: true,
    } as const
    const { api, fetchMock } = mockApi(jsonResponse(payload))

    await expect(api.health()).resolves.toEqual(payload)

    expect(fetchMock).toHaveBeenCalledTimes(1)
    const [input, init] = fetchMock.mock.calls[0]
    expect(input).toBe("/api/health")
    expect(init?.credentials).toBe("include")
    expect(new Headers(init?.headers).get("Accept")).toBe("application/json")
    expect(new Headers(init?.headers).has("Content-Type")).toBe(false)
  })

  it("checks the same-origin owner session with credentials", async () => {
    const payload = {
      ok: true,
      authenticated: true,
      setupRequired: false,
      expiresAt: "2026-07-11 01:00:00",
    }
    const { api, fetchMock } = mockApi(jsonResponse(payload))

    await expect(api.session()).resolves.toEqual(payload)
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/auth/session",
      expect.objectContaining({ credentials: "include" }),
    )
  })

  it.each([
    ["setupOwner", "/api/auth/setup", "POST", { password: "correct horse" }],
    ["login", "/api/auth/login", "POST", { password: "correct horse" }],
    [
      "rotatePassword",
      "/api/auth/password",
      "PUT",
      { currentPassword: "correct horse", newPassword: "new correct horse" },
    ],
  ] as const)("sends %s credentials only in a JSON request body", async (
    methodName,
    endpoint,
    method,
    expectedBody,
  ) => {
    const session = {
      ok: true,
      authenticated: true,
      setupRequired: false,
      expiresAt: "2026-07-11T01:00:00Z",
    }
    const { api, fetchMock } = mockApi(jsonResponse(session))

    if (methodName === "rotatePassword") {
      await api.rotatePassword("correct horse", "new correct horse")
    } else {
      await api[methodName]("correct horse")
    }

    const [input, init] = fetchMock.mock.calls[0]
    expect(input).toBe(endpoint)
    expect(String(input)).not.toContain("correct horse")
    expect(init?.method).toBe(method)
    expect(init?.credentials).toBe("include")
    expect(JSON.parse(String(init?.body))).toEqual(expectedBody)
  })

  it("logs out without exposing or retaining a browser token", async () => {
    const session = {
      ok: true,
      authenticated: false,
      setupRequired: false,
      expiresAt: null,
    }
    const { api, fetchMock } = mockApi(jsonResponse(session))

    await expect(api.logout()).resolves.toEqual(session)
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/auth/logout",
      expect.objectContaining({ method: "POST", credentials: "include" }),
    )
    expect(fetchMock.mock.calls[0][1]?.body).toBeUndefined()
  })

  it("lists projects without inventing browser-visible absolute paths", async () => {
    const { api, fetchMock } = mockApi(jsonResponse({
      ok: true,
      projects: [project],
    }))

    await expect(api.listProjects()).resolves.toEqual([project])
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/projects",
      expect.objectContaining({ credentials: "include" }),
    )
    expect(project).not.toHaveProperty("path")
  })

  it("creates a project with a JSON name request and preserves registry metadata", async () => {
    const { api, fetchMock } = mockApi(jsonResponse({ ok: true, project }, {
      status: 201,
    }))

    await expect(api.createProject("Research Notes")).resolves.toEqual(project)

    const [input, init] = fetchMock.mock.calls[0]
    expect(input).toBe("/api/projects")
    expect(init?.method).toBe("POST")
    expect(init?.credentials).toBe("include")
    expect(new Headers(init?.headers).get("Content-Type")).toBe(
      "application/json",
    )
    expect(JSON.parse(String(init?.body))).toEqual({ name: "Research Notes" })
  })
})

describe("webApi project file requests", () => {
  it("loads a typed tree with encoded query options and full file metadata", async () => {
    const payload = {
      ok: true,
      path: "wiki/My notes",
      skippedSymlinks: ["wiki/My notes/linked.md"],
      nodes: [
        {
          name: "draft.md",
          path: "wiki/My notes/draft.md",
          isDir: false,
          size: 12,
          modifiedAtMs: 1_720_000_000_001,
          version: "12:1720000000001",
        },
      ],
    }
    const { api, fetchMock } = mockApi(jsonResponse(payload))

    await expect(api.tree("project-1", "wiki/My notes", {
      includeHidden: true,
      maxDepth: 2,
    })).resolves.toEqual(payload.nodes)

    const [input, init] = fetchMock.mock.calls[0]
    const url = requestUrl(input)
    expect(url.pathname).toBe("/api/projects/project-1/files/tree")
    expect(url.searchParams.get("path")).toBe("wiki/My notes")
    expect(url.searchParams.get("includeHidden")).toBe("true")
    expect(url.searchParams.get("maxDepth")).toBe("2")
    expect(init?.credentials).toBe("include")
  })

  it("allows the empty path only for a project-root tree", async () => {
    const { api, fetchMock } = mockApi(jsonResponse({
      ok: true,
      path: "",
      nodes: [],
      skippedSymlinks: [],
    }))

    await api.tree("project-1", "")

    const url = requestUrl(fetchMock.mock.calls[0][0])
    expect(url.searchParams.get("path")).toBe("")
  })

  it("reads text together with the version needed for a later write", async () => {
    const { api, fetchMock } = mockApi(jsonResponse({
      ok: true,
      file: fileInfo,
      contents: "# Notes\n",
    }))

    await expect(api.readText("project-1", "wiki/notes.md")).resolves.toEqual({
      file: fileInfo,
      contents: "# Notes\n",
    })

    const url = requestUrl(fetchMock.mock.calls[0][0])
    expect(url.pathname).toBe("/api/projects/project-1/files/read")
    expect(url.searchParams.get("path")).toBe("wiki/notes.md")
  })

  it("writes text with an expected version and returns the new metadata", async () => {
    const updated = {
      ...fileInfo,
      size: 16,
      version: "16:1720000000100",
    }
    const { api, fetchMock } = mockApi(jsonResponse({ ok: true, file: updated }))

    await expect(api.writeText(
      "project-1",
      "wiki/notes.md",
      "# Updated Notes\n",
      fileInfo.version,
    )).resolves.toEqual(updated)

    const [input, init] = fetchMock.mock.calls[0]
    expect(input).toBe("/api/projects/project-1/files/write")
    expect(init?.method).toBe("PUT")
    expect(init?.credentials).toBe("include")
    expect(JSON.parse(String(init?.body))).toEqual({
      path: "wiki/notes.md",
      contents: "# Updated Notes\n",
      expectedVersion: fileInfo.version,
    })
  })

  it("omits expectedVersion when creating a new text file", async () => {
    const { api, fetchMock } = mockApi(jsonResponse({ ok: true, file: fileInfo }))

    await api.writeText("project-1", "wiki/notes.md", "# Notes\n")

    expect(JSON.parse(String(fetchMock.mock.calls[0][1]?.body))).toEqual({
      path: "wiki/notes.md",
      contents: "# Notes\n",
    })
  })

  it("deletes through the trash endpoint with an optional version precondition", async () => {
    const payload = {
      ok: true,
      path: "wiki/notes.md",
      trashPath: ".llm-wiki/trash/20260710/wiki/notes.md",
    }
    const { api, fetchMock } = mockApi(jsonResponse(payload))

    await expect(api.deletePath(
      "project-1",
      "wiki/notes.md",
      fileInfo.version,
    )).resolves.toEqual({
      path: payload.path,
      trashPath: payload.trashPath,
    })

    const [input, init] = fetchMock.mock.calls[0]
    const url = requestUrl(input)
    expect(url.pathname).toBe("/api/projects/project-1/files")
    expect(url.searchParams.get("path")).toBe("wiki/notes.md")
    expect(url.searchParams.get("expectedVersion")).toBe(fileInfo.version)
    expect(init?.method).toBe("DELETE")
    expect(init?.credentials).toBe("include")
  })

  it("builds a same-origin authenticated preview route without fetching it", () => {
    const fetchMock = vi.fn<typeof fetch>()
    const api = createWebApi(fetchMock)

    const url = requestUrl(api.previewUrl(
      "project/id",
      "raw/sources/My report & notes.pdf",
    ))

    expect(url.pathname).toBe(
      "/api/projects/project%2Fid/files/preview",
    )
    expect(url.searchParams.get("path")).toBe(
      "raw/sources/My report & notes.pdf",
    )
    expect(fetchMock).not.toHaveBeenCalled()
  })
})

describe("webApi input validation", () => {
  it.each([
    "/etc/passwd",
    "../outside.md",
    "wiki/../outside.md",
    "wiki/./notes.md",
    "C:/Users/owner/notes.md",
    "wiki/C:/Users/owner/notes.md",
    "C:\\Users\\owner\\notes.md",
    "\\\\server\\share\\notes.md",
    "https://example.test/notes.md",
  ])("rejects non-project-relative path %s before fetching", async (path) => {
    const fetchMock = vi.fn<typeof fetch>()
    const api = createWebApi(fetchMock)

    await expect(api.readText("project-1", path)).rejects.toThrow(
      /project-relative path/i,
    )
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("rejects an empty text path and an empty project id", async () => {
    const fetchMock = vi.fn<typeof fetch>()
    const api = createWebApi(fetchMock)

    await expect(api.readText("project-1", " ")).rejects.toThrow(
      /path is required/i,
    )
    await expect(api.readText(" ", "wiki/notes.md")).rejects.toThrow(
      /project id is required/i,
    )
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("rejects invalid tree depths before fetching", async () => {
    const fetchMock = vi.fn<typeof fetch>()
    const api = createWebApi(fetchMock)

    await expect(api.tree("project-1", "wiki", { maxDepth: 0 })).rejects.toThrow(
      /positive integer/i,
    )
    await expect(api.tree("project-1", "wiki", { maxDepth: 1.5 })).rejects.toThrow(
      /positive integer/i,
    )
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("validates preview paths synchronously", () => {
    const api = createWebApi(vi.fn<typeof fetch>())

    expect(() => api.previewUrl("project-1", "../secret.txt")).toThrow(
      /project-relative path/i,
    )
  })
})

describe("webApi search, graph, and vector requests", () => {
  it("searches by project id with graph expansion and no filesystem path", async () => {
    const payload = {
      ok: true,
      mode: "graph" as const,
      results: [{
        path: "wiki/concepts/vector-search.md",
        title: "Vector Search",
        snippet: "Retrieval",
        titleMatch: true,
        score: 10,
        images: [],
      }],
      tokenHits: 1,
      graphHits: 0,
      vectorHits: 0,
    }
    const { api, fetchMock } = mockApi(jsonResponse(payload))

    await expect(api.search("project-1", " vector search ", {
      topK: 12,
    })).resolves.toEqual(payload)

    const [input, init] = fetchMock.mock.calls[0]
    expect(input).toBe("/api/projects/project-1/search")
    expect(init?.method).toBe("POST")
    expect(JSON.parse(String(init?.body))).toEqual({
      query: "vector search",
      expandGraph: true,
      topK: 12,
    })
    expect(String(init?.body)).not.toContain("/home/")
  })

  it("loads graph data and structural insights by project id", async () => {
    const payload = {
      ok: true,
      nodes: [],
      edges: [],
      communities: [],
      insights: { surprisingConnections: [], knowledgeGaps: [] },
    }
    const { api, fetchMock } = mockApi(jsonResponse(payload))

    await expect(api.graph("project-1")).resolves.toEqual(payload)
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/projects/project-1/graph",
      expect.objectContaining({ credentials: "include" }),
    )
  })

  it("queries vector status and explicit embeddings without provider secrets", async () => {
    const status = {
      ok: true,
      available: true,
      chunkCount: 2,
      legacyPageCount: 0,
    }
    const statusMock = mockApi(jsonResponse(status))
    await expect(statusMock.api.vectorStatus("project-1")).resolves.toEqual(status)

    const chunks = [{
      chunkId: "alpha#0",
      pageId: "alpha",
      chunkIndex: 0,
      chunkText: "alpha",
      headingPath: "# Alpha",
      score: 1,
    }]
    const searchMock = mockApi(jsonResponse({ ok: true, results: chunks }))
    await expect(
      searchMock.api.vectorSearch("project-1", [1, 0, 0], 5),
    ).resolves.toEqual(chunks)
    const [input, init] = searchMock.fetchMock.mock.calls[0]
    expect(input).toBe("/api/projects/project-1/vectors/search")
    expect(JSON.parse(String(init?.body))).toEqual({
      queryEmbedding: [1, 0, 0],
      topK: 5,
    })
  })

  it("rejects invalid content queries before fetch", async () => {
    const fetchMock = vi.fn<typeof fetch>()
    const api = createWebApi(fetchMock)

    await expect(api.search("project-1", "   ")).rejects.toThrow("Search query is required")
    await expect(api.vectorSearch("project-1", [])).rejects.toThrow(
      "queryEmbedding must not be empty",
    )
    await expect(api.vectorSearch("project-1", [Number.NaN])).rejects.toThrow(
      "queryEmbedding must contain only finite numbers",
    )
    expect(fetchMock).not.toHaveBeenCalled()
  })
})

describe("WebApiError", () => {
  it("notifies subscribers only when a protected API rejects the session", async () => {
    const listener = vi.fn()
    const unsubscribe = subscribeAuthRequired(listener)
    const loginFetch = vi.fn<typeof fetch>().mockResolvedValueOnce(jsonResponse({
      ok: false,
      error: "Invalid credentials",
    }, { status: 401 }))
    const protectedFetch = vi.fn<typeof fetch>().mockResolvedValue(jsonResponse({
      ok: false,
      error: "Authentication required",
    }, { status: 401 }))

    await expect(createWebApi(loginFetch).login("wrong password")).rejects.toMatchObject({
      status: 401,
    })
    expect(listener).not.toHaveBeenCalled()

    await expect(createWebApi(protectedFetch).listProjects()).rejects.toMatchObject({
      status: 401,
    })
    expect(listener).toHaveBeenCalledTimes(1)

    unsubscribe()
    await expect(createWebApi(protectedFetch).listProjects()).rejects.toMatchObject({
      status: 401,
    })
    expect(listener).toHaveBeenCalledTimes(1)
  })

  it("preserves structured server errors and their status", async () => {
    const body = { ok: false, error: "File changed since it was read" }
    const { api } = mockApi(jsonResponse(body, {
      status: 412,
      statusText: "Precondition Failed",
    }))

    const error = await api.writeText(
      "project-1",
      "wiki/notes.md",
      "stale",
      "old-version",
    ).catch((caught: unknown) => caught)

    expect(error).toBeInstanceOf(WebApiError)
    expect(error).toMatchObject({
      name: "WebApiError",
      message: body.error,
      status: 412,
      statusText: "Precondition Failed",
      requestUrl: "/api/projects/project-1/files/write",
      body,
    })
  })

  it("uses a non-JSON response body as the error message", async () => {
    const { api } = mockApi(new Response("proxy unavailable", {
      status: 502,
      statusText: "Bad Gateway",
      headers: { "Content-Type": "text/plain" },
    }))

    const error = await api.health().catch((caught: unknown) => caught)

    expect(error).toMatchObject({
      name: "WebApiError",
      message: "proxy unavailable",
      status: 502,
      body: "proxy unavailable",
    })
  })

  it("falls back to HTTP status when an error response is empty", async () => {
    const { api } = mockApi(new Response(null, {
      status: 503,
      statusText: "Service Unavailable",
    }))

    await expect(api.health()).rejects.toMatchObject({
      name: "WebApiError",
      message: "Request failed: 503 Service Unavailable",
      status: 503,
    })
  })

  it("wraps fetch failures with status zero and retains the cause", async () => {
    const fetchMock = vi.fn<typeof fetch>()
    const cause = new TypeError("Failed to fetch")
    fetchMock.mockRejectedValueOnce(cause)
    const api = createWebApi(fetchMock)

    await expect(api.listProjects()).rejects.toMatchObject({
      name: "WebApiError",
      message: "Network request failed: Failed to fetch",
      status: 0,
      requestUrl: "/api/projects",
      cause,
    })
  })
})
