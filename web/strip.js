'use strict';

const $ = (id) => document.getElementById(id);

/* Row heights must match strip.css; they decide how many rows each card can
   hold at the size the user dragged the window to. `gap` is the .rows gap. */
const GAP = 3;
const ROW = { agent: 27, quota: 26, today: 22, sum: 34, sumRich: 66 };
const CAP = { agent: 24, quota: 10, sum: 12 };   // sanity ceiling, not a layout limit

let snap = null;
let sse = null;
let pollTimer = null;
let retryTimer = null;
let backoff = 1000;
let relayout = 0;
let sumSig = '';

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
 * Last cell of a quota row: normally the reset countdown, but a row whose
 * source has not refreshed in hours shows how old it is instead — a countdown
 * derived from a stale sample counts down to nothing.
 */
function resetCell(q, fallback) {
  const age = q.sampled_at_ms ? Date.now() - q.sampled_at_ms : 0;
  if (age > STALE_MS) {
    const cell = el('span', 'q-reset stale');
    cell.dataset.stale = q.sampled_at_ms;
    cell.textContent = fmtAge(age) + ' 前';
    cell.title = '数据源 ' + fmtAge(age) + ' 未更新：' + (q.source || '');
    return cell;
  }
  const cell = el('span', 'q-reset');
  if (q.resets_at_ms) cell.dataset.until = q.resets_at_ms;
  else if (fallback !== undefined) cell.textContent = fallback;
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

// --- summary text -----------------------------------------------------

/** The summariser writes `标签：一句话` per line. Older rows are free-form
    bullets, so an unlabelled line is kept as a plain paragraph. */
function fields(body) {
  const out = [];
  for (let line of String(body || '').split('\n')) {
    line = line.trim().replace(/^[-*•]\s*/, '');
    if (!line) continue;
    const m = line.match(/^([^：:]{2,8})[：:]\s*(.+)$/);
    out.push(m ? { k: m[1], v: m[2] } : { k: null, v: line });
  }
  return out;
}

const FIELD_CLASS = { '待办': 'f-todo', '问题': 'f-issue', '结果': 'f-done' };

function isNothing(v) { return /^(无|none|n\/a)[。.\s]*$/i.test(v); }

/** One line for the card: what it did, plus the open item if there is one. */
function preview(body) {
  const fs = fields(body);
  if (!fs.length) return '';
  const did = fs.find((f) => f.k && f.k.indexOf('做') === 0) || fs[0];
  const todo = fs.find((f) => f.k === '待办' || f.k === '问题');
  const parts = [did.v];
  if (todo && todo !== did && !isNothing(todo.v)) parts.push(todo.k + '：' + todo.v);
  return parts.join('  ·  ');
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
  renderSummaries(snap.summaries || []);
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
    // Strip mode drops the reset column, so the row itself has to carry the
    // staleness signal there.
    if (q.sampled_at_ms && Date.now() - q.sampled_at_ms > STALE_MS) row.classList.add('q-stale');
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

/** One summary, as a clickable row. Shared by the card and the full list. */
function summaryRow(s, rich) {
  const item = el('div', 'sum');
  const meta = el('div', 'meta');
  meta.appendChild(el('span', null, s.project || s.harness));
  const ago = el('span');
  ago.dataset.since = s.created_at_ms;
  meta.appendChild(ago);
  item.appendChild(meta);
  const head = el('div', 'head', s.headline);
  head.title = s.headline;
  item.appendChild(head);
  if (rich && s.body) {
    const p = preview(s.body);
    if (p) item.appendChild(el('div', 'body', p));
  }
  item.addEventListener('click', () => openOverlay(s));
  return item;
}

function renderSummaries(summaries) {
  const box = $('summaries');
  // Show the summary body too, but only if doing so still leaves room for two.
  // Measured before the rebuild: the card's height is set by the layout, not by
  // what is currently inside it.
  const rich = fits(box, ROW.sumRich, CAP.sum) >= 2;

  // Now that the card scrolls, rebuilding it on every snapshot would throw the
  // reader back to the top -- and snapshots arrive whenever any agent so much as
  // changes state. Most of them do not touch this list, so leave it alone.
  // Ages stay fresh regardless: tick() rewrites them in place.
  const sig = rich + '|' + summaries.map((s) => s.harness + s.session_id + s.created_at_ms).join(',');
  if (sig === sumSig) return;
  sumSig = sig;

  box.textContent = '';
  box.classList.toggle('rich', rich);
  if (!summaries.length) {
    box.appendChild(el('div', 'empty', 'no recent summaries'));
    return;
  }

  // Every one the snapshot carries, not the two that fit: the card scrolls now.
  // Truncating here is what made everything past the second row unreachable --
  // there was no "more" affordance and no way to scroll to it either.
  for (const s of summaries) box.appendChild(summaryRow(s, rich));
}

/** Relative times move on their own clock, not on the server's. */
function tick() {
  const now = Date.now();
  for (const n of document.querySelectorAll('[data-since]')) {
    n.textContent = fmtAge(now - Number(n.dataset.since));
  }
  for (const n of document.querySelectorAll('[data-until]')) {
    n.textContent = fmtUntil(Number(n.dataset.until) - now);
  }
  for (const n of document.querySelectorAll('[data-stale]')) {
    n.textContent = fmtAge(now - Number(n.dataset.stale)) + ' 前';
  }
}

// --- overlay ----------------------------------------------------------

function openOverlay(s) {
  $('ov-tag').textContent = s.harness || '';
  $('ov-proj').textContent = s.project || '—';
  const when = $('ov-time');
  when.dataset.since = s.created_at_ms;
  $('ov-title').textContent = s.headline || '';

  const box = $('ov-body');
  box.textContent = '';
  const fs = fields(s.body);
  if (!fs.length) {
    box.appendChild(el('div', 'f-plain', '(没有正文)'));
  }
  for (const f of fs) {
    if (!f.k) {
      box.appendChild(el('div', 'f-plain', f.v));
      continue;
    }
    const cls = FIELD_CLASS[f.k];
    const row = el('div', 'f' + (cls ? ' ' + cls : ''));
    row.appendChild(el('span', 'f-k', f.k));
    row.appendChild(el('span', 'f-v', f.v));
    box.appendChild(row);
  }

  $('ov-meta').textContent = [s.model, s.session_id].filter(Boolean).join('  ·  ');
  $('overlay').hidden = false;
  tick();
}

function closeOverlay() {
  $('overlay').hidden = true;
}

// --- the full list ----------------------------------------------------

const HARNESSES = ['all', 'claude', 'codex', 'dsh'];
let list = { harness: 'all', before: null, more: false, loading: false, gen: 0 };

/** Fetches one page and appends it. `reset` starts the list over, which is what
    a filter change means.

    `gen` is what makes a filter change safe while a page is still in the air: a
    reset always goes ahead, and the page it overtook drops its rows on arrival
    instead of pasting the old harness under the new chip. */
function loadPage(reset) {
  if (list.loading && !reset) return;
  if (reset) { list.gen++; list.before = null; $('ls-rows').textContent = ''; }
  const gen = list.gen;
  list.loading = true;

  const q = new URLSearchParams({ limit: '30' });
  if (list.before !== null) q.set('before_ms', String(list.before));
  if (list.harness !== 'all') q.set('harness', list.harness);

  fetch('/api/summaries?' + q)
    .then((r) => (r.ok ? r.json() : Promise.reject(new Error('HTTP ' + r.status))))
    .then((d) => {
      if (gen !== list.gen) return;
      const rows = $('ls-rows');
      for (const s of d.summaries) {
        rows.appendChild(summaryRow(s, true));
        list.before = s.created_at_ms;
      }
      list.more = d.has_more;
      if (!rows.childElementCount) rows.appendChild(el('div', 'empty', '这个筛选下没有总结'));
      $('ls-count').textContent = rows.querySelectorAll('.sum').length + (d.has_more ? '+' : '');
      list.loading = false;
      tick();
    })
    .catch(() => {
      if (gen !== list.gen) return;
      list.loading = false;
      $('ls-count').textContent = '读不到';
    });
}

function openList() {
  const bar = $('ls-filters');
  bar.textContent = '';
  for (const h of HARNESSES) {
    const chip = el('button', 'chip' + (h === list.harness ? ' on' : ''), h);
    chip.type = 'button';
    chip.addEventListener('click', () => { list.harness = h; openList(); });
    bar.appendChild(chip);
  }
  $('list').hidden = false;
  loadPage(true);
}

function closeList() {
  $('list').hidden = true;
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

$('ov-close').addEventListener('click', closeOverlay);
// clicking the backdrop closes as well, so a missed 26px button is not a trap
$('overlay').addEventListener('click', (e) => { if (e.target.id === 'overlay') closeOverlay(); });

$('all').addEventListener('click', openList);
$('ls-close').addEventListener('click', closeList);
$('list').addEventListener('click', (e) => { if (e.target.id === 'list') closeList(); });

// Near the bottom, fetch the next page. The window is short, so a page is only
// a few screens and waiting for the very last pixel would read as an end.
$('ls-rows').addEventListener('scroll', (e) => {
  const b = e.target;
  if (list.more && b.scrollHeight - b.scrollTop - b.clientHeight < 120) loadPage(false);
});

// Escape peels one layer at a time: the detail card opens on top of the list,
// and closing both at once would lose the reader's place in it.
document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (!$('overlay').hidden) closeOverlay();
  else closeList();
});

/* The window is resizable, so row counts are only valid until the next drag.
   Coalesce to one re-render per frame — a resize drag fires continuously. */
new ResizeObserver(() => {
  cancelAnimationFrame(relayout);
  relayout = requestAnimationFrame(render);
}).observe(document.body);

setInterval(tick, 1000);
startPolling();
connect();
