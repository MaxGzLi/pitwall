'use strict';

const $ = (id) => document.getElementById(id);

/* Row heights must match strip.css; they decide how many rows each card can
   hold at the size the user dragged the window to. `gap` is the .rows gap. */
const GAP = 3;
const ROW = { agent: 27, quota: 26, today: 22 };
const CAP = { agent: 24, quota: 10 };            // sanity ceiling, not a layout limit

const HOUR_MS = 3600 * 1000;
/* Two days of buckets: 24 to draw, and the same hours a day earlier to say
   whether the hour in progress is a heavy one. */
const USAGE_HOURS = 48;
const USAGE_SHOWN = 24;

let snap = null;
let sse = null;
let pollTimer = null;
let retryTimer = null;
let backoff = 1000;
let relayout = 0;
let usage = null;

// --- formatting -------------------------------------------------------

function fmtAge(ms) {
  if (!isFinite(ms) || ms < 0) ms = 0;
  const s = Math.floor(ms / 1000);
  if (s < 60) return s + 's';
  const m = Math.floor(s / 60);
  if (m < 60) return m + 'm';
  const h = Math.floor(m / 60);
  if (h < 24) return h + 'h' + String(m % 60).padStart(2, '0');
  return Math.floor(h / 24) + 'd' + String(h % 24).padStart(2, '0');
}

function fmtUntil(ms) {
  if (!isFinite(ms)) return '—';
  if (ms <= 0) return 'now';
  const m = Math.floor(ms / 60000);
  if (m < 60) return m + 'm';
  const h = Math.floor(m / 60);
  if (h < 24) return h + 'h' + String(m % 60).padStart(2, '0');
  return Math.floor(h / 24) + 'd';
}

/** The reset column always reads forward. A bare `3h52` sitting one column away
    from an age got read as "3h52 ago" -- twice, by the person it was built for. */
function fmtReset(ms) {
  const t = fmtUntil(ms);
  return t === 'now' || t === '—' ? t : t + ' 后';
}

function fmtTok(n) {
  n = n || 0;
  if (n < 1000) return String(n);
  if (n < 1e6) return (n / 1000).toFixed(1) + 'k';
  return (n / 1e6).toFixed(1) + 'M';
}

function fmtUsd(x) {
  x = x || 0;
  if (x >= 100) return '$' + x.toFixed(0);
  if (x >= 1) return '$' + x.toFixed(2);
  return '$' + x.toFixed(3);
}

const PROVIDERS = { anthropic: 'Claude', openai: 'Codex', deepseek: 'DeepSeek', google: 'Gemini' };

function quotaLabel(q) {
  const p = PROVIDERS[q.provider] || q.provider;
  // windows arrive as '5h' | '7d' | 'weekly' | 'weekly:<model>'
  const w = q.window === 'balance' ? '' : String(q.window).replace(/^weekly:/, 'wk·');
  return (p + ' ' + w).trim();
}

/** A quota source older than this is reported as an age, not as live data. */
const STALE_MS = 6 * 3600 * 1000;

/**
 * Last cell of a quota row: when the window next resets, and nothing else.
 *
 * A reset time is wall clock. It does not stop being true because nobody has
 * re-read the source, so a row can be hours stale and still know exactly when
 * it rolls over. It stops being true once it has passed — by then the window
 * has rolled into one this machine has never seen — and only then does the
 * cell give up and say so. That the row is stale at all is the row's own job
 * (`q-stale`), not this column's: one column, one meaning.
 */
function resetCell(q, fallback) {
  const now = Date.now();
  if (q.resets_at_ms && q.resets_at_ms > now) {
    const cell = el('span', 'q-reset');
    cell.dataset.until = q.resets_at_ms;
    cell.textContent = fmtReset(q.resets_at_ms - now);
    return cell;
  }
  const age = q.sampled_at_ms ? now - q.sampled_at_ms : 0;
  if (age > STALE_MS) {
    const cell = el('span', 'q-reset stale', '未更新');
    cell.title = '数据源 ' + fmtAge(age) + ' 未更新：' + (q.source || '');
    return cell;
  }
  const cell = el('span', 'q-reset');
  if (fallback !== undefined) cell.textContent = fallback;
  return cell;
}

/** Which quota rows earn the first slots when the card is short. */
function quotaRank(q) {
  const k = q.provider + '/' + q.window;
  if (k === 'anthropic/5h') return 0;
  if (k === 'anthropic/7d') return 1;
  if (k === 'openai/weekly') return 2;
  if (q.window === 'balance') return 3;
  return 4;
}

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/** Working agents get three animated bars; every other state is one still dot,
    so motion in the column means something is actually running. */
