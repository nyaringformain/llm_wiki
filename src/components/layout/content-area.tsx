import { GraphView } from "@/components/graph/graph-view"
import { SearchView } from "@/components/search/search-view"
import { useWikiStore } from "@/stores/wiki-store"
import { PreviewPanel } from "./preview-panel"

export function ContentArea() {
  const activeView = useWikiStore((state) => state.activeView)

  switch (activeView) {
    case "search":
      return <SearchView />
    case "graph":
      return <GraphView />
    default:
      return <PreviewPanel />
  }
}
