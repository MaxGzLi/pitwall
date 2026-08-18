import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import {
  normalizeSessions,
  sessionsPayload,
  statusPayload,
  trimText,
} from '../src/payload.js'
import type { SessionPair, Snapshot, SummaryRow } from '../src/types.js'

/**
 * Both fixtures were captured from a running agent-monitord
 * (`GET /api/snapshot`, `GET /api/sessions?limit=8&since_ms=0`) and then had
 * every value replaced with synthetic data. The *shapes* are the daemon's —
 * field set, null distribution, the short-id/UUID length split — which is what
 * these tests are about; the values are made up on purpose.
 */
function fixture<T>(name: string): T {
  return JSON.parse(readFileSync(new URL(`./fixtures/${name}`, import.meta.url), 'utf8')) as T
}

const SNAPSHOT = fixture<Snapshot>('snapshot.json')
const SESSIONS = fixture<SessionPair[]>('sessions.json')

const TRIM = { maxTitleChars: 90, maxSummaryChars: 600 }

describe('trimText', () => {
  it('collapses whitespace and returns null for empty input', () => {
    expect(trimText('  a\n\n  b  ', 40)).toBe('a b')
    expect(trimText('   ', 40)).toBeNull()
    expect(trimText(null, 40)).toBeNull()
    expect(trimText(undefined, 40)).toBeNull()
  })

  it('marks the cut so a fragment is not read as the whole', () => {
    expect(trimText('abcdefghij', 10)).toBe('abcdefghij')
    expect(trimText('abcdefghij', 5)).toBe('abcd…')
    expect(trimText('abcdefghij', 5)).toHaveLength(5)
  })
})

describe('normalizeSessions', () => {
  it('unpacks the real [agent, summary|null] pairs the daemon serves', () => {
    const entries = normalizeSessions(SESSIONS)
    expect(entries).toHaveLength(SESSIONS.length)
    expect(entries[0]!.session.session_id).toBe(SESSIONS[0]![0].session_id)
    expect(entries.every(entry => typeof entry.session.harness === 'string')).toBe(true)
  })

  it('carries a summary when the daemon has one', () => {
    const summary: SummaryRow = {
      harness: 'claude',
      session_id: 'abc',
      project: 'agent-monitor',
      headline: '实现 DSH 插件的宿主半边',
      body: '写了 RPC 通道与两个模型工具。',
      model: 'deepseek-v4-flash',
      created_at_ms: 1_787_036_900_000,
      status: 'ok',
    }
    const pair: SessionPair = [SESSIONS[0]![0], summary]
    expect(normalizeSessions([pair])[0]!.summary).toEqual(summary)
  })

  it('drops anything that is not a pair rather than guessing', () => {
    expect(normalizeSessions(null)).toEqual([])
    expect(normalizeSessions({ sessions: [] })).toEqual([])
    expect(normalizeSessions([[{ harness: 'claude' }, null]])).toEqual([])
    expect(normalizeSessions([[], ['nope'], [SESSIONS[0]![0], null]])).toHaveLength(1)
  })
})

