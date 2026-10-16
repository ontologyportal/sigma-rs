<script setup lang="ts">
/** Knowledge base tab, in three sub-tabs (`?view=`): the unified constituent
 *  table (with the standard-set presets and the Import dialog in its
 *  header), the Sources card, and the WordNet panel. */
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import { fetchRepoBranches } from "../api/github";
import { Constituent } from "../models/Constituent";
import { GitOrigin } from "../models/Origin";
import { useKBStore } from "../stores/kb";
import { useLibraryStore } from "../stores/library";
import { useStatus } from "../composables/useStatus";
import { useTabQuery } from "../composables/useTabQuery";
import { updateParams } from "../router";
import { formatSize } from "../utils/format";
import Card from "../components/Card.vue";
import DropMenu from "../components/DropMenu.vue";
import StatusLine from "../components/StatusLine.vue";
import ConstituentTable from "../components/kb/ConstituentTable.vue";
import ImportDialog from "../components/kb/ImportDialog.vue";
import SourcesCard from "../components/kb/SourcesCard.vue";
import WordNetPanel from "../components/kb/WordNetPanel.vue";
import SubTabs from "../components/SubTabs.vue";

const kb = useKBStore();
const library = useLibraryStore();

onMounted(() => library.loadCatalogs());
onActivated(() => library.loadCatalogs());
// Accepting an update moves a pinned source's commit: list its files anew.
watch(
  () => library.repos.map((r) => library.catalogRef(r)).join("|"),
  () => library.loadCatalogs(),
);

type View = "constituents" | "sources" | "wordnet";
const VIEWS: { value: View; label: string }[] = [
  { value: "constituents", label: "Constituents" },
  { value: "sources", label: "Sources" },
  { value: "wordnet", label: "WordNet" },
];
const { query, str } = useTabQuery(["kb"]);
const view = computed<View>(() => {
  const v = str(query.value.view);
  return VIEWS.some((o) => o.value === v) ? (v as View) : "constituents";
});
function setView(v: View) {
  updateParams(v === "constituents" ? {} : { view: v });
}

const tableLog = useStatus();

const summary = computed(() => {
  const loaded = kb.constituents.length;
  const bytes = kb.constituents.reduce((sum, c) => sum + c.text.length, 0);
  return `${loaded} loaded · ${library.size} in library · ${formatSize(bytes)} loaded`;
});

// -- Standard constituent sets ------------------------------------------------
//
// File lists mirror the Sigma XML configuration. Order is preserved from it:
// ingest is order-independent here (everything is promoted together at the
// end), but keeping it makes the two lists diffable against the source.

const PRESETS: Record<string, { label: string; files: string[] }> = {
  minimal: {
    label: "Minimal SUMO",
    files: [
      "Merge.kif",
      "Mid-level-ontology.kif",
      "english_format.kif",
      "domainEnglishFormat.kif",
    ],
  },
  full: {
    label: "Full SUMO",
    files: Constituent.defaults().map((c) => c.name),
  },
};

const presetsBusy = ref(false);
const presetBtn = ref<HTMLElement | null>(null);
const presetMenuOpen = ref(false);
const importOpen = ref(false);
const loadedOrigin = kb.constituents.find((c) => c.origin.kind === "sumo")
  ?.origin as GitOrigin | undefined;
const owner = ref(loadedOrigin?.owner ?? library.defaultRepo.owner);
const repo = ref(loadedOrigin?.repo ?? library.defaultRepo.repo);
const branch = ref(loadedOrigin?.branch ?? library.defaultRepo.branch);
const branchBtn = ref<HTMLElement | null>(null);
const branchMenuOpen = ref(false);
const repository = ref(`${owner.value}/${repo.value}`);
const listedRepo = ref(repository.value);
const branches = ref<string[]>([]);
const branchSearch = ref("");
const branchesBusy = ref(false);
const branchError = ref("");
const filteredBranches = computed(() =>
  branches.value.filter((name) =>
    name.toLowerCase().includes(branchSearch.value.toLowerCase()),
  ),
);

