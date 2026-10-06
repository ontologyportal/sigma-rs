<script setup lang="ts">
import type { ProfileName } from "../stores/prover";
import type { HistoryFeed, ProofRun } from "../stores/runHistory";
import Card from "./Card.vue";
import ProofHistory from "./ProofHistory.vue";
import ProverOptionsCard from "./ProverOptionsCard.vue";

/** The classic layout's side column for a proving tab: `profile`'s prover
 *  options over the tab's run history (`feed`), pinned together beside the
 *  main column. The options keep their own height (up to their cap); the
 *  history takes what is left and scrolls on its own. The default slot adds
 *  notes under the options; `pick` passes on a picked run. */
defineProps<{
  profile: ProfileName;
  feed: HistoryFeed;
  disabled?: boolean;
}>();
const emit = defineEmits<{ pick: [run: ProofRun] }>();
</script>

<template>
  <div class="side-stack">
    <ProverOptionsCard
      class="options-card"
      :profile="profile"
      :sticky="false"
      :disabled="disabled"
    >
      <slot />
    </ProverOptionsCard>
    <Card title="History" class="history-card">
      <ProofHistory :feed="feed" @pick="(r) => emit('pick', r)" />
    </Card>
  </div>
</template>

<style scoped>
.side-stack {
  position: sticky;
  top: 10px;
  display: flex;
  flex-direction: column;
  max-height: calc(100vh - 20px);
}
.options-card {
  flex: none;
}
.history-card {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
}
</style>