describe('statusPayload', () => {
  const now = SNAPSHOT.generated_at_ms + 60_000
  const status = statusPayload(SNAPSHOT, TRIM, now)

  it('counts the live agents the daemon reported', () => {
    expect(status.available).toBe(true)
    expect(status.live).toBe(SNAPSHOT.agents.length)
    expect(status.running).toBe(SNAPSHOT.agents.filter(a => a.state === 'working' || a.state === 'blocked').length)
    expect(Object.values(status.byState).reduce((a, b) => a + b, 0)).toBe(SNAPSHOT.agents.length)
  })

  it('caps the agent list so a busy machine cannot blow up the prompt', () => {
    expect(SNAPSHOT.agents.length).toBeGreaterThan(12)
    expect(status.agents).toHaveLength(12)
    expect(statusPayload(SNAPSHOT, { ...TRIM, maxAgents: 3 }, now).agents).toHaveLength(3)
  })

  it('drops the snake_case keys the model does not need', () => {
    const first = status.agents[0]!
    expect(Object.keys(first).some(key => key.includes('_'))).toBe(false)
    expect(first).not.toHaveProperty('cwd')
    expect(first).not.toHaveProperty('started_at_ms')
  })

  it('trims long titles', () => {
    const long = statusPayload(SNAPSHOT, { ...TRIM, maxTitleChars: 20 }, now)
    for (const agent of long.agents) {
      if (agent.title !== null) expect(agent.title.length).toBeLessThanOrEqual(20)
    }
  })

  it('keeps only quota rows that carry a number, and rounds them', () => {
    expect(status.quota.length).toBeGreaterThan(0)
    for (const row of status.quota) {
      expect(row.usedPercent === null ? row.balance : row.usedPercent).not.toBeNull()
    }
    expect(status.quota.map(row => `${row.provider} ${row.window}`))
      .toEqual(SNAPSHOT.quota.filter(q => q.used_percent !== null || q.balance !== null)
        .map(q => `${q.provider} ${q.window}`))
  })

  it('sums today usage across harnesses', () => {
    const expected = SNAPSHOT.today.reduce((sum, day) => sum + day.cost_usd, 0)
    expect(status.todayCostUsd).toBeCloseTo(expected, 3)
    expect(status.todayTokens).toBeGreaterThan(0)
  })

  it('stays small enough to be cheap for the model', () => {
    expect(JSON.stringify(status).length).toBeLessThan(JSON.stringify(SNAPSHOT).length)
  })
})

describe('sessionsPayload', () => {
  const entries = normalizeSessions(SESSIONS)

  it('projects every real session and reports how many carry a summary', () => {
    const payload = sessionsPayload(entries, { ...TRIM, sinceHours: 24 })
    expect(payload.count).toBe(entries.length)
    expect(payload.withSummary).toBe(entries.filter(e => e.summary !== null).length)
    expect(payload.sinceHours).toBe(24)
    expect(payload.sessions[0]!.lastActivity).toMatch(/^\d{4}-\d{2}-\d{2}T/)
  })

  it('filters by harness', () => {
    const harnesses = new Set(entries.map(entry => entry.session.harness))
    expect(harnesses.size).toBeGreaterThan(1)
    const claude = sessionsPayload(entries, { ...TRIM, sinceHours: 24, harness: 'claude' })
    expect(claude.count).toBeGreaterThan(0)
    expect(claude.sessions.every(session => session.harness === 'claude')).toBe(true)
    expect(claude.count).toBeLessThan(entries.length)
  })

  it('truncates a long stored summary body to the configured budget', () => {
    const summary: SummaryRow = {
      harness: entries[0]!.session.harness,
      session_id: entries[0]!.session.session_id,
      project: null,
      headline: 'x'.repeat(400),
      body: 'y'.repeat(5_000),
      model: 'deepseek-v4-flash',
      created_at_ms: 1_787_036_900_000,
      status: 'ok',
    }
    const payload = sessionsPayload(
      [{ session: entries[0]!.session, summary }],
      { maxTitleChars: 90, maxSummaryChars: 120, sinceHours: 24 },
    )
    const projected = payload.sessions[0]!.summary!
    expect(projected.headline).toHaveLength(200)
    expect(projected.body).toHaveLength(120)
    expect(projected.body!.endsWith('…')).toBe(true)
    expect(projected.createdAt).toBe('2026-08-18T07:08:20.000Z')
  })

  it('computes duration from the daemon timestamps', () => {
    const payload = sessionsPayload(entries, { ...TRIM, sinceHours: 24 })
    for (const [index, session] of payload.sessions.entries()) {
      const raw = entries[index]!.session
      expect(session.durationMinutes)
        .toBe(Math.max(0, Math.round((raw.last_activity_ms - raw.started_at_ms) / 60_000)))
    }
  })
})
