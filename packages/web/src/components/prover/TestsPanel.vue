<script setup lang="ts">
import { ref } from "vue";
import BusyButton from "../BusyButton.vue";
import Card from "../Card.vue";
import StatusLine from "../StatusLine.vue";
import { useKBStore } from "../../stores/kb";
import { useTestsStore, type TestEntry } from "../../stores/tests";
import { errMsg } from "../../utils/format";

/** The imported-tests list (Ask/Tell's "Open test" panel): run, open into
 *  the panes, or remove each test; run them all. */
const emit = defineEmits<{
  /** The user picked a test to load into the Ask/Tell panes. */
  open: [t: TestEntry];
}>();

/** The status line under the list (v-model:log) -- shared with the Ask/Tell
 *  tab, whose Load/Save test buttons report here too. */
const log = defineModel<string>("log", { default: "" });

const tests = useTestsStore();
const kb = useKBStore();

/** `name|origin` of the test whose single run is in flight, if any. */
const running = ref<string | null>(null);
const runningAll = ref(false);

const key = (t: TestEntry) => `${t.name}|${t.origin}`;

/** Files a test's `(extra-files ...)` directive names that aren't loaded. */
const missingFiles = (p: any): string[] =>
  (p.extraFiles || []).filter(
    (f: string) => !kb.constituents.some((c) => c.name.endsWith(f)),
  );

const shortQuery = (q: string) => (q.length > 60 ? q.slice(0, 60) + "…" : q);

async function run(t: TestEntry) {
  running.value = key(t);
  try {
    await tests.run(t);
  } catch (err) {
    t.outcome = { cls: "bad", label: errMsg(err).slice(0, 60) };
  } finally {
    running.value = null;
  }
}

async function runAll() {
  runningAll.value = true;
  try {
    const { pass, ran } = await tests.runAll((t) => {
      log.value = `Running ${t.name}…`;
    });
    log.value = `${pass}/${ran} passed.`;
  } catch (e) {
    log.value = errMsg(e);
  } finally {
    runningAll.value = false;
  }
}
</script>

<template>
  <Card>
    <label
      >Tests — <code>.kif.tq</code> / <code>.p</code> /
      <code>.tptp</code></label
    >
    <div v-if="!tests.tests.length" class="hint">
      No tests imported. Use "Load test" above to import a
      <code>.kif.tq</code>, <code>.p</code>, or <code>.tptp</code>
      file — tests run against the loaded KB instead of joining it.
    </div>
    <ul class="results">
      <li v-for="t in tests.tests" :key="key(t)" class="loaded-row">
        <span>
          <span class="sym">{{ t.name }}</span>
          <span v-if="t.parsed.note" class="hint">{{ t.parsed.note }}</span>
          <code v-if="t.parsed.queryKif" class="hint">{{
            shortQuery(t.parsed.queryKif)
          }}</code>
          <span v-else class="hint">no (query)</span>
          <span v-if="t.parsed.expectedProof != null" class="hint"
            >expects {{ t.parsed.expectedProof ? "yes" : "no" }}</span
          >
          <span v-if="t.parsed.expectedAnswer" class="hint"
            >answer: {{ t.parsed.expectedAnswer.join(" ") }}</span
          >
          <span v-if="missingFiles(t.parsed).length" class="hint warn"
            >needs {{ missingFiles(t.parsed).join(", ") }}</span
          >
          <span
            v-if="t.outcome"
            class="tq-badge"
            :class="'tq-' + t.outcome.cls"
            >{{ t.outcome.label }}</span
          >
        </span>
        <span>
          <template v-if="t.parsed.queryKif">
            <a @click="run(t)">{{ running === key(t) ? "running…" : "run" }}</a>
            ·
          </template>
          <a @click="emit('open', t)">open</a> ·
          <a class="rm" @click="tests.remove(t.name, t.origin)">remove</a>
        </span>
      </li>
    </ul>
    <div class="inline center run-row">
      <BusyButton
        v-show="tests.tests.length"
        :busy="runningAll"
        label="Run all"
        @click="runAll"
      />
      <StatusLine :text="log" />
    </div>
  </Card>
</template>

<style scoped>
.warn {
  color: var(--warn);
}
.run-row {
  margin-top: 8px;
}
.tq-badge {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: 9px;
  font-weight: 600;
}
.tq-ok {
  background: color-mix(in srgb, var(--ok) 18%, transparent);
  color: var(--ok);
}
.tq-bad {
  background: color-mix(in srgb, var(--bad) 18%, transparent);
  color: var(--bad);
}
.tq-mut {
  background: color-mix(in srgb, var(--muted) 18%, transparent);
  color: var(--muted);
}
</style>