function stateBadge(state) {
  const badge = el('span', 'st');
  const ind = el('span', 'ind' + (state === 'working' ? '' : ' dot'));
  for (let i = 0; i < (state === 'working' ? 3 : 1); i++) ind.appendChild(el('i'));
  badge.appendChild(ind);
  badge.appendChild(el('span', null, state));
  return badge;
}

/** How many `rowH`-tall rows fit in a card, measured, never assumed. */
function fits(box, rowH, cap) {
  const h = box.clientHeight;
  if (h <= 0) return 1;                       // pre-layout; a later pass corrects it
  return Math.max(1, Math.min(cap, Math.floor((h + GAP) / (rowH + GAP))));
}

// --- render -----------------------------------------------------------

function render() {
  if (!snap) return;
  renderAgents(snap.agents || []);
  renderQuota(snap.quota || []);
  renderToday(snap.today || []);
  tick();
}

/** The two states that need a human come first, then what just finished.
    The server ranks `done` below `working`, which on a busy day pushes the
    just-finished sessions out of view entirely. */
const STATE_RANK = { blocked: 0, done: 1, waiting: 2, working: 3 };

function renderAgents(all) {
  const box = $('agents');
  box.textContent = '';

  if (!all.length) {
    $('agents-more').textContent = '';
    box.appendChild(el('div', 'empty', 'no live agents'));
    return;
  }

  const n = fits(box, ROW.agent, CAP.agent);
  $('agents-more').textContent = all.length > n ? '+' + (all.length - n) + ' more' : '';

  const agents = all.slice().sort((a, b) =>
    (STATE_RANK[a.state] ?? 9) - (STATE_RANK[b.state] ?? 9) || b.last_activity_ms - a.last_activity_ms
  );

  for (const a of agents.slice(0, n)) {
    const state = a.state || 'unknown';
    const row = el('div', 'agent s-' + state);
    row.appendChild(stateBadge(state));
    row.appendChild(el('span', 'tag', a.harness || ''));
    row.appendChild(el('span', 'proj', a.project || '—'));
    // Subagents are folded into this row, so the row has to say so: without it
    // a session fanning out to a dozen helpers looks identical to one thinking
    // by itself, and its token number would jump for no visible reason.
    const t = el('span', 'ttl');
    if (a.kids > 0) t.appendChild(el('i', 'fan', a.kids + ' sub'));
    t.appendChild(document.createTextNode(a.title || ''));
    t.title = (a.kids > 0 ? a.kids + ' subagents running · ' : '') + (a.title || '');
    row.appendChild(t);
    const age = el('span', 'age');
    age.dataset.since = a.started_at_ms;
    row.appendChild(age);
    row.appendChild(el('span', 'tok', fmtTok(a.tok_total)));
    box.appendChild(row);
  }
}

function renderQuota(all) {
  const box = $('quota');
  box.textContent = '';
  if (!all.length) {
    $('quota-more').textContent = '';
    box.appendChild(el('div', 'empty', 'no quota data'));
    return;
  }

  const n = fits(box, ROW.quota, CAP.quota);
  $('quota-more').textContent = all.length > n ? '+' + (all.length - n) : '';

  const quota = all.slice().sort((a, b) =>
    quotaRank(a) - quotaRank(b) || (b.used_percent || 0) - (a.used_percent || 0)
  ).slice(0, n);

  for (const q of quota) {
    const row = el('div', 'q');
    // The reset column says when, not how old, so the row carries staleness.
    const age = q.sampled_at_ms ? Date.now() - q.sampled_at_ms : 0;
    if (age > STALE_MS) {
      row.classList.add('q-stale');
      row.title = '数据源 ' + fmtAge(age) + ' 未更新：' + (q.source || '');
    }
    const label = el('span', 'q-label', quotaLabel(q));
    label.title = q.provider + ' ' + q.window + (q.plan ? ' (' + q.plan + ')' : '');
    row.appendChild(label);

    if (q.used_percent !== null && q.used_percent !== undefined) {
      const pct = Math.max(0, Math.min(100, q.used_percent));
      const level = pct > 85 ? 'crit' : pct > 70 ? 'warn' : '';
      const bar = el('span', 'bar');
      const fill = el('span', 'fill' + (level ? ' ' + level : ''));
      fill.style.width = pct.toFixed(1) + '%';
      bar.appendChild(fill);
      row.appendChild(bar);
      // The bar fills as the quota is consumed. Saying so on every row costs a
      // few pixels and removes the only way to read the bar backwards.
      const p = el('span', 'q-pct' + (level ? ' ' + level : ''), Math.round(pct) + '%');
      p.appendChild(el('span', 'q-used', 'used'));
      row.appendChild(p);
      row.appendChild(resetCell(q));
    } else if (q.balance !== null && q.balance !== undefined) {
      row.appendChild(el('span', 'q-bal', q.balance.toFixed(2) + ' ' + (q.currency || '')));
      row.appendChild(resetCell(q, 'bal'));
    } else {
      row.appendChild(el('span', 'q-bal', '—'));
      row.appendChild(resetCell(q, ''));
    }
    box.appendChild(row);
  }
}

