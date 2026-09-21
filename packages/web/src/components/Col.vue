<script setup lang="ts">
import { computed } from "vue";

interface Props {
  /**
   * Grid span (1-12).
   * If provided, calculates width as (span / 12) * 100%
   */
  span?: number;

  /**
   * Custom width string.
   * E.g., "50%", "200px", or "auto"
   */
  width?: string;

  /** If true, sets flex-grow to 1 */
  grow?: boolean;

  /** If true, sets flex-shrink to 0 (prevents shrinking) */
  shrink?: boolean;
}

const props = withDefaults(defineProps<Props>(), {
  span: undefined,
  width: "auto",
  grow: false,
  shrink: false,
});

/**
 * Logic to determine the flex-basis.
 * We prioritize the 'span' prop (grid system)
 * then fallback to the 'width' prop.
 */
const computedBasis = computed(() => {
  if (props.span !== undefined) {
    // Calculate percentage based on a 12-column grid
    return `calc(${(props.span / 12) * 100}% )`;
  }
  return props.width;
});

/**
 * Logic for flex-grow
 */
const flexGrow = computed(() => (props.grow ? 1 : 0));

/**
 * Logic for flex-shrink
 */
const flexShrink = computed(() => (props.shrink ? 0 : 1));
</script>

<template>
  <div ref="root" class="col">
    <slot />
  </div>
</template>

<style scoped>
.col {
  /* We use flex-basis to define the "size" of the column */
  flex-basis: v-bind("computedBasis");

  /* These control how the col behaves when space is available/missing */
  flex-grow: v-bind("flexGrow");
  flex-shrink: v-bind("flexShrink");

  /* Ensure the column doesn't allow content to overflow its basis */
  min-width: 0;
}
</style>
