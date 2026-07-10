import { normalizePath } from "@/lib/path-utils"
import { webApi } from "@/platform/web-api"
import { useWikiStore } from "@/stores/wiki-store"

const PASSTHROUGH_RE = /^(https?:|data:|blob:)/i
const LOCAL_SCHEME_RE = /^(file:|tauri:)/i

function decodePath(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

function isAbsolutePath(value: string): boolean {
  return value.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(value) || value.startsWith("//")
}

function projectRelativeDirectory(projectPath: string, currentFileDir: string): string | null {
  const projectRoot = normalizePath(projectPath).replace(/^\.?\//, "").replace(/\/+$/, "")
  const directory = normalizePath(currentFileDir).replace(/^\.\//, "").replace(/\/+$/, "")
  if (isAbsolutePath(directory)) return null
  if (directory === projectRoot) return ""
  if (projectRoot && directory.startsWith(`${projectRoot}/`)) {
    return directory.slice(projectRoot.length + 1)
  }
  return directory
}

function resolveRelativePath(baseDirectory: string, source: string): string | null {
  const parts = baseDirectory ? baseDirectory.split("/").filter(Boolean) : []
  for (const segment of source.split("/")) {
    if (!segment || segment === ".") continue
    if (segment === "..") {
      if (parts.length === 0) return null
      parts.pop()
      continue
    }
    parts.push(segment)
  }
  return parts.join("/")
}

/**
 * Resolve a markdown image to an authenticated Personal Server preview route.
 *
 * `projectPath` remains in the signature while desktop-era call sites are
 * migrated, but it is used only to strip the server's Data-Root-relative
 * registry prefix. The resulting request always contains the active project
 * id and a path relative to that project; absolute server paths are never
 * converted into browser-visible URLs.
 */
export function resolveMarkdownImageSrc(
  rawSrc: string,
  projectPath: string | null,
  currentFileDir?: string | null,
): string {
  if (!rawSrc || PASSTHROUGH_RE.test(rawSrc)) return rawSrc
  if (LOCAL_SCHEME_RE.test(rawSrc)) return rawSrc
  if (!projectPath) return rawSrc

  const decoded = normalizePath(decodePath(rawSrc.replace(/^\.\//, "")))
  if (isAbsolutePath(decoded)) return rawSrc

  const project = useWikiStore.getState().project
  if (!project) return rawSrc

  const generatedMedia = decoded.startsWith("media/") || decoded.startsWith("../media/")
  const source = decoded.startsWith("../media/") ? decoded.slice(3) : decoded
  const baseDirectory = generatedMedia
    ? "wiki"
    : currentFileDir
      ? projectRelativeDirectory(projectPath, currentFileDir)
      : "wiki"
  if (baseDirectory === null) return rawSrc

  const relativePath = resolveRelativePath(baseDirectory, source)
  if (!relativePath) return rawSrc

  try {
    return webApi.previewUrl(project.id, relativePath)
  } catch {
    return rawSrc
  }
}
