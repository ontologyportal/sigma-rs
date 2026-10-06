<script setup lang="ts">
import type { ProfileName } from "../stores/prover";
import Card from "./Card.vue";
import ProverOptions from "./ProverOptions.vue";

/** `profile`'s prover options as a side card for the classic layout: sticky
 *  beside the page's main column (unless `sticky: false`, for a caller that
 *  pins a whole stack of cards itself), scrolling on its own once taller than
 *  half the window. The default slot adds notes under the options. */
withDefaults(
  defineProps<{
    profile: ProfileName;
    hideTime?: boolean;
    disabled?: boolean;
    sticky?: boolean;
  }>(),
  { hideTime: false, disabled: false, sticky: true },
);
</script>

<template>
  <Card title="Prover options" class="side" :class="{ sticky }">
    <ProverOptions
      :profile="profile"
      :hide-time="hideTime"
      :disabled="disabled"
    />
    <slot />
  </Card>
</template>

<style scoped>
.side {
  max-height: 50vh;
  overflow-y: auto;
}
.side.sticky {
  position: sticky;
  top: 10px;
}
</style>
