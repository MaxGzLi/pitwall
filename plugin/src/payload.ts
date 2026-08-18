/**
 * Pure projections from the daemon's wire shapes to what the model and the
 * dock actually need. Everything the model does not read is dropped here — a
 * raw snapshot is mostly ids, paths and epoch millis it pays for and ignores.
 */

import type {
  AgentRow,
  SessionEntry,
  SessionPair,
  SessionPayload,
  SessionsPayload,
  Snapshot,
  StatusAgent,
  StatusPayload,
  StatusQuota,
  SummaryRow,
} from './types.js'

export const RUNNING_STATES = new Set(['working', 'blocked'])

export interface TrimOptions {
  maxTitleChars: number
  maxSummaryChars: number
  maxAgents?: number
}

/** Truncate on a character budget, marking the cut so the model does not read a fragment as the whole. */
export function trimText(value: string | null | undefined, max: number): string | null {
  if (value === undefined || value === null) return null
  const normalized = value.replace(/\s+/g, ' ').trim()
  if (normalized === '') return null
  if (normalized.length <= max) return normalized
  return `${normalized.slice(0, Math.max(1, max - 1))}…`
}

function isoOf(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return ''
  return new Date(ms).toISOString()
}

function minutesBetween(from: number, to: number): number {
  if (!Number.isFinite(from) || !Number.isFinite(to)) return 0
  return Math.max(0, Math.round((to - from) / 60_000))
}

function round(value: number, digits: number): number {
  if (!Number.isFinite(value)) return 0
  const factor = 10 ** digits
  return Math.round(value * factor) / factor
}

/**
 * `/api/sessions` hands back `[[agent, summary|null], …]`. Anything else means
 * a daemon we do not understand, so drop the entry rather than guess.
 */
export function normalizeSessions(raw: unknown): SessionEntry[] {
  if (!Array.isArray(raw)) return []
  const entries: SessionEntry[] = []
  for (const pair of raw as unknown[]) {
    if (!Array.isArray(pair) || pair.length === 0) continue
    const [session, summary] = pair as SessionPair
    if (typeof session !== 'object' || session === null) continue
    if (typeof session.session_id !== 'string' || typeof session.harness !== 'string') continue
    entries.push({
      session,
      summary: typeof summary === 'object' && summary !== null ? summary : null,
    })
  }
  return entries
}

function statusAgent(agent: AgentRow, now: number, options: TrimOptions): StatusAgent {
  return {
    harness: agent.harness,
    sessionId: agent.session_id,
    project: agent.project,
    branch: agent.git_branch,
    title: trimText(agent.title, options.maxTitleChars),
    model: agent.model,
    state: agent.state,
    turns: agent.turns,
    tokens: agent.tok_total,
    costUsd: round(agent.cost_usd, 4),
    idleMinutes: minutesBetween(agent.last_activity_ms, now),
    pane: agent.pane_id,
  }
}

function statusQuota(row: Snapshot['quota'][number], now: number): StatusQuota {
  return {
    provider: row.provider,
    window: row.window,
    usedPercent: row.used_percent === null ? null : round(row.used_percent, 1),
    plan: row.plan,
    balance: row.balance === null ? null : round(row.balance, 2),
    currency: row.currency,
    resetsInMinutes: row.resets_at_ms === null ? null : minutesBetween(now, row.resets_at_ms),
  }
}

/** The `monitor_status` value: "how many agents are running right now, and how much quota is left". */
export function statusPayload(snapshot: Snapshot, options: TrimOptions, now: number): StatusPayload {
  const maxAgents = options.maxAgents ?? 12
  const byState: Record<string, number> = {}
  for (const agent of snapshot.agents) {
    byState[agent.state] = (byState[agent.state] ?? 0) + 1
  }
  const agents = [...snapshot.agents]
    .sort((a, b) => b.last_activity_ms - a.last_activity_ms)
    .slice(0, maxAgents)
    .map(agent => statusAgent(agent, now, options))
  const todayCostUsd = snapshot.today.reduce((sum, day) => sum + day.cost_usd, 0)
  const todayTokens = snapshot.today.reduce(
    (sum, day) => sum + day.tok_input + day.tok_output + day.tok_cache_read + day.tok_cache_create,
    0,
  )
  return {
    available: true,
    generatedAt: isoOf(snapshot.generated_at_ms),
    day: snapshot.day,
    live: snapshot.agents.length,
    running: snapshot.agents.filter(agent => RUNNING_STATES.has(agent.state)).length,
    byState,
    agents,
    quota: snapshot.quota
      .filter(row => row.used_percent !== null || row.balance !== null)
      .map(row => statusQuota(row, now)),
    todayCostUsd: round(todayCostUsd, 4),
    todayTokens,
  }
}

function summaryPayload(summary: SummaryRow, maxSummaryChars: number): SessionPayload['summary'] {
  return {
    headline: trimText(summary.headline, 200) ?? '',
    body: trimText(summary.body, maxSummaryChars),
    model: summary.model,
    status: summary.status,
    createdAt: isoOf(summary.created_at_ms),
  }
}

export function sessionPayload(entry: SessionEntry, options: TrimOptions): SessionPayload {
  const { session, summary } = entry
  return {
    harness: session.harness,
    sessionId: session.session_id,
    project: session.project,
    branch: session.git_branch,
    title: trimText(session.title, options.maxTitleChars),
    model: session.model,
    state: session.state,
    turns: session.turns,
    tokens: session.tok_total,
    costUsd: round(session.cost_usd, 4),
    startedAt: isoOf(session.started_at_ms),
    lastActivity: isoOf(session.last_activity_ms),
    durationMinutes: minutesBetween(session.started_at_ms, session.last_activity_ms),
    summary: summary === null ? null : summaryPayload(summary, options.maxSummaryChars),
  }
}

/** The `monitor_sessions` value: recent work plus whatever the daemon already summarised. */
export function sessionsPayload(
  entries: SessionEntry[],
  options: TrimOptions & { sinceHours: number; harness?: string },
): SessionsPayload {
  const filtered = options.harness === undefined
    ? entries
    : entries.filter(entry => entry.session.harness === options.harness)
  const sessions = filtered.map(entry => sessionPayload(entry, options))
  return {
    available: true,
    count: sessions.length,
    withSummary: sessions.filter(session => session.summary !== null).length,
    sinceHours: options.sinceHours,
    sessions,
  }
}
