<script setup lang="ts">
/** The loaded-constituent list: totals line plus one row per constituent
 *  (open in the editor / remove). Emits `removed` after a removal so the
 *  parent can clear its channel log. */
import { MERGE } from "../../constants";
import { useKBStore } from "../../stores/kb";
import { navigate } from "../../router";
import type { Constituent } from "../../models/Constituent";
import type { OriginKind } from "../../models/Origin";

const emit = defineEmits<{ removed: [name: string] }>();

const kb = useKBStore();

const ORIGIN_LABELS: Record<OriginKind, string> = {
  sumo: "GitHub",
  file: "Local File",
  url: "Remote URL",
};

const sizeLabel = (c: Constituent) => `${(c.text.length / 1000).toFixed(0)} KB`;
const originLabel = (c: Constituent) =>
  ORIGIN_LABELS[c.origin.kind] || c.origin.kind;

function remove(c: Constituent) {
  emit("removed", c.name);
  kb.remove(c.name, c.origin.kind);
}
</script>

<template>
  <div class="inline totals">
    <div class="hint">
      <b>{{ kb.constituents.length }}</b> constituent(s) loaded ·
      {{ kb.diagnostics.length }} diagnostic(s)
    </div>
  </div>
  <ul class="results list">
    <li
      v-for="c in kb.constituents"
      :key="c.origin.kind + ':' + c.name"
      class="loaded-row"
    >
      <span>
        <a
          class="file-open"
          title="Open in the editor"
          @click="navigate('edit', { file: c.name })"
          >{{ c.name }}</a
        >
        <span class="hint meta">{{ sizeLabel(c) }} · {{ originLabel(c) }}</span>
      </span>
      <span v-if="c.name === MERGE" class="hint">core</span>
      <a v-else class="rm" @click="remove(c)">remove</a>
    </li>
  </ul>
</template>

<style scoped>
.meta {
  margin-left: 6px;
}
.totals {
  justify-content: space-between;
}
.list {
  margin-top: 8px;
}
</style>