async function listBranches() {
  if (branchesBusy.value) return;
  const value = repository.value.trim();
  if (!/^[\w.-]+\/[\w.-]+$/.test(value)) {
    branchError.value =
      "Enter a repository such as ontologyportal/sumo or your-username/sumo.";
    return;
  }
  branchesBusy.value = true;
  branchError.value = "";
  branches.value = [];
  branchSearch.value = "";
  try {
    const [account, name] = value.split("/");
    branches.value = await fetchRepoBranches(account, name);
    listedRepo.value = value;
  } catch (e) {
    branchError.value = e instanceof Error ? e.message : String(e);
  } finally {
    branchesBusy.value = false;
  }
}

async function toggleBranches() {
  branchMenuOpen.value = !branchMenuOpen.value;
  if (branchMenuOpen.value) {
    repository.value = `${owner.value}/${repo.value}`;
    await listBranches();
  }
}

async function selectBranch(name: string) {
  const [account, repositoryName] = listedRepo.value.split("/");
  branchMenuOpen.value = false;
  await loadPreset(
    currentPaths.value.length ? "current" : "minimal",
    new GitOrigin("github", account, repositoryName, name),
  );
}
onBeforeRouteLeave(() => !presetsBusy.value);

const currentPaths = computed(() => {
  if (
    !kb.constituents.length ||
    kb.constituents.some((c) => c.origin.kind !== "sumo")
  )
    return [];
  const origins = new Set(
    kb.constituents.map((c) => (c.origin as GitOrigin).id),
  );
  if (origins.size !== 1) return [];
  return kb.constituents.map((c) => (c.origin as GitOrigin).pathOf(c.name));
});

async function loadPreset(key: string, target?: GitOrigin) {
  if (presetsBusy.value) return;
  presetMenuOpen.value = false;
  const preset =
    key === "current"
      ? { label: "Current constituent set", files: currentPaths.value }
      : PRESETS[key];
  const origin =
    target ??
    new GitOrigin(
      "github",
      owner.value.trim(),
      repo.value.trim(),
      branch.value.trim(),
    );
  if (!origin.owner || !origin.repo || !origin.branch) {
    tableLog.set("Enter a GitHub owner, repository, and branch.", true);
    return;
  }
  if (
    !window.confirm(
      `Load ${preset.label} from ${origin.label}? This replaces the whole knowledge base and discards its local edits and any saved edits for the incoming files. Push or export work you want to keep first.`,
    )
  )
    return;
  presetsBusy.value = true;
  try {
    const total = preset.files.length;
    tableLog.set(`Fetching ${preset.label} — 0/${total}…`);
    const { failed } = await kb.loadBranch(origin, preset.files, (n) =>
      tableLog.set(`Fetching ${preset.label} — ${n}/${total}…`),
    );
    if (!failed.length) {
      owner.value = origin.owner;
      repo.value = origin.repo;
      branch.value = origin.branch;
    }

    tableLog.set(
      failed.length
        ? `${preset.label}: loaded ${kb.constituents.length}/${total}, ${failed.length} failed — ${failed[0]}`
        : `${origin.label}: ${preset.label} loaded — ${kb.constituents.length} constituents.`,
      failed.length > 0,
    );
  } catch (e) {
    tableLog.fail(e);
  } finally {
    presetsBusy.value = false;
  }
}
</script>

