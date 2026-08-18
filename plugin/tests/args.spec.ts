import { describe, expect, it } from 'vitest'
import { MonitorArgsError, parseSessionsArgs, parseStatusArgs, sinceMsFrom } from '../src/args.js'

const defaults = { limit: 10, maxLimit: 30, sinceHours: 24 }

describe('parseSessionsArgs', () => {
  it('fills in defaults for an empty or absent payload', () => {
    expect(parseSessionsArgs({}, defaults)).toEqual({ limit: 10, sinceHours: 24 })
    expect(parseSessionsArgs(undefined, defaults)).toEqual({ limit: 10, sinceHours: 24 })
    expect(parseSessionsArgs(null, defaults)).toEqual({ limit: 10, sinceHours: 24 })
  })

  it('keeps a valid harness and drops "all"', () => {
    expect(parseSessionsArgs({ harness: 'codex' }, defaults).harness).toBe('codex')
    expect(parseSessionsArgs({ harness: ' Claude ' }, defaults).harness).toBe('claude')
    expect(parseSessionsArgs({ harness: 'all' }, defaults).harness).toBeUndefined()
    expect(parseSessionsArgs({ harness: '' }, defaults).harness).toBeUndefined()
  })

  it('rejects an unknown harness', () => {
    expect(() => parseSessionsArgs({ harness: 'gemini' }, defaults)).toThrow(MonitorArgsError)
    expect(() => parseSessionsArgs({ harness: 'gemini' }, defaults)).toThrow(/claude, codex, dsh/)
  })

  it('accepts integral floats but rejects fractions', () => {
    expect(parseSessionsArgs({ limit: 12.0 }, defaults).limit).toBe(12)
    expect(() => parseSessionsArgs({ limit: 12.5 }, defaults)).toThrow(/whole number/)
  })

  it('rejects limits outside the configured ceiling', () => {
    expect(() => parseSessionsArgs({ limit: 0 }, defaults)).toThrow(/between 1 and 30/)
    expect(() => parseSessionsArgs({ limit: 31 }, defaults)).toThrow(/between 1 and 30/)
    expect(parseSessionsArgs({ limit: 30 }, defaults).limit).toBe(30)
  })

  it('caps sinceHours at 30 days', () => {
    expect(parseSessionsArgs({ sinceHours: 720 }, defaults).sinceHours).toBe(720)
    expect(() => parseSessionsArgs({ sinceHours: 721 }, defaults)).toThrow(/between 1 and 720/)
  })

  it('rejects non-object arguments and unknown keys', () => {
    expect(() => parseSessionsArgs('10', defaults)).toThrow(/must be an object/)
    expect(() => parseSessionsArgs([1, 2], defaults)).toThrow(/must be an object/)
    expect(() => parseSessionsArgs({ project: 'x' }, defaults)).toThrow(/unknown argument: project/)
  })

  it('rejects a non-numeric limit instead of coercing it', () => {
    expect(() => parseSessionsArgs({ limit: '5' }, defaults)).toThrow(/limit must be a number/)
    expect(() => parseSessionsArgs({ limit: Number.NaN }, defaults)).toThrow(/limit must be a number/)
  })
})

describe('parseStatusArgs', () => {
  it('accepts nothing', () => {
    expect(parseStatusArgs({})).toEqual({})
    expect(parseStatusArgs(undefined)).toEqual({})
  })

  it('rejects a stray payload rather than silently ignoring it', () => {
    expect(() => parseStatusArgs({ limit: 3 })).toThrow(/takes no arguments/)
  })
})

describe('sinceMsFrom', () => {
  it('walks back whole hours from now', () => {
    expect(sinceMsFrom(24, 1_787_036_900_000)).toBe(1_787_036_900_000 - 86_400_000)
  })
})
