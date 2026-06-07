import JSZip from "jszip"
import type { MineruConfig } from "@/stores/wiki-store"
import { readFileAsBase64 } from "@/commands/fs"
import { getHttpFetch } from "@/lib/tauri-fetch"

const API_BASE = "https://mineru.net/api/v4"
const POLL_INTERVAL_MS = 3_000
const POLL_TIMEOUT_MS = 300_000 // 5 minutes

// ── Types ──

interface TaskResponse {
  code: number
  data: { task_id: string }
  msg: string
}

interface TaskStatus {
  code: number
  data: {
    task_id: string
    state: "running" | "done" | "failed"
    full_zip_url?: string
    err_msg?: string
    extract_progress?: { extracted_pages: number; total_pages: number }
  }
  msg: string
}

interface BatchStatus {
  code: number
  data: {
    batch_id: string
    extract_result: Array<{
      file_name: string
      state: "running" | "done" | "failed"
      full_zip_url?: string
      err_msg?: string
    }>
  }
  msg: string
}

interface UploadUrlResponse {
  code: number
  data: {
    batch_id: string
    file_urls: string[]
  }
  msg: string
}

// ── API calls ──

async function mineruHeaders(token: string): Promise<HeadersInit> {
  return {
    "Content-Type": "application/json",
    Authorization: `Bearer ${token}`,
  }
}

async function submitUrlTask(
  token: string,
  url: string,
  modelVersion: string,
): Promise<string> {
  const httpFetch = await getHttpFetch()
  const res = await httpFetch(`${API_BASE}/extract/task`, {
    method: "POST",
    headers: await mineruHeaders(token),
    body: JSON.stringify({ url, model_version: modelVersion }),
  })
  if (!res.ok) throw new Error(`MinerU submit failed: HTTP ${res.status}`)
  const json: TaskResponse = await res.json()
  if (json.code !== 0) throw new Error(`MinerU API error: ${json.msg}`)
  return json.data.task_id
}

async function uploadFileForTask(
  token: string,
  fileName: string,
  fileBase64: string,
  modelVersion: string,
): Promise<{ batchId: string; uploadUrl: string }> {
  const httpFetch = await getHttpFetch()
  const headers = await mineruHeaders(token)

  // Step 1: Get upload URL
  const res = await httpFetch(`${API_BASE}/file-urls/batch`, {
    method: "POST",
    headers,
    body: JSON.stringify({
      files: [{ name: fileName, data_id: fileName }],
      model_version: modelVersion,
    }),
  })
  if (!res.ok) throw new Error(`MinerU batch submit failed: HTTP ${res.status}`)
  const json: UploadUrlResponse = await res.json()
  if (json.code !== 0) throw new Error(`MinerU API error: ${json.msg}`)

  const batchId = json.data.batch_id
  const uploadUrl = json.data.file_urls[0]

  // Step 2: Upload file binary (convert base64 back to binary)
  const binaryStr = atob(fileBase64)
  const bytes = new Uint8Array(binaryStr.length)
  for (let i = 0; i < binaryStr.length; i++) {
    bytes[i] = binaryStr.charCodeAt(i)
  }

  const uploadRes = await httpFetch(uploadUrl, {
    method: "PUT",
    body: bytes,
  })
  if (!uploadRes.ok && uploadRes.status !== 200 && uploadRes.status !== 201) {
    throw new Error(`MinerU file upload failed: HTTP ${uploadRes.status}`)
  }

  return { batchId, uploadUrl }
}

async function pollTask(token: string, taskId: string): Promise<string> {
  const httpFetch = await getHttpFetch()
  const headers = await mineruHeaders(token)
  const start = Date.now()

  while (Date.now() - start < POLL_TIMEOUT_MS) {
    const res = await httpFetch(`${API_BASE}/extract/task/${taskId}`, {
      headers,
    })
    if (!res.ok) throw new Error(`MinerU poll failed: HTTP ${res.status}`)
    const json: TaskStatus = await res.json()

    if (json.data.state === "done" && json.data.full_zip_url) {
      return json.data.full_zip_url
    }
    if (json.data.state === "failed") {
      throw new Error(`MinerU parsing failed: ${json.data.err_msg ?? "unknown error"}`)
    }

    await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS))
  }

  throw new Error("MinerU parsing timed out after 5 minutes")
}

