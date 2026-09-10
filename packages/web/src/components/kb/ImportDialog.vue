<script setup lang="ts">
/** "Import into the library": add local files (singly or a folder), a
 *  single-file URL, or a whole GitHub repo+branch. `accept` picks which
 *  local/URL files count: `.kif` constituents (the Knowledge base tab) or
 *  test files (the Problems tab); a repo is listed whole either way.
 *  Nothing here loads into the KB -- imported files show up as `available`
 *  rows in the table. Emits `imported` with a one-line summary the tab logs. */
import { computed, ref, watch } from "vue";
import { GitOrigin } from "../../models/Origin";
import { useKBStore } from "../../stores/kb";
import {
  acceptsFile,
  useLibraryStore,
  isDefaultRepo,
  originForRepo,
  type ImportAccept,
  type RepoRef,
} from "../../stores/library";
import { useStatus } from "../../composables/useStatus";
import BaseDialog from "../BaseDialog.vue";
import BusyButton from "../BusyButton.vue";
import StatusLine from "../StatusLine.vue";

const props = withDefaults(
  defineProps<{
    /** Open state (v-model). */
    modelValue: boolean;
    /** Which local/URL files to take: `.kif` constituents or test files. */
    accept?: ImportAccept;
  }>(),
  { accept: "kif" },
);

/** The `<input type=file>` accept list and the wording for `accept`. */
const kinds = computed(() =>
  props.accept === "kif"
    ? { exts: ".kif", label: ".kif", example: "ontology.kif" }
    : { exts: ".tq,.p,.tptp", label: "test", example: "TQG1.kif.tq" },
);

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
  imported: [message: string];
}>();

const kb = useKBStore();
const library = useLibraryStore();

type Mode = "local" | "url" | "github";
const MODES: { id: Mode; label: string }[] = [
  { id: "local", label: "Local" },
  { id: "url", label: "Website" },
  { id: "github", label: "GitHub" },
];
const mode = ref<Mode>("local");
const busy = ref(false);
const status = useStatus();

// -- Local --------------------------------------------------------------------

const files = ref<File[]>([]);
const fileInput = ref<HTMLInputElement | null>(null);
const folderInput = ref<HTMLInputElement | null>(null);

function onFilesChange(e: Event) {
  const input = e.target as HTMLInputElement;
  files.value = Array.from(input.files ?? []).filter((f) =>
    acceptsFile(props.accept, f.webkitRelativePath || f.name),
  );
  status.clear();
}

function resetFileInputs() {
  files.value = [];
  if (fileInput.value) fileInput.value.value = "";
  if (folderInput.value) folderInput.value.value = "";
}

// -- Website ------------------------------------------------------------------

const url = ref("");

// -- GitHub -------------------------------------------------------------------

const owner = ref(library.defaultRepo.owner);
const repo = ref(library.defaultRepo.repo);
const branch = ref(library.defaultRepo.branch);

/** A repo with loaded constituents cannot be removed. */
const loadedRepoIds = computed(() => {
  const ids = new Set<string>();
  for (const c of kb.constituents)
    if (c.origin.kind === "sumo") ids.add((c.origin as GitOrigin).id);
  return ids;
});
const removable = (r: RepoRef) =>
  !isDefaultRepo(r) && !loadedRepoIds.value.has(library.repoId(r));

function removeRepo(r: RepoRef) {
  if (!removable(r)) return;
  library.removeRepo(r);
  status.set(`Removed ${originForRepo(r).label}.`);
}

// -- Import -------------------------------------------------------------------

