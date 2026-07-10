import { ArrowLeftRight, FileText, LogOut, Network, Search } from "lucide-react"
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip"
import { useWikiStore } from "@/stores/wiki-store"
import { useTranslation } from "react-i18next"
import logoImg from "@/assets/logo.jpg"

const MILESTONE_VIEWS = [
  { view: "wiki" as const, icon: FileText, labelKey: "nav.wiki" },
  { view: "search" as const, icon: Search, labelKey: "nav.search" },
  { view: "graph" as const, icon: Network, labelKey: "nav.graph" },
]

interface IconSidebarProps {
  loggingOut: boolean
  onLogout: () => void | Promise<void>
  onSwitchProject: () => void
}

export function IconSidebar({
  loggingOut,
  onLogout,
  onSwitchProject,
}: IconSidebarProps) {
  const { t } = useTranslation()
  const activeView = useWikiStore((s) => s.activeView)
  const setActiveView = useWikiStore((s) => s.setActiveView)

  return (
    <TooltipProvider delay={300}>
      <div className="flex h-full w-12 flex-col items-center border-r bg-muted/50 py-2">
        {/* Logo */}
        <div className="mb-2 flex items-center justify-center">
          <img
            src={logoImg}
            alt="LLM Wiki"
            className="h-8 w-8 rounded-[22%]"
          />
        </div>
        {/* Web-only views backed by authenticated Personal Server APIs. */}
        <div className="flex flex-1 flex-col items-center gap-1">
          {MILESTONE_VIEWS.map(({ view, icon: Icon, labelKey }) => (
            <Tooltip key={view}>
              <TooltipTrigger
                onClick={() => setActiveView(view)}
                className={`flex h-10 w-10 items-center justify-center rounded-md transition-colors ${
                  activeView === view
                    ? "bg-accent text-accent-foreground"
                    : "text-muted-foreground hover:bg-accent/50 hover:text-accent-foreground"
                }`}
              >
                <Icon className="h-5 w-5" />
              </TooltipTrigger>
              <TooltipContent side="right">{t(labelKey)}</TooltipContent>
            </Tooltip>
          ))}
        </div>
        {/* Bottom: project selection is the only global action. */}
        <div className="flex flex-col items-center gap-1 pb-1">
          <Tooltip>
            <TooltipTrigger
              onClick={onSwitchProject}
              className="flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent/50 hover:text-accent-foreground"
            >
              <ArrowLeftRight className="h-5 w-5" />
            </TooltipTrigger>
            <TooltipContent side="right">{t("nav.switchProject")}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger
              onClick={onLogout}
              disabled={loggingOut}
              aria-label="Log out"
              className="flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent/50 hover:text-accent-foreground disabled:pointer-events-none disabled:opacity-50"
            >
              <LogOut className="h-5 w-5" />
            </TooltipTrigger>
            <TooltipContent side="right">
              {loggingOut ? "Logging out…" : "Log out"}
            </TooltipContent>
          </Tooltip>
        </div>
      </div>
    </TooltipProvider>
  )
}
