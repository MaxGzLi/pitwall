import type { ClientConnectionRpc } from '@deepseek-ai/dsh-client-connection/client'
import type { MonitorClientConfig } from '../monitor-rpc.js'
import type { SessionsPayload, StatusPayload, UnavailablePayload } from '../types.js'

export const MONITOR_RPC_CHANNEL = '/monitor-rpc'

export type MonitorResult<T> =
  | { ok: true; value: T }
  | { ok: false; error: string }

export interface SessionsRequest {
  limit?: number
  sinceHours?: number
  harness?: string
}

/** Browser half of the channel. It never touches the daemon origin itself. */
export class MonitorRpcClient {
  constructor(private readonly rpc: ClientConnectionRpc) {}

  config(signal?: AbortSignal): Promise<MonitorResult<MonitorClientConfig>> {
    return this.call('config', {}, signal)
  }

  snapshot(signal?: AbortSignal): Promise<MonitorResult<StatusPayload | UnavailablePayload>> {
    return this.call('snapshot', {}, signal)
  }

  sessions(request: SessionsRequest = {}, signal?: AbortSignal): Promise<MonitorResult<SessionsPayload | UnavailablePayload>> {
    return this.call('sessions', request, signal)
  }

  private async call<T>(endpoint: string, payload: unknown, signal?: AbortSignal): Promise<MonitorResult<T>> {
    try {
      const result = await this.rpc.call(MONITOR_RPC_CHANNEL, endpoint, payload, signal)
      if (!result.ok) return { ok: false, error: result.error.message }
      return { ok: true, value: result.value as T }
    } catch (error) {
      return { ok: false, error: error instanceof Error ? error.message : String(error) }
    }
  }
}
