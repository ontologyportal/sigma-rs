<script setup lang="ts">
/** Where contradictions are watched for: one row per GitHub issue
 *  tracker, report URL, or one-shot upload, each with the same update
 *  preference pill and "Check now" the constituent Sources card has
 *  (uploads have nothing to watch). New valid contradictions are imported
 *  into the engine's findings cache and surfaced on the Audit tab. */
import { computed, onActivated, onMounted, ref } from "vue";
import {
  useContradictionsStore,
  githubSource,
  urlSource,
  type ContradictionSource,
} from "../../stores/contradictions";
import type { UpdatePref } from "../../stores/kb";
import { navigate } from "../../router";
import { errMsg, fmtTime } from "../../utils/format";
import Card from "../Card.vue";
import Segmented from "../Segmented.vue";
import SourceLoc from "../SourceLoc.vue";

const store = useContradictionsStore();
onMounted(() => void store.refresh().catch(() => {}));
onActivated(() => void store.refresh().catch(() => {}));

const PREF_LABEL: Record<UpdatePref, string> = {
  "auto-update": "Auto-update",
  "auto-check": "Auto-check",
  "no-check": "No check",
};
const PREF_TITLE: Record<UpdatePref, string> = {
  "auto-update":
    "Auto-update: imports new contradictions on every load. Click to switch to auto-check.",
  "auto-check":
    "Auto-check: alerts when new contradictions are reported, without importing them. Click to switch to no-check.",
  "no-check":
    "No check: never looks for new contradictions. Click to switch to auto-update.",
};

type AddKind = "github" | "url" | "file";
const addKind = ref<AddKind>("github");
const addInput = ref("");
const addBusy = ref(false);
const addError = ref("");
const fileInput = ref<HTMLInputElement | null>(null);

const placeholder = computed(() =>
  addKind.value === "github"
    ? "owner/repository"
    : "https://example.org/contradictions.json",
);

async function add() {
  addError.value = "";
  if (addKind.value === "file") {
    fileInput.value?.click();
    return;
  }
  const value = addInput.value.trim();
  let source: ContradictionSource;
  if (addKind.value === "github") {
    const m =
      /^(?:https:\/\/github\.com\/)?([\w.-]+)\/([\w.-]+?)(?:\.git|\/.*)?$/.exec(
        value,
      );
    if (!m) {
      addError.value =
        "Enter a repository such as ontologyportal/sumo or its GitHub URL.";
      return;
    }
    source = githubSource(m[1], m[2]);
  } else {
    if (!/^https?:\/\//i.test(value)) {
      addError.value = "Enter an http(s) URL of a contradiction report.";
      return;
    }
    source = urlSource(value);
  }
  addBusy.value = true;
  try {
    store.addSource(source);
    await store.checkSource(source, true);
    addInput.value = "";
  } catch (e) {
    addError.value = errMsg(e);
  } finally {
    addBusy.value = false;
  }
}

async function selectFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  addBusy.value = true;
  addError.value = "";
  try {
    await store.importFile(file);
  } catch (err) {
    addError.value = errMsg(err);
  } finally {
    addBusy.value = false;
  }
}

function remove(source: ContradictionSource) {
  if (
    window.confirm(
      `Stop watching ${source.label}? Contradictions already recorded from it stay on the Audit tab.`,
    )
  )
    store.removeSource(source.id);
}

function detail(source: ContradictionSource): string {
  switch (source.kind) {
    case "github":
      return `Open issues titled [Contradiction] in github.com/${source.owner}/${source.repo}`;
    case "url":
      return source.url ?? "";
    case "file":
      return `Uploaded ${fmtTime(new Date(source.added))}`;
  }
}

function showFinding(key: string) {
  store.focusKey = key;
  navigate("audit");
}
</script>

