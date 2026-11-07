<script setup lang="ts">
/** Knowledge base tab, in four sub-tabs (`?view=`): the unified constituent
 *  table (with the standard-set presets and the Import dialog in its
 *  header), the Sources card, the contradiction sources, and the WordNet
 *  panel. */
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import ProviderIcon from "../components/ProviderIcon.vue";
import { useAuthStore } from "../stores/auth";
import { gitlabRequest, coordinates } from "../api/gitlab";
import { fetchRepoBranches } from "../api/github";
import { Constituent } from "../models/Constituent";
import { GitOrigin } from "../models/Origin";
import { useKBStore } from "../stores/kb";
import { useLibraryStore } from "../stores/library";
import { useStatus } from "../composables/useStatus";
import { useTabQuery } from "../composables/useTabQuery";
import { updateParams } from "../router";
import { formatSize } from "../utils/format";
import BaseDialog from "../components/BaseDialog.vue";
import Card from "../components/Card.vue";
import DropMenu from "../components/DropMenu.vue";
import StatusLine from "../components/StatusLine.vue";
import ConstituentTable from "../components/kb/ConstituentTable.vue";
import ContradictionSourcesCard from "../components/kb/ContradictionSourcesCard.vue";
import ImportDialog from "../components/kb/ImportDialog.vue";
import SourcesCard from "../components/kb/SourcesCard.vue";
import WordNetPanel from "../components/kb/WordNetPanel.vue";
import SubTabs from "../components/SubTabs.vue";

const auth = useAuthStore();
const kb = useKBStore();
const library = useLibraryStore();

onMounted(() => library.loadCatalogs());
onActivated(() => library.loadCatalogs());
// Accepting an update moves a pinned source's commit: list its files anew.
watch(
  () => library.repos.map((r) => library.catalogRef(r)).join("|"),
  () => library.loadCatalogs(),
);

