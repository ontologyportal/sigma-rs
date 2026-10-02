/**
 * Prover option profiles and their translation to the worker's wire config.
 *
 * Ask/Tell and Audit each own a {@link ProverProfile}; the backend and
 * Vampire args are shared. {@link toWireConfig} turns a profile into the
 * plain object the worker's `makeConfig` applies to a wasm `Config`:
 * scalar knobs, a partial `selection` (the engine's SineParams) and a
 * partial `strategy` (the native Strategy, snake_case keys -- the CLI's
 * `--strategy` JSON shape). Only non-default values are sent, so the engine's
 * own defaults stand for everything left alone.
 *
 * Pure: no Vue, no imports (unit-tested in isolation).
 */

export type Backend = "native" | "vampire" | "e";

/** How the SInE selection budget is chosen. */
export type BudgetMode = "default" | "pct" | "axioms" | "tolerance" | "all";

export interface SelectionSettings {
  mode: BudgetMode;
  /** `pct` mode: percent of the KB's axioms a selection may admit (1-99). */
  pct: number;
  /** `axioms` mode: the selection budget as an axiom count. */
  axioms: number;
  /** `tolerance` mode: fixed SInE trigger tolerance (>= 1). */
  tolerance: number;
  /** SInE expansion depth; null = unlimited. */
  depth: number | null;
  /** Widen/narrow the budget between reruns from prover feedback. */
  autoscale: boolean;
  /** Post-SInE augmentation; null = the backend's own default. */
  headFilter: boolean | null;
  liuRescue: boolean | null;
  liuRounds: number | null;
  liuTopK: number | null;
}

export interface ProverProfile {
  timeLimitSecs: number;
  maxSteps: number;
  maxLits: number;
  forwardClose: boolean;
  wantProof: boolean;
  profile: boolean;
  selection: SelectionSettings;
  /** Native strategy preset name ({@link DEFAULT_PRESET} = shipping default). */
  preset: string;
  /** Strategy keys overriding the preset. */
  strategy: Record<string, unknown>;
  /** E audits: check overlapping symbol-seeded e_axfilter subsets. */
  auditAxfilter: boolean;
  /** E audits: most distinct subsets checked. */
  auditSubsetLimit: number;
  /** E audits: e_axfilter's own time limit (s). */
  selectionTimeLimitSecs: number;
}

/** The worker's wire config (mirrors `ProverConfig` in worker/handlers.ts). */
export interface WireConfig {
  timeLimitSecs?: number;
  maxSteps?: number;
  maxLits?: number;
  forwardClose?: boolean;
  wantProof?: boolean;
  profile?: boolean;
  selectionTolerancePct?: number;
  selection?: Record<string, unknown>;
  strategy?: Record<string, unknown>;
  backend?: Backend;
  vampireArgs?: string;
  /** A fixed SInE budget with autoscaling off (0 = unset). */
  selectionBudget?: number;
  auditAxfilter?: boolean;
  auditSubsetLimit?: number;
  selectionTimeLimitSecs?: number;
}

export const DEFAULT_PRESET = "default";

/** Scalar knobs a caller may override per run (e.g. a test's own timeout). */
export type ScalarOverrides = Partial<
  Pick<ProverProfile, "timeLimitSecs" | "maxSteps" | "maxLits">
>;

export function defaultSelection(): SelectionSettings {
  return {
    mode: "default",
    pct: 10,
    axioms: 2000,
    tolerance: 1.2,
    depth: null,
    autoscale: true,
    headFilter: null,
    liuRescue: null,
    liuRounds: null,
    liuTopK: null,
  };
}

export function defaultProfile(timeLimitSecs = 30): ProverProfile {
  return {
    timeLimitSecs,
    maxSteps: 4000,
    maxLits: 8,
    forwardClose: true,
    wantProof: true,
    profile: false,
    selection: defaultSelection(),
    preset: DEFAULT_PRESET,
    strategy: {},
    auditAxfilter: false,
    auditSubsetLimit: 20,
    selectionTimeLimitSecs: 10,
  };
}

