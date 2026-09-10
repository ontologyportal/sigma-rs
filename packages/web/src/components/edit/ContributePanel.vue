<script setup lang="ts">
/**
 * The list of unpushed changes and the pull request that carries them
 * upstream to `ontologyportal/sumo`.
 *
 * The panel acts on a SELECTION, not on whichever file the editor happens to
 * show: every tracked change (see stores/changes.ts) is listed with a
 * checkbox, and the chosen files go up as one commit. What gets pushed is
 * each file's SAVED text, so a buffer with unsaved edits is refused rather
 * than silently contributing a stale version of a file the user is still
 * typing in.
 *
 * One submission is one branch. A selection spanning two open pull requests
 * has no single branch to commit on and is refused by name; a selection that
 * touches exactly one adds its commit to that branch, which updates the open
 * PR in place instead of opening a competing second one.
 */
import { computed, reactive, ref, watch } from "vue";
import { SUMO } from "../../constants";
import { contributeFiles } from "../../api/github";
import { useAuthStore } from "../../stores/auth";
import {
  useChangesStore,
  isActionable,
  type ChangeRow,
} from "../../stores/changes";
import { useKBStore } from "../../stores/kb";
import type { Origin } from "../../models/Origin";
import { errMsg } from "../../utils/format";

const props = defineProps<{
  /** Whether the panel is showing; opening it refreshes the change list. */
  open: boolean;
  /** The file in the editor, or null for a scratch buffer. */
  current: { name: string; origin: Origin } | null;
  /** True when the editor holds edits that have not been saved. */
  dirty: boolean;
}>();

const emit = defineEmits<{
  /** Show the diff / conflict dialog for a tracked change. */
  diff: [row: ChangeRow];
}>();

const auth = useAuthStore();
const changes = useChangesStore();
const kb = useKBStore();

const rowKey = (r: { name: string; origin: string }) => `${r.origin}:${r.name}`;
const textOf = (r: ChangeRow) => kb.find(r.name, r.origin)?.text ?? "";

// Which rows go in the next pull request, and the repo path chosen for each
// local file. Keyed by `origin:name` and reset when the row disappears.
const selected = reactive(new Set<string>());
const known = new Set<string>();
const localPaths = reactive(new Map<string, string>());

const rows = computed(() => changes.rows);

/**
 * Seed the selection for rows appearing for the first time: actionable changes
 * default to included, everything else (a file already in review, a local
 * scratch file that may never be meant for upstream) defaults to excluded.
 * An explicit tick by the user is never overridden.
 */
function syncSelection(list: ChangeRow[]) {
  const live = new Set(list.map(rowKey));
  for (const set of [selected, known]) {
    for (const k of [...set]) if (!live.has(k)) set.delete(k);
  }
  for (const k of [...localPaths.keys()])
    if (!live.has(k)) localPaths.delete(k);
  for (const r of list) {
    const k = rowKey(r);
    if (known.has(k)) continue;
    known.add(k);
    if (isActionable(r)) selected.add(k);
  }
}

watch(rows, syncSelection, { immediate: true });

const STATE_LABEL: Record<ChangeRow["state"], string> = {
  modified: "modified",
  amended: "changed since PR",
  review: "in review",
  local: "local only",
};

const groups = computed(() =>
  [
    {
      label: "From GitHub",
      rows: rows.value.filter((r) => r.origin === "sumo"),
    },
    {
      label: "Local uploads and new files",
      rows: rows.value.filter((r) => r.origin !== "sumo"),
    },
  ].filter((g) => g.rows.length),
);

const countText = computed(() =>
  rows.value.length ? `(${rows.value.length})` : "(none)",
);
const chosen = computed(() =>
  rows.value.filter((r) => selected.has(rowKey(r))),
);

const openPrs = (list: ChangeRow[]) =>
  new Map(
    list
      .filter((r) => r.proposed)
      .map((r) => [r.proposed!.number, r.proposed!]),
  );

const submitLabel = computed(() => {
  const prs = openPrs(chosen.value);
  return prs.size === 1
    ? `Update pull request #${[...prs.keys()][0]}`
    : "Create pull request";
});
const submitDisabled = computed(() => !chosen.value.length);

