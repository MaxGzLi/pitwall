import Schema from '@deepseek-ai/schemastery'

export class MonitorConfigError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'MonitorConfigError'
  }
}

export interface Config {
  serviceUrl: string
  toolsEnabled: boolean
  requestTimeoutMs: number
  pollIntervalMs: number
  defaultSessionLimit: number
  maxSessionLimit: number
  maxSummaryChars: number
  maxTitleChars: number
}

export const Config: Schema<Config> = Schema.object({
  serviceUrl: Schema.string().default('http://127.0.0.1:39917'),
  toolsEnabled: Schema.boolean().default(true),
  requestTimeoutMs: Schema.number().default(4_000),
  pollIntervalMs: Schema.number().default(5_000),
  defaultSessionLimit: Schema.number().default(10),
  maxSessionLimit: Schema.number().default(30),
  maxSummaryChars: Schema.number().default(600),
  maxTitleChars: Schema.number().default(90),
})

function integerBetween(value: number, field: string, min: number, max: number): number {
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new MonitorConfigError(`${field} must be an integer between ${min} and ${max}`)
  }
  return value
}

/**
 * The monitor is a loopback-only, unauthenticated service. Refusing anything
 * but a loopback origin keeps a mistyped config from turning this plugin into
 * an outbound client for someone else's host.
 */
export function normalizeServiceUrl(raw: string): string {
  let url: URL
  try {
    url = new URL(raw)
  } catch {
    throw new MonitorConfigError(`serviceUrl must be an absolute http URL, got: ${raw}`)
  }
  if (url.protocol !== 'http:') {
    throw new MonitorConfigError('serviceUrl must use http:// — the monitor listens on loopback only')
  }
  if (!['127.0.0.1', 'localhost', '[::1]', '::1'].includes(url.hostname)) {
    throw new MonitorConfigError(`serviceUrl host must be loopback, got: ${url.hostname}`)
  }
  if (url.username !== '' || url.password !== '') {
    throw new MonitorConfigError('serviceUrl must not carry credentials')
  }
  return `${url.protocol}//${url.host}`
}

export function resolveConfig(raw: Partial<Config> = {}): Config {
  const urlInput = raw.serviceUrl?.trim() || process.env.AGENT_MONITOR_URL?.trim()
  const config: Config = {
    serviceUrl: normalizeServiceUrl(urlInput && urlInput !== '' ? urlInput : 'http://127.0.0.1:39917'),
    toolsEnabled: raw.toolsEnabled ?? true,
    requestTimeoutMs: integerBetween(raw.requestTimeoutMs ?? 4_000, 'requestTimeoutMs', 200, 60_000),
    pollIntervalMs: integerBetween(raw.pollIntervalMs ?? 5_000, 'pollIntervalMs', 1_000, 600_000),
    defaultSessionLimit: integerBetween(raw.defaultSessionLimit ?? 10, 'defaultSessionLimit', 1, 100),
    maxSessionLimit: integerBetween(raw.maxSessionLimit ?? 30, 'maxSessionLimit', 1, 100),
    maxSummaryChars: integerBetween(raw.maxSummaryChars ?? 600, 'maxSummaryChars', 40, 8_000),
    maxTitleChars: integerBetween(raw.maxTitleChars ?? 90, 'maxTitleChars', 20, 500),
  }
  if (config.defaultSessionLimit > config.maxSessionLimit) {
    throw new MonitorConfigError('defaultSessionLimit must not exceed maxSessionLimit')
  }
  return config
}
