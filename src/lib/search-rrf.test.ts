/**
 * The RRF scoring implementation lives in the Personal Server. These tests
 * guard the browser wrapper: it addresses a registered project, requests graph
 * expansion, and keeps result paths relative to that project.
 */
import { beforeEach, describe, expect, it, vi } from "vitest"

const mockSearch = vi.fn()

vi.mock("@/platform/web-api", () => ({
  webApi: { search: (...args: unknown[]) => mockSearch(...args) },
}))

import { searchWiki, tokenizeQuery } from "./search"

beforeEach(() => {
  mockSearch.mockReset()
})

describe("searchWiki backend wrapper", () => {
  it("uses server-side graph expansion and leaves paths project-relative", async () => {
    mockSearch.mockResolvedValueOnce({
      mode: "hybrid",
      tokenHits: 1,
      graphHits: 0,
      vectorHits: 1,
      results: [
        {
          path: "wiki/concepts/attention.md",
          title: "Attention",
          snippet: "Attention",
          titleMatch: true,
          score: 1 / 61,
          images: [],
        },
      ],
    })

    const out = await searchWiki("project-id", "attention")

    expect(mockSearch).toHaveBeenCalledWith("project-id", "attention", {
      topK: 20,
      includeContent: false,
      expandGraph: true,
    })
    expect(out[0].path).toBe("wiki/concepts/attention.md")
  })

  it("requests keyword search without browser-side embedding credentials", async () => {
    mockSearch.mockResolvedValueOnce({
      mode: "keyword",
      tokenHits: 1,
      graphHits: 0,
      vectorHits: 0,
      results: [],
    })

    await searchWiki("project-id", "attention")

    expect(mockSearch).toHaveBeenCalledWith(
      "project-id",
      "attention",
      expect.not.objectContaining({ queryEmbedding: expect.anything() }),
    )
  })

  it("keeps CJK tokenization behavior for image caption filtering", () => {
    const tokens = tokenizeQuery("默会知识")
    expect(tokens).toContain("默会")
    expect(tokens).toContain("知识")
    expect(tokens).toContain("默")
  })
})
