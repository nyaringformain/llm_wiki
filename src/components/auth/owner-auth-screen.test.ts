import { describe, expect, it, vi } from "vitest"

vi.mock("@/platform/web-api", () => ({
  webApi: {
    setupOwner: vi.fn(),
    login: vi.fn(),
  },
}))

import { getOwnerAuthValidationError } from "./owner-auth-screen"

describe("getOwnerAuthValidationError", () => {
  it("requires the server's eight-character minimum", () => {
    expect(getOwnerAuthValidationError("setup", "1234567", "1234567"))
      .toMatch(/at least 8/i)
    expect(getOwnerAuthValidationError("login", "1234567", ""))
      .toMatch(/at least 8/i)
  })

  it("requires matching confirmation only during owner setup", () => {
    expect(getOwnerAuthValidationError(
      "setup",
      "correct horse",
      "different horse",
    )).toMatch(/do not match/i)
    expect(getOwnerAuthValidationError("login", "correct horse", ""))
      .toBeNull()
  })

  it("accepts a matching setup password without trimming it", () => {
    expect(getOwnerAuthValidationError(
      "setup",
      " passphrase ",
      " passphrase ",
    )).toBeNull()
  })
})