/** A non-negative integer from `v`, else `dflt`. */
export function toUint(v: unknown, dflt: number): number {
  const n = Math.floor(Number(v));
  return Number.isFinite(n) && n >= 0 ? n : dflt;
}

/** `p` with every field present and well-typed: a saved profile from an
 *  older page version (or hand-edited storage) is filled from `base`. */
export function normalizeProfile(
  p: unknown,
  base: ProverProfile,
): ProverProfile {
  const src = (p && typeof p === "object" ? p : {}) as Record<string, unknown>;
  const sel = (
    src.selection && typeof src.selection === "object" ? src.selection : {}
  ) as Record<string, unknown>;
  const out: ProverProfile = {
    ...base,
    selection: { ...base.selection },
    strategy: {},
  };
  for (const k of [
    "timeLimitSecs",
    "maxSteps",
    "maxLits",
    "auditSubsetLimit",
    "selectionTimeLimitSecs",
  ] as const)
    out[k] = toUint(src[k], base[k]);
  for (const k of [
    "forwardClose",
    "wantProof",
    "profile",
    "auditAxfilter",
  ] as const)
    if (typeof src[k] === "boolean") out[k] = src[k] as boolean;
  if (typeof src.preset === "string") out.preset = src.preset;
  if (src.strategy && typeof src.strategy === "object")
    out.strategy = { ...(src.strategy as Record<string, unknown>) };
  const modes: BudgetMode[] = ["default", "pct", "axioms", "tolerance", "all"];
  if (modes.includes(sel.mode as BudgetMode))
    out.selection.mode = sel.mode as BudgetMode;
  for (const k of ["pct", "axioms", "tolerance"] as const)
    if (typeof sel[k] === "number") out.selection[k] = sel[k] as number;
  if (typeof sel.autoscale === "boolean")
    out.selection.autoscale = sel.autoscale;
  for (const k of ["depth", "liuRounds", "liuTopK"] as const)
    if (sel[k] === null || typeof sel[k] === "number")
      out.selection[k] = sel[k] as number | null;
  for (const k of ["headFilter", "liuRescue"] as const)
    if (sel[k] === null || typeof sel[k] === "boolean")
      out.selection[k] = sel[k] as boolean | null;
  return out;
}

/** The selection part of the wire config. */
function selectionWire(
  s: SelectionSettings,
): Pick<WireConfig, "selectionTolerancePct" | "selection"> {
  const selection: Record<string, unknown> = {};
  let selectionTolerancePct: number | undefined;
  switch (s.mode) {
    case "pct":
      selectionTolerancePct = Math.min(99, Math.max(1, Number(s.pct) || 1));
      break;
    case "axioms":
      selection.auto_budget = Math.max(1, toUint(s.axioms, 1));
      break;
    case "tolerance":
      selection.auto_budget = null;
      selection.tolerance = Math.max(1, Number(s.tolerance) || 1);
      break;
    case "all":
      selection.select_all = true;
      break;
  }
  if (s.depth != null) selection.depth_limit = toUint(s.depth, 1);
  if (!s.autoscale) selection.autoscale = false;
  return {
    ...(selectionTolerancePct !== undefined ? { selectionTolerancePct } : {}),
    ...(Object.keys(selection).length ? { selection } : {}),
  };
}

/** The post-SInE augmentation knobs as native Strategy keys. The native
 *  prover reads them off its Strategy, so that is where they go. */
