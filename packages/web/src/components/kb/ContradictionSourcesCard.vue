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
  type NotApplicable,
} from "../../stores/contradictions";
import type { UpdatePref } from "../../stores/kb";
import { navigate } from "../../router";
import { errMsg, fmtTime } from "../../utils/format";
import Card from "../Card.vue";
import JsonDialog from "../JsonDialog.vue";
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

/** One line summarizing a source's last check. */
function checkSummary(source: ContradictionSource): string {
  const c = store.checks[source.id];
  if (!c) return "Not checked yet this session.";
  const when = fmtTime(new Date(c.at));
  if (c.error) return `Last check ${when}: ${c.error}.`;
  const parts = [`${c.reported} reported`, `${c.recorded.length} applicable`];
  if (c.notApplicable.length)
    parts.push(`${c.notApplicable.length} not applicable`);
  if (c.unreadable) parts.push(`${c.unreadable} unreadable`);
  return `Last check ${when}: ${parts.join(", ")}.`;
}

/** The axioms a skipped entry is missing, by a quick text match. */
function isMissing(entry: NotApplicable, kif: string): boolean {
  return entry.missing.some((m) => m.kif === kif);
}

const jsonOpen = ref(false);
const jsonTitle = ref("");
const jsonValue = ref<unknown>(null);
const jsonName = ref("contradiction.json");
function showJson(title: string, value: unknown, name: string) {
  jsonTitle.value = title;
  jsonValue.value = value;
  jsonName.value = name;
  jsonOpen.value = true;
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
        <div class="src-check">
          <span class="hint">{{ checkSummary(s) }}</span>
          <button
            v-if="store.checks[s.id]?.raw"
            class="btn ghost small"
            type="button"
            title="The report as fetched"
            @click="
              showJson(
                `Report from ${s.label}`,
                store.checks[s.id]!.raw,
                'contradiction-report.json',
              )
            "
          >
            View raw report
          </button>
        </div>
        <div v-if="!store.findingsFor(s).length" class="hint">
          No contradictions recorded from this source.
        </div>
        <div v-for="f in store.findingsFor(s)" :key="f.key" class="finding">
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
          <span class="finding-actions">
            <button
              class="btn ghost small"
              type="button"
              title="Show on the Audit tab"
              @click="showFinding(f.key)"
            >
              Show
            </button>
            <button
              class="btn ghost small"
              type="button"
              title="The recorded contradiction as JSON"
              @click="showJson('Recorded contradiction', f, `${f.key}.json`)"
            >
              JSON
            </button>
          </span>
        </div>
        <details
          v-if="store.checks[s.id]?.notApplicable.length"
          class="not-applicable"
        >
          <summary>
            Not applicable to this knowledge base ({{
              store.checks[s.id]!.notApplicable.length
            }})
            <span class="hint">
              -- reported contradictions citing axioms this KB does not hold
            </span>
          </summary>
          <div
            v-for="(entry, i) in store.checks[s.id]!.notApplicable"
            :key="i"
            class="finding skipped"
          >
            <span class="finding-axioms">
              <span
                v-for="(a, j) in entry.axioms"
                :key="j"
                class="axiom"
                :class="{ missing: isMissing(entry, a.kif) }"
                :title="
                  isMissing(entry, a.kif)
                    ? 'Not in the loaded knowledge base'
                    : 'Present in the loaded knowledge base'
                "
              >
                <SourceLoc
                  v-if="a.file"
                  :file="a.file"
                  :line="a.line ?? 0"
                  variant="loc"
                />
                <code>{{ a.kif }}</code>
              </span>
            </span>
            <span class="hint reason">{{ entry.reason }}</span>
            <span class="finding-actions">
              <button
                class="btn ghost small"
                type="button"
                title="The reported contradiction as JSON"
                @click="
                  showJson(
                    'Reported contradiction (not applicable)',
                    entry,
                    'contradiction.json',
                  )
                "
              >
                JSON
              </button>
            </span>
          </div>
        </details>
      </div>
    </details>

    <JsonDialog
      v-model="jsonOpen"
      :title="jsonTitle"
      :value="jsonValue"
      :filename="jsonName"
    />

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
  list-style: none;
}
/* `display: flex` on a summary drops the browser's disclosure marker, so
   draw one: a chevron that turns down while the row is open. */
.src-report summary::-webkit-details-marker,
.not-applicable summary::-webkit-details-marker {
  display: none;
}
.src-report summary::before,
.not-applicable summary::before {
  content: "";
  flex-shrink: 0;
  width: 6px;
  height: 6px;
  margin: 0 4px 0 2px;
  border-right: 2px solid var(--muted);
  border-bottom: 2px solid var(--muted);
  transform: rotate(-45deg);
  transition: transform 0.15s ease;
}
.src-report[open] > summary::before,
.not-applicable[open] > summary::before {
  transform: rotate(45deg);
}
.src-report summary:hover::before,
.not-applicable summary:hover::before {
  border-color: var(--accent);
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
  flex-wrap: wrap;
  align-items: center;
  gap: 6px 10px;
  width: 100%;
  box-sizing: border-box;
  padding: 6px 8px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--bg);
  color: var(--fg);
  font-size: 12px;
}
.finding-actions {
  margin-left: auto;
  display: flex;
  gap: 6px;
}
.src-check {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px 10px;
}
.not-applicable summary {
  cursor: pointer;
  font-size: 12px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 6px;
  list-style: none;
}
.not-applicable .finding {
  margin-top: 6px;
}
.skipped .finding-axioms {
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
}
.axiom {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 6px;
}
.axiom code {
  font-family: var(--mono);
  font-size: 11px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.axiom.missing code {
  text-decoration: underline wavy color-mix(in srgb, var(--accent) 70%, red);
}
.axiom.missing::after {
  content: "missing";
  font-size: 10px;
  font-weight: 700;
  color: color-mix(in srgb, var(--accent) 70%, red);
}
.reason {
  flex-basis: 100%;
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
