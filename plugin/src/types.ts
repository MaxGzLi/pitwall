/**
 * Wire shapes served by agent-monitord, plus the trimmed shapes this plugin
 * hands to the model and the browser half.
 *
 * The daemon side of the contract is `daemon/src/model.rs` — snake_case, all
 * timestamps epoch milliseconds. Everything below is read-only to us.
 */

export const HARNESSES = ['claude', 'codex', 'dsh'] as const
export type Harness = typeof HARNESSES[number]

export const LIVE_STATES = ['working', 'blocked', 'waiting', 'idle', 'done', 'unknown'] as const

/** `Snapshot.agents[]` and the first element of each `/api/sessions` pair. */
export interface AgentRow {
  harness: string
  session_id: string
  project: string | null
  cwd: string | null
  git_branch: string | null
  title: string | null
  model: string | null
  state: string
  started_at_ms: number
  last_activity_ms: number
  ended_at_ms: number | null
  tok_total: number
  cost_usd: number
  turns: number
  pane_id: string | null
  herdr_status: string | null
}

export interface QuotaRow {
  provider: string
  window: string
  used_percent: number | null
  balance: number | null
  currency: string | null
  plan: string | null
  resets_at_ms: number | null
  sampled_at_ms: number
  source: string
}

export interface SummaryRow {
  harness: string
  session_id: string
  project: string | null
  headline: string
  body: string | null
  model: string
  created_at_ms: number
  status: string
}

export interface DayUsage {
  harness: string
  tok_input: number
  tok_output: number
  tok_cache_read: number
  tok_cache_create: number
  tok_reasoning: number
  cost_usd: number
}

export interface Snapshot {
  generated_at_ms: number
  day: string
  agents: AgentRow[]
  quota: QuotaRow[]
  today: DayUsage[]
  summaries: SummaryRow[]
}

/**
 * `GET /api/sessions` serialises `Vec<(AgentRow, Option<SummaryRow>)>`, i.e. a
 * JSON array of two-element arrays — not an array of objects.
 */
export type SessionPair = [AgentRow, SummaryRow | null]

export interface SessionEntry {
  session: AgentRow
  summary: SummaryRow | null
}

// -- trimmed, model- and browser-facing projections ----------------------

export interface StatusAgent {
  harness: string
  sessionId: string
  project: string | null
  branch: string | null
  title: string | null
  model: string | null
  state: string
  turns: number
  tokens: number
  costUsd: number
  idleMinutes: number
  pane: string | null
}

export interface StatusQuota {
  provider: string
  window: string
  usedPercent: number | null
  plan: string | null
  balance: number | null
  currency: string | null
  resetsInMinutes: number | null
}

export interface StatusPayload {
  available: true
  generatedAt: string
  day: string
  live: number
  running: number
  byState: Record<string, number>
  agents: StatusAgent[]
  quota: StatusQuota[]
  todayCostUsd: number
  todayTokens: number
}

export interface SessionSummaryPayload {
  headline: string
  body: string | null
  model: string
  status: string
  createdAt: string
}

export interface SessionPayload {
  harness: string
  sessionId: string
  project: string | null
  branch: string | null
  title: string | null
  model: string | null
  state: string
  turns: number
  tokens: number
  costUsd: number
  startedAt: string
  lastActivity: string
  durationMinutes: number
  summary: SessionSummaryPayload | null
}

export interface SessionsPayload {
  available: true
  count: number
  withSummary: number
  sinceHours: number
  sessions: SessionPayload[]
}

/** What every daemon-backed call degrades to when agent-monitord is not up. */
export interface UnavailablePayload {
  available: false
  reason: string
  hint: string
}
