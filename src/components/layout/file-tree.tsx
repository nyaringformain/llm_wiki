import { useEffect, useRef, useState } from "react"
import { ChevronRight, ChevronDown, File, Folder, Trash2 } from "lucide-react"
import { ScrollArea } from "@/components/ui/scroll-area"
import { useWikiStore } from "@/stores/wiki-store"
import type { FileNode } from "@/types/wiki"
import { useTranslation } from "react-i18next"
import { webApi, type WebFileNode } from "@/platform/web-api"
import { replaceNodeChildren } from "./file-tree-utils"

type DisplayFileNode = Pick<WebFileNode, "name" | "path" | "isDir" | "version"> & {
  children?: DisplayFileNode[]
}

function treeNodes(result: WebFileNode[] | { nodes: WebFileNode[] }): WebFileNode[] {
  return Array.isArray(result) ? result : result.nodes
}

function toStoreFileNode(node: WebFileNode): FileNode {
  return {
    name: node.name,
    path: node.path,
    is_dir: node.isDir,
    size: node.size,
    modifiedAtMs: node.modifiedAtMs,
    version: node.version,
    children: node.children?.map(toStoreFileNode),
  }
}

function toDisplayFileNode(node: FileNode): DisplayFileNode {
  return {
    name: node.name,
    path: node.path,
    isDir: node.is_dir,
    version: node.version ?? "",
    children: node.children?.map(toDisplayFileNode),
  }
}

function removeStoreNode(nodes: FileNode[], path: string): FileNode[] {
  return nodes
    .filter((node) => node.path !== path)
    .map((node) => node.children
      ? { ...node, children: removeStoreNode(node.children, path) }
      : node)
}

function TreeNode({
  node,
  depth,
  onLoadChildren,
  onDelete,
  deletingPath,
}: {
  node: DisplayFileNode
  depth: number
  onLoadChildren: (node: DisplayFileNode) => Promise<void>
  onDelete: (node: DisplayFileNode) => Promise<void>
  deletingPath: string | null
}) {
  const { t } = useTranslation()
  const [expanded, setExpanded] = useState(depth < 1)
  const [loadingChildren, setLoadingChildren] = useState(false)
  const selectedFile = useWikiStore((s) => s.selectedFile)
  const openPathInPreview = useWikiStore((s) => s.openPathInPreview)

  const isSelected = selectedFile === node.path
  const paddingLeft = 12 + depth * 16

  if (node.isDir) {
    const handleToggle = async () => {
      const nextExpanded = !expanded
      setExpanded(nextExpanded)
      if (!nextExpanded || node.children) return
      setLoadingChildren(true)
      try {
        await onLoadChildren(node)
      } finally {
        setLoadingChildren(false)
      }
    }

    return (
      <div>
        <button
          onClick={() => void handleToggle()}
          className="flex w-full items-center gap-1 py-1 text-sm text-muted-foreground hover:bg-accent/50 hover:text-accent-foreground"
          style={{ paddingLeft }}
        >
          {expanded ? (
            <ChevronDown className="h-3.5 w-3.5 shrink-0" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5 shrink-0" />
          )}
          <Folder className="h-3.5 w-3.5 shrink-0 text-blue-400" />
          <span className="truncate">{node.name}</span>
          {loadingChildren && (
            <span className="ml-auto pr-2 text-[10px] text-muted-foreground">
              {t("common.loading", { defaultValue: "Loading..." })}
            </span>
          )}
        </button>
        {expanded && node.children?.map((child) => (
          <TreeNode
            key={child.path}
            node={child}
            depth={depth + 1}
            onLoadChildren={onLoadChildren}
            onDelete={onDelete}
            deletingPath={deletingPath}
          />
        ))}
      </div>
    )
  }

  return (
    <div className={`group flex items-center ${isSelected ? "bg-accent" : "hover:bg-accent/50"}`}>
      <button
        onClick={() => openPathInPreview(node.path)}
        className={`flex min-w-0 flex-1 items-center gap-1 py-1 text-sm ${
          isSelected
            ? "text-accent-foreground"
            : "text-muted-foreground group-hover:text-accent-foreground"
        }`}
        style={{ paddingLeft: paddingLeft + 14 }}
      >
        <File className="h-3.5 w-3.5 shrink-0" />
        <span className="truncate">{node.name}</span>
      </button>
      <button
        type="button"
        onClick={() => void onDelete(node)}
        disabled={deletingPath === node.path}
        className="mr-1 rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100 disabled:opacity-50"
        title="Move file to Project Trash"
        aria-label={`Move ${node.name} to Project Trash`}
      >
        <Trash2 className="h-3.5 w-3.5" />
      </button>
    </div>
  )
}

