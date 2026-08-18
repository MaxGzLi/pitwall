import { useEffect, useState, useSyncExternalStore } from 'react'
import { IconCloseOutline16, IconRefreshOutline16 } from '@deepseek-ai/dsh-client-ui-primitives'
import type { PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type {
  SessionPayload,
  SessionsPayload,
  StatusAgent,
  StatusPayload,
  UnavailablePayload,
} from '../types.js'
import {
  dockSummary,
  formatAge,
  formatCost,
  formatTokens,
  isUnavailable,
  quotaLabel,
  stateLabel,
} from './format.js'
import type { MonitorPanelController } from './panel-controller.js'
import type { MonitorRpcClient } from './rpc.js'

export type MonitorDockProps = PropsRuntime<'shell.overlay'> & {
  monitor: MonitorRpcClient
  panel: MonitorPanelController
}

const DEFAULT_POLL_MS = 5_000

function AgentRow({ agent }: { agent: StatusAgent }) {
  return (
    <div className="dsh-monitor-row">
      <div className="dsh-monitor-row-head">
        <span className="dsh-monitor-tag" data-state={agent.state}>{agent.harness}</span>
        <strong>{agent.title ?? agent.project ?? agent.sessionId.slice(0, 8)}</strong>
      </div>
      <div className="dsh-monitor-row-meta">
        <span>{stateLabel(agent.state)}</span>
        {agent.project !== null && <span>{agent.project}</span>}
        <span>{agent.turns} 轮</span>
        <span>{formatTokens(agent.tokens)} tok</span>
        <span>{formatCost(agent.costUsd)}</span>
        <span>{formatAge(agent.idleMinutes)}</span>
      </div>
    </div>
  )
}

function SessionRow({ session }: { session: SessionPayload }) {
  return (
    <div className="dsh-monitor-row">
      <div className="dsh-monitor-row-head">
        <span className="dsh-monitor-tag" data-state={session.state}>{session.harness}</span>
        <strong>{session.summary?.headline ?? session.title ?? session.sessionId.slice(0, 8)}</strong>
      </div>
      {session.summary?.body != null && <p>{session.summary.body}</p>}
      <div className="dsh-monitor-row-meta">
        {session.project !== null && <span>{session.project}</span>}
        <span>{session.turns} 轮</span>
        <span>{formatTokens(session.tokens)} tok</span>
        <span>{formatCost(session.costUsd)}</span>
        <span>{session.durationMinutes} 分钟</span>
        {session.summary === null && <span>无总结</span>}
      </div>
    </div>
  )
}

/** Compact always-on pill; clicking it expands the recent-session list underneath. */
export function MonitorDock({ monitor, panel }: MonitorDockProps) {
  const panelState = useSyncExternalStore(panel.subscribe, panel.getSnapshot)
  const [pollMs, setPollMs] = useState(DEFAULT_POLL_MS)
  const [status, setStatus] = useState<StatusPayload | UnavailablePayload | null>(null)
  const [sessions, setSessions] = useState<SessionsPayload | UnavailablePayload | null>(null)
  const [error, setError] = useState<string | null>(null)

  // Client plugins are created without config, so the cadence comes over RPC.
  useEffect(() => {
    const controller = new AbortController()
    void monitor.config(controller.signal).then((result) => {
      if (controller.signal.aborted || !result.ok) return
      setPollMs(result.value.pollIntervalMs)
    })
    return () => { controller.abort() }
  }, [monitor])

  useEffect(() => {
    const controller = new AbortController()
    const tick = async (): Promise<void> => {
      const result = await monitor.snapshot(controller.signal)
      if (controller.signal.aborted) return
      if (result.ok) {
        setStatus(result.value)
        setError(null)
      } else {
        setError(result.error)
      }
    }
    void tick()
    const timer = setInterval(() => { void tick() }, pollMs)
    return () => {
      controller.abort()
      clearInterval(timer)
    }
  }, [monitor, pollMs, panelState.revision])

  useEffect(() => {
    if (!panelState.expanded) return undefined
    const controller = new AbortController()
    const tick = async (): Promise<void> => {
      const result = await monitor.sessions({ limit: 12, sinceHours: 24 }, controller.signal)
      if (controller.signal.aborted) return
      if (result.ok) setSessions(result.value)
      else setError(result.error)
    }
    void tick()
    const timer = setInterval(() => { void tick() }, pollMs * 3)
    return () => {
      controller.abort()
      clearInterval(timer)
    }
  }, [monitor, pollMs, panelState.expanded, panelState.revision])

  const live = status !== null && status.available ? status : null
  const summary = live === null ? null : dockSummary(live)
  const offline = isUnavailable(status)
  const sessionList = sessions !== null && sessions.available ? sessions.sessions : []

  return (
    <div className="dsh-monitor-dock" aria-label="本机 Agent 监控">
      {panelState.expanded && (
        <section className="dsh-monitor-panel" aria-label="最近的 Agent 会话">
          <header>
            <div>
              <strong>本机 Agent</strong>
              <span>
                {offline
                  ? '监控守护进程未运行'
                  : `${summary?.running ?? 0} 个在跑 / ${summary?.live ?? 0} 个活跃 · 今日 ${formatCost(live?.todayCostUsd ?? 0)}`}
              </span>
            </div>
            <button type="button" aria-label="刷新" onClick={() => { panel.refresh() }}>
              <IconRefreshOutline16 />
            </button>
            <button type="button" aria-label="收起" onClick={() => { panel.close() }}>
              <IconCloseOutline16 />
            </button>
          </header>
          <div className="dsh-monitor-scroll">
            {offline && (
              <p className="dsh-monitor-empty">
                {(status as UnavailablePayload).reason}
                <br />
                {(status as UnavailablePayload).hint}
              </p>
            )}
            {live !== null && live.agents.length > 0 && (
              <>
                <p className="dsh-monitor-section-title">LIVE</p>
                {live.agents.map(agent => (
                  <AgentRow key={`${agent.harness}:${agent.sessionId}`} agent={agent} />
                ))}
              </>
            )}
            {live !== null && live.quota.length > 0 && (
              <>
                <p className="dsh-monitor-section-title">QUOTA</p>
                <div className="dsh-monitor-row">
                  <div className="dsh-monitor-row-meta">
                    {live.quota.map(row => (
                      <span key={`${row.provider}:${row.window}`}>
                        {quotaLabel(row)} {row.usedPercent === null ? '—' : `${Math.round(row.usedPercent)}%`}
                      </span>
                    ))}
                  </div>
                </div>
              </>
            )}
            <p className="dsh-monitor-section-title">RECENT · 24H</p>
            {sessionList.length === 0
              ? <p className="dsh-monitor-empty">最近 24 小时没有记录到会话。</p>
              : sessionList.map(session => (
                <SessionRow key={`${session.harness}:${session.sessionId}`} session={session} />
              ))}
            {error !== null && <p className="dsh-monitor-error">{error}</p>}
          </div>
        </section>
      )}

      <button
        type="button"
        className="dsh-monitor-pill"
        data-offline={offline}
        aria-expanded={panelState.expanded}
        aria-label="本机 Agent 监控"
        onClick={() => { panel.toggle() }}
      >
        <span
          className="dsh-monitor-beacon"
          data-live={(summary?.running ?? 0) > 0}
          data-blocked={(live?.byState.blocked ?? 0) > 0}
        />
        <span className="dsh-monitor-pill-count">
          <strong>{offline ? '—' : summary?.running ?? 0}</strong>
          在跑
        </span>
        {summary?.quota != null && (
          <span className="dsh-monitor-pill-quota" data-hot={(summary.quota.usedPercent ?? 0) >= 80}>
            <strong>{summary.quotaText}</strong>
            {quotaLabel(summary.quota)}
          </span>
        )}
      </button>
    </div>
  )
}
