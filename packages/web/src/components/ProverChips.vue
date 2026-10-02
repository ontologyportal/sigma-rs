<script setup lang="ts">
import { computed } from "vue";
import { useProverStore, type ProfileName } from "../stores/prover";

/** The prover options toggle plus one chip per non-default setting of
 *  `profile`, each removable -- so what a run will use is visible without
 *  opening the panel. `fixed` chips (e.g. an audit's derived per-check
 *  limit) are shown first and can't be removed here. */
const props = defineProps<{
  profile: ProfileName;
  open: boolean;
  fixed?: string[];
  disabled?: boolean;
}>();

const emit = defineEmits<{ "update:open": [open: boolean] }>();

const prover = useProverStore();
const changes = computed(() => prover.changes(props.profile));
</script>

<template>
  <div class="chips">
    <button
      class="btn ghost opts-btn"
      type="button"
      :aria-expanded="open"
      title="Prover options: backend, time, axiom selection, search strategy"
      @click="emit('update:open', !open)"
    >
      <span aria-hidden="true">⚙</span> Options
    </button>
    <span class="chip backend">{{ prover.backendLabel }}</span>
    <span v-for="f in fixed" :key="f" class="chip">{{ f }}</span>
    <span v-for="c in changes" :key="c.id" class="chip changed">
      {{ c.label }}
      <button
        type="button"
        class="x"
        :title="`Reset: ${c.label}`"
        :aria-label="`Reset ${c.label}`"
        :disabled="disabled"
        @click="prover.resetOne(profile, c.id)"
      >
        ×
      </button>
    </span>
  </div>
</template>

<style scoped>
.chips {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.opts-btn {
  gap: 6px;
}
.chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  padding: 2px 9px;
  border-radius: 20px;
  border: 1px solid var(--line);
  color: var(--muted);
  background: var(--bg);
  white-space: nowrap;
}
.chip.backend {
  color: var(--fg);
  font-weight: 600;
}
.chip.changed {
  border-color: color-mix(in srgb, var(--accent) 45%, var(--line));
  color: var(--fg);
}
.x {
  font: inherit;
  line-height: 1;
  background: none;
  border: none;
  padding: 0 0 0 2px;
  color: var(--muted);
  cursor: pointer;
}
.x:hover:not(:disabled) {
  color: var(--bad);
}
</style>
