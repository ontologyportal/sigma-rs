<script setup lang="ts">
import { onActivated, onDeactivated, ref } from "vue";
import { navigate } from "../router";
import TaxonomyUniverse from "../components/browse/TaxonomyUniverse.vue";

const active = ref(false);
onActivated(() => (active.value = true));
onDeactivated(() => (active.value = false));

function openTerm(symbol: string) {
  navigate("browse", { sym: symbol });
}
</script>

<template>
  <TaxonomyUniverse v-if="active" @open="openTerm" />
</template>
