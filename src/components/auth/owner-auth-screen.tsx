import { useState, type FormEvent } from "react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { webApi, type WebSession } from "@/platform/web-api"

export type OwnerAuthMode = "setup" | "login"

interface OwnerAuthScreenProps {
  mode: OwnerAuthMode
  onAuthenticated: (session: WebSession) => void | Promise<void>
}

export function getOwnerAuthValidationError(
  mode: OwnerAuthMode,
  password: string,
  confirmation: string,
): string | null {
  if (Array.from(password).length < 8) {
    return "Password must be at least 8 characters."
  }
  if (mode === "setup" && password !== confirmation) {
    return "Passwords do not match."
  }
  return null
}

export function OwnerAuthScreen({
  mode,
  onAuthenticated,
}: OwnerAuthScreenProps) {
  const [password, setPassword] = useState("")
  const [confirmation, setConfirmation] = useState("")
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const setup = mode === "setup"

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const validationError = getOwnerAuthValidationError(
      mode,
      password,
      confirmation,
    )
    if (validationError) {
      setError(validationError)
      return
    }

    setSubmitting(true)
    setError(null)
    try {
      const session = setup
        ? await webApi.setupOwner(password)
        : await webApi.login(password)
      await onAuthenticated(session)
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="flex h-full items-center justify-center bg-background p-6">
      <div className="w-full max-w-sm rounded-xl border bg-card p-6 shadow-sm">
        <h1 className="text-xl font-semibold">
          {setup ? "Create the owner password" : "Sign in to LLM Wiki"}
        </h1>
        <p className="mt-2 text-sm text-muted-foreground">
          {setup
            ? "This password protects the Personal Server. It is stored only as an Argon2 hash."
            : "Enter the single-owner password for this Personal Server."}
        </p>

        <form className="mt-6 space-y-4" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <Label htmlFor="owner-password">Password</Label>
            <Input
              id="owner-password"
              type="password"
              autoComplete={setup ? "new-password" : "current-password"}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              disabled={submitting}
              aria-invalid={Boolean(error)}
              autoFocus
              required
            />
          </div>

          {setup && (
            <div className="space-y-2">
              <Label htmlFor="owner-password-confirmation">Confirm password</Label>
              <Input
                id="owner-password-confirmation"
                type="password"
                autoComplete="new-password"
                value={confirmation}
                onChange={(event) => setConfirmation(event.target.value)}
                disabled={submitting}
                aria-invalid={Boolean(error)}
                required
              />
            </div>
          )}

          <p
            className="min-h-5 text-sm text-destructive"
            role="alert"
            aria-live="polite"
          >
            {error}
          </p>

          <Button className="w-full" type="submit" disabled={submitting}>
            {submitting
              ? setup ? "Creating owner…" : "Signing in…"
              : setup ? "Create owner" : "Sign in"}
          </Button>
        </form>
      </div>
    </div>
  )
}
