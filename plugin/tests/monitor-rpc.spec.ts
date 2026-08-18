import { readFileSync } from 'node:fs'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { resolveConfig } from '../src/config.js'
import { MonitorClient } from '../src/monitor-client.js'
import { handleMonitorRpc } from '../src/monitor-rpc.js'
import type { SessionsPayload, StatusPayload, UnavailablePayload } from '../src/types.js'

const SNAPSHOT_JSON = readFileSync(new URL('./fixtures/snapshot.json', import.meta.url), 'utf8')
const SESSIONS_JSON = readFileSync(new URL('./fixtures/sessions.json', import.meta.url), 'utf8')

const config = resolveConfig({ serviceUrl: 'http://127.0.0.1:39931' })
const client = new MonitorClient(config.serviceUrl, config.requestTimeoutMs)

function stubFetch(handler: (url: string) => Response): string[] {
  const seen: string[] = []
  vi.stubGlobal('fetch', async (input: string | URL) => {
    const url = String(input)
    seen.push(url)
    return handler(url)
  })
  return seen
}

function ok(body: string): Response {
  return new Response(body, { status: 200, headers: { 'content-type': 'application/json' } })
}

afterEach(() => { vi.unstubAllGlobals() })

describe('handleMonitorRpc', () => {
  it('hands the browser half its config, since client plugins get none', async () => {
    const result = await handleMonitorRpc(client, config, 'config', {}, new AbortController().signal)
    expect(result).toEqual({
      ok: true,
      value: {
        serviceUrl: 'http://127.0.0.1:39931',
        pollIntervalMs: 5_000,
        defaultSessionLimit: 10,
        maxSessionLimit: 30,
      },
    })
  })

  it('projects a real snapshot for the dock', async () => {
    stubFetch(() => ok(SNAPSHOT_JSON))
    const result = await handleMonitorRpc(client, config, 'snapshot', {}, new AbortController().signal)
    expect(result.ok).toBe(true)
    const value = (result as { ok: true; value: StatusPayload }).value
    expect(value.available).toBe(true)
    expect(value.live).toBeGreaterThan(0)
    expect(value.agents.length).toBeLessThanOrEqual(12)
  })

  it('turns sinceHours into the daemon since_ms query', async () => {
    const seen = stubFetch(() => ok(SESSIONS_JSON))
    const now = 1_787_036_900_000
    const result = await handleMonitorRpc(
      client, config, 'sessions', { limit: 5, sinceHours: 6 }, new AbortController().signal, () => now,
    )
    expect(seen[0]).toBe(`http://127.0.0.1:39931/api/sessions?limit=5&since_ms=${now - 6 * 3_600_000}`)
    const value = (result as { ok: true; value: SessionsPayload }).value
    expect(value.sinceHours).toBe(6)
    expect(value.count).toBeGreaterThan(0)
  })

  it('reports a bad argument instead of quietly clamping it', async () => {
    const result = await handleMonitorRpc(client, config, 'sessions', { limit: 900 }, new AbortController().signal)
    expect(result).toEqual({
      ok: false,
      error: { code: 'bad-request', message: 'limit must be between 1 and 30', details: { issues: [] } },
    })
  })

  it('rejects an unknown endpoint', async () => {
    const result = await handleMonitorRpc(client, config, 'delete-everything', {}, new AbortController().signal)
    expect(result.ok).toBe(false)
  })

  it('degrades to an available:false value when the daemon is down', async () => {
    vi.stubGlobal('fetch', async () => { throw new TypeError('fetch failed') })
    const result = await handleMonitorRpc(client, config, 'snapshot', {}, new AbortController().signal)
    expect(result.ok).toBe(true)
    const value = (result as { ok: true; value: UnavailablePayload }).value
    expect(value.available).toBe(false)
    expect(value.reason).toContain('http://127.0.0.1:39931')
  })

  it('short-circuits an already-cancelled call', async () => {
    const controller = new AbortController()
    controller.abort()
    const result = await handleMonitorRpc(client, config, 'snapshot', {}, controller.signal)
    expect(result).toEqual({ ok: false, error: { code: 'cancelled', message: 'request cancelled', details: {} } })
  })
})

describe('MonitorClient', () => {
  it('treats a 404 summary as "no summary yet", not a failure', async () => {
    stubFetch(() => new Response('{"error":"no summary for that session"}', { status: 404 }))
    await expect(client.summary('dsh', 'a5dcae44')).resolves.toBeNull()
  })

  it('percent-encodes path segments', async () => {
    const seen = stubFetch(() => ok('{"headline":"x"}'))
    await client.summary('claude', 'a b/c')
    expect(seen[0]).toBe('http://127.0.0.1:39931/api/summary/claude/a%20b%2Fc')
  })

  it('surfaces a 500 as unavailable rather than parsing the error body as data', async () => {
    stubFetch(() => new Response('{"error":"boom"}', { status: 500 }))
    await expect(client.snapshot()).rejects.toThrow(/500 on \/api\/snapshot/)
  })
})