/** A title that describes the selection, not whatever file is open. */
function defaultTitle(list: ChangeRow[]) {
  if (list.length === 1) return `Update ${list[0].name}`;
  return list.length ? `Update ${list.length} SUMO files` : "Update SUMO";
}

const listOpen = ref(false);
const title = ref("");
const body = ref("");
const status = reactive({ text: "", isError: false });
const submitBusy = ref(false);

type Result =
  | { kind: "text"; text: string }
  | { kind: "error"; text: string }
  | {
      kind: "pr";
      amended: boolean;
      url: string;
      number: number;
      branch: string;
      forked: boolean;
      what: string;
    };
const result = ref<Result | null>(null);

function setStatus(text: string, bad = false) {
  status.text = text;
  status.isError = bad;
}

const pathId = (r: ChangeRow) => `ghPath-${rowKey(r)}`;
const pathOf = (r: ChangeRow) => localPaths.get(rowKey(r)) ?? r.path;

function toggle(r: ChangeRow, e: Event) {
  const k = rowKey(r);
  if ((e.target as HTMLInputElement).checked) selected.add(k);
  else selected.delete(k);
}

function setPath(r: ChangeRow, e: Event) {
  localPaths.set(rowKey(r), (e.target as HTMLInputElement).value);
}

async function onOpened() {
  setStatus("");
  if (!title.value) title.value = defaultTitle(chosen.value);
  // Costs at most one tree read plus one read per tracked pull request, and
  // only when something is actually tracked (both are no-ops otherwise).
  await changes.refreshProposals();
  await changes.refreshUpstreamShas({ force: true });
}

watch(
  () => props.open,
  (on) => {
    if (on) onOpened();
  },
);

async function submit() {
  const token = auth.token;
  result.value = null;

  // Everything that can be judged without the network first, so an unusable
  // selection is reported as such rather than as "you're not logged in".
  let picked = chosen.value;
  if (!picked.length) {
    setStatus("Tick at least one file to include.", true);
    return;
  }

  // What goes up is the SAVED text; an unsaved buffer would be pushed as its
  // last-saved version without the user realizing.
  const open = props.current;
  if (open && selected.has(`${open.origin.kind}:${open.name}`) && props.dirty) {
    setStatus(
      `Save ${open.name} first — the editor has unsaved changes.`,
      true,
    );
    return;
  }

  // One submission is one branch. Checked before anything touches the network
  // so an impossible selection fails immediately.
  const prs = openPrs(picked);
  if (prs.size > 1) {
    const names = [...prs.keys()].map((n) => `#${n}`).join(" and ");
    setStatus(
      `The selected files belong to different pull requests (${names}) — a single commit can only go on one branch, so submit them separately.`,
      true,
    );
    return;
  }

  const missingPath = picked.find((r) => !pathOf(r)?.trim());
  if (missingPath) {
    setStatus(`Give ${missingPath.name} a path in the repository.`, true);
    return;
  }

  // Defensive: the GitHub button already gates opening this panel on being
  // signed in, but a session can expire while the panel is open.
  if (!token) {
    auth.openLoginDialog();
    return;
  }

  submitBusy.value = true;
  try {
    // Authoritative staleness check: the panel's own check may be minutes old,
    // and this is the last moment before a write.
    setStatus("Checking for upstream changes…");
    await changes.refreshUpstreamShas({ force: true });
    const fresh = chosen.value;
    const landed = picked.filter(
      (c) => !fresh.some((f) => rowKey(f) === rowKey(c)),
    );
    picked = fresh;
    if (!picked.length) {
      setStatus("");
      result.value = {
        kind: "text",
        text: landed.length
          ? `Nothing left to submit — upstream already has ${landed.map((r) => r.name).join(", ")}.`
          : "Nothing selected to submit.",
      };
      return;
    }
    const stale = picked.find((r) => r.stale);
    if (stale) {
      setStatus(
        `${stale.name} changed upstream — resolve it before submitting.`,
        true,
      );
      emit("diff", stale);
      return;
    }
    // Recomputed post-refresh: the file that carried the pull request may have
    // landed in the meantime and dropped out of the selection.
    const existing = [...openPrs(picked).values()][0] ?? null;

    const files = picked.map((r) => ({
      name: r.name,
      origin: r.origin,
      path: pathOf(r).trim(),
      content: textOf(r),
    }));
    const prTitle = title.value.trim() || defaultTitle(picked);
    const pr = await contributeFiles({
      token,
      owner: SUMO.owner,
      repo: SUMO.repo,
      files,
      title: prTitle,
      body: body.value.trim(),
      existing,
      onStep: (s) => setStatus(s),
    });
    changes.markProposed(
      files.map((f, i) => ({ ...f, blobSha: pr.blobShas[i] })),
      pr,
    );
    setStatus("");
    const what = `${files.length} file${files.length === 1 ? "" : "s"}`;
    result.value = {
      kind: "pr",
      amended: pr.amended,
      url: pr.url,
      number: pr.number,
      branch: pr.branch,
      forked: pr.forked,
      what,
    };
  } catch (e) {
    setStatus("");
    result.value = { kind: "error", text: errMsg(e) };
  } finally {
    submitBusy.value = false;
  }
}
</script>

