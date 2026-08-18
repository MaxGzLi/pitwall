import type { Context } from '@deepseek-ai/cordis'
import type { HostConnectionHandle } from '@deepseek-ai/dsh-client-connection'
import type { RpcResult } from '@deepseek-ai/dsh-host-apiproxy/api'
import { MonitorArgsError, parseSessionsArgs, sinceMsFrom } from './args.js'
import type { Config } from './config.js'
import { MonitorClient, MonitorUnavailableError, unavailablePayload } from './monitor-client.js'
import { sessionsPayload, statusPayload } from './payload.js'

export const MONITOR_RPC_CHANNEL = '/monitor-rpc'

/**
 * Client plugins are created with no config, so the browser half learns its
 * poll cadence and the daemon's address from this endpoint instead.
 */
export interface MonitorClientConfig {
  serviceUrl: string
  pollIntervalMs: number
  defaultSessionLimit: number
  maxSessionLimit: number
}

interface MonitorRpcConnectionContext {
  connection: HostConnectionHandle
}

interface MonitorRpcHostContext {
  inject(services: string[], callback: (ctx: MonitorRpcConnectionContext) => void | (() => void)): unknown
}

function success<T>(value: T): RpcResult<T> {
  return { ok: true, value }
}

function failure(error: unknown): RpcResult<never> {
  if (error instanceof MonitorArgsError) {
    return { ok: false, error: { code: 'bad-request', message: error.message, details: { issues: [] } } }
  }
  return { ok: false, error: { code: 'internal', message: 'monitor request failed', details: {} } }
}

/** Transport-independent handler, so the tests can drive it without a DSH server. */
export async function handleMonitorRpc(
  client: MonitorClient,
  config: Config,
  endpoint: string,
  payload: unknown,
  signal: AbortSignal,
  now: () => number = Date.now,
): Promise<RpcResult<unknown>> {
  if (signal.aborted) {
    return { ok: false, error: { code: 'cancelled', message: 'request cancelled', details: {} } }
  }
  try {
    if (endpoint === 'config') {
      const value: MonitorClientConfig = {
        serviceUrl: config.serviceUrl,
        pollIntervalMs: config.pollIntervalMs,
        defaultSessionLimit: config.defaultSessionLimit,
        maxSessionLimit: config.maxSessionLimit,
      }
      return success(value)
    }
    if (endpoint === 'snapshot') {
      const at = now()
      return success(statusPayload(await client.snapshot(signal), config, at))
    }
    if (endpoint === 'sessions') {
      const query = parseSessionsArgs(payload, {
        limit: config.defaultSessionLimit,
        maxLimit: config.maxSessionLimit,
        sinceHours: 24,
      })
      const at = now()
      const entries = await client.sessions(
        { limit: query.limit, sinceMs: sinceMsFrom(query.sinceHours, at) },
        signal,
      )
      return success(sessionsPayload(entries, { ...config, ...query }))
    }
    throw new MonitorArgsError(`unknown monitor endpoint: ${endpoint}`)
  } catch (error) {
    if (error instanceof MonitorUnavailableError) return success(unavailablePayload(error))
    return failure(error)
  }
}

/** Loopback-only channel; the browser half never reaches the daemon directly. */
export function registerMonitorRpc(ctx: Context, client: MonitorClient, config: Config): void {
  const runtime = ctx as unknown as MonitorRpcHostContext
  runtime.inject(['connection'], (webCtx) => {
    webCtx.connection.rpc.handle(
      MONITOR_RPC_CHANNEL,
      (endpoint, payload, signal) => handleMonitorRpc(client, config, endpoint, payload, signal),
      { authority: 'loopback' },
    )
  })
}
