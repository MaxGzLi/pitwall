import { IconGoalOutline16, Tooltip } from '@deepseek-ai/dsh-client-ui-primitives'
import type { PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'

export type MonitorSidebarActionProps = PropsRuntime<'sidebar.footer.action'> & {
  openMonitor: () => void
}

export function MonitorSidebarAction({ wide, openMonitor }: MonitorSidebarActionProps) {
  const label = '打开 Agent 监控'
  return (
    <Tooltip label={label} side="right">
      <button
        type="button"
        className="dsh-monitor-sidebar-action"
        aria-label={label}
        onClick={openMonitor}
      >
        <IconGoalOutline16 />
        {wide && <span>Agent 监控</span>}
      </button>
    </Tooltip>
  )
}
