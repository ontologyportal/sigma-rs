<script setup lang="ts">
/**
 * History tab: a file's commit timeline from GitHub.
 *
 * Plain GitHub REST -- the same public API the Knowledge base tab uses for
 * the file catalog, so no token is required. That caps an unauthenticated
 * visitor at 60 requests/hour per IP, so results are cached per file for the
 * session and only refetched on an explicit Refresh.
 */
import { computed, ref, watch } from "vue";
import Card from "../components/Card.vue";
import { SUMO } from "../constants";
import { githubApi } from "../api/github";
import { updateParams } from "../router";
import { useKBStore } from "../stores/kb";
import { useTabQuery } from "../composables/useTabQuery";
import { errMsg, fmtDate } from "../utils/format";

const kb = useKBStore();
const { query, onQuery, str } = useTabQuery(["history"]);

/** Only `sumo`-origin constituents exist on GitHub; uploads/URLs have no history. */
const files = computed(() => kb.sumoNames);

/** The URL's `?file=` when it names a loaded SUMO file, else the first one. */
const file = computed(() => {
  const f = str(query.value.file);
  return files.value.includes(f) ? f : (files.value[0] ?? "");
});

const status = ref("");
const commits = ref<any[]>([]);
const error = ref("");

const cache = new Map<string, any[]>(); // file -> commits[]
let shown: string | null = null; // file currently rendered, so re-entry is free

async function fetchCommits(name: string): Promise<any[]> {
  const hit = cache.get(name);
  if (hit) return hit;
  const result = await githubApi(
    `/repos/${SUMO.owner}/${SUMO.repo}/commits?path=${encodeURIComponent(name)}&per_page=30`,
  );
  cache.set(name, result);
  return result;
}

async function load(name: string, { force = false } = {}) {
  if (!name) {
    commits.value = [];
    error.value = "";
    status.value = "";
    shown = null;
    return;
  }
  if (!force && name === shown && cache.has(name)) return;
  if (force) cache.delete(name);
  shown = name;
  status.value = "loading…";
  commits.value = [];
  error.value = "";
  try {
    const result = await fetchCommits(name);
    if (shown !== name) return; // a newer request won
    status.value = `${result.length} commit${result.length === 1 ? "" : "s"}`;
    commits.value = result;
  } catch (e) {
    if (shown !== name) return;
    status.value = "";
    shown = null; // let a retry re-fetch
    error.value = errMsg(e);
  }
}

onQuery(() => load(file.value));
watch(file, (f) => load(f));

function onPickerChange(e: Event) {
  updateParams({ file: (e.target as HTMLSelectElement).value });
}

function refresh() {
  return load(file.value, { force: true });
}

const allCommitsUrl = computed(() =>
  file.value
    ? `https://github.com/${SUMO.owner}/${SUMO.repo}/commits/${SUMO.ref}/${encodeURI(file.value)}`
    : "",
);

function commitMsg(c: any): string {
  return (c.commit?.message || "(no message)").split("\n")[0];
}
function commitWho(c: any): string {
  return c.commit?.author?.name || c.author?.login || "unknown";
}
function commitWhen(c: any): string {
  const iso = c.commit?.author?.date;
  return iso ? fmtDate(new Date(iso)) : "";
}
function commitSha(c: any): string {
  return (c.sha || "").slice(0, 7);
}
</script>

<template>
  <Card>
    <div class="inline" style="justify-content: space-between">
      <div style="min-width: 260px">
        <label for="historyPicker">File</label>
        <select id="historyPicker" :value="file" @change="onPickerChange">
          <option v-if="!files.length" value="">
            (no SUMO-sourced files loaded)
          </option>
          <option v-for="f in files" :key="f" :value="f">{{ f }}</option>
        </select>
      </div>
      <div class="inline" style="gap: 8px">
        <span class="hint">{{ status }}</span>
        <button class="btn" type="button" @click="refresh">Refresh</button>
      </div>
    </div>
    <p class="hint" style="margin: 8px 0 0">
      Commit history from <code>ontologyportal/sumo</code> via the public GitHub
      API (no sign-in; unauthenticated requests are rate-limited to 60/hour).
    </p>
  </Card>
  <div>
    <Card v-if="error" class="hint" style="color: var(--bad)">{{ error }}</Card>
    <Card v-else-if="file && !commits.length && !status" class="hint">
      No commits found for <code>{{ file }}</code
      >.
    </Card>
    <Card v-else-if="commits.length">
      <ol class="timeline">
        <li v-for="c in commits" :key="c.sha">
          <div class="commit-msg">
            <a :href="c.html_url || '#'" target="_blank" rel="noopener">{{
              commitMsg(c)
            }}</a>
          </div>
          <div class="commit-meta">
            {{ commitWho(c)
            }}<template v-if="commitWhen(c)"> · {{ commitWhen(c) }}</template> ·
            <span class="sha">{{ commitSha(c) }}</span>
          </div>
        </li>
      </ol>
      <div
        class="hint"
        style="
          margin-top: 12px;
          padding-top: 10px;
          border-top: 1px solid var(--line);
        "
      >
        Showing the {{ commits.length }} most recent —
        <a :href="allCommitsUrl" target="_blank" rel="noopener"
          >full commit history for {{ file }} on GitHub ↗</a
        >
      </div>
    </Card>
  </div>
</template>

<style scoped>
ol.timeline {
  list-style: none;
  margin: 0;
  padding: 0 0 0 18px;
  border-left: 2px solid var(--line);
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
  box-shadow: 0 0 0 3px var(--bg);
}
ol.timeline li:last-child {
  padding-bottom: 0;
}
.commit-msg {
  font-size: 14px;
}
.commit-msg a {
  color: var(--fg);
}
.commit-msg a:hover {
  color: var(--accent);
}
.commit-meta {
  font-size: 12px;
  color: var(--muted);
  margin-top: 2px;
}
.commit-meta .sha {
  font-family: var(--mono);
}
</style>
