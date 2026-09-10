/**
 * Prover settings (the wasm `Config`), shared by Ask/Tell, Audit, and the
 * `ProverOptions` Vue component (one shared settings panel, toggled from
 * either tab's cog button).
 *
 * State lives here as plain Vue reactives so it survives independently of
 * any one component's lifecycle -- `ProverOptions.vue` binds its form to
 * these directly, and the vanilla `tabs/tests.ts`/`tabs/prover.ts` (not yet
 * converted) keep reading it through the same functional API they always
 * have. Values are read fresh on each run and sent to the worker, which
 * builds the Config there -- the page never holds a wasm object.
 */

import { reactive, ref, computed } from 'vue';

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

// One descriptor per Config knob, driving the form, the summary and the
// object sent to the worker.
const CFG_KNOBS = [
  { key: 'timeLimitSecs', dflt: 30 },
  { key: 'maxSteps', dflt: 4000 },
  { key: 'maxLits', dflt: 8 },
  { key: 'forwardClose', dflt: true },
  { key: 'wantProof', dflt: true },
  { key: 'profile', dflt: false },
  // 0 means "engine default" (sent to the worker as `null` -- see
  // makeConfig in sigma.worker.ts -- never as a literal 0% budget). 100
  // searches the whole KB, same as the old standalone "disable axiom
  // selection" toggle.
  { key: 'selectionTolerancePct', dflt: 0 },
] as const;
const CFG_DEFAULTS: Record<string, number | boolean> = Object.fromEntries(
  CFG_KNOBS.map((k) => [k.key, k.dflt]),
);

/** The Config knobs, reactive -- `ProverOptions.vue` v-models straight onto
 *  these fields. */
export const cfg = reactive<Record<string, number | boolean>>({ ...CFG_DEFAULTS });

/** Which backend Ask/Tell and Audit prove against. */
export const backend = ref<'native' | 'vampire'>('native');
export const vampireSelected = () => backend.value === 'vampire';

/** Vampire's raw extra CLI args (advanced knob, native-backend knobs sit
 *  beside it in the panel but don't apply to it). */
export const vampireArgs = ref('');

/** `"kif"` (default) or `"tptp"` -- which rendering of the proof/contradiction
 *  transcript to show. For a proof/contradiction *result* this is a pure
 *  display choice: every step carries both a `kif` and a `tptp` rendering
 *  (see `ProofStepView` in the sdk), so switching it just re-renders the
 *  last result rather than re-running the query. On the Ask/Tell tab it
 *  ALSO selects the *input* dialect: `"tptp"` parses the assertions/query
 *  text as TPTP instead of SUO-KIF before it's asked -- so switching it
 *  there also live-revalidates the editors and, if a proof is showing,
 *  re-runs nothing on its own (the user must press Prove again with the new
 *  input dialect). */
export const proofLang = ref<'kif' | 'tptp'>('kif');
export const proofLanguage = (): 'kif' | 'tptp' => proofLang.value;

/** Settings panel's "plain proof" checkbox: render proof/contradiction steps
 *  as unstyled plain text (no citation footer, paraphrase, syntax
 *  highlighting, or symbol links) -- just the formula text, one per line, in
 *  whichever dialect {@link proofLanguage} selects. Orthogonal to the
 *  dialect choice. Physically rendered once (in Ask/Tell's result card) but
 *  shared with Audit -- hence living here rather than as component state. */
export const plainProofFlag = ref(false);
export const plainProof = (): boolean => plainProofFlag.value;

/** Ask/Tell's TPTP-mode "Use SUMO" checkbox: off (default) proves the pane's
 *  TPTP problem standalone, with its own local axioms as the only support
 *  and no symbol decoding. On, it proves against the whole loaded KB
 *  (SInE-selected, same as an ordinary KIF Ask/Tell) with SUMO-mangled
 *  symbol names (`s__foo`, …) decoded back to their real SUMO names first,
 *  so a SUMO-generated TPTP problem's symbols actually unify with the live
 *  KB's. Meaningless outside TPTP mode. */
export const useSumoFlag = ref(false);
export const useSumo = (): boolean => useSumoFlag.value;

/** Whether the shared `#proverSettings` panel is open -- one ref rather than
 *  the old two-DOM-lookup `togglePanel` hack, since both tabs' cog buttons
 *  now just bind their own `aria-expanded` to this directly. */
export const settingsOpen = ref(false);
export function toggleProverSettings(force?: boolean): boolean {
  settingsOpen.value = force !== undefined ? force : !settingsOpen.value;
  return settingsOpen.value;
}

/** Current settings as a plain object for the worker. Numeric fields coerce to
 *  u32-safe ints; `overrides` wins, so callers with their own input (Audit's
 *  time limit) get the same coercion instead of redoing it. */
export function proverConfig(overrides: ProverConfig = {}): ProverConfig {
  const out: Record<string, number | boolean> = {};
  for (const { key, dflt } of CFG_KNOBS) {
    if (typeof dflt === 'boolean') { out[key] = cfg[key]; continue; }
    const raw = key in overrides ? (overrides as Record<string, unknown>)[key] : cfg[key];
    const v = Math.floor(Number(raw));
    out[key] = Number.isFinite(v) && v >= 0 ? v : dflt;
  }
  return out as ProverConfig;
}

/** Reset every prover setting (Config knobs, proof language, plain proof,
 *  use-SUMO) to its default -- the settings panel's "Reset to defaults". */
export function resetProverConfig() {
  Object.assign(cfg, CFG_DEFAULTS);
  proofLang.value = 'kif';
  plainProofFlag.value = false;
  useSumoFlag.value = false;
}

/** One-line summary next to the cog, so non-default settings are visible without opening the panel. */
export const cfgSummary = computed(() => {
  const diffs = Object.keys(CFG_DEFAULTS).filter((k) => cfg[k] !== CFG_DEFAULTS[k]);
  return diffs.length
    ? `${cfg.timeLimitSecs}s · ${cfg.maxSteps} steps · ${diffs.length} non-default`
    : `${cfg.timeLimitSecs}s · ${cfg.maxSteps} steps · defaults`;
});

/** Live "N% of axioms" / "engine default" label under the selection-budget slider. */
export const selectionPctLabel = computed(() => {
  const pct = Number(cfg.selectionTolerancePct);
  return pct === 0
    ? 'engine default — % of axioms a query-relevant selection may admit (also applies to Vampire; 100% searches the whole KB)'
    : `${pct}% of axioms admitted into a query-relevant selection (also applies to Vampire; 100% searches the whole KB)`;
});

/** Vampire is a fixed-strategy refutation search, not a tunable given-clause
 *  loop -- most of the native backend's knobs (given-clause budget, literal
 *  cap, forward closure, profiling) are silently ignored if sent to it.
 *  `ProverOptions.vue` greys them out of the shared settings panel (and
 *  Audit's native-only "max found", which Vampire can't honor -- it's a
 *  single-shot run, not an enumerator) when this is true, and shows
 *  Vampire's own knob (raw CLI args) instead. */
export const backendHint = computed(() => vampireSelected()
  ? 'Vampire is a fixed refutation search — only time limit, selection budget, and extra CLI args apply.'
  : 'given-clause knobs below apply to the native backend only');