type View = "constituents" | "sources" | "contradictions" | "wordnet";
const VIEWS: { value: View; label: string }[] = [
  { value: "constituents", label: "Constituents" },
  { value: "sources", label: "Sources" },
  { value: "contradictions", label: "Contradictions" },
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

const activeRepository = computed(() => {
  const loaded = kb.constituents.find((c) => c.origin.kind === "sumo")
    ?.origin as GitOrigin | undefined;
  const source = loaded ?? library.defaultRepo;
  const provider = source.service ?? "github";
  const host =
    provider === "gitlab"
      ? source.server || "https://gitlab.com"
      : "https://github.com";
  return {
    provider,
    name: `${source.owner}/${source.repo}`,
    url: `${host}/${source.owner.split("/").map(encodeURIComponent).join("/")}/${encodeURIComponent(source.repo)}`,
  };
});

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
function selectRepoProvider(provider: "github" | "gitlab") {
  if (
    branchesBusy.value ||
    presetsBusy.value ||
    (provider === "gitlab" && !auth.gitlabUser)
  )
    return;
  service.value = provider;
  if (provider === "gitlab") server.value = auth.gitlabUser!.server;
}
function switchRepository() {
  if (service.value === "gitlab" && !auth.gitlabUser) service.value = "github";
  branchMenuOpen.value = false;
  branches.value = [];
  branchError.value = "";
  repoFormOpen.value = true;
}
const repoFormOpen = ref(false);
const loadedOrigin = kb.constituents.find((c) => c.origin.kind === "sumo")
  ?.origin as GitOrigin | undefined;
const service = ref<"github" | "gitlab">(
  loadedOrigin?.service ?? library.defaultRepo.service ?? "github",
);
const listedService = ref(service.value);
const server = ref(loadedOrigin?.server ?? library.defaultRepo.server ?? "");
const listedServer = ref(server.value);
const owner = ref(loadedOrigin?.owner ?? library.defaultRepo.owner);
const repo = ref(loadedOrigin?.repo ?? library.defaultRepo.repo);
const branch = ref(loadedOrigin?.branch ?? library.defaultRepo.branch);
const branchBtn = ref<HTMLElement | null>(null);
const branchMenuOpen = ref(false);
const repository = ref(`${owner.value}/${repo.value}`);
const listedRepo = ref(repository.value);
watch(service, () => {
  repository.value = "";
  branches.value = [];
  branchError.value = "";
});
const branches = ref<string[]>([]);
const branchSearch = ref("");
const branchesBusy = ref(false);
const branchError = ref("");
watch(
  [service, server, repository],
  () => {
    branches.value = [];
    branchError.value = "";
  },
  { flush: "sync" },
);
const filteredBranches = computed(() =>
  branches.value.filter((name) =>
    name.toLowerCase().includes(branchSearch.value.toLowerCase()),
  ),
);

watch(
  () => str(query.value.chooseRepo),
  (provider) => {
    if (provider !== "github" && provider !== "gitlab") return;
    service.value =
      provider === "gitlab" && !auth.gitlabUser ? "github" : provider;
    if (provider === "gitlab") server.value = str(query.value.server);
    repository.value = "";
    branches.value = [];
    branchError.value = "";
    repoFormOpen.value = true;
    void updateParams({
      ...query.value,
      chooseRepo: undefined,
      server: undefined,
    });
    branchMenuOpen.value = false;
  },
  { immediate: true },
);

async function listBranches() {
  if (branchesBusy.value) return;
  let value = repository.value.trim();
  if (service.value === "gitlab" && /^https?:\/\//i.test(value)) {
    try {
      const project = new URL(value);
      if (project.protocol !== "https:" || project.username || project.password)
        throw new Error();
      const parts = project.pathname.split("/").filter(Boolean);
      const separator = parts.indexOf("-");
      value = (separator < 0 ? parts : parts.slice(0, separator))
        .join("/")
        .replace(/\.git$/, "");
      server.value = project.origin;
      repository.value = value;
    } catch {
      branchError.value =
        "Enter an HTTPS GitLab project URL or namespace/project path.";
      return;
    }
  }
  if (
    !(
      service.value === "gitlab"
        ? /^[\w.-]+(?:\/[\w.-]+)+$/
        : /^[\w.-]+\/[\w.-]+$/
    ).test(value)
  ) {
    branchError.value =
      "Enter a project path including its name, such as group/project. A group URL alone has no branches.";
    return;
  }
  branchesBusy.value = true;
  branchError.value = "";
  branches.value = [];
  branchSearch.value = "";
  try {
    const parts = value.split("/");
    const name = parts.pop()!;
    const account = parts.join("/");
    branches.value =
      service.value === "gitlab"
        ? await gitlabRequest<string[]>(
            "branches",
            coordinates({
              owner: account,
              repo: name,
              branch: branch.value,
              server: server.value || undefined,
            }),
          )
        : await fetchRepoBranches(account, name);
    listedService.value = service.value;
    listedServer.value = server.value;
    listedRepo.value = value;
  } catch (e) {
    branchError.value = e instanceof Error ? e.message : String(e);
  } finally {
    branchesBusy.value = false;
  }
}

async function loadSelectedRepository() {
  if (branchesBusy.value || presetsBusy.value) return;
  if (service.value === "gitlab" && !auth.gitlabUser) return;
  await listBranches();
  if (branchError.value) return;
  const initialBranch = branches.value.includes("master")
    ? "master"
    : branches.value.includes("main")
      ? "main"
      : "";
  if (!initialBranch) {
    branchError.value = "This repository has no master or main branch.";
    return;
  }
  await selectBranch(initialBranch);
}

async function toggleBranches() {
  branchMenuOpen.value = !branchMenuOpen.value;
  if (branchMenuOpen.value) {
    const active = kb.constituents.find((c) => c.origin.kind === "sumo")
      ?.origin as GitOrigin | undefined;
    const source = active ?? library.defaultRepo;
    service.value = source.service ?? "github";
    await Promise.resolve();
    server.value = source.server ?? "";
    repository.value = `${owner.value}/${repo.value}`;
    await listBranches();
  }
}

async function selectBranch(name: string) {
  const parts = listedRepo.value.split("/");
  const repositoryName = parts.pop()!;
  const account = parts.join("/");
  branchMenuOpen.value = false;
  await loadPreset(
    currentPaths.value.length ? "current" : "minimal",
    new GitOrigin(
      listedService.value,
      account,
      repositoryName,
      name,
      listedServer.value || undefined,
    ),
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
      service.value,
      owner.value.trim(),
      repo.value.trim(),
      branch.value.trim(),
      server.value || undefined,
    );
  if (!origin.owner || !origin.repo || !origin.branch) {
    tableLog.set("Enter an owner or namespace, repository, and branch.", true);
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
      service.value = origin.service;
      server.value = origin.server ?? "";
      owner.value = origin.owner;
      repo.value = origin.repo;
      branch.value = origin.branch;
      repoFormOpen.value = false;
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
    class="constituents-card"
    :description="summary"
  >
    <template #header>
      <a
        class="active-repository"
        :href="activeRepository.url"
        :title="activeRepository.url"
        target="_blank"
        rel="noopener noreferrer"
        aria-label="Current repository"
      >
        <ProviderIcon :provider="activeRepository.provider" :size="22" />
        <span>{{ activeRepository.name }}</span>
      </a>
    </template>
    <div class="constituent-actions">
      <div class="import-actions">
        <button
          class="btn ghost"
          type="button"
          :disabled="presetsBusy"
          @click="importOpen = true"
          title="Import local files, a folder, or a file URL"
        >
          Import files
        </button>
        <button
          class="btn ghost"
          type="button"
          :disabled="presetsBusy"
          @click="switchRepository"
          title="Switch the active knowledge base to a GitHub or GitLab repository"
        >
          Switch repository
        </button>
      </div>
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
    </div>
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

  <BaseDialog
    v-model="repoFormOpen"
    title="Switch repository"
    width="min(520px, 92vw)"
    :dismissible="!presetsBusy"
  >
    <div class="repository-dialog">
      <p class="hint">
        Choose the repository to use as your knowledge base. Switching replaces
        the current source and loads master or main automatically.
      </p>
      <form
        id="switch-repository-form"
        @submit.prevent="loadSelectedRepository"
      >
        <div
          class="repo-provider-tabs"
          role="tablist"
          aria-label="Repository provider"
        >
          <button
            v-for="provider in ['github', 'gitlab'] as const"
            :key="provider"
            :id="`repo-${provider}-tab`"
            type="button"
            role="tab"
            :aria-selected="service === provider"
            aria-controls="repo-provider-panel"
            :disabled="
              branchesBusy ||
              presetsBusy ||
              (provider === 'gitlab' && !auth.gitlabUser)
            "
            :title="
              provider === 'gitlab' && !auth.gitlabUser
                ? 'Sign in with GitLab to switch to a GitLab repository'
                : undefined
            "
            @click="selectRepoProvider(provider)"
          >
            <ProviderIcon :provider="provider" :size="22" />
            {{ provider === "github" ? "GitHub" : "GitLab" }}
          </button>
        </div>
        <div
          id="repo-provider-panel"
          role="tabpanel"
          :aria-labelledby="`repo-${service}-tab`"
          class="repo-provider-panel"
        >
          <label
            >Repository
            <input
              v-model="repository"
              placeholder="namespace/project or full project URL"
              :disabled="branchesBusy"
            />
          </label>
          <p class="hint">
            Enter namespace/project, including any subgroups, or paste the full
            GitLab project URL.
          </p>
        </div>
      </form>
      <p v-if="branchError" class="hint" role="alert">{{ branchError }}</p>
    </div>
    <template #actions>
      <button
        class="btn ghost"
        :disabled="presetsBusy"
        @click="repoFormOpen = false"
      >
        Cancel
      </button>
      <button
        class="btn"
        :disabled="!repository.trim() || branchesBusy || presetsBusy"
        type="submit"
        form="switch-repository-form"
      >
        {{ presetsBusy || branchesBusy ? "Loading..." : "Switch repository" }}
      </button>
    </template>
  </BaseDialog>

  <ImportDialog
    v-model="importOpen"
    sources="files"
    @imported="(msg) => tableLog.set(msg)"
  />

  <SourcesCard v-show="view === 'sources'" />

  <ContradictionSourcesCard v-show="view === 'contradictions'" />

  <Card v-show="view === 'wordnet'" title="WordNet lexicon">
    <template #description>
      Powers synonym-aware search (results tagged "wn") — fetched from the same
      <code>ontologyportal/sumo</code> repo as the KIF constituents.
    </template>
    <WordNetPanel />
  </Card>
</template>

<style scoped>
.constituents-card :deep(.card-head) {
  align-items: center;
}

.active-repository {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  max-width: 100%;
  font-size: 17px;
  font-weight: 600;
  text-decoration: none;
  color: var(--fg);
}
.active-repository span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.active-repository:hover {
  text-decoration: underline;
}
.constituent-actions {
  margin: 14px 0;
}

.repo-provider-tabs {
  display: flex;
  gap: 4px;
  padding: 4px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--bg);
}
.repo-provider-tabs button {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  padding: 12px;
  font: inherit;
  font-weight: 600;
  color: var(--muted);
  border: 1px solid transparent;
  border-radius: 7px;
  background: transparent;
  cursor: pointer;
}
.repo-provider-tabs button[aria-selected="true"] {
  background: var(--card);
  color: var(--fg);
  border-color: var(--line);
}
.repo-provider-tabs button:disabled {
  opacity: 0.45;
  cursor: default;
}
.repo-provider-tabs button:focus-visible {
  outline: 2px solid currentColor;
  outline-offset: 2px;
}
.repo-provider-panel {
  display: grid;
  gap: 12px;
}

.repository-dialog,
.repository-dialog form,
.repository-dialog label {
  display: grid;
  gap: 12px;
}
.repository-dialog {
  gap: 18px;
}
.repository-dialog input,
.repository-dialog select {
  width: 100%;
  box-sizing: border-box;
}

.constituent-actions,
.import-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
@media (max-width: 640px) {
  .constituent-actions {
    gap: 10px;
  }
}

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
