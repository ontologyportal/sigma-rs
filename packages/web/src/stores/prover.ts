/**
 * Prover settings (the wasm `Config`), shared by Ask/Tell and Audit: one
 * settings panel, toggled from either tab's cog button. Values are read
 * fresh on each run and sent to the worker, which builds the Config there --
 * the page never holds a wasm object.
 */

import { defineStore } from "pinia";

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

/** The Config knobs with every field present. */
export interface ProverKnobs {
  timeLimitSecs: number;
  maxSteps: number;
  maxLits: number;
  forwardClose: boolean;
  wantProof: boolean;
  profile: boolean;
  selectionTolerancePct: number;
}

// One descriptor per Config knob, driving the form, the summary and the
// object sent to the worker.
const CFG_KNOBS = [
  { key: "timeLimitSecs", dflt: 30 },
  { key: "maxSteps", dflt: 4000 },
  { key: "maxLits", dflt: 8 },
  { key: "forwardClose", dflt: true },
  { key: "wantProof", dflt: true },
  { key: "profile", dflt: false },
  // 0 means "engine default" (sent to the worker as `null` -- see
  // makeConfig in sigma.worker.ts -- never as a literal 0% budget). 100
  // searches the whole KB, same as the old standalone "disable axiom
  // selection" toggle.
  { key: "selectionTolerancePct", dflt: 0 },
] as const;

const CFG_DEFAULTS = Object.fromEntries(
  CFG_KNOBS.map((k) => [k.key, k.dflt]),
) as unknown as ProverKnobs;

export const useProverStore = defineStore("prover", {
  state: () => ({
    /** The Config knobs; the settings form v-models straight onto these. */
    cfg: { ...CFG_DEFAULTS } as ProverKnobs,
    /** Which backend Ask/Tell and Audit prove against. */
    backend: "native" as "native" | "vampire",
    /** Vampire's raw extra CLI args (advanced knob; the native-backend knobs
     *  sit beside it in the panel but don't apply to it). */
    vampireArgs: "",
    /** `"kif"` (default) or `"tptp"` -- which rendering of the
     *  proof/contradiction transcript to show. For a result this is a pure
     *  display choice: every step carries both renderings, so switching it
     *  re-renders the last result. On Ask/Tell it ALSO selects the input
     *  dialect: `"tptp"` parses the assertions/query text as TPTP, so
     *  switching there live-revalidates the editors (the user must press
     *  Prove again with the new input dialect). */
    proofLang: "kif" as "kif" | "tptp",
    /** Render proof/contradiction steps as unstyled plain text (no citation
     *  footer, paraphrase, highlighting, or symbol links) -- one formula per
     *  line in whichever dialect `proofLang` selects. Rendered in Ask/Tell's
     *  result card but shared with Audit. */
    plainProof: false,
    /** Ask/Tell's TPTP-mode "Use SUMO" checkbox: off proves the pane's TPTP
     *  problem standalone; on proves against the whole loaded KB with
     *  SUMO-mangled symbol names (`s__foo`) decoded back to their real names
     *  first. Meaningless outside TPTP mode. */
    useSumo: false,
    /** Whether the shared settings panel is open (Ask/Tell + Audit). */
    settingsOpen: false,
  }),
  getters: {
    vampireSelected: (state) => state.backend === "vampire",
    /** One-line summary next to the cog, so non-default settings are visible
     *  without opening the panel. */
    cfgSummary: (state) => {
      const diffs = (Object.keys(CFG_DEFAULTS) as (keyof ProverKnobs)[]).filter(
        (k) => state.cfg[k] !== CFG_DEFAULTS[k],
      );
      return diffs.length
        ? `${state.cfg.timeLimitSecs}s · ${state.cfg.maxSteps} steps · ${diffs.length} non-default`
        : `${state.cfg.timeLimitSecs}s · ${state.cfg.maxSteps} steps · defaults`;
    },
    /** Live "N% of axioms" / "engine default" label under the selection-budget slider. */
    selectionPctLabel: (state) => {
      const pct = Number(state.cfg.selectionTolerancePct);
      return pct === 0
        ? "engine default — % of axioms a query-relevant selection may admit (also applies to Vampire; 100% searches the whole KB)"
        : `${pct}% of axioms admitted into a query-relevant selection (also applies to Vampire; 100% searches the whole KB)`;
    },
    /** Vampire is a fixed-strategy refutation search, not a tunable
     *  given-clause loop -- most of the native backend's knobs are silently
     *  ignored if sent to it, so the panel greys them out and shows Vampire's
     *  own knob (raw CLI args) instead. */
    backendHint: (state) =>
      state.backend === "vampire"
        ? "Vampire is a fixed refutation search — only time limit, selection budget, and extra CLI args apply."
        : "given-clause knobs below apply to the native backend only",
  },
  actions: {
    /** Current settings as a plain object for the worker. Numeric fields
     *  coerce to u32-safe ints (falling back to the default when not a
     *  non-negative number); `overrides` wins, so callers with their own
     *  input get the same coercion. */
    config(overrides: ProverConfig = {}): ProverConfig {
      const out: Record<string, number | boolean> = {};
      for (const { key, dflt } of CFG_KNOBS) {
        if (typeof dflt === "boolean") {
          out[key] = this.cfg[key];
          continue;
        }
        const raw =
          key in overrides
            ? (overrides as Record<string, unknown>)[key]
            : this.cfg[key];
        const v = Math.floor(Number(raw));
        out[key] = Number.isFinite(v) && v >= 0 ? v : dflt;
      }
      return out as ProverConfig;
    },

    /** Reset every prover setting (Config knobs, proof language, plain
     *  proof, use-SUMO) to its default -- the panel's "Reset to defaults". */
    reset() {
      Object.assign(this.cfg, CFG_DEFAULTS);
      this.proofLang = "kif";
      this.plainProof = false;
      this.useSumo = false;
    },

    /** Flip (or force) the shared settings panel; returns the new state. */
    toggleSettings(force?: boolean): boolean {
      this.settingsOpen = force !== undefined ? force : !this.settingsOpen;
      return this.settingsOpen;
    },
  },
});
