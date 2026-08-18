const STYLE_ID = '@maxgzli/dsh-monitor/client'

// Bottom-right, lifted clear of the 22px dock that @maxgzli/dsh-brain owns, so
// both plugins can be installed in the same profile without overlapping.
const CSS = String.raw`
.dsh-monitor-sidebar-action { display:flex; align-items:center; justify-content:center; gap:8px; min-width:32px; height:32px; padding:0 8px; border:0; border-radius:8px; color:var(--dsw-alias-text-secondary, #5f6368); background:transparent; cursor:pointer; font:inherit; white-space:nowrap; }
.dsh-monitor-sidebar-action:hover { color:var(--dsw-alias-text-primary, #202124); background:var(--dsw-alias-button-ghost-hover, rgba(0,0,0,.06)); }

.dsh-monitor-dock { position:fixed; right:22px; bottom:80px; z-index:59; display:flex; flex-direction:column; align-items:flex-end; gap:10px; pointer-events:auto; color:var(--dsw-alias-text-primary, #202124); font:inherit; }
.dsh-monitor-pill { display:flex; align-items:center; gap:10px; height:38px; padding:0 12px; border:1px solid rgba(23,24,22,.14); border-radius:19px; background:color-mix(in srgb, var(--dsw-alias-bg-primary, #fff) 94%, transparent); box-shadow:0 14px 36px rgba(15,23,42,.16); backdrop-filter:blur(18px); cursor:pointer; font:inherit; }
.dsh-monitor-pill:hover { border-color:rgba(23,24,22,.26); }
.dsh-monitor-pill[data-offline="true"] { border-color:#e2c3c3; }
.dsh-monitor-beacon { width:8px; height:8px; border-radius:50%; background:#9aa0a6; }
.dsh-monitor-beacon[data-live="true"] { background:#3f9d54; box-shadow:0 0 0 3px rgba(63,157,84,.16); }
.dsh-monitor-beacon[data-blocked="true"] { background:#d08a1e; box-shadow:0 0 0 3px rgba(208,138,30,.16); }
.dsh-monitor-pill-count { display:flex; align-items:baseline; gap:4px; font-size:11px; color:var(--dsw-alias-text-secondary, #656960); }
.dsh-monitor-pill-count strong { color:var(--dsw-alias-text-primary, #202124); font-size:14px; font-weight:700; }
.dsh-monitor-pill-quota { display:flex; align-items:baseline; gap:4px; padding-left:10px; border-left:1px solid var(--dsw-alias-border-l3, #e4e5df); font-size:11px; color:var(--dsw-alias-text-secondary, #656960); }
.dsh-monitor-pill-quota strong { color:var(--dsw-alias-text-primary, #202124); font-size:13px; font-weight:700; }
.dsh-monitor-pill-quota[data-hot="true"] strong { color:#c0392b; }

.dsh-monitor-panel { width:min(420px, calc(100vw - 28px)); max-height:min(62vh, 560px); display:flex; flex-direction:column; overflow:hidden; border:1px solid rgba(23,24,22,.14); border-radius:16px; background:var(--dsw-alias-bg-primary, #fff); box-shadow:0 22px 64px rgba(15,23,42,.2); }
.dsh-monitor-panel > header { flex:0 0 auto; display:flex; align-items:flex-start; justify-content:space-between; gap:12px; padding:14px 16px 10px; border-bottom:1px solid var(--dsw-alias-border-l3, #e4e5df); }
.dsh-monitor-panel > header > div { min-width:0; display:flex; flex-direction:column; gap:3px; }
.dsh-monitor-panel > header strong { font-size:13px; }
.dsh-monitor-panel > header span { color:var(--dsw-alias-text-secondary, #6b7280); font-size:10px; }
.dsh-monitor-panel > header button { display:flex; align-items:center; justify-content:center; width:26px; height:26px; border:0; border-radius:8px; color:inherit; background:transparent; cursor:pointer; }
.dsh-monitor-panel > header button:hover { background:var(--dsw-alias-button-ghost-hover, rgba(0,0,0,.06)); }
.dsh-monitor-scroll { min-height:0; flex:1 1 auto; overflow-y:auto; padding:10px 12px 14px; }
.dsh-monitor-section-title { margin:6px 4px 6px; color:#768064; font-family:ui-monospace, SFMono-Regular, Menlo, monospace; font-size:9px; font-weight:700; letter-spacing:.16em; }

.dsh-monitor-row { display:flex; flex-direction:column; gap:4px; padding:9px 10px; border-radius:11px; }
.dsh-monitor-row + .dsh-monitor-row { margin-top:4px; }
.dsh-monitor-row:hover { background:var(--dsw-alias-bg-secondary, #f5f6f2); }
.dsh-monitor-row-head { display:flex; align-items:center; gap:7px; }
.dsh-monitor-row-head strong { min-width:0; overflow:hidden; font-size:12px; font-weight:650; text-overflow:ellipsis; white-space:nowrap; }
.dsh-monitor-tag { flex:0 0 auto; padding:1px 6px; border-radius:6px; background:var(--dsw-alias-bg-secondary, #eef0ec); color:var(--dsw-alias-text-secondary, #5f6a58); font-family:ui-monospace, SFMono-Regular, Menlo, monospace; font-size:9px; text-transform:uppercase; }
.dsh-monitor-tag[data-state="working"] { background:#e5f3e8; color:#2f6f40; }
.dsh-monitor-tag[data-state="blocked"] { background:#fbf0dd; color:#8a5a10; }
.dsh-monitor-row p { margin:0; overflow:hidden; color:var(--dsw-alias-text-secondary, #6b7066); font-size:11px; line-height:1.5; display:-webkit-box; -webkit-box-orient:vertical; -webkit-line-clamp:3; }
.dsh-monitor-row-meta { display:flex; flex-wrap:wrap; gap:4px 10px; color:var(--dsw-alias-text-secondary, #858980); font-size:9px; }
.dsh-monitor-empty { margin:18px 6px; color:var(--dsw-alias-text-secondary, #858980); font-size:11px; line-height:1.6; text-align:center; }
.dsh-monitor-error { margin:10px 4px 0; padding:9px 11px; border-radius:10px; background:#fdf2f2; color:#8a3232; font-size:10px; line-height:1.55; }

@media (max-width: 640px) {
  .dsh-monitor-dock { right:10px; bottom:66px; }
  .dsh-monitor-pill-quota { display:none; }
}
`

export function installMonitorStyles(): () => void {
  const existing = document.querySelector<HTMLStyleElement>(`style[data-plugin-css="${STYLE_ID}"]`)
  if (existing !== null) return () => undefined
  const style = document.createElement('style')
  style.dataset.plugin = '@maxgzli/dsh-monitor'
  style.dataset.pluginCss = STYLE_ID
  style.textContent = CSS
  document.head.append(style)
  return () => { style.remove() }
}
