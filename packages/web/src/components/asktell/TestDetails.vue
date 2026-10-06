<script setup lang="ts">
import { computed } from "vue";
import { metaSummary, type TestMeta } from "../../utils/testMeta";
import Disclosure from "../Disclosure.vue";

/** Ask/Tell's `.kif.tq` directives, folded under a summary of what is set:
 *  v-model the details, v-model:open the fold. */
const meta = defineModel<TestMeta>({ required: true });
const open = defineModel<boolean>("open", { default: false });

const summary = computed(() => metaSummary(meta.value));
</script>

<template>
  <Disclosure :summary="summary" :open="open" @toggle="open = $event">
    <div class="meta-grid">
      <div>
        <label for="tqNote">note</label>
        <input
          id="tqNote"
          v-model="meta.note"
          type="text"
          placeholder="e.g. Astronomy_4"
        />
      </div>
      <div>
        <label for="tqCategories">categories</label>
        <input
          id="tqCategories"
          v-model="meta.categories"
          type="text"
          placeholder="comma-separated"
        />
      </div>
      <div>
        <label for="tqAnswer">expected answer</label>
        <div class="answer">
          <select id="tqAnswer" v-model="meta.answer">
            <option value="yes">yes (provable)</option>
            <option value="no">no (not provable)</option>
            <option value="bindings">bindings</option>
            <option value="none">unspecified</option>
          </select>
          <input
            v-if="meta.answer === 'bindings'"
            v-model="meta.bindings"
            type="text"
            placeholder="e.g. Rex Fido"
            aria-label="Expected bindings"
          />
        </div>
      </div>
      <div>
        <label for="tqFiles">required files</label>
        <input
          id="tqFiles"
          v-model="meta.files"
          type="text"
          placeholder="e.g. Astronomy.kif"
        />
      </div>
    </div>
    <div class="hint sub">
      The <code>.kif.tq</code> directives Save test writes. Its
      <code>(time N)</code> is the prover options' time limit.
    </div>
  </Disclosure>
</template>

<style scoped>
.meta-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
  gap: 10px 14px;
  margin-top: 8px;
}
.meta-grid .answer {
  display: flex;
  gap: 6px;
}
.meta-grid select {
  min-width: 0;
}
.sub {
  font-size: 12px;
  margin-top: 6px;
}
</style>
