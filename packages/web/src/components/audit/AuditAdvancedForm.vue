<script setup lang="ts">
import { useProvidedAuditSweep } from "../../composables/useAuditSweep";

/** The Audit tab's Advanced fields: the per-round knobs (prefilled from the
 *  plan the total time implies) and, for a sweep, its position. Edits the
 *  form the tab provides. */
defineProps<{
  /** A single sentence is being audited: no rounds, batch or position. */
  focused: boolean;
  disabled: boolean;
}>();

const {
  sweep,
  saveSweep,
  perCheckField,
  roundsField,
  batchField,
  limitField,
  totalField,
  relink,
  randomSeed,
} = useProvidedAuditSweep();
</script>

<template>
  <div class="section-label">
    <span>Rounds</span>
    <span v-if="sweep.linked" class="hint">derived from the total time</span>
    <button v-else class="linkish" type="button" @click="relink">
      Re-derive from the total time
    </button>
  </div>
  <div class="grid">
    <div>
      <label for="auditPerCheck">time per round (s)</label>
      <input
        id="auditPerCheck"
        v-model.number="perCheckField"
        type="number"
        min="0"
        :disabled="disabled"
      />
    </div>
    <div v-if="!focused">
      <label for="auditRounds">rounds</label>
      <input
        id="auditRounds"
        v-model.number="roundsField"
        type="number"
        min="1"
        placeholder="until time's up"
        :disabled="disabled"
      />
    </div>
    <div v-if="!focused">
      <label for="auditBatch">axioms per round</label>
      <input
        id="auditBatch"
        v-model.number="batchField"
        type="number"
        min="1"
        :disabled="disabled"
      />
    </div>
    <div>
      <label for="auditLimit">stop after contradictions</label>
      <input
        id="auditLimit"
        v-model.number="limitField"
        type="number"
        min="1"
        :disabled="disabled"
      />
    </div>
    <div>
      <label for="auditTotal">total time (s)</label>
      <input
        id="auditTotal"
        v-model.number="totalField"
        type="number"
        min="0"
        placeholder="0 = no limit"
        :disabled="disabled"
      />
    </div>
  </div>
  <template v-if="!focused">
    <div class="section-label"><span>Sweep position</span></div>
    <div class="grid">
      <div>
        <label for="auditSeed">seed</label>
        <div class="seed-row">
          <input
            id="auditSeed"
            v-model.number="sweep.seed"
            type="number"
            min="0"
            :disabled="disabled"
            @change="saveSweep"
          />
          <button
            class="btn ghost small"
            type="button"
            title="New random seed (restarts at step 0)"
            aria-label="New random seed"
            :disabled="disabled"
            @click="randomSeed"
          >
            ↻
          </button>
        </div>
      </div>
      <div>
        <label for="auditStep">start at step</label>
        <input
          id="auditStep"
          v-model.number="sweep.step"
          type="number"
          min="0"
          :disabled="disabled"
          @change="saveSweep"
        />
      </div>
    </div>
    <div class="hint sub">
      The seed fixes a pseudorandom order of the axioms; the step is the
      position in it. Both are saved per KB, so a later run continues where the
      last one stopped.
    </div>
  </template>
</template>

<style scoped>
.section-label {
  margin-top: 16px;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
  gap: 10px 14px;
  margin-top: 8px;
}
.seed-row {
  display: flex;
  gap: 6px;
}
.sub {
  font-size: 12px;
  margin-top: 6px;
}
</style>
