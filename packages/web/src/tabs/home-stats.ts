/**
 * Home: what is loaded, at a glance — plus the clickable stat tiles' popovers.
 *
 * Counts come from the worker (one pass over the KB); the upstream commit info
 * is one GitHub call, cached for the session so revisiting the tab (or checking
 * the KB snapshot cache — see fetchLastCommitInfo's other caller in
 * tryRestoreFromCache) does not spend the 60/hour budget twice.
 */

import { SUMO } from '../constants.ts';
import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, esc, escAttr, fmtNum, fmtDate, targetEl } from '../dom.ts';
import { githubApi } from '../github-api.ts';
import { langLabel } from './browse.ts';

// Cache the promise, not the resolved value: two overlapping callers would
// both see a null value and each fire a request, spending two of the 60/hour
// unauthenticated budget on one page load.
let lastCommitPromise = null;

/** `{ sha, date }` of the latest commit on `SUMO.ref`, or `null` fields when
 *  the API call fails — best-effort, never thrown past this function.
 *  Shared by the stats tile (date) and the KB snapshot cache (sha, the
 *  version signal 'sumo'-origin constituents are pinned to). */
export async function fetchLastCommitInfo() {
  if (!lastCommitPromise) {
    lastCommitPromise = (async () => {
      const commits = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/commits?per_page=1`);
      const c = commits[0];
      const iso = c?.commit?.author?.date;
      return { sha: c?.sha ?? null, date: iso ? new Date(iso) : null };
    })().catch((e) => { lastCommitPromise = null; throw e; });
  }
  return lastCommitPromise;
}

// The latest stats payload, kept for the clickable tiles' popovers.
let lastStats = null;

// -- Stat-tile popovers -------------------------------------------------------
//
// Clickable tiles (marked with the corner ⓘ) open a small animated overlay
// anchored beneath the tile. One popover at a time; outside click, Esc, or
// re-clicking the tile dismisses it.

function closeStatPopover() {
  document.querySelector('.stat-pop')?.remove();
  document.removeEventListener('click', onStatPopoverOutsideClick, true);
  document.removeEventListener('keydown', onStatPopoverEsc);
}

function onStatPopoverOutsideClick(e) {
  if (!targetEl(e).closest<HTMLElement>('.stat-pop')) closeStatPopover();
}

function onStatPopoverEsc(e) {
  if (e.key === 'Escape') closeStatPopover();
}

function showStatPopover(tile, html) {
  const already = document.querySelector<HTMLElement>('.stat-pop');
  closeStatPopover();
  if (already && already.dataset.for === tile.id) return;   // toggle off
  const pop = document.createElement('div');
  pop.className = 'stat-pop';
  pop.dataset.for = tile.id;
  pop.innerHTML = html;
  document.body.appendChild(pop);
  const r = tile.getBoundingClientRect();
  pop.style.top = `${r.bottom + 6 + scrollY}px`;
  pop.style.left = `${Math.max(8, Math.min(r.left, innerWidth - pop.offsetWidth - 12)) + scrollX}px`;
  requestAnimationFrame(() => pop.classList.add('show'));
  // Deferred so the opening click doesn't immediately dismiss it.
  setTimeout(() => {
    document.addEventListener('click', onStatPopoverOutsideClick, true);
    document.addEventListener('keydown', onStatPopoverEsc);
  }, 0);
}

// Row grid: label | meter (or spacer) | right-aligned value.
const popRow = (label, value, extra = '', title = '') =>
  `<div class="pop-row"${title ? ` title="${escAttr(title)}"` : ''}><span class="pop-label">${label}</span>${
    extra || '<span></span>'}<span class="pop-n">${value}</span></div>`;

$('tileDoc')?.addEventListener('click', () => {
  const s = lastStats;
  if (!s) return;
  // Sub-1% coverage still deserves a signal, not a rounded-to-zero 0%.
  const pctTxt = (n) => {
    if (!n || !s.symbols) return '—';
    const p = (100 * n) / s.symbols;
    return p >= 1 ? `${Math.round(p)}%` : `${p.toFixed(1)}%`;
  };
  // Union of both coverage kinds: many languages ship termFormat labels
  // without a single documentation string (SUMO's German, French, …).
  const docs  = new Map<string, number>((s.doc_languages  ?? []).map((l) => [l.language, l.documented]));
  const terms = new Map<string, number>((s.term_languages ?? []).map((l) => [l.language, l.documented]));
  const langs = [...new Set([...docs.keys(), ...terms.keys()])].sort((a, b) =>
    (docs.get(b) ?? 0) - (docs.get(a) ?? 0) || (terms.get(b) ?? 0) - (terms.get(a) ?? 0));
  const rows = langs.map((lang) => popRow(
    esc(langLabel(lang)),
    pctTxt(terms.get(lang)),
    `<span class="pop-n">${pctTxt(docs.get(lang))}</span>`,
    `docs: ${fmtNum(docs.get(lang) ?? 0)} · labels: ${fmtNum(terms.get(lang) ?? 0)} · of ${fmtNum(s.symbols)} symbols`,
  )).join('');
  const header = popRow('<span class="hint">language</span>',
    '<span class="hint">labels</span>', '<span class="hint">docs</span>');
  showStatPopover($('tileDoc'), `<h4>Coverage by language</h4>${
    rows ? header + rows : '<div class="hint">no documentation or labels loaded</div>'}`);
});

$('tileRelations')?.addEventListener('click', () => {
  const s = lastStats;
  if (!s || !Number.isFinite(s.relations)) return;
  const other = Math.max(0, s.relations - (s.predicates ?? 0) - (s.functions ?? 0));
  showStatPopover($('tileRelations'), `<h4>Relations</h4>${
    popRow('predicates', fmtNum(s.predicates ?? 0))}${
    popRow('functions', fmtNum(s.functions ?? 0))}${
    popRow('other relations', fmtNum(other))}`);
});

/** The only part of Home derived from `promoting` — cheap, no RPC, so the
 *  post-processing window can redraw it without repeating a whole-KB pass. */
export function updateHomeNote(error?: unknown) {
  $('statNote').textContent = error ? `Could not read KB stats: ${error}`
    : state.promoting ? 'Post-processing — counts will settle once axiomatization finishes.'
    : '';
}

// The counts change only when the KB does, but every visit to Browse asks for
// them — so a whole-KB `stats` pass is skipped unless something invalidated it.
let statsFresh = false;

export function markStatsStale() { statsFresh = false; }

export async function refreshHomeStats() {
  if (statsFresh) return;
  statsFresh = true;
  // KB counts. These are only meaningful once promotion has run; while it is
  // still in flight the numbers are simply what has been ingested so far.
  try {
    const { stats } = await call('stats');
    lastStats = stats;
    // A stale engine (older wasm) omits newer fields — show a dash, not NaN.
    const num = (v) => (Number.isFinite(v) ? fmtNum(v) : '—');
    $('statFiles').textContent     = num(stats.files);
    $('statSymbols').textContent   = num(stats.symbols);
    $('statAxioms').textContent    = num(stats.axioms);
    $('statRules').textContent     = num(stats.rules);
    $('statClasses').textContent   = num(stats.classes);
    $('statInstances').textContent = num(stats.instances);
    $('statRelations').textContent = num(stats.relations);
    // Documentation coverage: % of symbols carrying a documentation string.
    const pct = stats.symbols && Number.isFinite(stats.documented)
      ? Math.round((100 * stats.documented) / stats.symbols) : null;
    $('statDoc').textContent = pct == null ? '—' : `${pct}%`;
    $('statDocBar').style.width = `${pct ?? 0}%`;
    $('statDoc').closest('.stat').title =
      `${fmtNum(stats.documented)} of ${fmtNum(stats.symbols)} symbols have documentation; ` +
      `${fmtNum(stats.labeled)} have a termFormat label`;
    // Diagnostics (errors/warnings) are intentionally not on this stat grid —
    // the landing view leads with what's loaded, not what's wrong with it.
    // The count is still fully visible, just under its own tab: the
    // Diagnostics tab-bar badge (diagnostics.ts renderDiagnostics) and that
    // tab's own summary line.
    updateHomeNote();
  } catch (e) {
    statsFresh = false;
    updateHomeNote(e.message || e);
  }

  // Upstream commit date, best effort — the rest of the page is useful without it.
  try {
    const { date: d } = await fetchLastCommitInfo();
    $('statCommit').textContent = d ? fmtDate(d) : 'unknown';
    $('statCommit').title = d ? d.toString() : '';
  } catch (e) {
    $('statCommit').textContent = '—';
    $('statCommit').title = `Could not reach GitHub: ${e.message || e}`;
  }
}
