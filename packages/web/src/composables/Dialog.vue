<script setup lang="ts">
const model = defineModel<boolean>();
</script>

<template>
<div v-if="model">

</div>
</template>


<style lang="css" scoped>
div {
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card);
    color: var(--fg);
    padding: 22px 28px;
    width: min(360px, 92vw);
    box-sizing: border-box;
}
</style>