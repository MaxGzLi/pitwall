import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import {
  dockSummary,
  formatAge,
  formatCost,
  formatTokens,
  isUnavailable,
  peakQuota,
  stateLabel,
} from '../src/client/format.js'
import { statusPayload } from '../src/payload.js'
import type { Snapshot } from '../src/types.js'

const SNAPSHOT = JSON.parse(
  readFileSync(new URL('./fixtures/snapshot.json', import.meta.url), 'utf8'),
) as Snapshot

describe('dock formatting', () => {
  it('abbreviates token counts', () => {
    expect(formatTokens(0)).toBe('0')
    expect(formatTokens(940)).toBe('940')
    expect(formatTokens(9_583)).toBe('9.6k')
    expect(formatTokens(6_331_327)).toBe('6.33M')
  })

  it('never shows a misleading $0.00 for a nonzero cost', () => {
    expect(formatCost(0)).toBe('$0')
    expect(formatCost(0.0014)).toBe('<$0.01')
    expect(formatCost(7.3658)).toBe('$7.37')
  })

  it('renders ages in the unit a human reads', () => {
    expect(formatAge(0)).toBe('刚刚')
    expect(formatAge(7)).toBe('7 分钟前')
    expect(formatAge(150)).toBe('2.5 小时前')
    expect(formatAge(4_320)).toBe('3 天前')
  })

  it('falls back to the raw state for anything unmapped', () => {
    expect(stateLabel('working')).toBe('运行中')
    expect(stateLabel('teleporting')).toBe('teleporting')
  })
})

describe('peakQuota', () => {
  it('picks the row closest to exhaustion from the real quota table', () => {
    const status = statusPayload(SNAPSHOT, { maxTitleChars: 90, maxSummaryChars: 600 }, SNAPSHOT.generated_at_ms)
    const peak = peakQuota(status.quota)
    expect(peak).not.toBeNull()
    const highest = Math.max(...status.quota.map(row => row.usedPercent ?? -1))
    expect(peak!.usedPercent).toBe(highest)
  })

  it('ignores rows with no percentage', () => {
    expect(peakQuota([])).toBeNull()
    expect(peakQuota([
      { provider: 'deepseek', window: 'balance', usedPercent: null, plan: null, balance: 12, currency: 'CNY', resetsInMinutes: null },
    ])).toBeNull()
  })
})

describe('dockSummary', () => {
  it('summarises the real snapshot into the pill values', () => {
    const status = statusPayload(SNAPSHOT, { maxTitleChars: 90, maxSummaryChars: 600 }, SNAPSHOT.generated_at_ms)
    const summary = dockSummary(status)
    expect(summary.running).toBe(status.running)
    expect(summary.live).toBe(status.live)
    expect(summary.quotaText).toMatch(/^\d+%$/)
  })

  it('shows a dash when no quota is known', () => {
    const summary = dockSummary({ ...statusPayload(SNAPSHOT, { maxTitleChars: 90, maxSummaryChars: 600 }, 0), quota: [] })
    expect(summary.quotaText).toBe('—')
    expect(summary.quota).toBeNull()
  })
})

describe('isUnavailable', () => {
  it('narrows the offline payload', () => {
    expect(isUnavailable(null)).toBe(false)
    expect(isUnavailable({ available: false, reason: 'r', hint: 'h' })).toBe(true)
  })
})