<template>
  <SubTabs :model-value="view" :options="VIEWS" @update:model-value="setView" />

  <Card
    v-show="view === 'constituents'"
    title="Constituents"
    :description="summary"
  >
    <template #header>
      <span class="inline tight">
        <button
          ref="branchBtn"
          class="btn ghost branch-button"
          type="button"
          :title="branch"
          :disabled="presetsBusy"
          :aria-expanded="branchMenuOpen"
          aria-haspopup="menu"
          @click="toggleBranches"
        >
          <span>Branch:</span> <span class="branch-name">{{ branch }}</span>
          <span aria-hidden="true">▾</span>
        </button>
        <button
          ref="presetBtn"
          class="btn ghost"
          type="button"
          :disabled="presetsBusy"
          :aria-expanded="presetMenuOpen"
          aria-haspopup="menu"
          @click="presetMenuOpen = !presetMenuOpen"
        >
          {{ presetsBusy ? "Loading…" : "Load a standard set ▾" }}
        </button>
        <button class="btn ghost" type="button" @click="importOpen = true">
          Import…
        </button>
      </span>
    </template>
    <DropMenu v-model="branchMenuOpen" :anchor="branchBtn">
      <div class="branch-picker" @keydown.esc="branchMenuOpen = false">
        <strong>{{ listedRepo }}</strong>
        <label class="branch-filter"
          >Find a branch
          <input
            v-model="branchSearch"
            type="search"
            placeholder="Search branches"
            :disabled="branchesBusy"
          />
        </label>
        <p v-if="branchesBusy" class="hint" role="status">
          Loading branches...
        </p>
        <p v-else-if="branchError" class="hint" role="alert">
          {{ branchError }}
        </p>
        <div v-else class="branch-list">
          <button
            v-for="name in filteredBranches"
            :key="name"
            type="button"
            role="menuitemradio"
            :aria-checked="name === branch && listedRepo === `${owner}/${repo}`"
            @click="selectBranch(name)"
          >
            <span>{{ name }}</span>
            <span
              v-if="name === branch && listedRepo === `${owner}/${repo}`"
              class="hint"
              >Current</span
            >
          </button>
          <p v-if="!filteredBranches.length" class="hint">
            No matching branches.
          </p>
        </div>
        <p class="hint">
          Selecting a branch loads your current files, or Minimal SUMO if no
          single source is loaded.
        </p>
        <details>
          <summary>Use another repository or fork</summary>
          <form @submit.prevent="listBranches">
            <label
              >GitHub repository
              <input
                v-model="repository"
                placeholder="your-username/sumo"
                :disabled="branchesBusy"
              />
            </label>
            <p class="hint">
              Use account/repository, for example ontologyportal/sumo. For your
              fork, replace ontologyportal with your GitHub username.
            </p>
            <button class="btn ghost" type="submit" :disabled="branchesBusy">
              Show branches
            </button>
          </form>
        </details>
      </div>
    </DropMenu>
    <DropMenu v-model="presetMenuOpen" :anchor="presetBtn">
      <button
        v-for="(preset, key) in PRESETS"
        :key="key"
        type="button"
        role="menuitem"
        @click="loadPreset(key)"
      >
        {{ preset.label }}
      </button>
    </DropMenu>
    <ConstituentTable @log="(text, error) => tableLog.set(text, error)" />
    <StatusLine class="mt-sm" :text="tableLog.text" :error="tableLog.error" />
  </Card>

  <ImportDialog v-model="importOpen" @imported="(msg) => tableLog.set(msg)" />

  <SourcesCard v-show="view === 'sources'" />

  <Card v-show="view === 'wordnet'" title="WordNet lexicon">
    <template #description>
      Powers synonym-aware search (results tagged "wn") — fetched from the same
      <code>ontologyportal/sumo</code> repo as the KIF constituents.
    </template>
    <WordNetPanel />
  </Card>
</template>

<style scoped>
.branch-button {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  max-width: min(260px, 60vw);
  min-width: 0;
}
.branch-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}
.branch-button > :first-child,
.branch-button > :last-child {
  flex-shrink: 0;
}

.branch-picker {
  width: min(340px, calc(100vw - 32px));
  padding: 10px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-height: 70vh;
  overflow-y: auto;
}
.branch-picker label {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.branch-picker input {
  width: 100%;
  box-sizing: border-box;
}
.branch-picker p {
  margin: 0;
}
.branch-picker summary {
  cursor: pointer;
}
.branch-picker form {
  display: grid;
  gap: 10px;
  margin-top: 12px;
}
.branch-list {
  max-height: 240px;
  overflow-y: auto;
}
.branch-list button {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  width: 100%;
  padding: 8px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--fg);
  text-align: left;
  cursor: pointer;
  overflow-wrap: anywhere;
}
.branch-list button:hover,
.branch-list button:focus-visible {
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
</style>