async function doImport() {
  busy.value = true;
  try {
    let message = "";
    if (mode.value === "local") {
      if (!files.value.length) {
        status.set(
          `Choose one or more ${kinds.value.label} files first.`,
          true,
        );
        return;
      }
      status.set(`Importing ${files.value.length} file(s)…`);
      const { added, skipped } = await library.importFiles(
        files.value,
        props.accept,
      );
      message =
        `Imported ${added.length} file(s) into the library` +
        (skipped.length
          ? ` (${skipped.length} skipped, not ${kinds.value.label}).`
          : ".");
      resetFileInputs();
    } else if (mode.value === "url") {
      if (!url.value.trim()) {
        status.set("Enter a URL first.", true);
        return;
      }
      status.set("Fetching…");
      const entry = await library.importUrl(url.value);
      message = `Imported ${entry.name} into the library.`;
      url.value = "";
    } else {
      const r: RepoRef = {
        owner: owner.value,
        repo: repo.value,
        branch: branch.value,
      };
      status.set(`Listing ${originForRepo(r).label}…`);
      await library.addRepo(r);
      const n = library.catalogs[library.repoId(r)]?.length ?? 0;
      message = `Registered ${originForRepo(r).label} — ${n} file(s).`;
    }
    status.set(message);
    emit("imported", message);
  } catch (e) {
    status.fail(e);
  } finally {
    busy.value = false;
  }
}

function close() {
  emit("update:modelValue", false);
}

watch(
  () => props.modelValue,
  (open) => {
    if (open) status.clear();
  },
);
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    title="Import into the library"
    width="min(520px, 92vw)"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <div class="inline tight segments" role="group" aria-label="Import source">
      <button
        v-for="m in MODES"
        :key="m.id"
        class="btn ghost"
        type="button"
        :aria-pressed="mode === m.id"
        :disabled="busy"
        @click="mode = m.id"
      >
        {{ m.label }}
      </button>
    </div>

    <div v-if="mode === 'local'" class="mt">
      <label for="importFiles">Files</label>
      <input
        id="importFiles"
        ref="fileInput"
        type="file"
        :accept="kinds.exts"
        multiple
        @change="onFilesChange"
      />
      <label class="mt" for="importFolder">Or a whole folder</label>
      <input
        id="importFolder"
        ref="folderInput"
        type="file"
        webkitdirectory
        multiple
        @change="onFilesChange"
      />
      <div class="hint mt-sm">
        {{ files.length }} {{ kinds.label }} file(s) selected
      </div>
    </div>

    <div v-else-if="mode === 'url'" class="mt">
      <label for="importUrl">Address of a {{ kinds.label }} file</label>
      <input
        id="importUrl"
        v-model="url"
        type="text"
        :placeholder="`https://example.org/${kinds.example}`"
        @keydown.enter="doImport"
      />
    </div>

    <div v-else class="mt">
      <div class="repo-grid">
        <div>
          <label for="importOwner">User</label>
          <input id="importOwner" v-model="owner" type="text" />
        </div>
        <div>
          <label for="importRepo">Repository</label>
          <input id="importRepo" v-model="repo" type="text" />
        </div>
        <div>
          <label for="importBranch">Branch</label>
          <input
            id="importBranch"
            v-model="branch"
            type="text"
            @keydown.enter="doImport"
          />
        </div>
      </div>
      <ul class="repos mt">
        <li v-for="r in library.repos" :key="library.repoId(r)" class="repo">
          <span class="mono">{{ originForRepo(r).label }}</span>
          <a
            v-if="removable(r)"
            class="hint"
            title="Forget this repository"
            @click="removeRepo(r)"
            >remove</a
          >
          <span v-else-if="isDefaultRepo(r)" class="hint">upstream</span>
          <span v-else class="hint">in use</span>
        </li>
      </ul>
    </div>

    <StatusLine class="mt-sm" :text="status.text" :error="status.error" />

    <template #actions>
      <button class="btn ghost" type="button" :disabled="busy" @click="close">
        Close
      </button>
      <span class="spacer"></span>
      <BusyButton
        :busy="busy"
        label="Import"
        busy-label="Importing…"
        @click="doImport"
      />
    </template>
  </BaseDialog>
</template>

<style scoped>
.segments > button[aria-pressed="true"] {
  color: var(--accent);
  border-color: var(--accent);
}
.repo-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
  gap: 10px;
}
.repos {
  list-style: none;
  margin: 0;
  padding: 0;
  font-size: 13px;
}
.repo {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  padding: 6px 0;
  border-top: 1px solid var(--line);
}
</style>
