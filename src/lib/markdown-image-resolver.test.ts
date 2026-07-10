import { beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  previewUrl: vi.fn((projectId: string, path: string) =>
    `/api/projects/${projectId}/files/preview?path=${encodeURIComponent(path)}`,
  ),
  project: { id: "project-1", name: "Project", path: "project" } as
    | { id: string; name: string; path: string }
    | null,
}))

vi.mock("@/platform/web-api", () => ({
  webApi: { previewUrl: mocks.previewUrl },
}))

vi.mock("@/stores/wiki-store", () => ({
  useWikiStore: {
    getState: () => ({ project: mocks.project }),
  },
}))

import { resolveMarkdownImageSrc } from "./markdown-image-resolver"

describe("resolveMarkdownImageSrc", () => {
  beforeEach(() => {
    mocks.project = { id: "project-1", name: "Project", path: "project" }
    mocks.previewUrl.mockClear()
  })

  it("passes browser-safe remote and inline URLs through", () => {
    expect(resolveMarkdownImageSrc("https://example.com/image.png", "project")).toBe(
      "https://example.com/image.png",
    )
    expect(resolveMarkdownImageSrc("data:image/png;base64,abc", "project")).toBe(
      "data:image/png;base64,abc",
    )
    expect(resolveMarkdownImageSrc("blob:123", "project")).toBe("blob:123")
  })

  it("does not convert absolute or local-scheme paths", () => {
    expect(resolveMarkdownImageSrc("/etc/passwd", "project")).toBe("/etc/passwd")
    expect(resolveMarkdownImageSrc("C:/Users/me/image.png", "project")).toBe(
      "C:/Users/me/image.png",
    )
    expect(resolveMarkdownImageSrc("file:///tmp/image.png", "project")).toBe(
      "file:///tmp/image.png",
    )
    expect(mocks.previewUrl).not.toHaveBeenCalled()
  })

  it("resolves generated media references from the wiki root", () => {
    expect(resolveMarkdownImageSrc("media/topic/image.png", "project")).toBe(
      "/api/projects/project-1/files/preview?path=wiki%2Fmedia%2Ftopic%2Fimage.png",
    )
    expect(resolveMarkdownImageSrc("../media/topic/image.png", "project", "wiki/sources")).toBe(
      "/api/projects/project-1/files/preview?path=wiki%2Fmedia%2Ftopic%2Fimage.png",
    )
  })

  it("resolves ordinary relative images against the markdown file directory", () => {
    expect(resolveMarkdownImageSrc("../assets/image.png", "project", "raw/sources")).toBe(
      "/api/projects/project-1/files/preview?path=raw%2Fassets%2Fimage.png",
    )
  })

  it("strips the transitional Data-Root-relative project prefix", () => {
    expect(resolveMarkdownImageSrc("diagram.png", "project", "project/wiki/concepts")).toBe(
      "/api/projects/project-1/files/preview?path=wiki%2Fconcepts%2Fdiagram.png",
    )
  })

  it("decodes percent-encoded project-relative paths once", () => {
    resolveMarkdownImageSrc("media/%E6%B5%8B%E8%AF%95.png", "project")
    expect(mocks.previewUrl).toHaveBeenCalledWith(
      "project-1",
      "wiki/media/测试.png",
    )
  })

  it("rejects traversal above the project root", () => {
    expect(resolveMarkdownImageSrc("../../../secret.png", "project", "wiki")).toBe(
      "../../../secret.png",
    )
  })

  it("leaves paths unchanged when no project is active", () => {
    mocks.project = null
    expect(resolveMarkdownImageSrc("media/image.png", "project")).toBe(
      "media/image.png",
    )
  })
})
