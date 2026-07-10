import { useCallback, useEffect, useRef, useState } from "react"
import { AlertTriangle, RotateCcw, X } from "lucide-react"
import { FilePreview } from "@/components/editor/file-preview"
import { WikiEditor } from "@/components/editor/wiki-editor"
import { Button } from "@/components/ui/button"
import { getFileCategory, isTextReadable } from "@/lib/file-types"
import { getFileName } from "@/lib/path-utils"
import { WebApiError, webApi, type WebFileInfo } from "@/platform/web-api"
import { useWikiStore } from "@/stores/wiki-store"
import type { FileNode } from "@/types/wiki"

interface PendingSave {
  timer: ReturnType<typeof setTimeout>
  projectId: string
  path: string
  markdown: string
}

function updateFileMetadata(
  nodes: FileNode[],
  path: string,
  file: WebFileInfo,
): FileNode[] {
  return nodes.map((node) => {
    if (node.path === path) {
      return {
        ...node,
        size: file.size,
        modifiedAtMs: file.modifiedAtMs,
        version: file.version,
      }
    }
    return node.children
      ? { ...node, children: updateFileMetadata(node.children, path, file) }
      : node
  })
}

export function PreviewPanel() {
  const project = useWikiStore((state) => state.project)
  const selectedFile = useWikiStore((state) => state.selectedFile)
  const fileContent = useWikiStore((state) => state.fileContent)
  const setFileContent = useWikiStore((state) => state.setFileContent)
  const setSelectedFile = useWikiStore((state) => state.setSelectedFile)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [loadedFileKey, setLoadedFileKey] = useState<string | null>(null)
  const [reloadCounter, setReloadCounter] = useState(0)
  const pendingSavesRef = useRef(new Map<string, PendingSave>())
  const fileVersionsRef = useRef(new Map<string, string>())
  const lastLoadedRef = useRef("")
  const loadSequenceRef = useRef(0)
  const writeChainRef = useRef<Promise<void>>(Promise.resolve())

  useEffect(() => {
    const sequence = ++loadSequenceRef.current
    lastLoadedRef.current = ""
    setLoadedFileKey(null)
    setLoadError(null)
    setSaveError(null)
    setFileContent("")

    if (!project || !selectedFile) {
      return
    }

    const category = getFileCategory(selectedFile)
    if (!isTextReadable(category)) {
      return
    }

    const viewKey = `${project.id}:${selectedFile}:${reloadCounter}`
    webApi.readText(project.id, selectedFile)
      .then((response) => {
        if (sequence !== loadSequenceRef.current) return
        fileVersionsRef.current.set(`${project.id}:${selectedFile}`, response.file.version)
        lastLoadedRef.current = response.contents
        setFileContent(response.contents)
        setLoadedFileKey(viewKey)
      })
      .catch((error) => {
        if (sequence !== loadSequenceRef.current) return
        const message = error instanceof Error ? error.message : String(error)
        setFileContent("")
        setLoadError(message)
        setLoadedFileKey(viewKey)
      })
  }, [project, selectedFile, reloadCounter, setFileContent])

  const queueWrite = useCallback((projectId: string, path: string, markdown: string) => {
    const fileKey = `${projectId}:${path}`

    writeChainRef.current = writeChainRef.current
      .catch(() => undefined)
      .then(async () => {
        const expectedVersion = fileVersionsRef.current.get(fileKey)
        if (!expectedVersion) {
          throw new Error("This file has not finished loading; reload it before saving.")
        }

        const file = await webApi.writeText(
          projectId,
          path,
          markdown,
          expectedVersion,
        )
        fileVersionsRef.current.set(fileKey, file.version)
        const state = useWikiStore.getState()
        if (state.project?.id === projectId) {
          state.setFileTree(
            updateFileMetadata(state.fileTree, path, file),
          )
        }
        if (state.project?.id === projectId && state.selectedFile === path) {
          lastLoadedRef.current = markdown
          setFileContent(markdown)
          setSaveError(null)
        }
      })
      .catch((error) => {
        if (
          useWikiStore.getState().project?.id !== projectId ||
          useWikiStore.getState().selectedFile !== path
        ) return
        const conflict = error instanceof WebApiError &&
          (error.status === 412 || error.status === 428)
        setSaveError(
          conflict
            ? "This file changed on the server. Reload it before editing again."
            : error instanceof Error
              ? error.message
              : String(error),
        )
      })
  }, [setFileContent])

  const handleSave = useCallback((markdown: string, options?: { immediate?: boolean }) => {
    if (!project || !selectedFile || saveError) return
    const currentFileKey = `${project.id}:${selectedFile}:${reloadCounter}`
    if (loadedFileKey !== currentFileKey) return
    if (markdown === lastLoadedRef.current) return
    const projectId = project.id
    const path = selectedFile
    const key = `${projectId}:${path}`
    const existing = pendingSavesRef.current.get(key)
    if (existing) clearTimeout(existing.timer)

    const flush = () => {
      pendingSavesRef.current.delete(key)
      if (
        useWikiStore.getState().project?.id === projectId &&
        useWikiStore.getState().selectedFile === path
      ) {
        setFileContent(markdown)
      }
      queueWrite(projectId, path, markdown)
    }

    if (options?.immediate) {
      flush()
      return
    }

    const timer = setTimeout(flush, 1000)
    pendingSavesRef.current.set(key, { timer, projectId, path, markdown })
  }, [loadedFileKey, project, queueWrite, reloadCounter, saveError, selectedFile, setFileContent])

  useEffect(() => () => {
    for (const pending of pendingSavesRef.current.values()) {
      clearTimeout(pending.timer)
      queueWrite(pending.projectId, pending.path, pending.markdown)
    }
    pendingSavesRef.current.clear()
  }, [queueWrite])

  if (!project || !selectedFile) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        Select a file to preview
      </div>
    )
  }

  const category = getFileCategory(selectedFile)
  const fileName = getFileName(selectedFile)
  const currentFileKey = `${project.id}:${selectedFile}:${reloadCounter}`
  const loadingFile = isTextReadable(category) && loadedFileKey !== currentFileKey

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b px-3 py-1.5">
        <span className="truncate text-xs text-muted-foreground" title={selectedFile}>
          {fileName}
        </span>
        <button
          type="button"
          onClick={() => setSelectedFile(null)}
          className="shrink-0 rounded p-1 text-muted-foreground hover:bg-accent"
          aria-label="Close preview"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>

      {(loadError || saveError) && (
        <div className="flex items-center gap-2 border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
          <span className="min-w-0 flex-1">{loadError ?? saveError}</span>
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="h-7"
            onClick={() => setReloadCounter((value) => value + 1)}
          >
            <RotateCcw className="mr-1 h-3 w-3" />
            Reload
          </Button>
        </div>
      )}

      <div className="min-w-0 flex-1 overflow-auto">
        {loadingFile ? (
          <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
            Loading file…
          </div>
        ) : category === "markdown" ? (
          <WikiEditor
            key={`${project.id}:${selectedFile}:${reloadCounter}`}
            content={fileContent}
            onSave={handleSave}
            filePath={selectedFile}
          />
        ) : (
          <FilePreview
            key={`${project.id}:${selectedFile}:${reloadCounter}`}
            projectId={project.id}
            filePath={selectedFile}
            textContent={fileContent}
          />
        )}
      </div>
    </div>
  )
}
