import type { Context } from '@deepseek-ai/cordis'
import { Config, resolveConfig, type Config as MonitorConfig } from './config.js'
import type { MonitorHostContext } from './dsh-context.js'
import { MonitorClient } from './monitor-client.js'
import { registerMonitorRpc } from './monitor-rpc.js'
import { registerMonitorTools } from './tools.js'

export const name = 'dsh-monitor'
export const inject = ['tools']
export { Config }

/** Host half: one loopback reader, one RPC channel for the dock, and optional model tools. */
export function apply(ctx: Context, rawConfig: MonitorConfig): void {
  const runtime = ctx as unknown as MonitorHostContext
  const config = resolveConfig(rawConfig)
  const logger = runtime.logger('dsh-monitor')
  const client = new MonitorClient(config.serviceUrl, config.requestTimeoutMs)

  registerMonitorRpc(ctx, client, config)

  if (config.toolsEnabled) {
    const disposers = registerMonitorTools(runtime.tools, client, config)
    runtime.effect(() => () => {
      for (const dispose of disposers) dispose()
    }, 'dsh-monitor: model tools')
  }

  logger.info('agent monitor bridge ready at %s (tools: %s)', config.serviceUrl, config.toolsEnabled)
}

export { MonitorClient, MonitorUnavailableError } from './monitor-client.js'
export { handleMonitorRpc, registerMonitorRpc, MONITOR_RPC_CHANNEL } from './monitor-rpc.js'
export type { MonitorClientConfig } from './monitor-rpc.js'
export { registerMonitorTools } from './tools.js'
export { normalizeSessions, sessionsPayload, statusPayload, trimText } from './payload.js'
export { MonitorArgsError, parseSessionsArgs, parseStatusArgs } from './args.js'
export { MonitorConfigError, normalizeServiceUrl, resolveConfig } from './config.js'
export type * from './types.js'
