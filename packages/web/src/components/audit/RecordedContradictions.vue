<script setup lang="ts">
/** The contradictions recorded for this knowledge base -- found by local
 *  audits or imported from the contradiction sources. The list is the
 *  engine's: a finding disappears on its own once one of its cited axioms
 *  is edited or unloaded. Shown as its own card in the classic layout's
 *  side column, or `embedded` as the compact layout's toggled panel under
 *  the audit form (the shape of Ask/Tell's History). */
import { computed } from "vue";
import type { AuditFinding } from "sigmakee/sdk";
import { useContradictionsStore } from "../../stores/contradictions";
import { navigate } from "../../router";
import Card from "../Card.vue";
import RecordedContradictionList from "./RecordedContradictionList.vue";

defineProps<{
  /** A run is in progress: replay is unavailable. */
  busy: boolean;
  /** Render as a panel inside the form card instead of a card of its own. */
  embedded?: boolean;
}>();
const emit = defineEmits<{ replay: [finding: AuditFinding] }>();

const store = useContradictionsStore();
const count = computed(() => store.findings.length);
const description = computed(() =>
  count.value
    ? `${count.value} contradiction${count.value === 1 ? "" : "s"} of this knowledge base, from audits and the configured sources. Each stays recorded until one of its axioms changes.`
    : "Contradictions found by audits, or reported by the configured sources, are recorded here until one of their axioms changes.",
);

async function clearAll() {
  if (
    window.confirm(
      "Forget every recorded contradiction? They are recorded again when an audit finds them or a source reports them.",
    )
  )
    await store.clearFindings();
}
</script>

<template>
  <div v-if="embedded" class="embedded">
    <div class="embedded-hd">
      <span class="embedded-title">Recorded contradictions</span>
      <span class="inline tight">
        <button
          class="btn ghost small"
          type="button"
          title="Add or check contradiction sources"
          @click="navigate('kb', { view: 'contradictions' })"
        >
          Sources
        </button>
        <button
          v-if="count"
          class="btn ghost small"
          type="button"
          @click="clearAll"
        >
          Clear all
        </button>
      </span>
    </div>
    <div class="hint">{{ description }}</div>
    <RecordedContradictionList :busy="busy" @replay="emit('replay', $event)" />
  </div>
  <Card v-else title="Recorded contradictions" :description="description">
    <template #header>
      <span class="inline tight">
        <button
          class="btn ghost small"
          type="button"
          title="Add or check contradiction sources"
          @click="navigate('kb', { view: 'contradictions' })"
        >
          Sources
        </button>
        <button
          v-if="count"
          class="btn ghost small"
          type="button"
          @click="clearAll"
        >
          Clear all
        </button>
      </span>
    </template>
    <RecordedContradictionList :busy="busy" @replay="emit('replay', $event)" />
  </Card>
</template>

<style scoped>
/* The embedded panel mirrors Ask/Tell's history panel: set off from the
   actions row above by a rule, with a compact heading of its own. */
.embedded {
  padding-top: 10px;
  border-top: 1px solid var(--line);
}
.embedded-hd {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 4px;
}
.embedded-title {
  font-size: 13px;
  font-weight: 600;
}
.embedded > .hint {
  margin-bottom: 6px;
}
</style>
