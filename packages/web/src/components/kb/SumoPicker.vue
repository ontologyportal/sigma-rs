<script setup lang="ts">
/** The upstream-repo file picker: a filterable multi-select over the KIF
 *  catalog minus what is already loaded (constituents and imported tests).
 *  Emits `add` with the selected paths. */
import { computed, ref } from "vue";
import { useKBStore } from "../../stores/kb";
import { useTestsStore } from "../../stores/tests";

defineProps<{
  /** True while the parent is fetching/ingesting a previous selection. */
  busy: boolean;
}>();
const emit = defineEmits<{ add: [paths: string[]] }>();

const kb = useKBStore();
const tests = useTestsStore();

const filter = ref("");
const pickerEl = ref<HTMLSelectElement | null>(null);

const options = computed(() => {
  if (!kb.sumoCatalog) return [];
  const needle = filter.value.toLowerCase();
  const loaded = new Set([...kb.sumoNames, ...tests.sumoTestNames]);
  return kb.sumoCatalog.filter(
    (p) => !loaded.has(p) && p.toLowerCase().includes(needle),
  );
});

const status = computed(() => {
  if (kb.sumoCatalog) return `${options.value.length} file(s) available`;
  if (kb.catalogError) return `could not load file list: ${kb.catalogError}`;
  return "loading file list…";
});

function submit() {
  const el = pickerEl.value;
  emit("add", el ? [...el.selectedOptions].map((o) => o.value) : []);
}
</script>

<template>
  <label for="fileFilter"
    >Add SUMO constituents from <code>ontologyportal/sumo</code></label
  >
  <input
    id="fileFilter"
    v-model="filter"
    type="search"
    placeholder="filter file list…"
    autocomplete="off"
  />
  <select ref="pickerEl" class="picker" multiple size="10">
    <option v-for="p in options" :key="p" :value="p">{{ p }}</option>
  </select>
  <div class="inline actions">
    <span class="hint">{{ status }}</span>
    <button class="btn" type="button" :disabled="busy" @click="submit">
      {{ busy ? "Working…" : "Add selected" }}
    </button>
  </div>
</template>

<style scoped>
.picker {
  width: 100%;
  margin-top: 8px;
}
.actions {
  margin-top: 8px;
  justify-content: space-between;
  align-items: center;
}
</style>
