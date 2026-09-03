/**
 * Prover settings (the wasm `Config`), shared by Ask/Tell and Audit.
 *
 * The cog next to Prove toggles a panel over the same knobs `Config` exposes.
 * Values are read fresh on each run and sent to the worker, which builds the
 * Config there — the page never holds a wasm object.
 */

import { $, togglePanel } from './dom.ts';

/** Shape sent to the worker as a plain object (see sigma.worker.ts's
 *  makeConfig) -- every field optional, since only knobs the UI has touched
 *  (or an explicit override) are included. */
export interface ProverConfig {
  timeLimitSecs?: number;
  maxSteps?: number;
  maxLits?: number;
  forwardClose?: boolean;
  wantProof?: boolean;
  profile?: boolean;
  selectionTolerancePct?: number;
}

// One descriptor per Config knob, driving the form, the summary and the object
// sent to the worker. Adding a knob is one row here plus the markup, rather
// than four coordinated edits where a typo'd id fails silently.
const CFG_KNOBS = [
  { key: 'timeLimitSecs', id: 'cfgTimeLimit',    dflt: 30   },
  { key: 'maxSteps',      id: 'cfgMaxSteps',     dflt: 4000 },
  { key: 'maxLits',       id: 'cfgMaxLits',      dflt: 8    },
  { key: 'forwardClose',  id: 'cfgForwardClose', dflt: true },
  { key: 'wantProof',     id: 'cfgWantProof',    dflt: true },
  { key: 'profile',       id: 'cfgProfile',      dflt: false },
  // 0 means "engine default" (sent to the worker as `null` — see makeConfig
  // in sigma.worker.ts — never as a literal 0% budget). 100 searches the
  // whole KB, same as the old standalone "disable axiom selection" toggle.
  { key: 'selectionTolerancePct', id: 'cfgSelectionPct', dflt: 0 },
];
const CFG_DEFAULTS = Object.fromEntries(CFG_KNOBS.map((k) => [k.key, k.dflt]));

/** Current settings as a plain object for the worker. Numeric fields coerce to
 *  u32-safe ints; `overrides` wins, so callers with their own input (Audit's
 *  time limit) get the same coercion instead of redoing it. */
export function proverConfig(overrides: ProverConfig = {}): ProverConfig {
  const cfg: Record<string, number | boolean> = {};
  for (const { key, id, dflt } of CFG_KNOBS) {
    if (typeof dflt === 'boolean') { cfg[key] = $(id).checked; continue; }
    const raw = key in overrides ? (overrides as Record<string, unknown>)[key] : $(id).value;
    const v = Math.floor(Number(raw));
    cfg[key] = Number.isFinite(v) && v >= 0 ? v : dflt;
  }
  return cfg as ProverConfig;
}

function applyProverConfig(c: ProverConfig) {
  for (const { key, id, dflt } of CFG_KNOBS) {
    const el = $(id);
    if (typeof dflt === 'boolean') el.checked = c[key]; else el.value = c[key];
  }
  renderCfgSummary();
}

/** One-line summary next to the cog, so non-default settings are visible without opening the panel. */
function renderCfgSummary() {
  const c = proverConfig();
  const diffs = Object.keys(CFG_DEFAULTS).filter((k) => c[k] !== CFG_DEFAULTS[k]);
  $('proverCfgSummary').textContent = diffs.length
    ? `${c.timeLimitSecs}s · ${c.maxSteps} steps · ${diffs.length} non-default`
    : `${c.timeLimitSecs}s · ${c.maxSteps} steps · defaults`;
}

// The panel is one shared instance toggled by either tab's cog — keep BOTH
// buttons' aria-expanded in sync with it (only the one actually clicked would
// otherwise update, leaving the other stale after a tab switch).
export function toggleProverSettings(force?: boolean) {
  const open = togglePanel('proverSettingsBtn', 'proverSettings', force);
  $('auditSettingsBtn').setAttribute('aria-expanded', String(open));
  return open;
}
$('proverSettingsBtn').onclick = () => toggleProverSettings();
$('auditSettingsBtn').onclick = () => toggleProverSettings();
$('cfgReset').onclick = () => applyProverConfig(CFG_DEFAULTS);
for (const { id } of CFG_KNOBS) $(id).addEventListener('input', renderCfgSummary);

// -- Backend-specific settings visibility -------------------------------------
//
// Vampire is a fixed-strategy refutation search, not a tunable given-clause
// loop — most of the native backend's knobs (given-clause budget, literal
// cap, forward closure, profiling) are silently ignored if sent to it. Grey
// them out of the shared settings panel (and Audit's native-only "max found",
// which Vampire can't honor — it's a single-shot run, not an enumerator) when
// Vampire is selected, and show Vampire's own knob (raw CLI args) instead —
// rather than leave editable controls that quietly do nothing.
function updateBackendVisibility() {
  const vampire = $('proverBackend').value === 'vampire';
  document.querySelectorAll<HTMLElement>('[data-native-only]').forEach((el) => { el.hidden = vampire; });
  document.querySelectorAll<HTMLElement>('[data-vampire-only]').forEach((el) => { el.hidden = !vampire; });
  $('proverBackendHint').textContent = vampire
    ? 'Vampire is a fixed refutation search — only time limit, selection budget, and extra CLI args apply.'
    : 'given-clause knobs below apply to the native backend only';
  // A downloadable TPTP input only exists for a completed Vampire run, and a
  // prior one (if any) was for whichever backend was selected at the time —
  // switching away invalidates it rather than leaving a stale download.
  $('downloadVampireTptp').hidden = true;
}
$('proverBackend').addEventListener('change', updateBackendVisibility);
updateBackendVisibility();

/** `true` when the Vampire backend is selected — Ask/Tell and Audit both
 *  branch on it. */
export function vampireSelected() {
  return $('proverBackend').value === 'vampire';
}

/** Live "N% of axioms" / "engine default" label under the selection-budget slider. */
function renderSelectionPctLabel() {
  const pct = Number($('cfgSelectionPct').value);
  $('cfgSelectionPctVal').textContent = pct === 0
    ? 'engine default — % of axioms a query-relevant selection may admit (also applies to Vampire; 100% searches the whole KB)'
    : `${pct}% of axioms admitted into a query-relevant selection (also applies to Vampire; 100% searches the whole KB)`;
}
$('cfgSelectionPct').addEventListener('input', renderSelectionPctLabel);
renderSelectionPctLabel();
renderCfgSummary();
