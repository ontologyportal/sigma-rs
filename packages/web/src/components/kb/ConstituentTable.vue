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

const staleTip = (reason: string) =>
  `Stale: this file couldn't be fetched from its source (${reason}), so the ` +
  `copy saved in this browser from an earlier load is in use instead. It may ` +
  `be out of date, or the file may have been moved, renamed or deleted ` +
  `upstream. It is fetched again on the next load.`;
const unavailableTip = (reason: string) =>
  `This file couldn't be fetched from its source (${reason}) and there is no ` +
  `copy saved in this browser, so it isn't loaded. It is tried again on the ` +
  `next load; tick it to retry now, or delete it to stop loading it.`;

const rows = computed<FileRow[]>(() => {
  const out: FileRow[] = kb.constituents.map((c) => {
    const core = c.name === MERGE && c.origin.kind === "sumo";
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
      ...(c.stale ? { flag: { label: "stale", tip: staleTip(c.stale) } } : {}),
    };
  });
  for (const u of kb.unavailable)
    out.push({
      key: rowKey(u.origin, u.name),
      name: u.name,
      origin: u.origin,
      source: sourceLabel(u.origin),
      size: 0,
      loaded: false,
      deletable: true,
      status: "",
      statusKind: "out",
      flag: { label: "unavailable", tip: unavailableTip(u.reason) },
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

/** Pinned sources list their files as of the accepted commit, so a file
 *  added upstream since shows up only once that update is accepted. */
const pinNote = computed(() =>
  library.repos
    .map((repo) => {
      const origin = originForRepo(repo);
      const pin = kb.pinnedRef(origin);
      return pin
        ? `${sourceLabel(origin)}: files as of the accepted commit ${pin.slice(0, 7)} (${kb.prefFor(origin)})`
        : "";
    })
    .filter(Boolean),
);

const selected = ref(new Set<string>());
const saving = ref(false);

async function deleteRow(row: FileRow) {
  if (row.flag?.label === "unavailable") {
    kb.forgetUnavailable(row.name, row.origin.kind);
    emit("log", `${row.name} will no longer be loaded.`);
    return;
  }
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
      ? await fetchAllTexts(
          adds,
          6,
          (n) => {
            emit("log", `Fetching ${n}/${adds.length}…`);
          },
          kb.pinnedRef,
        )
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
  <div v-if="pinNote.length" class="hint pin-note">
    {{ pinNote.join(" · ") }}. Files added upstream since appear once you accept
    the update (Sources → Update now).
  </div>
  <FileTable
    v-model:selected="selected"
    checked-is-loaded
    :rows="rows"
    :saving="saving"
    :catalog-note="library.catalogNote.text"
    :catalog-error="library.catalogNote.error"
    @save="save"
    @delete="deleteRow"
    @open="(row) => navigate('edit', { file: row.name })"
  />
</template>

<style scoped>
.pin-note {
  margin-top: 8px;
}
</style>
