/**
 * Prover settings shared by Ask/Tell, Audit and Inference Tests. Each tab owns a profile
 * (its own time limit, selection and strategy -- an audit check wants a much
 * shorter limit than a single query); the backend, Vampire args and the
 * proof display options are shared. Values are read fresh on each run and
 * sent to the worker, which builds the wasm Config there -- the page never
 * holds a wasm object.
 *
 * Profiles, backend and Vampire args are remembered in localStorage as a
 * per-viewer convenience; the page works without it.
 */

import { defineStore } from "pinia";
import { computed, reactive, ref, watch } from "vue";
import type { SelectionParams, StrategyPreset } from "sigmakee/sdk";
import { PROVER_SETTINGS_KEY } from "../constants";
import { call } from "../services/sigma";
import {
  DEFAULT_PRESET,
  defaultProfile,
  normalizeProfile,
  profileChanges,
  profileFromWire,
  resetChange,
  toWireConfig,
  type Backend,
  type Change,
  type ProverProfile,
  type ScalarOverrides,
  type WireConfig,
} from "../utils/proverOptions";

export type ProfileName = "ask" | "audit" | "test";

/** The plain config object sent to the worker (see worker/handlers.ts). */
export type ProverConfig = WireConfig;

const PROFILE_TIME: Record<ProfileName, number> = {
  ask: 30,
  audit: 10,
  test: 30,
};

/** A fresh default profile for `name`. */
export const profileDefaults = (name: ProfileName): ProverProfile =>
  defaultProfile(PROFILE_TIME[name]);

interface Saved {
  profiles?: Partial<Record<ProfileName, unknown>>;
  backend?: unknown;
  vampireArgs?: unknown;
}

function loadSaved(): Saved {
  try {
    const v = JSON.parse(localStorage.getItem(PROVER_SETTINGS_KEY) || "null");
    return v && typeof v === "object" ? v : {};
  } catch {
    return {};
  }
}

export const useProverStore = defineStore("prover", () => {
  const saved = loadSaved();
  const profiles = reactive<Record<ProfileName, ProverProfile>>({
    ask: normalizeProfile(saved.profiles?.ask, profileDefaults("ask")),
    audit: normalizeProfile(saved.profiles?.audit, profileDefaults("audit")),
    test: normalizeProfile(saved.profiles?.test, profileDefaults("test")),
  });
  /** Which backend Ask/Tell, Audit and Inference Tests prove against. */
  const backend = ref<Backend>(
    saved.backend === "vampire" || saved.backend === "e"
      ? saved.backend
      : "native",
  );
  /** Vampire's raw extra CLI args. */
  const vampireArgs = ref(
    typeof saved.vampireArgs === "string" ? saved.vampireArgs : "",
  );
  /** `"kif"` (default) or `"tptp"` -- which rendering of proof/contradiction
   *  steps to show. On Ask/Tell it also selects the input dialect: `"tptp"`
   *  parses the assertions pane as one TPTP problem. */
  const proofLang = ref<"kif" | "tptp">("kif");
  /** Render proof steps as unstyled plain text, one formula per line. */
  const plainProof = ref(false);
  /** Ask/Tell TPTP mode: prove against the whole loaded KB (decoding
   *  SUMO-mangled names) instead of the problem standalone. */
  const useSumo = ref(false);
  /** The engine's default selection and strategy presets (first = the
   *  shipping default), once {@link loadDefaults} has fetched them. */
  const defaults = ref<{
    selection: SelectionParams;
    presets: StrategyPreset[];
  } | null>(null);
  let loading: Promise<void> | null = null;

  watch(
    [profiles, backend, vampireArgs],
    () => {
      try {
        localStorage.setItem(
          PROVER_SETTINGS_KEY,
          JSON.stringify({
            profiles,
            backend: backend.value,
            vampireArgs: vampireArgs.value,
          }),
        );
      } catch {
        /* storage unavailable */
      }
    },
    { deep: true },
  );

  const vampireSelected = computed(() => backend.value === "vampire");
  /** Vampire or E: a fixed external search, without SUPr's given-clause and
   *  strategy knobs. */
  const externalSelected = computed(() => backend.value !== "native");
  /** The backend's display name. */
  const backendLabel = computed(() =>
    backend.value === "e"
      ? "E"
      : backend.value === "vampire"
        ? "Vampire"
        : "SUPr",
  );
  const presets = computed<Record<string, Record<string, unknown>>>(() =>
    Object.fromEntries(
      (defaults.value?.presets ?? []).map((p) => [p.name, p.strategy]),
    ),
  );
  /** The full default strategy, or null before {@link loadDefaults}. */
  const baseStrategy = computed(
    () => defaults.value?.presets[0]?.strategy ?? null,
  );

  /** Fetch the engine defaults once (needed before a run that uses a named
   *  strategy preset, and by the options form). */
  function loadDefaults(): Promise<void> {
    if (defaults.value) return Promise.resolve();
    loading ??= call("proverDefaults")
      .then((d) => {
        defaults.value = d;
        repairProfiles();
      })
      .catch((e) => {
        loading = null;
        throw e;
      });
    return loading;
  }

  /** Saved profiles outlive engine versions: a preset that no longer exists
   *  falls back to the default, and overrides of strategy fields the engine
   *  no longer has are dropped -- either would fail every run. */
  function repairProfiles() {
    const base = baseStrategy.value;
    for (const p of Object.values(profiles)) {
      if (p.preset !== DEFAULT_PRESET && !(p.preset in presets.value))
        p.preset = DEFAULT_PRESET;
      if (base)
        for (const k of Object.keys(p.strategy))
          if (!(k in base)) delete p.strategy[k];
    }
  }

  /** `name`'s settings as the worker's config object; `overrides` wins for
   *  the scalar knobs it names (coerced like the form's input). */
  function config(
    name: ProfileName = "ask",
    overrides: ScalarOverrides = {},
  ): ProverConfig {
    const wire = toWireConfig(profiles[name], {
      backend: backend.value,
      vampireArgs: vampireArgs.value,
      presets: presets.value,
      overrides,
    });
    // The strategy/selection objects are reactive proxies, which
    // postMessage can't clone; the config is plain JSON by construction.
    return JSON.parse(JSON.stringify(wire));
  }

  /** `name`'s non-default settings that apply to the current backend. */
  function changes(name: ProfileName): Change[] {
    return profileChanges(profiles[name], profileDefaults(name), backend.value);
  }

  /** Put one setting of `name` (a {@link changes} id) back to its default. */
  function resetOne(name: ProfileName, id: string): void {
    resetChange(profiles[name], profileDefaults(name), id);
  }

  /** Put every setting of `name` back to its default. */
  function reset(name: ProfileName): void {
    Object.assign(profiles[name], profileDefaults(name));
  }

  /** Adopt a saved wire config (an audit report's) as the backend and
   *  `name`'s settings, so the form shows what will run. */
  function adoptConfig(name: ProfileName, wire: ProverConfig): void {
    backend.value = wire.backend ?? "native";
    vampireArgs.value = wire.vampireArgs ?? "";
    Object.assign(profiles[name], profileFromWire(wire, profileDefaults(name)));
  }

  return {
    profiles,
    backend,
    vampireArgs,
    proofLang,
    plainProof,
    useSumo,
    defaults,
    vampireSelected,
    externalSelected,
    backendLabel,
    presets,
    baseStrategy,
    loadDefaults,
    config,
    changes,
    resetOne,
    reset,
    adoptConfig,
  };
});
