export interface MonitorPanelSnapshot {
  expanded: boolean
  revision: number
}

const COLLAPSED: MonitorPanelSnapshot = Object.freeze({ expanded: false, revision: 0 })

/** Tiny external store shared by the sidebar button and the overlay dock. */
export class MonitorPanelController {
  private snapshot: MonitorPanelSnapshot = COLLAPSED
  private readonly listeners = new Set<() => void>()

  readonly getSnapshot = (): MonitorPanelSnapshot => this.snapshot

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  open(): void {
    if (this.snapshot.expanded) {
      this.refresh()
      return
    }
    this.publish({ expanded: true, revision: this.snapshot.revision + 1 })
  }

  close(): void {
    if (!this.snapshot.expanded) return
    this.publish({ ...this.snapshot, expanded: false })
  }

  toggle(): void {
    if (this.snapshot.expanded) this.close()
    else this.open()
  }

  refresh(): void {
    this.publish({ ...this.snapshot, revision: this.snapshot.revision + 1 })
  }

  private publish(snapshot: MonitorPanelSnapshot): void {
    this.snapshot = Object.freeze(snapshot)
    for (const listener of [...this.listeners]) listener()
  }
}
