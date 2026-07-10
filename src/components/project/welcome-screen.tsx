import { Clock, LogOut, Plus } from "lucide-react"
import { useTranslation } from "react-i18next"
import { Button } from "@/components/ui/button"
import type { WikiProject } from "@/types/wiki"

interface WelcomeScreenProps {
  projects: WikiProject[]
  loggingOut: boolean
  onCreateProject: () => void
  onLogout: () => void | Promise<void>
  onSelectProject: (project: WikiProject) => void
}

export function WelcomeScreen({
  projects,
  loggingOut,
  onCreateProject,
  onLogout,
  onSelectProject,
}: WelcomeScreenProps) {
  const { t } = useTranslation()

  return (
    <div className="relative flex h-full items-center justify-center bg-background">
      <Button
        type="button"
        variant="ghost"
        className="absolute right-4 top-4"
        onClick={onLogout}
        disabled={loggingOut}
      >
        <LogOut className="h-4 w-4" />
        {loggingOut ? "Logging out…" : "Log out"}
      </Button>
      <div className="flex w-full max-w-lg flex-col items-center gap-8 px-4">
        <div className="text-center">
          <h1 className="text-3xl font-bold">{t("app.title")}</h1>
          <p className="mt-2 text-muted-foreground">{t("app.subtitle")}</p>
        </div>

        <Button onClick={onCreateProject}>
          <Plus className="mr-2 h-4 w-4" />
          {t("welcome.newProject")}
        </Button>

        <div className="w-full">
          <div className="mb-2 flex items-center gap-2 text-sm text-muted-foreground">
            <Clock className="h-3.5 w-3.5" />
            Registered projects
          </div>
          {projects.length === 0 ? (
            <div className="rounded-lg border border-dashed px-4 py-8 text-center text-sm text-muted-foreground">
              No projects are registered under the server Data Root yet.
            </div>
          ) : (
            <div className="rounded-lg border">
              {projects.map((project) => (
                <button
                  key={project.id}
                  type="button"
                  onClick={() => onSelectProject(project)}
                  className="flex w-full items-center border-b px-4 py-3 text-left transition-colors last:border-b-0 hover:bg-accent"
                >
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{project.name}</div>
                    <div className="truncate text-xs text-muted-foreground">
                      {project.relativePath ?? project.path}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
