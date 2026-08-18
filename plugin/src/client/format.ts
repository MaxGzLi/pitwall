/** Pure display helpers for the dock. No DOM, no React — safe to unit test. */

import type { StatusPayload, StatusQuota, UnavailablePayload } from '../types.js'

export const STATE_LABELS: Record<string, string> = {
  working: '运行中',
  blocked: '等待输入',
  waiting: '等待中',
  idle: '空闲',
  done: '已完成',
  ended: '已结束',
  unknown: '未知',
}

export function stateLabel(state: string): string {
  return STATE_LABELS[state] ?? state
}

export function formatTokens(total: number): string {
  if (!Number.isFinite(total) || total <= 0) return '0'
  if (total < 1_000) return String(Math.round(total))
  if (total < 1_000_000) return `${(total / 1_000).toFixed(1)}k`
  return `${(total / 1_000_000).toFixed(2)}M`
}

export function formatCost(usd: number): string {
  if (!Number.isFinite(usd) || usd <= 0) return '$0'
  if (usd < 0.01) return '<$0.01'
  return `$${usd.toFixed(2)}`
}

export function formatAge(minutes: number): string {
  if (!Number.isFinite(minutes) || minutes <= 0) return '刚刚'
  if (minutes < 60) return `${Math.round(minutes)} 分钟前`
  const hours = minutes / 60
  if (hours < 24) return `${hours.toFixed(hours < 10 ? 1 : 0)} 小时前`
  return `${Math.round(hours / 24)} 天前`
}

export function quotaLabel(quota: StatusQuota): string {
  return `${quota.provider} ${quota.window}`
}

/** The single quota row worth putting on a 200px pill: the one closest to exhaustion. */
export function peakQuota(quota: StatusQuota[]): StatusQuota | null {
  let peak: StatusQuota | null = null
  for (const row of quota) {
    if (row.usedPercent === null) continue
    if (peak === null || row.usedPercent > (peak.usedPercent ?? -1)) peak = row
  }
  return peak
}

export interface DockSummary {
  running: number
  live: number
  quota: StatusQuota | null
  quotaText: string
}

export function dockSummary(status: StatusPayload): DockSummary {
  const quota = peakQuota(status.quota)
  return {
    running: status.running,
    live: status.live,
    quota,
    quotaText: quota === null || quota.usedPercent === null
      ? '—'
      : `${Math.round(quota.usedPercent)}%`,
  }
}

export function isUnavailable(
  value: StatusPayload | UnavailablePayload | null,
): value is UnavailablePayload {
  return value !== null && value.available === false
}
