<script setup lang="ts">
/** The Knowledge base's file table: loaded constituents and every KIF file
 *  the library offers (unloaded local/URL entries and repo catalog files).
 *  "Save changes" loads the unloaded rows and unloads the loaded ones in
 *  ONE batch (one post-processing pass). Unloading keeps a file in the
 *  library; local/URL entries have a separate "delete" action. Emits `log`
 *  for the messages the parent shows in its status line. */
import { computed, ref } from "vue";
import { MERGE } from "../../constants";
import {
  LocalOrigin,
  RemoteOrigin,
  sourceLabel,
  type Origin,
} from "../../models/Origin";
import { navigate } from "../../router";
import { fetchAllTexts } from "../../services/sources";
import { useKBStore } from "../../stores/kb";
import { isKif, useLibraryStore, originForRepo } from "../../stores/library";
import { errMsg } from "../../utils/format";
import FileTable, { type FileRow } from "../FileTable.vue";

const emit = defineEmits<{ log: [text: string, error?: boolean] }>();

const kb = useKBStore();
const library = useLibraryStore();

const rowKey = (origin: Origin, name: string) => `${origin.kind}:${name}`;

const rows = computed<FileRow[]>(() => {
  const out: FileRow[] = kb.constituents.map((c) => {
    const core = c.name === MERGE;
    return {
      key: rowKey(c.origin, c.name),
      name: c.name,
      origin: c.origin,
      source: sourceLabel(c.origin),
      size: c.text.length,
      loaded: true,
      locked: core,
      status: core ? "core" : "loaded",
      statusKind: core ? "locked" : "in",
    };
  });
  const loaded = new Set(out.map((r) => r.key));
  for (const e of library.entries) {
    if (!isKif(e.name)) continue;
    const origin =
      e.kind === "url" ? new RemoteOrigin(e.url) : new LocalOrigin();
    if (loaded.has(rowKey(origin, e.name))) continue;
    out.push({
      key: rowKey(origin, e.name),
      name: e.name,
      origin,
      source: sourceLabel(origin),
      size: e.size,
      loaded: false,
      deletable: true,
      status: "available",
      statusKind: "out",
    });
  }
  for (const repo of library.repos) {
    const origin = originForRepo(repo);
    const source = sourceLabel(origin);
    for (const e of library.catalogs[library.repoId(repo)] ?? []) {
      if (!isKif(e.path)) continue;
      const name = origin.nameFor(e.path);
      if (loaded.has(rowKey(origin, name))) continue;
      out.push({
        key: rowKey(origin, name),
        name,
        origin,
        source,
        size: e.size,
        loaded: false,
        status: "available",
        statusKind: "out",
      });
    }
  }
  return out;
});

const selected = ref(new Set<string>());
const saving = ref(false);

async function deleteRow(row: FileRow) {
  if (!window.confirm(`Delete ${row.name} from the library?`)) return;
  try {
    await library.deleteEntry(row.name, row.origin.kind);
    emit("log", `Deleted ${row.name} from the library.`);
  } catch (e) {
    emit("log", errMsg(e), true);
  }
}

async function save(adds: FileRow[], removes: FileRow[]) {
  saving.value = true;
  try {
    const failed: string[] = [];
    const texts = adds.length
      ? await fetchAllTexts(adds, 6, (n) => {
          emit("log", `Fetching ${n}/${adds.length}…`);
        })
      : [];
    const add: { name: string; text: string; origin: Origin }[] = [];
    adds.forEach((row, i) => {
      const text = texts[i];
      if (text instanceof Error) failed.push(`${row.name}: ${text.message}`);
      else add.push({ name: row.name, text, origin: row.origin });
    });
    const remove = removes.map((r) => ({ name: r.name, kind: r.origin.kind }));
    emit(
      "log",
      `Loading ${add.length}, unloading ${remove.length}; axiomatizing…`,
    );
    const r = await kb.applyChanges({ add, remove });
    failed.push(...r.failed);
    selected.value = new Set();
    emit(
      "log",
      `Added ${r.added}, removed ${r.removed}` +
        (failed.length ? ` (${failed.length} failed — ${failed[0]})` : "."),
      failed.length > 0,
    );
  } catch (e) {
    emit("log", errMsg(e), true);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <FileTable
    v-model:selected="selected"
    :rows="rows"
    :saving="saving"
    :catalog-note="library.catalogNote.text"
    :catalog-error="library.catalogNote.error"
    @save="save"
    @delete="deleteRow"
    @open="(row) => navigate('edit', { file: row.name })"
  />
</template>
