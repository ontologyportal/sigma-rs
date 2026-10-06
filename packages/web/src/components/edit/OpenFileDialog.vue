<script setup lang="ts">
import { ref } from "vue";
import { useTestsStore } from "../../stores/tests";
import type { Origin } from "../../models/Origin";
import { errMsg } from "../../utils/format";
import BaseDialog from "../BaseDialog.vue";
import { useKBStore } from "../../stores/kb";
import type { Constituent } from "../../models/Constituent";

defineProps<{
  /** Open state (v-model). */
  modelValue: boolean;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
  /** A loaded constituent was chosen. */
  pick: [c: Pick<Constituent, "name" | "origin" | "text">];
  /** "+ New file": start an unnamed buffer (Save asks for the name). */
  create: [];
}>();

const kb = useKBStore();
const tests = useTestsStore();

function close() {
  emit("update:modelValue", false);
}

function pick(c: Pick<Constituent, "name" | "origin" | "text">) {
  close();
  emit("pick", c);
}

/** A library test not loaded yet is fetched first. */
const loadError = ref("");
async function pickTest(t: { name: string; origin: Origin }) {
  loadError.value = "";
  try {
    pick(await tests.ensure(t.name, t.origin));
  } catch (e) {
    loadError.value = `${t.name}: ${errMsg(e)}`;
  }
}

function create() {
  close();
  emit("create");
}
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    title="Open file"
    width="min(420px, 92vw)"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <ul class="results open-list">
      <li v-if="loadError" class="hint bad">{{ loadError }}</li>
      <li
        v-if="
          !kb.constituents.length &&
          !tests.tests.length &&
          !tests.available.length
        "
        class="hint"
      >
        no files loaded yet — create one below
      </li>
      <li v-for="c in kb.constituents" :key="c.origin.kind + ':' + c.name">
        <a class="open-file" @click="pick(c)">{{ c.name }}</a>
        <span class="hint origin">{{ c.origin.kind }}</span>
      </li>
      <li
        v-for="t in [...tests.tests, ...tests.available]"
        :key="t.origin.kind + ':' + t.name"
      >
        <a class="open-file" @click="pickTest(t)">{{ t.name }}</a>
        <span class="hint origin">Inference test</span>
      </li>
    </ul>
    <template #actions>
      <span class="spacer"></span>
      <button class="btn ghost" type="button" @click="close">Cancel</button>
      <button class="btn" type="button" @click="create">+ New file</button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.open-list {
  max-height: 300px;
  overflow-y: auto;
}
.open-list a {
  font-family: var(--mono);
  cursor: pointer;
}
.open-list .origin {
  float: right;
}
</style>
