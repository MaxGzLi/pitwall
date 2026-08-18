import { describe, expect, it } from 'vitest'
import { MonitorConfigError, normalizeServiceUrl, resolveConfig } from '../src/config.js'

describe('normalizeServiceUrl', () => {
  it('keeps a loopback origin and drops the path', () => {
    expect(normalizeServiceUrl('http://127.0.0.1:39917')).toBe('http://127.0.0.1:39917')
    expect(normalizeServiceUrl('http://127.0.0.1:39917/')).toBe('http://127.0.0.1:39917')
    expect(normalizeServiceUrl('http://localhost:39917/api/')).toBe('http://localhost:39917')
  })

  it('refuses to point the plugin at a non-loopback host', () => {
    expect(() => normalizeServiceUrl('http://example.com:39917')).toThrow(MonitorConfigError)
    expect(() => normalizeServiceUrl('https://127.0.0.1:39917')).toThrow(/loopback only/)
    expect(() => normalizeServiceUrl('http://user:pw@127.0.0.1:39917')).toThrow(/credentials/)
    expect(() => normalizeServiceUrl('39917')).toThrow(/absolute http URL/)
  })
})

describe('resolveConfig', () => {
  it('defaults to the daemon port', () => {
    const config = resolveConfig({})
    expect(config.serviceUrl).toBe('http://127.0.0.1:39917')
    expect(config.toolsEnabled).toBe(true)
    expect(config.defaultSessionLimit).toBe(10)
  })

  it('rejects nonsense numbers up front', () => {
    expect(() => resolveConfig({ pollIntervalMs: 10 })).toThrow(/pollIntervalMs/)
    expect(() => resolveConfig({ maxSummaryChars: 0 })).toThrow(/maxSummaryChars/)
    expect(() => resolveConfig({ defaultSessionLimit: 40, maxSessionLimit: 30 }))
      .toThrow(/must not exceed maxSessionLimit/)
  })
})