export function FileTree() {
  const { t } = useTranslation()
  const fileTree = useWikiStore((s) => s.fileTree)
  const setFileTree = useWikiStore((s) => s.setFileTree)
  const project = useWikiStore((s) => s.project)
  const loadedPaths = useRef(new Set<string>())
  const loadingPaths = useRef(new Set<string>())
  const [loadingRoot, setLoadingRoot] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [deletingPath, setDeletingPath] = useState<string | null>(null)

  useEffect(() => {
    loadedPaths.current.clear()
    loadingPaths.current.clear()
    setLoadError(null)

    const projectId = project?.id
    if (!projectId) {
      setLoadingRoot(false)
      setFileTree([])
      return
    }

    let cancelled = false
    setLoadingRoot(true)
    setFileTree([])
    void webApi.tree(projectId, "", { maxDepth: 1 })
      .then((result) => {
        if (cancelled || useWikiStore.getState().project?.id !== projectId) return
        const nodes = treeNodes(result)
        setFileTree(nodes.map(toStoreFileNode))
      })
      .catch((err) => {
        if (cancelled || useWikiStore.getState().project?.id !== projectId) return
        setLoadError(err instanceof Error ? err.message : String(err))
      })
      .finally(() => {
        if (!cancelled && useWikiStore.getState().project?.id === projectId) {
          setLoadingRoot(false)
        }
      })

    return () => {
      cancelled = true
    }
  }, [project?.id, setFileTree])

  const handleLoadChildren = async (node: DisplayFileNode) => {
    if (!project) return
    if (loadedPaths.current.has(node.path) || loadingPaths.current.has(node.path)) return
    loadingPaths.current.add(node.path)
    const projectId = project.id
    try {
      const children = treeNodes(await webApi.tree(projectId, node.path, { maxDepth: 1 }))
      if (useWikiStore.getState().project?.id !== projectId) return
      const currentTree = useWikiStore.getState().fileTree
      const result = replaceNodeChildren(currentTree, node.path, children.map(toStoreFileNode))
      if (!result.matched) return
      loadedPaths.current.add(node.path)
      setLoadError(null)
      setFileTree(result.nodes)
    } catch (err) {
      console.error("[FileTree] load children failed:", err)
      setLoadError(err instanceof Error ? err.message : String(err))
    } finally {
      loadingPaths.current.delete(node.path)
    }
  }

  const handleDelete = async (node: DisplayFileNode) => {
    if (!project || deletingPath) return
    if (!window.confirm(`Move ${node.name} to this Project's Trash?`)) return

    setDeletingPath(node.path)
    try {
      await webApi.deletePath(project.id, node.path, node.version || undefined)
      if (useWikiStore.getState().project?.id !== project.id) return
      const currentTree = useWikiStore.getState().fileTree
      setFileTree(removeStoreNode(currentTree, node.path))
      const selected = useWikiStore.getState().selectedFile
      if (selected === node.path) useWikiStore.getState().setSelectedFile(null)
      setLoadError(null)
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error))
    } finally {
      setDeletingPath(null)
    }
  }

  if (!project) {
    return (
      <div className="flex h-full items-center justify-center p-4 text-sm text-muted-foreground">
        {t("fileTree.noProject")}
      </div>
    )
  }

  return (
    <div className="flex h-full min-w-0 flex-col overflow-hidden">
      <ScrollArea className="min-h-0 flex-1 overflow-hidden">
        <div className="p-2">
          <div className="mb-2 px-2 text-xs font-semibold uppercase text-muted-foreground">
            {project.name}
          </div>
          {loadingRoot && (
            <div className="px-2 py-1 text-xs text-muted-foreground">
              {t("common.loading", { defaultValue: "Loading..." })}
            </div>
          )}
          {loadError && (
            <div className="px-2 py-1 text-xs text-destructive">
              {loadError}
            </div>
          )}
          {fileTree.map(toDisplayFileNode).map((node) => (
            <TreeNode
              key={node.path}
              node={node}
              depth={0}
              onLoadChildren={handleLoadChildren}
              onDelete={handleDelete}
              deletingPath={deletingPath}
            />
          ))}
        </div>
      </ScrollArea>
    </div>
  )
}
