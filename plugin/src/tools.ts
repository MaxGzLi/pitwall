import { parseSessionsArgs, parseStatusArgs, sinceMsFrom } from './args.js'
import type { Config } from './config.js'
import type { ToolDefinitionLike, ToolRegistryLike } from './dsh-context.js'
import { MonitorClient, MonitorUnavailableError, unavailablePayload } from './monitor-client.js'
import { sessionsPayload, statusPayload } from './payload.js'
import { HARNESSES } from './types.js'

const JSON_OUTPUT = {
  schema: { type: 'object' },
  render: (_args: unknown, value: unknown): Array<{ type: 'text'; text: string }> => [
    { type: 'text', text: JSON.stringify(value, null, 2) },
  ],
}

function statusTool(client: MonitorClient, config: Config): ToolDefinitionLike {
  return {
    name: 'monitor_status',
    description: 'Report which local AI coding agents (Claude Code, Codex, DSH) are running right now, '
      + 'what each is working on, and the current provider quota. Read-only; answers questions like '
      + '"现在有几个 agent 在跑" or "还剩多少额度".',
    parameters: { type: 'object', additionalProperties: false, properties: {} },
    output: JSON_OUTPUT,
    isConcurrencySafe: () => true,
    async execute(rawArgs, execution) {
      parseStatusArgs(rawArgs)
      try {
        return statusPayload(await client.snapshot(execution.signal), config, Date.now())
      } catch (error) {
        if (error instanceof MonitorUnavailableError) return unavailablePayload(error)
        throw error
      }
    },
  }
}

function sessionsTool(client: MonitorClient, config: Config): ToolDefinitionLike {
  return {
    name: 'monitor_sessions',
    description: 'List recent local agent sessions with the summary the monitor already stored for each. '
      + 'Use it to answer "我今天/这周让 agent 做了什么" or to recap one harness\'s work. Read-only; '
      + 'summaries are pre-computed, so this does not start a new summarisation.',
    parameters: {
      type: 'object',
      additionalProperties: false,
      properties: {
        limit: {
          type: 'integer',
          minimum: 1,
          maximum: config.maxSessionLimit,
          description: `How many sessions to return, newest first (default ${config.defaultSessionLimit}).`,
        },
        sinceHours: {
          type: 'integer',
          minimum: 1,
          maximum: 720,
          description: 'Only sessions active within this many hours (default 24).',
        },
        harness: {
          type: 'string',
          enum: [...HARNESSES],
          description: 'Restrict to one harness. Omit for all of them.',
        },
      },
    },
    output: JSON_OUTPUT,
    isConcurrencySafe: () => true,
    async execute(rawArgs, execution) {
      const query = parseSessionsArgs(rawArgs, {
        limit: config.defaultSessionLimit,
        maxLimit: config.maxSessionLimit,
        sinceHours: 24,
      })
      const now = Date.now()
      try {
        const entries = await client.sessions(
          { limit: query.limit, sinceMs: sinceMsFrom(query.sinceHours, now) },
          execution.signal,
        )
        return sessionsPayload(entries, { ...config, ...query })
      } catch (error) {
        if (error instanceof MonitorUnavailableError) return unavailablePayload(error)
        throw error
      }
    },
  }
}

export function registerMonitorTools(
  registry: ToolRegistryLike,
  client: MonitorClient,
  config: Config,
): Array<() => void> {
  return [
    registry.register(statusTool(client, config)),
    registry.register(sessionsTool(client, config)),
  ]
}
