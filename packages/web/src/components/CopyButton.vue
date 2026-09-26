<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue";

/** A small ghost button that copies `text` to the clipboard and briefly
 *  confirms. Icon-only unless `label` is given. Clicks neither propagate nor
 *  run default actions, so it can sit inside a clickable row or a
 *  `<summary>` without also jumping or toggling. */
const props = defineProps<{
  /** The text to copy, or a thunk producing it at click time (for large
   *  lists that shouldn't be joined on every render). */
  text: string | (() => string);
  /** Visible label; omitted = icon-only. */
  label?: string;
  /** Tooltip and accessible name. */
  title?: string;
}>();

const state = ref<"idle" | "copied" | "failed">("idle");
let timer: ReturnType<typeof setTimeout> | undefined;

async function copy(e: Event) {
  e.stopPropagation();
  e.preventDefault();
  const text = typeof props.text === "function" ? props.text() : props.text;
  try {
    await navigator.clipboard.writeText(text);
    state.value = "copied";
  } catch {
    state.value = "failed";
  }
  clearTimeout(timer);
  timer = setTimeout(() => (state.value = "idle"), 1500);
}

onBeforeUnmount(() => clearTimeout(timer));
</script>

<template>
  <button
    class="btn ghost copy-btn"
    :class="{ 'icon-only': !label }"
    type="button"
    :title="title ?? 'Copy'"
    :aria-label="title ?? label ?? 'Copy'"
    @click="copy"
  >
    <svg
      v-if="state === 'idle'"
      viewBox="0 0 16 16"
      width="12"
      height="12"
      fill="currentColor"
      aria-hidden="true"
    >
      <path
        d="M4 2.75C4 1.78 4.78 1 5.75 1h7.5c.97 0 1.75.78 1.75 1.75v7.5c0 .97-.78 1.75-1.75 1.75h-7.5C4.78 12 4 11.22 4 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .14.11.25.25.25h7.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25ZM1 5.75C1 4.78 1.78 4 2.75 4H3v1.5h-.25a.25.25 0 0 0-.25.25v7.5c0 .14.11.25.25.25h7.5a.25.25 0 0 0 .25-.25V13H12v.25c0 .97-.78 1.75-1.75 1.75h-7.5C1.78 15 1 14.22 1 13.25Z"
      />
    </svg>
    <span v-if="state === 'copied'">Copied</span>
    <span v-else-if="state === 'failed'">Copy failed</span>
    <span v-else-if="label">{{ label }}</span>
  </button>
</template>

<style scoped>
.copy-btn {
  gap: 6px;
  white-space: nowrap;
}
/* Sits inline in list rows and panel headers, so not the full 41px `.btn`. */
button.copy-btn.icon-only {
  height: 24px;
  padding: 0 7px;
  font-size: 11px;
}
</style>
