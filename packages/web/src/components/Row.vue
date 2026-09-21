<script setup lang="ts">
/**
 * We define the props using standard Flexbox CSS property names.
 * Using 'string' allows us to pass values like 'center', 'space-between', etc.
 */
interface Props {
  justify?:
    | "flex-start"
    | "flex-end"
    | "center"
    | "space-between"
    | "space-around"
    | "space-evenly"; // justify-content
  align?:
    | "normal"
    | "stretch"
    | "center"
    | "flex-start"
    | "flex-end"
    | "start"
    | "end"
    | "baseline"; // align-items
  wrap?: "nowrap" | "wrap" | "wrap-reverse";
  gap?: string | number; // gap (e.g., '10px' or 20)
}

const props = withDefaults(defineProps<Props>(), {
  justify: "flex-start",
  align: "stretch",
  wrap: "nowrap",
  gap: "0px",
});

/**
 * Helper to ensure the gap prop is always a valid CSS string.
 * If the user passes a number (e.g., 20), it converts it to '20px'.
 */
const formattedGap = () => {
  return typeof props.gap === "number" ? `${props.gap}px` : props.gap;
};
</script>

<template>
  <!-- The slot allows you to pass any children into the row -->
  <div class="flex-row">
    <slot />
  </div>
</template>

<style scoped>
.flex-row {
  display: flex;
  flex-direction: row; /* This component is specifically a "Row" */

  /* The Magic: Binding Props directly to CSS */
  justify-content: v-bind("props.justify");
  align-items: v-bind("props.align");
  flex-wrap: v-bind("props.wrap");
  gap: v-bind("formattedGap()");
}
</style>
