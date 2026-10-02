<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useProverStore, type ProfileName } from "../stores/prover";
import {
  DEFAULT_PRESET,
  strategyDiff,
  strategyProblems,
  type BudgetMode,
} from "../utils/proverOptions";
import { STRATEGY_KNOBS, type StrategyKnob } from "../utils/strategyKnobs";
import { downloadText, errMsg } from "../utils/format";
import Disclosure from "./Disclosure.vue";
import Segmented from "./Segmented.vue";

/** The prover options panel for one profile (Ask/Tell or Audit): backend
 *  and time up top, then axiom selection (every backend), the native search
 *  strategy (preset, curated knobs, raw JSON, import/export in the CLI's
 *  `--strategy` format), Vampire's CLI args and E's audit subsets. `hideTime` drops the time
 *  field for a caller that sets its own per-run limit. */
const props = defineProps<{
  profile: ProfileName;
  hideTime?: boolean;
  disabled?: boolean;
}>();

const prover = useProverStore();
const p = computed(() => prover.profiles[props.profile]);
const native = computed(() => !prover.externalSelected);
const loadError = ref("");

onMounted(() => {
  prover.loadDefaults().catch((e) => {
    loadError.value = `Could not load the engine defaults: ${errMsg(e)}`;
  });
});

const backendOptions = [
  {
    value: "native" as const,
    label: "SUPr",
    title: "The native saturation prover",
  },
  {
    value: "vampire" as const,
    label: "Vampire",
    title: "Vampire, compiled to WebAssembly",
  },
  { value: "e" as const, label: "E", title: "E, compiled to WebAssembly" },
];

const budgetModes: { value: BudgetMode; label: string }[] = [
  { value: "default", label: "Engine default" },
  { value: "pct", label: "% of the KB" },
  { value: "axioms", label: "Max axioms" },
  { value: "tolerance", label: "Fixed tolerance" },
  { value: "all", label: "Whole KB (no selection)" },
];

const defaultBudget = computed(() => prover.defaults?.selection.auto_budget);

// -- tri-state (backend default / on / off) --------------------------------

type Tri = "" | "on" | "off";
const tri = (v: boolean | null): Tri => (v === null ? "" : v ? "on" : "off");
const untri = (v: Tri): boolean | null => (v === "" ? null : v === "on");
const headFilter = computed({
  get: () => tri(p.value.selection.headFilter),
  set: (v: Tri) => (p.value.selection.headFilter = untri(v)),
});
const liuRescue = computed({
  get: () => tri(p.value.selection.liuRescue),
  set: (v: Tri) => (p.value.selection.liuRescue = untri(v)),
});
/** Blank number field <-> null ("backend default" / "unlimited"). */
function nullableNum(v: unknown): number | null {
  if (v === "" || v === null || v === undefined) return null;
  const n = Math.floor(Number(v));
  return Number.isFinite(n) && n >= 0 ? n : null;
}

// -- strategy --------------------------------------------------------------

const presetStrategy = computed<Record<string, unknown> | null>(() =>
  p.value.preset === DEFAULT_PRESET
    ? prover.baseStrategy
    : (prover.presets[p.value.preset] ?? null),
);

/** The value the prover will use for `key`: override, else preset. */
function knobValue(k: StrategyKnob): unknown {
  return k.key in p.value.strategy
    ? p.value.strategy[k.key]
    : presetStrategy.value?.[k.key];
}

function setKnob(k: StrategyKnob, raw: unknown) {
  let v: unknown = raw;
  if (k.kind !== "bool") {
    const n = Math.floor(Number(raw));
    if (!Number.isFinite(n) || n < 0) return;
    v = n;
  }
  if (presetStrategy.value && presetStrategy.value[k.key] === v)
    delete p.value.strategy[k.key];
  else p.value.strategy[k.key] = v;
}

const overridden = (k: StrategyKnob) => k.key in p.value.strategy;

/** Raw overrides editor: the text tracks the profile unless it holds an
 *  edit that doesn't parse yet. */
const jsonText = ref("");
const jsonProblems = ref<string[]>([]);
const overridesJson = computed(() =>
  Object.keys(p.value.strategy).length
    ? JSON.stringify(p.value.strategy, null, 2)
    : "",
);
watch(
  overridesJson,
  (t) => {
    if (!jsonProblems.value.length) jsonText.value = t;
  },
  { immediate: true },
);

