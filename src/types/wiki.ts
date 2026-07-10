export interface WikiProject {
  /** Stable UUID, persisted inside the project at .llm-wiki/project.json.
   *  Survives the user moving or renaming the project folder. */
  id: string
  name: string
  /**
   * Data-Root-relative registry path returned by the Personal Server.
   * New web code should use this only for display; filesystem API calls use
   * the project id plus a path relative to the project root.
   */
  relativePath?: string
  /**
   * Legacy desktop locator. During the web transition this is populated with
   * `relativePath` so untouched UI code never receives an absolute server
   * filesystem path. New web code must not use it for API addressing.
   */
  path: string
}

export interface FileNode {
  name: string
  path: string
  is_dir: boolean
  size?: number
  modifiedAtMs?: number
  version?: string
  children?: FileNode[]
}

export interface WikiPage {
  path: string
  content: string
  frontmatter: Record<string, unknown>
}
