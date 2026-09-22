<script setup lang="ts">
/**
 * The Edit tab's "ⓘ" panel: a file's KB footprint (axiom/term counts, the
 * documentation/typing/conditionals/facts breakdown, and the other loaded
 * files it depends on) plus its GitHub commit history -- the latter only for
 * a `sumo`-origin file (the former History tab's content, folded in here).
 */
import { computed, ref, watch } from "vue";
import BaseDialog from "../BaseDialog.vue";
import { fetchFileCommits, type RepoCommit } from "../../api/github";
import { navigate } from "../../router";
import { call } from "../../services/sigma";
import type { Origin } from "../../models/Origin";
import type { FileStats } from "sigmakee/sdk";
import { errMsg, fmtDate, fmtNum } from "../../utils/format";

const props = defineProps<{
  /** Open state (v-model). */
  modelValue: boolean;
  /** The file shown; null renders an empty dialog. */
  current: { name: string; origin: Origin } | null;
}>();

const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

const stats = ref<FileStats | null>(null);
const statsStatus = ref("");
const statsError = ref("");

const commits = ref<RepoCommit[]>([]);
const commitsStatus = ref("");
const commitsError = ref("");

const fromSumo = computed(() => props.current?.origin.kind === "sumo");

const num = (v: number | undefined) =>
  v !== undefined && Number.isFinite(v) ? fmtNum(v) : "—";

async function loadStats(c: { name: string; origin: Origin }) {
  statsStatus.value = "loading…";
  statsError.value = "";
  stats.value = null;
  try {
    const r = await call("fileStats", { file: c.name });
    if (props.current !== c) return; // a newer request won
    stats.value = r.stats;
    statsStatus.value = r.stats ? "" : "No axioms loaded from this file.";
  } catch (e) {
    if (props.current !== c) return;
    statsStatus.value = "";
    statsError.value = errMsg(e);
  }
}

async function loadCommits(c: { name: string; origin: Origin }) {
  commits.value = [];
  commitsError.value = "";
  if (c.origin.kind !== "sumo") {
    commitsStatus.value = "";
    return;
  }
  commitsStatus.value = "loading…";
  try {
    const result = await fetchFileCommits(c.name);
    if (props.current !== c) return;
    commitsStatus.value = `${result.length} commit${result.length === 1 ? "" : "s"}`;
    commits.value = result;
  } catch (e) {
    if (props.current !== c) return;
    commitsStatus.value = "";
    commitsError.value = errMsg(e);
  }
}

function load() {
  const c = props.current;
  if (!props.modelValue || !c) {
    stats.value = null;
    statsStatus.value = "";
    statsError.value = "";
    commits.value = [];
    commitsStatus.value = "";
    commitsError.value = "";
    return;
  }
  loadStats(c);
  loadCommits(c);
}

watch(() => [props.modelValue, props.current] as const, load, {
  immediate: true,
});

function close() {
  emit("update:modelValue", false);
}

function gotoFile(name: string) {
  close();
  navigate("edit", { file: name });
}

const allCommitsUrl = computed(() => {
  const c = props.current;
  return c && fromSumo.value
    ? `https://github.com/ontologyportal/sumo/commits/HEAD/${encodeURI(c.name)}`
    : "";
});