function applyJson() {
  const text = jsonText.value.trim();
  if (!text) {
    p.value.strategy = {};
    jsonProblems.value = [];
    return;
  }
  let obj: unknown;
  try {
    obj = JSON.parse(text);
  } catch (e) {
    jsonProblems.value = [`not valid JSON: ${errMsg(e)}`];
    return;
  }
  const base = prover.baseStrategy;
  const problems = base ? strategyProblems(obj, base) : [];
  jsonProblems.value = problems;
  if (problems.length) return;
  const { name: _name, ...rest } = obj as Record<string, unknown>;
  p.value.strategy = rest;
  jsonText.value = overridesJson.value;
}

/** The whole strategy this profile runs, in the CLI's `--strategy` shape. */
function exportStrategy() {
  const full = {
    ...(presetStrategy.value ?? {}),
    ...p.value.strategy,
    name: p.value.preset === DEFAULT_PRESET ? "custom" : p.value.preset,
  };
  downloadText("strategy.json", JSON.stringify(full, null, 2) + "\n");
}

const fileInput = ref<HTMLInputElement | null>(null);
const importNote = ref("");

async function importStrategy(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  importNote.value = "";
  try {
    const obj = JSON.parse(await file.text());
    const base = prover.baseStrategy;
    if (!base) throw new Error("engine defaults not loaded yet");
    const problems = strategyProblems(obj, base);
    if (problems.length) throw new Error(problems.join("; "));
    p.value.preset = DEFAULT_PRESET;
    p.value.strategy = strategyDiff(obj as Record<string, unknown>, base);
    jsonProblems.value = [];
    const n = Object.keys(p.value.strategy).length;
    importNote.value = `Imported ${file.name}: ${n} setting${n === 1 ? "" : "s"} differ from the default.`;
  } catch (err) {
    importNote.value = `Could not import ${file.name}: ${errMsg(err)}`;
  }
}

function resetStrategy() {
  p.value.preset = DEFAULT_PRESET;
  p.value.strategy = {};
  jsonProblems.value = [];
}
</script>