function augmentationStrategy(s: SelectionSettings): Record<string, unknown> {
  // TODO(core): Liu rescue / head filter live on the native `Strategy`
  // (saturate/prove.rs builds `SelectionParams` from it), while the external
  // backend hard-codes them (prover/external/prove.rs `SelectionParams {..}`)
  // and neither audit path runs them at all (saturate/consistency.rs and
  // external/consistency.rs call SInE directly). Once they move onto
  // `SineParams`, send them in `selection` for every backend instead.
  const out: Record<string, unknown> = {};
  if (s.headFilter !== null) out.head_filter = s.headFilter;
  if (s.liuRescue !== null) out.liu_rescue = s.liuRescue;
  if (s.liuRounds !== null) out.liu_rounds = toUint(s.liuRounds, 1);
  if (s.liuTopK !== null) out.liu_top_k = toUint(s.liuTopK, 32);
  return out;
}

/** The strategy to send for `p` (native only), or undefined for the engine
 *  default. `presets` maps preset name -> full strategy object; a named
 *  preset that isn't in it throws rather than silently running the default. */
export function strategyWire(
  p: ProverProfile,
  presets: Record<string, Record<string, unknown>>,
): Record<string, unknown> | undefined {
  const aug = augmentationStrategy(p.selection);
  const custom = p.preset !== DEFAULT_PRESET;
  if (!custom && !Object.keys(p.strategy).length && !Object.keys(aug).length)
    return undefined;
  const preset = custom ? presets[p.preset] : {};
  if (!preset) throw new Error(`unknown strategy preset "${p.preset}"`);
  return { ...preset, ...p.strategy, ...aug };
}

/** The wire config for `p` on `backend`. */
export function toWireConfig(
  p: ProverProfile,
  opts: {
    backend: Backend;
    vampireArgs: string;
    presets: Record<string, Record<string, unknown>>;
    overrides?: ScalarOverrides;
  },
): WireConfig {
  const base = defaultProfile(p.timeLimitSecs);
  const pick = (k: keyof ScalarOverrides) =>
    toUint(
      opts.overrides && k in opts.overrides ? opts.overrides[k] : p[k],
      base[k],
    );
  const wire: WireConfig = {
    timeLimitSecs: pick("timeLimitSecs"),
    maxSteps: pick("maxSteps"),
    maxLits: pick("maxLits"),
    forwardClose: p.forwardClose,
    wantProof: p.wantProof,
    profile: p.profile,
    ...selectionWire(p.selection),
    backend: opts.backend,
    vampireArgs: opts.vampireArgs.trim(),
  };
  if (opts.backend === "native") {
    const strategy = strategyWire(p, opts.presets);
    if (strategy) wire.strategy = strategy;
  }
  if (opts.backend === "e") {
    wire.auditAxfilter = p.auditAxfilter;
    wire.auditSubsetLimit = Math.max(1, toUint(p.auditSubsetLimit, 20));
    wire.selectionTimeLimitSecs = Math.max(
      1,
      toUint(p.selectionTimeLimitSecs, 10),
    );
  }
  return wire;
}

/** A profile carrying `w`'s settings over `base` -- how a saved wire config
 *  (an audit report's) is shown and edited in the options form. A fixed
 *  `selectionBudget` becomes the axiom-count budget with autoscaling off. */
export function profileFromWire(
  w: WireConfig,
  base: ProverProfile,
): ProverProfile {
  const p = normalizeProfile(base, base);
  for (const k of [
    "timeLimitSecs",
    "maxSteps",
    "maxLits",
    "auditSubsetLimit",
    "selectionTimeLimitSecs",
  ] as const)
    if (w[k] != null) p[k] = toUint(w[k], base[k]);
  for (const k of [
    "forwardClose",
    "wantProof",
    "profile",
    "auditAxfilter",
  ] as const)
    if (typeof w[k] === "boolean") p[k] = w[k] as boolean;
  const sel = w.selection ?? {};
  if (w.selectionBudget)
    Object.assign(p.selection, {
      mode: "axioms",
      axioms: toUint(w.selectionBudget, 1),
      autoscale: false,
    });
  else if (sel.select_all === true) p.selection.mode = "all";
  else if (typeof sel.auto_budget === "number")
    Object.assign(p.selection, { mode: "axioms", axioms: sel.auto_budget });
  else if (sel.auto_budget === null && typeof sel.tolerance === "number")
    Object.assign(p.selection, { mode: "tolerance", tolerance: sel.tolerance });
  else if (w.selectionTolerancePct)
    Object.assign(p.selection, {
      mode: w.selectionTolerancePct >= 100 ? "all" : "pct",
      pct: w.selectionTolerancePct,
    });
  if (typeof sel.depth_limit === "number") p.selection.depth = sel.depth_limit;
  if (sel.autoscale === false) p.selection.autoscale = false;
  if (w.strategy) {
    const { name: _name, ...rest } = w.strategy;
    p.strategy = rest;
  }
  return p;
}

