import { describe, expect, it, vi } from "vitest"

vi.mock("@/platform/web-api", () => ({
  webApi: {
    createProject: vi.fn(),
  },
}))

import { getCreateProjectFormStatus } from "./create-project-dialog"

describe("getCreateProjectFormStatus", () => {
  it("keeps the initial disabled state quiet before interaction", () => {
    expect(getCreateProjectFormStatus("", "", false)).toEqual({
      missingRequired: true,
      canCreate: false,
      footerError: "",
      footerMessageKey: null,
    })
  })

  it("shows the required hint after interaction while the name is missing", () => {
    expect(getCreateProjectFormStatus("", "", true)).toEqual({
      missingRequired: true,
      canCreate: false,
      footerError: "",
      footerMessageKey: "project.requiredHint",
    })
  })

  it("treats whitespace-only names as missing", () => {
    expect(getCreateProjectFormStatus("   ", "", true)).toEqual({
      missingRequired: true,
      canCreate: false,
      footerError: "",
      footerMessageKey: "project.requiredHint",
    })
  })

  it("enables creation when a name is present", () => {
    expect(getCreateProjectFormStatus("Research", "", true)).toEqual({
      missingRequired: false,
      canCreate: true,
      footerError: "",
      footerMessageKey: null,
    })
  })

  it("prefers server errors over the required-fields hint", () => {
    expect(getCreateProjectFormStatus("", "Permission denied", true)).toEqual({
      missingRequired: true,
      canCreate: false,
      footerError: "Permission denied",
      footerMessageKey: null,
    })
  })
})
