import { readFileSync } from 'node:fs'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { resolveConfig } from '../src/config.js'
import type { ToolDefinitionLike } from '../src/dsh-context.js'
import { MonitorClient } from '../src/monitor-client.js'
import { registerMonitorTools } from '../src/tools.js'
import type { SessionsPayload, StatusPayload, UnavailablePayload } from '../src/types.js'

const SNAPSHOT_JSON = readFileSync(new URL('./fixtures/snapshot.json', import.meta.url), 'utf8')
const SESSIONS_JSON = readFileSync(new URL('./fixtures/sessions.json', import.meta.url), 'utf8')

const config = resolveConfig({ serviceUrl: 'http://127.0.0.1:39931' })
const client = new MonitorClient(config.serviceUrl, config.requestTimeoutMs)

function collect(): { registry: { register(d: ToolDefinitionLike): () => void }; tools: Map<string, ToolDefinitionLike> } {
  const tools = new Map<string, ToolDefinitionLike>()
  return {
    registry: {
      register(definition) {
        tools.set(definition.name, definition)
        return () => { tools.delete(definition.name) }
      },
    },
    tools,
  }
}

afterEach(() => { vi.unstubAllGlobals() })

describe('registerMonitorTools', () => {
  it('registers exactly the two model-callable tools and returns disposers', () => {
    const { registry, tools } = collect()
    const disposers = registerMonitorTools(registry, client, config)
    expect([...tools.keys()].sort()).toEqual(['monitor_sessions', 'monitor_status'])
    for (const dispose of disposers) dispose()
    expect(tools.size).toBe(0)
  })

  it('declares closed JSON Schema parameters', () => {
    const { registry, tools } = collect()
    registerMonitorTools(registry, client, config)
    const sessions = tools.get('monitor_sessions')!
    expect(sessions.parameters.additionalProperties).toBe(false)
    expect(sessions.parameters.required).toBeUndefined()
    const properties = sessions.parameters.properties as Record<string, { maximum?: number; enum?: string[] }>
    expect(properties.limit!.maximum).toBe(config.maxSessionLimit)
    expect(properties.harness!.enum).toEqual(['claude', 'codex', 'dsh'])
    expect(tools.get('monitor_status')!.parameters.properties).toEqual({})
  })

  it('answers "how many agents are running" from a real snapshot', async () => {
    vi.stubGlobal('fetch', async () => new Response(SNAPSHOT_JSON, { status: 200 }))
    const { registry, tools } = collect()
    registerMonitorTools(registry, client, config)
    const value = await tools.get('monitor_status')!.execute({}, {}) as StatusPayload
    expect(value.available).toBe(true)
    expect(value.running).toBeGreaterThan(0)
    expect(value.agents.every(agent => agent.sessionId.length > 0)).toBe(true)
  })

  it('forwards the execution signal so a cancelled call aborts the fetch', async () => {
    const seen: Array<AbortSignal | undefined> = []
    vi.stubGlobal('fetch', async (_url: string, init: RequestInit) => {
      seen.push(init.signal ?? undefined)
      return new Response(SESSIONS_JSON, { status: 200 })
    })
    const { registry, tools } = collect()
    registerMonitorTools(registry, client, config)
    const controller = new AbortController()
    const value = await tools.get('monitor_sessions')!
      .execute({ limit: 3, harness: 'claude' }, { signal: controller.signal }) as SessionsPayload
    expect(seen[0]).toBeInstanceOf(AbortSignal)
    controller.abort()
    expect(seen[0]!.aborted).toBe(true)
    expect(value.sessions.every(session => session.harness === 'claude')).toBe(true)
  })

  it('rejects invalid model arguments', async () => {
    const { registry, tools } = collect()
    registerMonitorTools(registry, client, config)
    await expect(tools.get('monitor_sessions')!.execute({ harness: 'gemini' }, {}))
      .rejects.toThrow(/harness must be one of/)
    await expect(tools.get('monitor_status')!.execute({ limit: 1 }, {}))
      .rejects.toThrow(/takes no arguments/)
  })

  it('tells the model the daemon is down instead of throwing at it', async () => {
    vi.stubGlobal('fetch', async () => { throw new TypeError('fetch failed') })
    const { registry, tools } = collect()
    registerMonitorTools(registry, client, config)
    const value = await tools.get('monitor_sessions')!.execute({}, {}) as UnavailablePayload
    expect(value.available).toBe(false)
    expect(value.hint).toContain('agent-monitord')
  })

  it('renders its value as plain JSON text', () => {
    const { registry, tools } = collect()
    registerMonitorTools(registry, client, config)
    const blocks = tools.get('monitor_status')!.output.render({}, { available: true })
    expect(blocks).toEqual([{ type: 'text', text: '{\n  "available": true\n}' }])
  })
})