<template>
  <Card title="Contradiction sources">
    <template #description>
      Where reported contradictions are watched for. A reported contradiction is
      recorded when every axiom it cites is in this knowledge base, and dropped
      as soon as one of them changes.
    </template>

    <div v-if="store.alerts.length" class="src-alerts">
      <div v-for="a in store.alerts" :key="a.key" class="src-alert">
        <span>{{ a.message }}</span>
        <span class="src-alert-actions">
          <button
            v-if="a.pending"
            class="src-alert-review"
            type="button"
            @click="store.applyPending(a)"
          >
            Import
          </button>
          <button
            v-else-if="a.keys?.length"
            class="src-alert-review"
            type="button"
            @click="showFinding(a.keys![0])"
          >
            Show
          </button>
          <button
            class="src-alert-dismiss"
            type="button"
            title="Dismiss"
            aria-label="Dismiss"
            @click="store.dismissAlert(a.key)"
          >
            &times;
          </button>
        </span>
      </div>
    </div>

    <div v-if="!store.sources.length" class="hint">
      No contradiction sources. Add one below.
    </div>

    <details v-for="s in store.sources" :key="s.id" class="src-report">
      <summary>
        <span class="src-label">{{ s.label }}</span>
        <span
          class="src-count"
          :title="'recorded contradictions from this source'"
          >{{ store.findingsFor(s).length }}</span
        >
        <button
          v-if="s.kind !== 'file'"
          class="src-pref"
          type="button"
          :class="`src-pref-${store.prefFor(s)}`"
          :title="PREF_TITLE[store.prefFor(s)]"
          @click.stop.prevent="store.cyclePref(s)"
        >
          {{ PREF_LABEL[store.prefFor(s)] }}
        </button>
        <button
          v-if="s.kind !== 'file'"
          class="src-pref src-update-now"
          type="button"
          :disabled="store.checking.includes(s.id)"
          title="Fetch and import new contradictions now"
          @click.stop.prevent="store.checkSource(s, true)"
        >
          {{ store.checking.includes(s.id) ? "Checking…" : "Check now" }}
        </button>
        <button
          class="src-pref src-remove"
          type="button"
          title="Stop watching this source"
          aria-label="Remove source"
          @click.stop.prevent="remove(s)"
        >
          &times;
        </button>
      </summary>
      <div class="src-body">
        <div class="hint src-detail">{{ detail(s) }}</div>
        <div v-if="!store.findingsFor(s).length" class="hint">
          No contradictions recorded from this source.
        </div>
        <button
          v-for="f in store.findingsFor(s)"
          :key="f.key"
          class="finding"
          type="button"
          title="Show on the Audit tab"
          @click="showFinding(f.key)"
        >
          <span class="finding-axioms">
            <SourceLoc
              v-for="c in f.culprits"
              :key="c.sid"
              :file="c.file ?? '?'"
              :line="c.line ?? 0"
              variant="loc"
            />
          </span>
          <time class="hint" :datetime="new Date(f.found_at_ms).toISOString()">
            {{ fmtTime(new Date(f.found_at_ms)) }}
          </time>
        </button>
      </div>
    </details>

    <form class="add" @submit.prevent="add">
      <Segmented
        v-model="addKind"
        :options="[
          { value: 'github', label: 'GitHub issues' },
          { value: 'url', label: 'Report URL' },
          { value: 'file', label: 'Upload' },
        ]"
        label="Source kind"
        :disabled="addBusy"
      />
      <input
        v-if="addKind !== 'file'"
        v-model="addInput"
        type="text"
        :placeholder="placeholder"
        aria-label="Source location"
        :disabled="addBusy"
      />
      <button class="btn ghost" type="submit" :disabled="addBusy">
        {{
          addBusy
            ? "Adding…"
            : addKind === "file"
              ? "Choose report file…"
              : "Add source"
        }}
      </button>
      <input
        ref="fileInput"
        type="file"
        accept=".json,application/json"
        hidden
        @change="selectFile"
      />
    </form>
    <p v-if="addError" class="hint bad" role="alert">{{ addError }}</p>
    <p class="hint">
      A source may be a repository whose <code>[Contradiction]</code> issues
      embed a proof report, a URL serving a report (the nightly
      <code>latest-contradictions.json</code>, a shared report, or
      <code>sumo audit --json</code> output), or a report file uploaded once.
    </p>
  </Card>
</template>

<style scoped>
/* Mirrors SourcesCard: a collapsible row per source with pill controls. */
.src-report {
  border-top: 1px solid var(--line);
  padding: 8px 0;
}
.src-report:first-of-type {
  border-top: none;
}
.src-report summary {
  cursor: pointer;
  font-size: 13px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
}
.src-label {
  flex-shrink: 0;
}
.src-count {
  display: inline-block;
  min-width: 15px;
  padding: 0 5px;
  margin-left: auto;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 700;
  line-height: 15px;
  text-align: center;
  background: color-mix(in srgb, var(--accent) 20%, transparent);
  color: var(--accent);
}
.src-pref {
  font: inherit;
  height: 22px;
  padding: 0 8px;
  font-size: 11px;
  font-weight: 600;
  line-height: 1;
  cursor: pointer;
  border: 1px solid var(--line);
  border-radius: 5px;
  background: var(--bg);
  color: var(--muted);
}
.src-pref:hover {
  border-color: var(--accent);
  color: var(--fg);
}
.src-pref-auto-update {
  border-color: color-mix(in srgb, var(--accent) 50%, var(--line));
  color: var(--accent);
}
.src-pref-auto-check {
  color: var(--fg);
}
.src-update-now:disabled {
  cursor: default;
  opacity: 0.6;
}
.src-remove {
  padding: 0 6px;
  font-size: 14px;
}
.src-body {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: 8px;
}
.src-detail {
  word-break: break-all;
}
.finding {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  width: 100%;
  padding: 6px 8px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--bg);
  color: var(--fg);
  font: inherit;
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}
.finding:hover {
  border-color: var(--accent);
}
.finding-axioms {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 10px;
  min-width: 0;
}
.finding time {
  flex-shrink: 0;
  font-variant-numeric: tabular-nums;
}

.src-alerts {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 10px;
}
.src-alert {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 6px 10px;
  border-radius: 6px;
  font-size: 12px;
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.src-alert-actions {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  gap: 8px;
}
.src-alert-review {
  font: inherit;
  font-size: 11px;
  font-weight: 700;
  line-height: 1;
  cursor: pointer;
  border: 1px solid var(--accent);
  border-radius: 5px;
  padding: 3px 8px;
  background: none;
  color: var(--accent);
}
.src-alert-review:hover {
  background: color-mix(in srgb, var(--accent) 15%, transparent);
}
.src-alert-dismiss {
  flex-shrink: 0;
  border: none;
  background: none;
  color: inherit;
  font-size: 15px;
  line-height: 1;
  cursor: pointer;
  padding: 0 2px;
  opacity: 0.7;
}
.src-alert-dismiss:hover {
  opacity: 1;
}

.add {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid var(--line);
}
.add input[type="text"] {
  flex: 1 1 220px;
  min-width: 0;
}
.add + .hint,
.add ~ .hint {
  margin: 8px 0 0;
}
</style>
