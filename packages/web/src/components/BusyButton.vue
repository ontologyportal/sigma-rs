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
  }>(),
  { busy: false, busyLabel: "Working…", ghost: false, disabled: false },
);
</script>

<template>
  <button
    class="btn"
    :class="{ ghost }"
    type="button"
    :disabled="busy || disabled"
  >
    {{ busy ? busyLabel : label }}
  </button>
</template>
