<script setup lang="ts">
import { computed, onMounted } from "vue";
import { useBootStore } from "../stores/boot";

const boot = useBootStore();

const pct = computed(() =>
  Math.min(100, Math.round((boot.step / boot.total) * 100)),
);

onMounted(() => {
  boot.start();
});
</script>

<template>
  <div v-if="!boot.finished" id="overlay">
    <div id="overlayTitle">
      {{ boot.failed ? "Failed to load SUMO" : "Loading SUMO" }}
    </div>
    <div
      v-if="!boot.failed"
      id="bootBar"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      :aria-valuenow="pct"
    >
      <div id="bootBarFill" :style="{ width: `${pct}%` }"></div>
    </div>
    <div id="overlayMsg">{{ boot.failed ? "" : boot.msg }}</div>
    <div
      v-if="boot.failed"
      id="overlayErr"
      class="hint"
      style="color: var(--bad)"
    >
      {{ boot.error }}
    </div>
  </div>
</template>

<style lang="css" scoped>
#overlay {
  position: fixed;
  inset: 0;
  background: var(--bg);
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  text-align: center;
  padding: 20px;
}

#overlayTitle {
  font-size: 15px;
  font-weight: 600;
}

/* Linear boot progress: a track with a width-animated fill. */
#bootBar {
  width: min(380px, 72vw);
  height: 6px;
  background: var(--line);
  border-radius: 999px;
  overflow: hidden;
}

#bootBarFill {
  height: 100%;
  width: 0%;
  background: var(--accent);
  border-radius: 999px;
  transition: width 0.25s ease;
}

/* The currently-loading constituent, kept deliberately quiet. */
#overlayMsg {
  font-size: 12px;
  color: var(--muted);
  min-height: 1.2em;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
</style>