<template>
  <div class="settings">
    <!-- Every tracked change, collapsed by default: the badge says how
       many there are, this says which. -->
    <div class="gh-changes">
      <button
        class="gh-changes-toggle"
        type="button"
        :aria-expanded="listOpen"
        @click="listOpen = !listOpen"
      >
        <span class="tri">{{ listOpen ? "▾" : "▸" }}</span> Files changed
        <span class="hint">{{ countText }}</span>
      </button>
      <div v-show="listOpen" class="gh-changes-list">
        <div v-if="!rows.length" class="hint">
          No changes yet — files you edit and save appear here until they are
          pushed.
        </div>
        <template v-for="g in groups" :key="g.label">
          <div class="gh-group">{{ g.label }}</div>
          <template v-for="r in g.rows" :key="rowKey(r)">
            <div class="gh-row" :class="{ stale: r.stale }">
              <input
                type="checkbox"
                :checked="selected.has(rowKey(r))"
                :aria-label="`Include ${r.name} in the pull request`"
                @change="toggle(r, $event)"
              />
              <span class="gh-row-name mono">{{ r.name }}</span>
              <span class="gh-chip" :class="'st-' + r.state">{{
                STATE_LABEL[r.state]
              }}</span>
              <span v-if="r.stale" class="gh-chip st-stale"
                >upstream changed</span
              >
              <a
                v-if="r.proposed"
                class="gh-chip st-pr"
                :href="r.proposed.url"
                target="_blank"
                rel="noopener"
                >#{{ r.proposed.number }} ↗</a
              >
              <a
                v-if="r.prClosed"
                class="gh-chip st-closed"
                :href="r.prClosed.url"
                target="_blank"
                rel="noopener"
                >#{{ r.prClosed.number }}
                {{ r.prClosed.merged ? "merged" : "closed" }} ↗</a
              >
              <span class="gh-row-gap"></span>
              <a
                v-if="r.origin === 'sumo'"
                class="gh-act"
                @click="emit('diff', r)"
                >{{ r.stale ? "resolve" : "diff" }}</a
              >
            </div>
            <div
              v-if="r.origin !== 'sumo' && selected.has(rowKey(r))"
              class="gh-row-path"
            >
              <label :for="pathId(r)">Path in repository</label>
              <input
                type="text"
                :id="pathId(r)"
                class="gh-path"
                :value="pathOf(r)"
                spellcheck="false"
                @input="setPath(r, $event)"
              />
            </div>
          </template>
        </template>
      </div>
    </div>
    <p class="hint" style="margin: 0 0 10px">
      Opens a pull request against <code>ontologyportal/sumo</code> with the
      ticked files, as one commit. You will be forked automatically if you lack
      push access.
    </p>
    <div class="settings-grid">
      <div>
        <label for="ghTitle">Pull request title</label>
        <input
          id="ghTitle"
          v-model="title"
          type="text"
          placeholder="Update Merge.kif"
        />
      </div>
    </div>
    <div style="margin-top: 10px">
      <label for="ghBody">Description</label>
      <textarea
        id="ghBody"
        v-model="body"
        rows="3"
        placeholder="What changed and why."
      ></textarea>
    </div>
    <div
      class="inline"
      style="justify-content: space-between; margin-top: 10px"
    >
      <span
        class="hint"
        :style="{ color: status.isError ? 'var(--bad)' : '' }"
        >{{ status.text }}</span
      >
      <div class="inline" style="gap: 8px">
        <button
          class="btn"
          type="button"
          :disabled="submitDisabled || submitBusy"
          @click="submit"
        >
          {{ submitBusy ? "Submitting…" : submitLabel }}
        </button>
      </div>
    </div>
    <div class="hint" style="margin-top: 8px">
      <template v-if="result?.kind === 'pr'">
        <template v-if="result.amended"
          >Added {{ result.what }} to
          <a :href="result.url" target="_blank" rel="noopener"
            >pull request #{{ result.number }} ↗</a
          >
          on <code>{{ result.branch }}</code
          >.</template
        >
        <template v-else
          >Opened
          <a :href="result.url" target="_blank" rel="noopener"
            >pull request #{{ result.number }} ↗</a
          >
          with {{ result.what }} from <code>{{ result.branch }}</code
          >{{ result.forked ? " (via your fork)" : "" }}.</template
        >
      </template>
      <span v-else-if="result?.kind === 'error'" class="bad">{{
        result.text
      }}</span>
      <template v-else-if="result">{{ result.text }}</template>
    </div>
  </div>