function renderToday(today) {
  const box = $('today');
  box.textContent = '';
  let cost = 0;
  let tokens = 0;

  if (!today.length) {
    box.appendChild(el('div', 'empty', 'nothing yet'));
  }
  for (const d of today) {
    cost += d.cost_usd || 0;
    const total = (d.tok_input || 0) + (d.tok_output || 0) + (d.tok_cache_read || 0) + (d.tok_cache_create || 0);
    tokens += total;
    const row = el('div', 't');
    row.appendChild(el('span', 't-h', d.harness));
    row.appendChild(el('span', 't-v', fmtTok(total)));
    box.appendChild(row);
  }

  const foot = el('div', 't-total');
  const row = el('div', 'row');
  row.appendChild(el('span', 'k', 'total'));
  row.appendChild(el('span', 'v', fmtTok(tokens)));
  foot.appendChild(row);
  const cst = el('div', 't-cost', '\u2248 ' + fmtUsd(cost));
  cst.title = 'priced from models.dev list rates — an estimate, not billed spend';
  foot.appendChild(cst);
  box.appendChild(foot);
}

// --- 24h spend --------------------------------------------------------

/**
 * What the day cost, hour by hour.
 *
 * Fetched on its own timer rather than carried in the snapshot: the snapshot
 * goes out whenever any agent so much as changes state, several times a
 * minute, and two days of buckets riding along with that would be a lot of
 * bytes to say nothing. An hour bucket that moves a sixtieth between polls
 * looks identical anyway.
 */
function loadUsage() {
  fetch('/api/usage/hourly?hours=' + USAGE_HOURS)
    .then((r) => (r.ok ? r.json() : Promise.reject(new Error('HTTP ' + r.status))))
    .then((d) => { usage = d.hours || []; renderUsage(); })
    .catch(() => { /* the dot already says whether the daemon is reachable */ });
}

function renderUsage() {
  if (!usage) return;
  const by = new Map(usage.map((r) => [r.hour_ms, r]));
  const now = Date.now();
  const cur = Math.floor(now / HOUR_MS) * HOUR_MS;

  // Walk the clock, not the rows. The query only returns hours that saw spend,
  // so a machine that slept comes back with holes -- and a hole is exactly what
  // the curve should show, rather than closing up and shifting everything left.
  const slots = [];
  for (let i = USAGE_SHOWN - 1; i >= 0; i--) {
    const h = cur - i * HOUR_MS;
    const r = by.get(h);
    slots.push({ h, cost: r ? r.cost_usd : 0, tok: r ? r.tokens : 0 });
  }

  const peak = slots.reduce((m, s) => Math.max(m, s.cost), 0);
  const total = slots.reduce((a, s) => a + s.cost, 0);
  const head = $('u-total');
  head.textContent = fmtUsd(total);
  head.title = '过去 24 小时估算花费 — 按 models.dev 挂牌价折算，不是账单';

  const bars = $('u-bars');
  const axis = $('u-axis');
  bars.textContent = '';
  axis.textContent = '';
  for (const s of slots) {
    const hour = new Date(s.h).getHours();
    const bar = el('div', 'u-bar' + (s.h === cur ? ' now' : ''));
    const fill = el('i');
    // A floor of two percent: against a $177 peak, a $1 hour is half a pixel,
    // and an hour that spent something must not look like one that spent nothing.
    if (s.cost > 0) fill.style.height = Math.max(2, (s.cost / peak) * 100) + '%';
    bar.appendChild(fill);
    bar.title = String(hour).padStart(2, '0') + ':00  ' + fmtUsd(s.cost) +
                (s.tok ? '  ' + fmtTok(s.tok) + ' tok' : '');
    bars.appendChild(bar);
    // Every sixth hour on the clock, not every sixth bar, so the labels stay
    // where they are as the window rolls forward instead of marching left.
    axis.appendChild(el('span', null, hour % 6 === 0 ? String(hour).padStart(2, '0') : ''));
  }

  renderUsageFoot(slots[slots.length - 1], by.get(cur - 24 * HOUR_MS), (now - cur) / HOUR_MS);
}