/** One non-default setting, as a removable chip. */
export interface Change {
  id: string;
  label: string;
}

const NATIVE_ONLY = new Set(["steps", "lits", "fc", "profile", "preset"]);
const E_ONLY = new Set(["axfilter"]);

/** The settings of `p` that differ from `dflt`, labelled for chips. Settings
 *  `backend` ignores are left out. */
export function profileChanges(
  p: ProverProfile,
  dflt: ProverProfile,
  backend: Backend,
): Change[] {
  const out: Change[] = [];
  const add = (id: string, label: string) => {
    if (
      E_ONLY.has(id)
        ? backend === "e"
        : backend === "native" ||
          !(
            NATIVE_ONLY.has(id) ||
            id.startsWith("strategy:") ||
            id === "augment"
          )
    )
      out.push({ id, label });
  };
  if (p.timeLimitSecs !== dflt.timeLimitSecs)
    add(
      "time",
      p.timeLimitSecs ? `${p.timeLimitSecs}s limit` : "no time limit",
    );
  if (p.maxSteps !== dflt.maxSteps) add("steps", `${p.maxSteps} steps`);
  if (p.maxLits !== dflt.maxLits) add("lits", `${p.maxLits} literals`);
  if (p.forwardClose !== dflt.forwardClose)
    add("fc", p.forwardClose ? "forward closure" : "no forward closure");
  if (p.wantProof !== dflt.wantProof)
    add("proof", p.wantProof ? "proofs" : "no proofs");
  if (p.profile !== dflt.profile)
    add("profile", p.profile ? "profiling" : "no profiling");
  const s = p.selection;
  const d = dflt.selection;
  if (s.mode !== d.mode || selectionValueChanged(s, d))
    add("budget", budgetLabel(s));
  if (s.depth !== d.depth)
    add("depth", s.depth == null ? "unlimited depth" : `depth ${s.depth}`);
  if (s.autoscale !== d.autoscale)
    add("autoscale", s.autoscale ? "autoscale" : "no autoscale");
  const aug = [
    s.headFilter !== d.headFilter,
    s.liuRescue !== d.liuRescue,
    s.liuRounds !== d.liuRounds,
    s.liuTopK !== d.liuTopK,
  ].filter(Boolean).length;
  if (aug)
    add("augment", `${aug} selection-repair change${aug === 1 ? "" : "s"}`);
  if (p.preset !== dflt.preset) add("preset", `${p.preset} strategy`);
  if (
    p.auditAxfilter !== dflt.auditAxfilter ||
    (p.auditAxfilter &&
      (p.auditSubsetLimit !== dflt.auditSubsetLimit ||
        p.selectionTimeLimitSecs !== dflt.selectionTimeLimitSecs))
  )
    add(
      "axfilter",
      p.auditAxfilter
        ? `e_axfilter ≤ ${p.auditSubsetLimit} subsets`
        : "no e_axfilter",
    );
  for (const k of Object.keys(p.strategy))
    add(`strategy:${k}`, `${k} = ${JSON.stringify(p.strategy[k])}`);
  return out;
}

