/**
 * Host-side reader for agent-monitord. Node 22 global fetch, GET only — the
 * daemon exposes nothing that mutates and this plugin must never invent one.
 */

import { normalizeSessions } from './payload.js'
import type { SessionEntry, Snapshot, SummaryRow, UnavailablePayload } from './types.js'

export class MonitorUnavailableError extends Error {
  constructor(readonly baseUrl: string, readonly detail: string) {
    super(`agent-monitord is not answering at ${baseUrl} (${detail})`)
    this.name = 'MonitorUnavailableError'
  }
}

export function unavailablePayload(error: MonitorUnavailableError): UnavailablePayload {
  return {
    available: false,
    reason: error.message,
    hint: 'Start the daemon with `agent-monitord`, or point serviceUrl at the port it uses.',
  }
}

export interface SessionsRequest {
  limit: number
  sinceMs: number
}

export class MonitorClient {
  constructor(readonly baseUrl: string, private readonly timeoutMs: number) {}

  async snapshot(signal?: AbortSignal): Promise<Snapshot> {
    return await this.getJson<Snapshot>('/api/snapshot', signal)
  }

  async sessions(request: SessionsRequest, signal?: AbortSignal): Promise<SessionEntry[]> {
    const query = new URLSearchParams({
      limit: String(request.limit),
      since_ms: String(Math.max(0, Math.trunc(request.sinceMs))),
    })
    return normalizeSessions(await this.getJson<unknown>(`/api/sessions?${query.toString()}`, signal))
  }

  /** `null` when the daemon has no summary yet — a 404 here is an answer, not a failure. */
  async summary(harness: string, sessionId: string, signal?: AbortSignal): Promise<SummaryRow | null> {
    const path = `/api/summary/${encodeURIComponent(harness)}/${encodeURIComponent(sessionId)}`
    const response = await this.send(path, signal)
    if (response.status === 404) return null
    return await this.readJson<SummaryRow>(response, path)
  }

  private async getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
    return await this.readJson<T>(await this.send(path, signal), path)
  }

  private async send(path: string, signal?: AbortSignal): Promise<Response> {
    const timeout = AbortSignal.timeout(this.timeoutMs)
    const merged = signal === undefined ? timeout : AbortSignal.any([signal, timeout])
    try {
      return await fetch(`${this.baseUrl}${path}`, {
        method: 'GET',
        headers: { accept: 'application/json' },
        signal: merged,
      })
    } catch (error) {
      if (signal?.aborted === true) throw error
      throw new MonitorUnavailableError(this.baseUrl, error instanceof Error ? error.message : String(error))
    }
  }

  private async readJson<T>(response: Response, path: string): Promise<T> {
    if (!response.ok) {
      throw new MonitorUnavailableError(this.baseUrl, `${response.status} on ${path}`)
    }
    try {
      return await response.json() as T
    } catch (error) {
      throw new MonitorUnavailableError(this.baseUrl, `unreadable body on ${path}: ${String(error)}`)
    }
  }
}
