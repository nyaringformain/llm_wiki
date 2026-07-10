import { useCallback, useRef, useState } from "react"
import { ErrorBoundary } from "@/components/error-boundary"
import { ContentArea } from "./content-area"
import { IconSidebar } from "./icon-sidebar"
import { SidebarPanel } from "./sidebar-panel"

interface AppLayoutProps {
  loggingOut: boolean
  onLogout: () => void | Promise<void>
  onSwitchProject: () => void
}

export function AppLayout({
  loggingOut,
  onLogout,
  onSwitchProject,
}: AppLayoutProps) {
  const [leftWidth, setLeftWidth] = useState(240)
  const dragging = useRef(false)
  const containerRef = useRef<HTMLDivElement>(null)

  const startDrag = useCallback((event: React.MouseEvent) => {
    event.preventDefault()
    dragging.current = true
    document.body.style.cursor = "col-resize"
    document.body.style.userSelect = "none"

    const handleMouseMove = (moveEvent: MouseEvent) => {
      if (!dragging.current || !containerRef.current) return
      const bounds = containerRef.current.getBoundingClientRect()
      setLeftWidth(Math.max(180, Math.min(400, moveEvent.clientX - bounds.left)))
    }

    const handleMouseUp = () => {
      dragging.current = false
      document.body.style.cursor = ""
      document.body.style.userSelect = ""
      document.removeEventListener("mousemove", handleMouseMove)
      document.removeEventListener("mouseup", handleMouseUp)
    }

    document.addEventListener("mousemove", handleMouseMove)
    document.addEventListener("mouseup", handleMouseUp)
  }, [])

  return (
    <div className="flex h-full bg-background text-foreground">
      <IconSidebar
        loggingOut={loggingOut}
        onLogout={onLogout}
        onSwitchProject={onSwitchProject}
      />
      <div ref={containerRef} className="relative flex min-w-0 flex-1 overflow-hidden">
        <div
          className="shrink-0 overflow-hidden border-r"
          style={{ width: leftWidth }}
        >
          <SidebarPanel />
        </div>
        <div
          className="w-1.5 shrink-0 cursor-col-resize bg-border/40 transition-colors hover:bg-primary/30 active:bg-primary/40"
          onMouseDown={startDrag}
        />
        <div className="min-w-0 flex-1 overflow-hidden">
          <ErrorBoundary>
            <ContentArea />
          </ErrorBoundary>
        </div>
      </div>
    </div>
  )
}
