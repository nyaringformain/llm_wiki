export type AppTheme = "light" | "dark" | "system"

let activeTheme: AppTheme = "system"
let mediaQuery: MediaQueryList | null = null
let mediaListenerInstalled = false

function systemPrefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches
}

export function applyTheme(theme: AppTheme): void {
  activeTheme = theme
  const root = document.documentElement
  const resolved = theme === "system"
    ? systemPrefersDark()
      ? "dark"
      : "light"
    : theme

  root.classList.remove("light", "dark")
  root.classList.add(resolved)
  root.dataset.theme = theme
}

export function watchSystemTheme(): void {
  if (mediaListenerInstalled) return
  mediaQuery = window.matchMedia("(prefers-color-scheme: dark)")
  mediaQuery.addEventListener("change", () => {
    if (activeTheme === "system") applyTheme("system")
  })
  mediaListenerInstalled = true
}

export async function loadAndApplyTheme(): Promise<AppTheme> {
  // Server-backed UI preferences are intentionally deferred until an
  // authenticated, allowlisted settings endpoint exists. Do not persist this
  // in browser storage: the legacy Tauri store also contains provider secrets.
  applyTheme("system")
  return "system"
}
