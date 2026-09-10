<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useOutsideClick } from "../../composables/useOutsideClick";

/** One `label | extra | value` line of the popover grid. */
export interface PopRow {
  label: string;
  value: string;
  extra?: string;
  title?: string;
}

const props = defineProps<{
  /** The stat tile the popover hangs beneath; clicks on it do not count as outside. */
  anchor: HTMLElement;
  title: string;
  rows: PopRow[];
  /** Shown instead of the grid when `rows` is empty. */
  empty?: string;
  /** Column captions rendered as a muted first row. */
  header?: { label: string; value: string; extra: string };
}>();

const emit = defineEmits<{ close: [] }>();

const pop = ref<HTMLElement | null>(null);
const shown = ref(false);
const top = ref("0px");
const left = ref("0px");

onMounted(() => {
  const r = props.anchor.getBoundingClientRect();
  const width = pop.value?.offsetWidth ?? 0;
  top.value = `${r.bottom + 6 + scrollY}px`;
  left.value = `${Math.max(8, Math.min(r.left, innerWidth - width - 12)) + scrollX}px`;
  requestAnimationFrame(() => {
    shown.value = true;
  });
});

useOutsideClick([pop, () => props.anchor], () => emit("close"), ref(true));
</script>

<template>
  <Teleport to="body">
    <div
      ref="pop"
      class="stat-pop"
      :class="{ show: shown }"
      :style="{ top, left }"
    >
      <!-- Outside-click/Esc/re-click already dismiss it, but a visible close
                 affordance matters on touch, where none of those are discoverable. -->
      <button
        type="button"
        class="stat-pop-close"
        aria-label="Close"
        @click="emit('close')"
      >
        &times;
      </button>
      <h4>{{ title }}</h4>
      <template v-if="rows.length">
        <div v-if="header" class="pop-row">
          <span class="pop-label hint">{{ header.label }}</span>
          <span class="hint">{{ header.extra }}</span>
          <span class="hint">{{ header.value }}</span>
        </div>
        <div
          v-for="(row, i) in rows"
          :key="i"
          class="pop-row"
          :title="row.title || undefined"
        >
          <span class="pop-label">{{ row.label }}</span>
          <span v-if="row.extra" class="pop-n">{{ row.extra }}</span>
          <span v-else></span>
          <span class="pop-n">{{ row.value }}</span>
        </div>
      </template>
      <div v-else class="hint">{{ empty }}</div>
    </div>
  </Teleport>
</template>

<style scoped>
/* Animated popover anchored to a clicked tile. */
.stat-pop {
  position: absolute;
  z-index: 60;
  min-width: 220px;
  max-width: 320px;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 12px 14px;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.28);
  font-size: 13px;
  opacity: 0;
  transform: translateY(-6px) scale(0.96);
  transition:
    opacity 0.18s ease,
    transform 0.18s ease;
}
.stat-pop.show {
  opacity: 1;
  transform: none;
}
.stat-pop-close {
  position: absolute;
  top: 6px;
  right: 6px;
  width: 22px;
  height: 22px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: none;
  border: none;
  border-radius: 6px;
  padding: 0;
  font-size: 16px;
  line-height: 1;
  color: var(--muted);
  cursor: pointer;
}
.stat-pop-close:hover {
  color: var(--fg);
  background: var(--bg);
}
.stat-pop h4 {
  margin: 0 22px 8px 0;
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--muted);
}
.stat-pop .pop-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 56px 56px;
  gap: 10px;
  align-items: center;
  padding: 3px 0;
}
.stat-pop .pop-row .pop-label {
  overflow-wrap: anywhere;
}
.stat-pop .pop-row .pop-n {
  font-variant-numeric: tabular-nums;
  font-weight: 600;
  text-align: right;
}
.stat-pop .pop-row .hint {
  text-align: right;
}
.stat-pop .pop-row .pop-label.hint,
.stat-pop .pop-row .pop-label .hint {
  text-align: left;
}
</style>
