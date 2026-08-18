/** Argument validation shared by the model tools and the browser RPC endpoints. */

import { HARNESSES, type Harness } from './types.js'

export class MonitorArgsError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'MonitorArgsError'
  }
}

export const MAX_SINCE_HOURS = 720

export interface SessionsQuery {
  limit: number
  sinceHours: number
  harness?: Harness
}

export interface SessionsQueryDefaults {
  limit: number
  maxLimit: number
  sinceHours: number
}

function record(value: unknown, field = 'arguments'): Record<string, unknown> {
  if (value === undefined || value === null) return {}
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw new MonitorArgsError(`${field} must be an object`)
  }
  return value as Record<string, unknown>
}

function integer(value: unknown, field: string, fallback: number, min: number, max: number): number {
  if (value === undefined || value === null) return fallback
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new MonitorArgsError(`${field} must be a number`)
  }
  // Models routinely send 12.0 for an integer field; accept it, reject 12.5.
  if (!Number.isInteger(value)) throw new MonitorArgsError(`${field} must be a whole number`)
  if (value < min || value > max) {
    throw new MonitorArgsError(`${field} must be between ${min} and ${max}`)
  }
  return value
}

function harnessOf(value: unknown): Harness | undefined {
  if (value === undefined || value === null) return undefined
  if (typeof value !== 'string') throw new MonitorArgsError('harness must be a string')
  const normalized = value.trim().toLowerCase()
  if (normalized === '' || normalized === 'all') return undefined
  if (!(HARNESSES as readonly string[]).includes(normalized)) {
    throw new MonitorArgsError(`harness must be one of: ${HARNESSES.join(', ')}`)
  }
  return normalized as Harness
}

/** `{limit?, sinceHours?, harness?}` from the model, clamped to what the daemon will serve. */
export function parseSessionsArgs(raw: unknown, defaults: SessionsQueryDefaults): SessionsQuery {
  const args = record(raw)
  const known = new Set(['limit', 'sinceHours', 'harness'])
  const unknownKeys = Object.keys(args).filter(key => !known.has(key))
  if (unknownKeys.length > 0) {
    throw new MonitorArgsError(`unknown argument: ${unknownKeys.join(', ')}`)
  }
  const harness = harnessOf(args.harness)
  return {
    limit: integer(args.limit, 'limit', defaults.limit, 1, defaults.maxLimit),
    sinceHours: integer(args.sinceHours, 'sinceHours', defaults.sinceHours, 1, MAX_SINCE_HOURS),
    ...(harness === undefined ? {} : { harness }),
  }
}

/** `monitor_status` takes no arguments; still reject a stray payload rather than ignoring it. */
export function parseStatusArgs(raw: unknown): Record<string, never> {
  const args = record(raw)
  const keys = Object.keys(args)
  if (keys.length > 0) throw new MonitorArgsError(`monitor_status takes no arguments, got: ${keys.join(', ')}`)
  return {}
}

export function sinceMsFrom(sinceHours: number, now: number): number {
  return now - sinceHours * 3_600_000
}