function selectionValueChanged(s: SelectionSettings, d: SelectionSettings) {
  return (
    (s.mode === "pct" && s.pct !== d.pct) ||
    (s.mode === "axioms" && s.axioms !== d.axioms) ||
    (s.mode === "tolerance" && s.tolerance !== d.tolerance)
  );
}

/** The selection budget in words. */
export function budgetLabel(s: SelectionSettings): string {
  switch (s.mode) {
    case "pct":
      return `${s.pct}% of the KB`;
    case "axioms":
      return `≤ ${s.axioms} axioms`;
    case "tolerance":
      return `tolerance ${s.tolerance}`;
    case "all":
      return "whole KB";
    default:
      return "default selection";
  }
}

/** `p` with the setting `id` (from {@link profileChanges}) back at `dflt`. */
export function resetChange(
  p: ProverProfile,
  dflt: ProverProfile,
  id: string,
): void {
  const s = p.selection;
  const d = dflt.selection;
  switch (id) {
    case "time":
      p.timeLimitSecs = dflt.timeLimitSecs;
      break;
    case "steps":
      p.maxSteps = dflt.maxSteps;
      break;
    case "lits":
      p.maxLits = dflt.maxLits;
      break;
    case "fc":
      p.forwardClose = dflt.forwardClose;
      break;
    case "proof":
      p.wantProof = dflt.wantProof;
      break;
    case "profile":
      p.profile = dflt.profile;
      break;
    case "budget":
      Object.assign(s, {
        mode: d.mode,
        pct: d.pct,
        axioms: d.axioms,
        tolerance: d.tolerance,
      });
      break;
    case "depth":
      s.depth = d.depth;
      break;
    case "autoscale":
      s.autoscale = d.autoscale;
      break;
    case "augment":
      Object.assign(s, {
        headFilter: d.headFilter,
        liuRescue: d.liuRescue,
        liuRounds: d.liuRounds,
        liuTopK: d.liuTopK,
      });
      break;
    case "preset":
      p.preset = dflt.preset;
      break;
    case "axfilter":
      p.auditAxfilter = dflt.auditAxfilter;
      p.auditSubsetLimit = dflt.auditSubsetLimit;
      p.selectionTimeLimitSecs = dflt.selectionTimeLimitSecs;
      break;
    default:
      if (id.startsWith("strategy:"))
        delete p.strategy[id.slice("strategy:".length)];
  }
}

/** Problems with a strategy object against the full default `base`: unknown
 *  keys and values whose JSON type differs. Empty when it is usable. */
export function strategyProblems(
  obj: unknown,
  base: Record<string, unknown>,
): string[] {
  if (!obj || typeof obj !== "object" || Array.isArray(obj))
    return ["a strategy must be a JSON object"];
  const kind = (v: unknown) =>
    v === null ? "null" : Array.isArray(v) ? "array" : typeof v;
  const out: string[] = [];
  for (const [k, v] of Object.entries(obj)) {
    if (!(k in base)) {
      out.push(`unknown key "${k}"`);
      continue;
    }
    const want = kind(base[k]);
    const got = kind(v);
    // A `null` default is an optional number (e.g. derived_width_cap).
    const ok =
      want === "null" ? got === "null" || got === "number" : got === want;
    if (!ok)
      out.push(
        `"${k}" should be ${want === "null" ? "a number or null" : `a ${want}`}, not ${got}`,
      );
    else if (got === "number" && (!Number.isInteger(v) || (v as number) < 0))
      out.push(`"${k}" should be a non-negative integer`);
  }
  return out;
}

/** The keys of `full` whose values differ from `base` (`name` excluded) --
 *  the overrides an imported strategy file amounts to. */
export function strategyDiff(
  full: Record<string, unknown>,
  base: Record<string, unknown>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(full))
    if (k !== "name" && JSON.stringify(v) !== JSON.stringify(base[k]))
      out[k] = v;
  return out;
}