/**
 * The hour in progress, against the same hour yesterday.
 *
 * Yesterday's hour is finished and this one is twenty minutes old, so the two
 * numbers side by side are not a comparison. Scaling yesterday by how much of
 * the hour has gone is the honest version: at :20 it asks whether this hour is
 * ahead of where yesterday stood at :20. The tooltip says so, because the row
 * has no room to. Early in the hour it says nothing at all -- at :02 the scaled
 * figure is small enough that any ratio against it is noise.
 */
function renderUsageFoot(cur, prev, elapsed) {
  const foot = $('u-foot');
  foot.textContent = '';
  foot.appendChild(el('span', null, '本小时 ' + fmtUsd(cur.cost)));

  const yesterday = prev ? prev.cost_usd : 0;
  if (!yesterday) {
    foot.title = '昨天这个钟点没有花费记录';
    foot.appendChild(el('span', 'u-vs', '昨日同时段 —'));
    return;
  }
  const scaled = yesterday * elapsed;
  foot.title = '昨日同一小时整点共 ' + fmtUsd(yesterday) + '，本小时已过 ' +
               Math.round(elapsed * 60) + ' 分钟，按同样进度应为 ' + fmtUsd(scaled);
  foot.appendChild(el('span', 'u-vs', '昨日同时段 ' + fmtUsd(yesterday)));
  if (elapsed < 0.1) return;

  const d = Math.round((cur.cost / scaled - 1) * 100);
  const cls = d > 0 ? 'u-d up' : d < 0 ? 'u-d down' : 'u-d';
  foot.appendChild(el('span', cls, (d > 0 ? '\u2191' : d < 0 ? '\u2193' : '') + Math.abs(d) + '%'));
}

/** Writes only when the text actually moved. Most of these read in minutes or
    hours, so a once-a-second pass changes almost nothing, and an unconditional
    assignment would dirty every one of them anyway. */
function retime(n, text) {
  if (n.textContent !== text) n.textContent = text;
}

/** Relative times move on their own clock, not on the server's. */
function tick() {
  const now = Date.now();
  for (const n of document.querySelectorAll('[data-since]')) {
    retime(n, fmtAge(now - Number(n.dataset.since)));
  }
  for (const n of document.querySelectorAll('[data-until]')) {
    retime(n, fmtReset(Number(n.dataset.until) - now));
  }
}

// --- transport --------------------------------------------------------

function setConn(kind, why) {
  const dot = $('conn');
  dot.className = 'conn ' + kind;
  dot.title = why;
}

function apply(data) {
  snap = data;
  render();
}

function startPolling() {
  if (pollTimer) return;
  const once = () => fetch('/api/snapshot')
    .then((r) => (r.ok ? r.json() : Promise.reject(new Error('HTTP ' + r.status))))
    .then((d) => { apply(d); if (!sse) setConn('poll', 'polling every 5s'); })
    .catch(() => setConn('down', 'daemon unreachable'));
  once();
  pollTimer = setInterval(once, 5000);
}

function stopPolling() {
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
}

function connect() {
  clearTimeout(retryTimer);
  try {
    sse = new EventSource('/api/stream');
  } catch (e) {
    sse = null;
    dropped();
    return;
  }
  sse.addEventListener('open', () => setConn('live', 'streaming'));
  sse.addEventListener('snapshot', (e) => {
    stopPolling();
    backoff = 1000;
    setConn('live', 'streaming');
    try { apply(JSON.parse(e.data)); } catch (_) { /* keep the last good frame */ }
  });
  sse.addEventListener('snapshot_error', (e) => setConn('poll', 'daemon: ' + e.data));
  sse.onerror = () => {
    if (sse) { sse.close(); sse = null; }
    dropped();
  };
}

function dropped() {
  setConn('poll', 'stream lost, polling');
  startPolling();
  backoff = Math.min(backoff * 2, 30000);
  clearTimeout(retryTimer);
  retryTimer = setTimeout(connect, backoff);
}

// --- boot -------------------------------------------------------------

/* The window is resizable, so row counts are only valid until the next drag.
   Coalesce to one re-render per frame — a resize drag fires continuously. */
new ResizeObserver(() => {
  cancelAnimationFrame(relayout);
  relayout = requestAnimationFrame(render);
}).observe(document.body);

setInterval(tick, 1000);
// Its own cadence: hour buckets do not move fast enough to ride the stream.
loadUsage();
setInterval(loadUsage, 60000);
startPolling();
connect();
