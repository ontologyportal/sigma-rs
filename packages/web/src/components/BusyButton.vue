<script setup lang="ts">
/** A `.btn` that disables itself and swaps its label while `busy`. Other
 *  attributes (`@click`, `title`, `aria-*`) fall through to the button. */
withDefaults(
  defineProps<{
    busy?: boolean;
    label: string;
    busyLabel?: string;
    /** Render as the outlined `.btn.ghost` variant. */
    ghost?: boolean;
    disabled?: boolean;
    /** While busy, fill the button left-to-right by this fraction (0-1);
     *  null = no fill. */
    progress?: number | null;
  }>(),
  {
    busy: false,
    busyLabel: "Working…",
    ghost: false,
    disabled: false,
    progress: null,
  },
);
</script>

<template>
  <button
    class="btn"
    :class="{ ghost, filling: busy && progress !== null }"
    type="button"
    :disabled="busy || disabled"
    :style="
      busy && progress !== null
        ? { '--progress': `${Math.min(1, Math.max(0, progress)) * 100}%` }
        : undefined
    "
  >
    <span class="busy-label">{{ busy ? busyLabel : label }}</span>
  </button>
</template>

<style scoped>
/* Busy labels may carry a live counter; fixed-width digits stop it jiggling. */
button {
  font-variant-numeric: tabular-nums;
}
/* Progress fill: a faded track with the solid accent sweeping across it,
   instead of the plain half-opacity disabled look. */
button.btn.filling {
  position: relative;
  overflow: hidden;
  opacity: 1;
  background: color-mix(in srgb, var(--accent) 45%, var(--bg));
}
button.btn.filling::before {
  content: "";
  position: absolute;
  inset: 0 auto 0 0;
  width: var(--progress, 0%);
  background: var(--accent);
  transition: width 0.25s linear;
}
.busy-label {
  position: relative;
}
/* Keeps the white label legible over the pale, unfilled part of the track. */
.filling .busy-label {
  text-shadow: 0 1px 2px rgb(0 0 0 / 0.35);
}
</style>
