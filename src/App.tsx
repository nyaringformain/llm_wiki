import { useCallback, useEffect, useState } from "react"
import {
  OwnerAuthScreen,
  type OwnerAuthMode,
} from "@/components/auth/owner-auth-screen"
import { AppLayout } from "@/components/layout/app-layout"
import { CreateProjectDialog } from "@/components/project/create-project-dialog"
import { WelcomeScreen } from "@/components/project/welcome-screen"
import {
  subscribeAuthRequired,
  webApi,
  WebApiError,
  type WebProject,
  type WebSession,
} from "@/platform/web-api"
import { useWikiStore } from "@/stores/wiki-store"
import type { WikiProject } from "@/types/wiki"

function asWikiProject(project: WebProject): WikiProject {
  return {
    id: project.id,
    name: project.name,
    relativePath: project.relativePath,
    // Transitional compatibility for desktop-era code that still reads
    // `project.path`. This value is intentionally Data-Root-relative, never an
    // absolute path from the Personal Server.
    path: project.relativePath,
  }
}

function App() {
  const project = useWikiStore((state) => state.project)
  const setProject = useWikiStore((state) => state.setProject)
  const setFileTree = useWikiStore((state) => state.setFileTree)
  const setSelectedFile = useWikiStore((state) => state.setSelectedFile)
  const setFileContent = useWikiStore((state) => state.setFileContent)
  const setActiveView = useWikiStore((state) => state.setActiveView)
  const [projects, setProjects] = useState<WikiProject[]>([])
  const [loading, setLoading] = useState(true)
  const [startupError, setStartupError] = useState<string | null>(null)
  const [serverReady, setServerReady] = useState(false)
  const [authMode, setAuthMode] = useState<OwnerAuthMode | "authenticated" | null>(null)
  const [sessionExpiresAt, setSessionExpiresAt] = useState<string | null>(null)
  const [loggingOut, setLoggingOut] = useState(false)
  const [showCreateDialog, setShowCreateDialog] = useState(false)

  const clearWorkspace = useCallback(() => {
    setProject(null)
    setFileTree([])
    setSelectedFile(null)
    setFileContent("")
    setActiveView("wiki")
    setProjects([])
    setShowCreateDialog(false)
  }, [
    setActiveView,
    setFileContent,
    setFileTree,
    setProject,
    setSelectedFile,
  ])

  const requireLogin = useCallback(() => {
    clearWorkspace()
    setSessionExpiresAt(null)
    setAuthMode("login")
  }, [clearWorkspace])

  useEffect(() => {
    let cancelled = false

    async function initialize() {
      try {
        const health = await webApi.health()
        if (cancelled) return

        const ready = health.dataRootConfigured && health.databaseReady
        setServerReady(ready)
        if (!ready) return

        if (!health.ownerConfigured) {
          setAuthMode("setup")
          return
        }

        const session = await webApi.session()
        if (!session.authenticated) {
          setAuthMode("login")
          return
        }

        const registered = await webApi.listProjects()
        if (!cancelled) {
          setProjects(registered.map(asWikiProject))
          setSessionExpiresAt(session.expiresAt)
          setAuthMode("authenticated")
        }
      } catch (error) {
        if (!cancelled) {
          if (error instanceof WebApiError && error.status === 401) {
            requireLogin()
          } else {
            setStartupError(error instanceof Error ? error.message : String(error))
          }
        }
      } finally {
        if (!cancelled) setLoading(false)
      }
    }

    void initialize()
    return () => {
      cancelled = true
    }
  }, [requireLogin])

  useEffect(() => subscribeAuthRequired(requireLogin), [requireLogin])

  useEffect(() => {
    if (!sessionExpiresAt) return

    const expiresAt = Date.parse(sessionExpiresAt)
    if (!Number.isFinite(expiresAt)) return
    const remainingMs = expiresAt - Date.now()
    if (remainingMs <= 0) {
      requireLogin()
      return
    }

    const timer = window.setTimeout(
      requireLogin,
      Math.min(remainingMs, 2_147_483_647),
    )
    return () => window.clearTimeout(timer)
  }, [requireLogin, sessionExpiresAt])

  async function handleAuthenticated(session: WebSession) {
    if (!session.authenticated) {
      throw new Error("The server did not create an owner session.")
    }

    const registered = await webApi.listProjects()
    setProjects(registered.map(asWikiProject))
    setSessionExpiresAt(session.expiresAt)
    setAuthMode("authenticated")
    setStartupError(null)
  }

  async function handleLogout() {
    setLoggingOut(true)
    try {
      await webApi.logout()
      requireLogin()
    } catch (error) {
      setStartupError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoggingOut(false)
    }
  }

  function openRegisteredProject(nextProject: WikiProject) {
    setProject(nextProject)
    setFileTree([])
    setSelectedFile(null)
    setFileContent("")
    setActiveView("wiki")
  }

  function handleProjectCreated(created: WebProject) {
    const nextProject = asWikiProject(created)
    setProjects((current) => [
      nextProject,
      ...current.filter((candidate) => candidate.id !== nextProject.id),
    ])
    openRegisteredProject(nextProject)
  }

  function handleSwitchProject() {
    setProject(null)
    setFileTree([])
    setSelectedFile(null)
    setFileContent("")
    setActiveView("wiki")
  }

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center bg-background text-muted-foreground">
        Loading Personal Server…
      </div>
    )
  }

  if (startupError) {
    return (
      <div className="flex h-full items-center justify-center bg-background p-6">
        <div className="max-w-lg rounded-lg border border-destructive/40 bg-destructive/10 p-5">
          <h1 className="font-semibold text-destructive">Personal Server unavailable</h1>
          <p className="mt-2 text-sm text-muted-foreground">{startupError}</p>
        </div>
      </div>
    )
  }

  if (!serverReady) {
    return (
      <div className="flex h-full items-center justify-center bg-background p-6">
        <div className="max-w-lg rounded-lg border p-5">
          <h1 className="font-semibold">Server setup required</h1>
          <p className="mt-2 text-sm text-muted-foreground">
            Configure <code>LLM_WIKI_DATA_ROOT</code> on the server and restart it.
            Browser-based Data Root setup is not available in the current backend.
          </p>
        </div>
      </div>
    )
  }

  if (authMode === "setup" || authMode === "login") {
    return (
      <OwnerAuthScreen
        key={authMode}
        mode={authMode}
        onAuthenticated={handleAuthenticated}
      />
    )
  }

  if (authMode !== "authenticated") {
    return (
      <div className="flex h-full items-center justify-center bg-background text-muted-foreground">
        Checking owner session…
      </div>
    )
  }

  if (!project) {
    return (
      <div className="h-full">
        <WelcomeScreen
          projects={projects}
          onCreateProject={() => setShowCreateDialog(true)}
          onLogout={handleLogout}
          onSelectProject={openRegisteredProject}
          loggingOut={loggingOut}
        />
        <CreateProjectDialog
          open={showCreateDialog}
          onOpenChange={setShowCreateDialog}
          onCreated={handleProjectCreated}
        />
      </div>
    )
  }

  return (
    <AppLayout
      onLogout={handleLogout}
      onSwitchProject={handleSwitchProject}
      loggingOut={loggingOut}
    />
  )
}

export default App