function commitMsg(c: RepoCommit): string {
  return (c.commit?.message || "(no message)").split("\n")[0];
}
function commitWho(c: RepoCommit): string {
  return c.commit?.author?.name || c.author?.login || "unknown";
}
function commitWhen(c: RepoCommit): string {
  const iso = c.commit?.author?.date;
  return iso ? fmtDate(new Date(iso)) : "";
}
function commitSha(c: RepoCommit): string {
  return (c.sha || "").slice(0, 7);
}
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    :title="current ? current.name : 'File info'"
    width="min(560px, 94vw)"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <p v-if="!current" class="hint">
      Open or save a file to see its KB footprint and edit history.
    </p>
    <template v-else>
      <section class="stats-section">
        <p v-if="statsStatus" class="hint">{{ statsStatus }}</p>
        <p v-else-if="statsError" class="hint bad">{{ statsError }}</p>
        <template v-else-if="stats">
          <div class="stats">
            <div class="stat">
              <div class="stat-n">{{ num(stats.axioms) }}</div>
              <div class="stat-l">axioms</div>
            </div>
            <div class="stat">
              <div class="stat-n">{{ num(stats.terms) }}</div>
              <div class="stat-l">terms</div>
            </div>
            <div class="stat">
              <div class="stat-n">{{ num(stats.terms_unique) }}</div>
              <div class="stat-l">terms unique to this file</div>
            </div>
            <div class="stat">
              <div class="stat-n">{{ num(stats.depends_on.length) }}</div>
              <div class="stat-l">files depended on</div>
            </div>
          </div>

          <div class="kinds">
            <div class="kind">
              <span class="kind-l">Facts</span>
              <span class="kind-n">{{ fmtNum(stats.axiom_kinds.facts) }}</span>
            </div>
            <div class="kind">
              <span class="kind-l">Conditionals</span>
              <span class="kind-n">{{
                fmtNum(stats.axiom_kinds.conditionals)
              }}</span>
            </div>
            <div class="kind">
              <span class="kind-l">Typing</span>
              <span class="kind-n">{{ fmtNum(stats.axiom_kinds.typing) }}</span>
            </div>
            <div class="kind">
              <span class="kind-l">Documentation</span>
              <span class="kind-n">{{
                fmtNum(stats.axiom_kinds.documentation)
              }}</span>
            </div>
          </div>

          <div v-if="stats.depends_on.length" class="depends">
            <span class="hint">Depends on:</span>
            <button
              v-for="f in stats.depends_on"
              :key="f"
              type="button"
              class="dep-chip"
              @click="gotoFile(f)"
            >
              {{ f }}
            </button>
          </div>
        </template>
      </section>

      <section v-if="fromSumo" class="history-section">
        <div class="inline between">
          <h4>History</h4>
          <span class="hint">{{ commitsStatus }}</span>
        </div>
        <p v-if="commitsError" class="hint bad">{{ commitsError }}</p>
        <ol v-else-if="commits.length" class="timeline">
          <li v-for="c in commits" :key="c.sha">
            <div class="commit-msg">
              <a :href="c.html_url || '#'" target="_blank" rel="noopener">{{
                commitMsg(c)
              }}</a>
            </div>
            <div class="commit-meta">
              {{ commitWho(c)
              }}<template v-if="commitWhen(c)"> · {{ commitWhen(c) }}</template>
              · <span class="sha">{{ commitSha(c) }}</span>
            </div>
          </li>
        </ol>
        <div v-if="commits.length" class="hint more">
          <a :href="allCommitsUrl" target="_blank" rel="noopener"
            >Full commit history on GitHub ↗</a
          >
        </div>
      </section>
    </template>
    <template #actions>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="close">Close</button>
    </template>
  </BaseDialog>
</template>

<style scoped>
h4 {
  margin: 0;
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--muted);
}
.stats-section {
  margin-bottom: 16px;
}
.stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
  gap: 8px;
  margin-bottom: 10px;
}
.stat {
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 10px;
  text-align: center;
}
.stat-n {
  font-size: 18px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}
.stat-l {
  font-size: 11px;
  color: var(--muted);
  margin-top: 2px;
}
.kinds {
  display: flex;
  flex-wrap: wrap;
  gap: 6px 14px;
  padding: 8px 0 0;
  border-top: 1px solid var(--line);
}
.kind {
  display: flex;
  align-items: baseline;
  gap: 5px;
  font-size: 12px;
}
.kind-l {
  color: var(--muted);
}
.kind-n {
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.depends {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin-top: 10px;
}
.dep-chip {
  font: inherit;
  font-size: 12px;
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 999px;
  color: var(--fg);
  cursor: pointer;
  padding: 3px 10px;
}
.dep-chip:hover {
  border-color: var(--accent);
  color: var(--accent);
}
.history-section {
  padding-top: 14px;
  border-top: 1px solid var(--line);
}
.inline.between {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
}
ol.timeline {
  list-style: none;
  margin: 0;
  padding: 0 0 0 18px;
  border-left: 2px solid var(--line);
  max-height: 260px;
  overflow-y: auto;
}
ol.timeline li {
  position: relative;
  padding: 0 0 14px 14px;
}
ol.timeline li::before {
  content: "";
  position: absolute;
  left: -23px;
  top: 5px;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--accent);
  box-shadow: 0 0 0 3px var(--card);
}
ol.timeline li:last-child {
  padding-bottom: 0;
}
.commit-msg {
  font-size: 13px;
}
.commit-msg a {
  color: var(--fg);
}
.commit-msg a:hover {
  color: var(--accent);
}
.commit-meta {
  font-size: 11px;
  color: var(--muted);
  margin-top: 2px;
}
.commit-meta .sha {
  font-family: var(--mono);
}
.more {
  margin-top: 10px;
}
</style>