<template>
  <div class="prover-options" :class="{ disabled }">
    <div v-if="loadError" class="hint bad">{{ loadError }}</div>
    <fieldset :disabled="disabled">
      <div class="basics">
        <div>
          <label>backend</label>
          <Segmented
            v-model="prover.backend"
            :options="backendOptions"
            label="Prover backend"
          />
        </div>
        <div v-if="!hideTime" class="narrow">
          <label :for="`${profile}-time`">time limit (s)</label>
          <input
            :id="`${profile}-time`"
            v-model.number="p.timeLimitSecs"
            type="number"
            min="0"
          />
        </div>
        <div>
          <label :for="`${profile}-budget`">axiom selection</label>
          <div class="budget">
            <select :id="`${profile}-budget`" v-model="p.selection.mode">
              <option v-for="m in budgetModes" :key="m.value" :value="m.value">
                {{ m.label }}
              </option>
            </select>
            <input
              v-if="p.selection.mode === 'pct'"
              v-model.number="p.selection.pct"
              type="number"
              min="1"
              max="99"
              aria-label="Percent of the KB"
            />
            <input
              v-else-if="p.selection.mode === 'axioms'"
              v-model.number="p.selection.axioms"
              type="number"
              min="1"
              aria-label="Maximum axioms"
            />
            <input
              v-else-if="p.selection.mode === 'tolerance'"
              v-model.number="p.selection.tolerance"
              type="number"
              min="1"
              step="0.1"
              aria-label="SInE tolerance"
            />
          </div>
        </div>
      </div>
      <div class="hint sub">
        <template v-if="p.selection.mode === 'default'"
          >SInE picks the axioms relevant to the problem, starting from a budget
          of {{ defaultBudget ?? "~2000" }} and widening or narrowing it from
          the prover's feedback.</template
        >
        <template v-else-if="p.selection.mode === 'pct'"
          >Up to {{ p.selection.pct }}% of the KB's axioms may be
          selected.</template
        >
        <template v-else-if="p.selection.mode === 'axioms'"
          >Up to {{ p.selection.axioms }} axioms may be selected.</template
        >
        <template v-else-if="p.selection.mode === 'tolerance'"
          >A symbol triggers an axiom when it is at most
          {{ p.selection.tolerance }}× as common as the axiom's rarest symbol;
          lower is stricter.</template
        >
        <template v-else
          >Every axiom goes to the prover. Slow on large KBs.</template
        >
      </div>

      <Disclosure summary="Axiom selection details">
        <div class="grid">
          <div>
            <label :for="`${profile}-depth`">expansion depth</label>
            <input
              :id="`${profile}-depth`"
              :value="p.selection.depth ?? ''"
              type="number"
              min="0"
              placeholder="unlimited"
              @change="
                p.selection.depth = nullableNum(
                  ($event.target as HTMLInputElement).value,
                )
              "
            />
          </div>
          <div>
            <label :for="`${profile}-head`">bookkeeping filter</label>
            <select :id="`${profile}-head`" v-model="headFilter">
              <option value="">backend default</option>
              <option value="on">drop</option>
              <option value="off">keep</option>
            </select>
          </div>
          <div>
            <label :for="`${profile}-liu`">structural rescue</label>
            <select :id="`${profile}-liu`" v-model="liuRescue">
              <option value="">backend default</option>
              <option value="on">on</option>
              <option value="off">off</option>
            </select>
          </div>
          <div>
            <label :for="`${profile}-liu-rounds`">rescue rounds</label>
            <input
              :id="`${profile}-liu-rounds`"
              :value="p.selection.liuRounds ?? ''"
              type="number"
              min="0"
              placeholder="default"
              @change="
                p.selection.liuRounds = nullableNum(
                  ($event.target as HTMLInputElement).value,
                )
              "
            />
          </div>
          <div>
            <label :for="`${profile}-liu-k`">rescue per round</label>
            <input
              :id="`${profile}-liu-k`"
              :value="p.selection.liuTopK ?? ''"
              type="number"
              min="0"
              placeholder="default"
              @change="
                p.selection.liuTopK = nullableNum(
                  ($event.target as HTMLInputElement).value,
                )
              "
            />
          </div>
        </div>
        <label class="check mt"
          ><input v-model="p.selection.autoscale" type="checkbox" /> autoscale
          <span class="hint"
            >— rerun with a wider or narrower selection from the prover's
            feedback</span
          ></label
        >
        <div class="hint sub mt-sm">
          The bookkeeping filter drops <code>documentation</code>-style
          sentences; the structural rescue adds goal-near axioms SInE missed.
          <!-- TODO(core): these two only reach SUPr's ask path today; see
               augmentationStrategy in utils/proverOptions.ts. -->
          <strong v-if="!native"
            >{{ prover.backendLabel }} always uses its defaults for
            these.</strong
          >
          <strong v-else-if="profile === 'audit'"
            >Audits don't apply them yet.</strong
          >
        </div>
      </Disclosure>

      <Disclosure v-if="native" summary="Search (SUPr)">
        <div class="grid">
          <div>
            <label :for="`${profile}-steps`">max steps</label>
            <input
              :id="`${profile}-steps`"
              v-model.number="p.maxSteps"
              type="number"
              min="0"
              step="100"
            />
          </div>
          <div>
            <label :for="`${profile}-lits`">max literals</label>
            <input
              :id="`${profile}-lits`"
              v-model.number="p.maxLits"
              type="number"
              min="0"
            />
          </div>
          <div>
            <label :for="`${profile}-preset`">strategy preset</label>
            <select :id="`${profile}-preset`" v-model="p.preset">
              <option :value="DEFAULT_PRESET">default</option>
              <option
                v-for="pr in (prover.defaults?.presets ?? []).slice(1)"
                :key="pr.name"
                :value="pr.name"
              >
                {{ pr.name }}
              </option>
            </select>
          </div>
        </div>
        <div class="checks mt">
          <label class="check"
            ><input v-model="p.forwardClose" type="checkbox" /> forward closure
            <span class="hint"
              >— saturate ground facts before the loop</span
            ></label
          >
          <label class="check"
            ><input v-model="p.wantProof" type="checkbox" /> build proofs
            <span class="hint">— the proof, graph and prose</span></label
          >
          <label class="check"
            ><input v-model="p.profile" type="checkbox" /> profile
            <span class="hint">— phase timings in the raw output</span></label
          >
        </div>

        <div v-if="!presetStrategy" class="hint mt">Loading strategy…</div>
        <template v-else>
          <div v-for="g in STRATEGY_KNOBS" :key="g.title" class="knob-group">
            <div class="group-title">{{ g.title }}</div>
            <div class="grid">
              <div
                v-for="k in g.knobs"
                :key="k.key"
                :class="{ overridden: overridden(k) }"
                :title="k.help"
              >
                <label
                  v-if="k.kind !== 'bool'"
                  :for="`${profile}-k-${k.key}`"
                  >{{ k.label }}</label
                >
                <select
                  v-if="k.kind === 'choice'"
                  :id="`${profile}-k-${k.key}`"
                  :value="knobValue(k)"
                  @change="
                    setKnob(k, ($event.target as HTMLSelectElement).value)
                  "
                >
                  <option
                    v-for="c in k.choices"
                    :key="c.value"
                    :value="c.value"
                  >
                    {{ c.label }}
                  </option>
                </select>
                <input
                  v-else-if="k.kind === 'int'"
                  :id="`${profile}-k-${k.key}`"
                  :value="knobValue(k)"
                  type="number"
                  min="0"
                  @change="
                    setKnob(k, ($event.target as HTMLInputElement).value)
                  "
                />
                <label v-else class="check bool-knob"
                  ><input
                    type="checkbox"
                    :checked="!!knobValue(k)"
                    @change="
                      setKnob(k, ($event.target as HTMLInputElement).checked)
                    "
                  />
                  {{ k.label }}</label
                >
                <div class="hint sub">{{ k.help }}</div>
              </div>
            </div>
          </div>

          <div class="group-title">All strategy settings (JSON overrides)</div>
          <textarea
            v-model="jsonText"
            rows="5"
            spellcheck="false"
            placeholder='{ "pick_ratio": 3, "tier_weight": [1, 1, 4] }'
            @blur="applyJson"
          />
          <div v-for="(m, i) in jsonProblems" :key="i" class="hint bad">
            {{ m }}
          </div>
          <div class="hint sub">
            Keys are the engine's <code>Strategy</code> fields; anything left
            out keeps the preset's value. Applied when the box loses focus.
          </div>
          <div class="inline tight center mt">
            <button class="btn ghost" type="button" @click="fileInput?.click()">
              Import strategy.json
            </button>
            <button class="btn ghost" type="button" @click="exportStrategy">
              Export
            </button>
            <button class="btn ghost" type="button" @click="resetStrategy">
              Reset strategy
            </button>
            <input
              ref="fileInput"
              type="file"
              accept=".json,application/json"
              hidden
              @change="importStrategy"
            />
          </div>
          <div class="hint sub">
            Same format as <code>sumo --strategy strategy.json</code>.
            {{ importNote }}
          </div>
        </template>
      </Disclosure>

      <Disclosure v-else-if="prover.vampireSelected" summary="Vampire">
        <label :for="`${profile}-vargs`">extra command-line arguments</label>
        <input
          :id="`${profile}-vargs`"
          v-model="prover.vampireArgs"
          type="text"
          placeholder="e.g. --avatar off"
          spellcheck="false"
        />
        <div class="hint sub">
          Appended after the fixed arguments; later flags win.
        </div>
      </Disclosure>

      <Disclosure v-else-if="profile === 'audit'" summary="E audit subsets">
        <label class="check"
          ><input v-model="p.auditAxfilter" type="checkbox" /> audit e_axfilter
          subsets
          <span class="hint"
            >— check overlapping symbol-seeded subsets instead of one
            neighbourhood per axiom</span
          ></label
        >
        <div v-if="p.auditAxfilter" class="grid">
          <div>
            <label :for="`${profile}-subsets`">maximum subsets</label>
            <input
              :id="`${profile}-subsets`"
              v-model.number="p.auditSubsetLimit"
              type="number"
              min="1"
              max="1000"
            />
          </div>
          <div>
            <label :for="`${profile}-filter-time`">filter time limit (s)</label>
            <input
              :id="`${profile}-filter-time`"
              v-model.number="p.selectionTimeLimitSecs"
              type="number"
              min="1"
              max="3600"
            />
          </div>
        </div>
        <div class="hint sub">
          The time per check applies to each subset separately. Finding no
          contradiction doesn't establish that the whole KB is consistent.
        </div>
      </Disclosure>

      <div class="inline mt footer">
        <button class="btn ghost" type="button" @click="prover.reset(profile)">
          Reset all to defaults
        </button>
      </div>
    </fieldset>
  </div>
</template>

<style scoped>
.prover-options {
  margin-top: 12px;
  padding: 12px 14px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--bg);
}
fieldset {
  border: none;
  margin: 0;
  padding: 0;
  min-width: 0;
}
.basics {
  display: flex;
  flex-wrap: wrap;
  gap: 12px 18px;
  align-items: end;
}
.narrow input {
  width: 110px;
}
.budget {
  display: flex;
  gap: 6px;
}
.budget input {
  width: 100px;
}
.sub {
  font-size: 12px;
  margin-top: 4px;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
  gap: 10px 14px;
  margin-top: 8px;
}
.grid select {
  width: 100%;
}
.checks {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.knob-group {
  margin-top: 14px;
}
.group-title {
  font-size: 12px;
  font-weight: 600;
  margin-top: 14px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--muted);
}
.overridden label,
.overridden .bool-knob {
  color: var(--accent);
}
.overridden input,
.overridden select {
  border-color: color-mix(in srgb, var(--accent) 55%, var(--line));
}
.bool-knob {
  min-height: 41px;
}
textarea {
  margin-top: 6px;
}
.footer {
  justify-content: flex-end;
}
</style>