</template>

<style scoped>
/* Contribute panel: the collapsible list of tracked changes */
.gh-changes {
  margin-bottom: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--line);
}
.gh-changes-toggle {
  font: inherit;
  font-size: 13px;
  display: inline-flex;
  align-items: center;
  gap: 7px;
  background: none;
  border: none;
  color: var(--fg);
  cursor: pointer;
  padding: 2px 0;
}
.gh-changes-toggle:hover {
  color: var(--accent);
}
.gh-changes-toggle .tri {
  color: var(--muted);
  font-size: 11px;
  width: 10px;
}
.gh-changes-list {
  margin-top: 8px;
}
.gh-group {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: var(--muted);
  margin: 10px 0 4px;
}
.gh-group:first-child {
  margin-top: 0;
}
.gh-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 6px;
  border-radius: 6px;
}
.gh-row:hover {
  background: color-mix(in srgb, var(--accent) 7%, transparent);
}
.gh-row.stale {
  background: color-mix(in srgb, var(--warn) 9%, transparent);
}
.gh-row input[type="checkbox"] {
  width: auto;
  margin: 0;
  flex: 0 0 auto;
}
.gh-row-name {
  font-size: 13px;
  overflow-wrap: anywhere;
}
.gh-row-gap {
  flex: 1 1 auto;
}
.gh-act {
  font-size: 12px;
  cursor: pointer;
  flex: 0 0 auto;
}
.gh-chip {
  font-size: 10px;
  padding: 1px 7px;
  border-radius: 9px;
  font-weight: 600;
  white-space: nowrap;
  background: color-mix(in srgb, var(--muted) 18%, transparent);
  color: var(--muted);
}
.gh-chip.st-modified {
  background: color-mix(in srgb, var(--accent) 18%, transparent);
  color: var(--accent);
}
.gh-chip.st-amended {
  background: color-mix(in srgb, var(--op) 20%, transparent);
  color: var(--op);
}
.gh-chip.st-stale {
  background: color-mix(in srgb, var(--warn) 22%, transparent);
  color: var(--warn);
}
.gh-chip.st-pr {
  background: color-mix(in srgb, var(--ok) 18%, transparent);
  color: var(--ok);
}
.gh-row-path {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 6px 8px 30px;
}
.gh-row-path label {
  font-size: 11px;
  color: var(--muted);
  margin: 0;
  flex: 0 0 auto;
}
.gh-row-path input {
  font-family: var(--mono);
  font-size: 12px;
  padding: 3px 6px;
  flex: 1 1 auto;
}
.bad {
  color: var(--bad);
}
</style>