async function pollBatchTask(
  token: string,
  batchId: string,
): Promise<string> {
  const httpFetch = await getHttpFetch()
  const headers = await mineruHeaders(token)
  const start = Date.now()

  while (Date.now() - start < POLL_TIMEOUT_MS) {
    const res = await httpFetch(
      `${API_BASE}/extract-results/batch/${batchId}`,
      { headers },
    )
    if (!res.ok) throw new Error(`MinerU batch poll failed: HTTP ${res.status}`)
    const json: BatchStatus = await res.json()

    const result = json.data.extract_result[0]
    if (result?.state === "done" && result.full_zip_url) {
      return result.full_zip_url
    }
    if (result?.state === "failed") {
      throw new Error(`MinerU parsing failed: ${result.err_msg ?? "unknown error"}`)
    }

    await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS))
  }

  throw new Error("MinerU parsing timed out after 5 minutes")
}

async function downloadAndExtractMarkdown(zipUrl: string): Promise<string> {
  const httpFetch = await getHttpFetch()
  const res = await httpFetch(zipUrl)
  if (!res.ok) throw new Error(`MinerU zip download failed: HTTP ${res.status}`)

  const buffer = await res.arrayBuffer()
  const zip = await JSZip.loadAsync(buffer)

  // Look for .md file in the zip (MinerU typically outputs full.md or <name>.md)
  const mdEntries: [string, JSZip.JSZipObject][] = []
  zip.forEach((relativePath, file) => {
    if (!file.dir && relativePath.endsWith(".md")) {
      mdEntries.push([relativePath, file])
    }
  })

  if (mdEntries.length === 0) {
    throw new Error("No Markdown file found in MinerU result zip")
  }

  return await mdEntries[0][1].async("string")
}

// ── Public API ──

/**
 * Parse a PDF file using MinerU cloud API.
 *
 * @param config MinerU configuration (token, model version)
 * @param sourcePath Local file path to the PDF
 * @param sourceUrl Optional URL if the PDF was fetched from the web — avoids re-upload
 * @param onProgress Optional progress callback
 * @returns Parsed Markdown content
 */
export async function parseWithMineru(
  config: MineruConfig,
  sourcePath: string,
  sourceUrl?: string,
  onProgress?: (msg: string) => void,
): Promise<string> {
  if (!config.token) throw new Error("MinerU API token not configured")

  let zipUrl: string

  if (sourceUrl) {
    onProgress?.("Submitting URL to MinerU...")
    const taskId = await submitUrlTask(config.token, sourceUrl, config.modelVersion)
    onProgress?.("Waiting for MinerU to finish...")
    zipUrl = await pollTask(config.token, taskId)
  } else {
    onProgress?.("Uploading file to MinerU...")

    // Read file as base64
    const fileName = sourcePath.split("/").pop() ?? "document.pdf"
    const { base64 } = await readFileAsBase64(sourcePath)

    const { batchId } = await uploadFileForTask(
      config.token,
      fileName,
      base64,
      config.modelVersion,
    )
    onProgress?.("Waiting for MinerU to finish...")
    zipUrl = await pollBatchTask(config.token, batchId)
  }

  onProgress?.("Downloading parsed result...")
  const markdown = await downloadAndExtractMarkdown(zipUrl)
  onProgress?.("Done")

  return markdown
}

/**
 * Test MinerU API connectivity by submitting a minimal task.
 * Returns true if the token is valid.
 */
export async function testMineruConnection(token: string): Promise<void> {
  const httpFetch = await getHttpFetch()
  const res = await httpFetch(`${API_BASE}/extract/task`, {
    method: "POST",
    headers: await mineruHeaders(token),
    body: JSON.stringify({
      url: "https://cdn-mineru.openxlab.org.cn/demo/example.pdf",
      model_version: "pipeline",
    }),
  })

  if (!res.ok) {
    const text = await res.text().catch(() => "")
    throw new Error(`HTTP ${res.status}: ${text}`)
  }

  const json: TaskResponse = await res.json()
  if (json.code !== 0) {
    throw new Error(json.msg || "Unknown API error")
  }
}
