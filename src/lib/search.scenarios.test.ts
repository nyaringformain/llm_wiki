/**
 * Search ranking lives in the Rust Personal Server. The browser only wraps
 * that authenticated API, so this file guards the HTTP-facing contract from
 * the TypeScript side instead of duplicating ranking logic in Node.
 */
import { beforeEach, describe, expect, it, vi } from "vitest"

const mockSearch = vi.fn()

vi.mock("@/platform/web-api", () => ({
  webApi: { search: (...args: unknown[]) => mockSearch(...args) },
}))

import { searchWiki } from "./search"

beforeEach(() => {
  mockSearch.mockReset()
})

describe("searchWiki backend command contract", () => {
  it("delegates ranking to the project-scoped search API and preserves relative paths", async () => {
    mockSearch.mockResolvedValueOnce({
      mode: "keyword",
      tokenHits: 1,
      graphHits: 0,
      vectorHits: 0,
      results: [
        {
          path: "wiki/concepts/attention.md",
          title: "Attention",
          snippet: "body",
          titleMatch: true,
          score: 1 / 61,
          images: [],
        },
      ],
    })

    const results = await searchWiki("project-id", "attention")

    expect(mockSearch).toHaveBeenCalledWith("project-id", "attention", {
      topK: 20,
      includeContent: false,
      expandGraph: true,
    })
    expect(results[0].path).toBe("wiki/concepts/attention.md")
  })
})
