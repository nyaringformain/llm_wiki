import { useState } from "react"
import { useTranslation } from "react-i18next"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { webApi, type WebProject } from "@/platform/web-api"

interface CreateProjectDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreated: (project: WebProject) => void
}

export interface CreateProjectFormStatus {
  missingRequired: boolean
  canCreate: boolean
  footerMessageKey: string | null
  footerError: string
}

export function getCreateProjectFormStatus(
  name: string,
  error: string,
  hasInteracted: boolean,
): CreateProjectFormStatus {
  const missingRequired = !name.trim()
  return {
    missingRequired,
    canCreate: !missingRequired,
    footerError: error,
    footerMessageKey:
      !error && hasInteracted && missingRequired ? "project.requiredHint" : null,
  }
}

export function CreateProjectDialog({
  open,
  onOpenChange,
  onCreated,
}: CreateProjectDialogProps) {
  const { t } = useTranslation()
  const [name, setName] = useState("")
  const [error, setError] = useState("")
  const [hasInteracted, setHasInteracted] = useState(false)
  const [creating, setCreating] = useState(false)
  const formStatus = getCreateProjectFormStatus(name, error, hasInteracted)

  function resetForm() {
    setName("")
    setError("")
    setHasInteracted(false)
  }

  function handleOpenChange(nextOpen: boolean) {
    if (!nextOpen) resetForm()
    onOpenChange(nextOpen)
  }

  async function handleCreate() {
    if (!name.trim()) {
      setError(t("project.errorNameRequired"))
      setHasInteracted(true)
      return
    }

    setCreating(true)
    setError("")
    try {
      const project = await webApi.createProject(name.trim())
      onCreated(project)
      handleOpenChange(false)
    } catch (createError) {
      setError(createError instanceof Error ? createError.message : String(createError))
    } finally {
      setCreating(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-lg gap-0 overflow-hidden p-0">
        <DialogHeader>
          <DialogTitle className="px-6 pt-6">{t("project.createTitle")}</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-4 px-6 py-5">
          <div className="flex flex-col gap-2">
            <Label htmlFor="name">
              {t("project.name")} <span className="text-destructive">{t("project.requiredMarker")}</span>
            </Label>
            <Input
              id="name"
              value={name}
              onChange={(event) => {
                setHasInteracted(true)
                setError("")
                setName(event.target.value)
              }}
              placeholder={t("project.namePlaceholder")}
              autoFocus
            />
            <p className="text-xs text-muted-foreground">
              The Personal Server creates this project inside its configured Data Root.
            </p>
          </div>
        </div>
        <DialogFooter className="mx-0 mb-0 flex-col border-t bg-background/95 px-6 py-4 sm:flex-row sm:items-center">
          <div className="min-h-5 flex-1 text-left text-sm text-destructive">
            {formStatus.footerError ||
              (formStatus.footerMessageKey ? t(formStatus.footerMessageKey) : "")}
          </div>
          <Button variant="outline" onClick={() => handleOpenChange(false)}>
            {t("project.cancel")}
          </Button>
          <Button onClick={handleCreate} disabled={creating || !formStatus.canCreate}>
            {creating ? t("project.creating") : t("project.create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
