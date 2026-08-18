import type { ClientContext } from '@deepseek-ai/dsh-client-runtime/client'
import type { ConnectionHandle } from '@deepseek-ai/dsh-client-connection/client'
import type {} from '@deepseek-ai/dsh-client-ui-layout/client'
import type {} from '@deepseek-ai/dsh-client-ui-sidebar/client'
import { MonitorDock } from './MonitorDock.js'
import { MonitorSidebarAction } from './MonitorSidebarAction.js'
import { MonitorPanelController } from './panel-controller.js'
import { MonitorRpcClient } from './rpc.js'
import { installMonitorStyles } from './styles.js'

type MonitorClientContext = ClientContext & { connection: ConnectionHandle }

export const inject = ['slots', 'connection']

/** Browser half: one overlay dock and one sidebar launcher, both fed by host RPC. */
export function apply(ctx: MonitorClientContext): void {
  const monitor = new MonitorRpcClient(ctx.connection.rpc)
  const panel = new MonitorPanelController()
  ctx.effect(installMonitorStyles, 'dsh-monitor: client styles')

  ctx.slots.inject('sidebar.footer.action', () => ctx.slots.register({
    name: 'sidebar.footer.action',
    id: 'dsh-monitor',
    order: 30,
    inject: () => ({ openMonitor: () => { panel.open() } }),
  }, MonitorSidebarAction))

  ctx.slots.inject('shell.overlay', () => ctx.slots.register({
    name: 'shell.overlay',
    id: 'dsh-monitor',
    order: 30,
    inject: () => ({ monitor, panel }),
  }, MonitorDock))
}
