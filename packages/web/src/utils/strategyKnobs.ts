/**
 * The native Strategy knobs the prover options form shows as controls, by
 * group. Keys are the engine's snake_case `Strategy` fields; every other
 * field stays reachable through the strategy JSON editor.
 */

export interface StrategyKnob {
  key: string;
  label: string;
  help: string;
  kind: "bool" | "int" | "choice";
  choices?: { value: number; label: string }[];
}

export interface StrategyKnobGroup {
  title: string;
  knobs: StrategyKnob[];
}

export const STRATEGY_KNOBS: StrategyKnobGroup[] = [
  {
    title: "Clause selection",
    knobs: [
      {
        key: "pick_ratio",
        label: "age pick ratio",
        help: "every Nth given clause is the oldest instead of the lightest (fairness)",
        kind: "int",
      },
      {
        key: "cw_lits",
        label: "weight per literal",
        help: "clause-weight coefficient per literal",
        kind: "int",
      },
      {
        key: "cw_size",
        label: "weight per symbol",
        help: "clause-weight coefficient per term-size leaf",
        kind: "int",
      },
      {
        key: "cw_vars",
        label: "weight per variable",
        help: "clause-weight coefficient per variable",
        kind: "int",
      },
      {
        key: "cw_skolem",
        label: "Skolem penalty",
        help: "weight multiplier per Skolem application (throttles existentials)",
        kind: "int",
      },
      {
        key: "goal_dist",
        label: "goal distance",
        help: "weigh clauses sharing nothing with the goal higher",
        kind: "bool",
      },
      {
        key: "lit_select",
        label: "literal selection",
        help: "which literal of the given clause resolution uses",
        kind: "choice",
        choices: [
          { value: 0, label: "fewest candidates" },
          { value: 1, label: "most candidates" },
          { value: 2, label: "first eligible" },
        ],
      },
      {
        key: "prec_seed",
        label: "precedence seed",
        help: "KBO symbol order; 0 = id order, anything else a different admissible order",
        kind: "int",
      },
    ],
  },
  {
    title: "Generation limits",
    knobs: [
      {
        key: "max_depth",
        label: "max term depth",
        help: "derived clauses nested deeper are discarded",
        kind: "int",
      },
      {
        key: "max_term_size",
        label: "max term size",
        help: "derived clauses with more leaves are discarded",
        kind: "int",
      },
      {
        key: "para_cap",
        label: "paramodulants per step",
        help: "equality rewrites generated per given clause",
        kind: "int",
      },
    ],
  },
  {
    title: "Inference rules",
    knobs: [
      {
        key: "schema",
        label: "schema channel",
        help: "mine symmetry, transitivity and Leibniz patterns",
        kind: "bool",
      },
      {
        key: "demod",
        label: "forward demodulation",
        help: "rewrite new clauses with oriented unit equations",
        kind: "bool",
      },
      {
        key: "bwd_demod",
        label: "backward demodulation",
        help: "rewrite existing clauses when a unit equation activates",
        kind: "bool",
      },
      {
        key: "subsumption",
        label: "subsumption",
        help: "drop new clauses an active clause subsumes",
        kind: "bool",
      },
      {
        key: "ordered_resolution",
        label: "ordered resolution",
        help: "resolve only on KBO-maximal literals",
        kind: "bool",
      },
      {
        key: "superposition",
        label: "superposition",
        help: "the complete ordered equality calculus",
        kind: "bool",
      },
      {
        key: "full_saturation",
        label: "full saturation",
        help: "let the axioms reason among themselves (no set-of-support)",
        kind: "bool",
      },
    ],
  },
  {
    title: "Forward closure and completion",
    knobs: [
      {
        key: "fc_rounds",
        label: "closure rounds",
        help: "forward-closure rounds before the loop",
        kind: "int",
      },
      {
        key: "fc_cap",
        label: "closure cap",
        help: "total conclusions forward closure may add",
        kind: "int",
      },
      {
        key: "fc_max_pos",
        label: "closure heads",
        help: "max positive heads per premise; above 1 derives short disjunctions",
        kind: "int",
      },
      {
        key: "def_completion",
        label: "definitional completion",
        help: "pull definitions for goal obligations nothing selected provides",
        kind: "bool",
      },
      {
        key: "defcomp_rounds",
        label: "completion rounds",
        help: "definition chains followed by completion",
        kind: "int",
      },
    ],
  },
];
