<script setup lang="ts">
import { fmtSecs } from "../../composables/useElapsed";
import type { LogEntry } from "../../composables/useAuditRun";
import { checkLabel, checkTone } from "../../utils/contradictionReport";
import { fmtNum } from "../../utils/format";
import Disclosure from "../Disclosure.vue";
import SourceLoc from "../SourceLoc.vue";

/** An audit's per-check log, folded: how each check ended, how long it
 *  took, and the sentences whose neighbourhood it searched. */
defineProps<{ entries: LogEntry[] }>();
</script>

<template>
  <Disclosure :summary="`check log (${entries.length})`">
    <ol class="batch-log">
      <li v-for="(b, i) in entries" :key="i">
        <span class="dot" :class="checkTone(b)" aria-hidden="true" />
        <span class="entry-status">{{ checkLabel(b) }}</span>
        <span class="hint num">{{ fmtSecs(b.elapsed_ms / 1000) }}</span>
        <span v-if="b.budget" class="hint"
          >≤ {{ fmtNum(b.budget) }} axioms</span
        >
        <span v-if="b.status === 'Crashed'" class="hint"
          >steps {{ b.range[0] }}–{{ b.range[1] }} (skipped)</span
        >
        <template v-for="(f, j) in b.focus" :key="j">
          <SourceLoc
            v-if="f.file"
            :file="f.file"
            :line="f.line"
            variant="loc"
          />
          <span v-else class="hint">(no location)</span>
        </template>
      </li>
    </ol>
  </Disclosure>
</template>

<style scoped>
.batch-log {
  margin: 6px 0;
  padding-left: 22px;
  max-height: 300px;
  overflow: auto;
}
.batch-log li {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: baseline;
  padding: 2px 0;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  align-self: center;
  background: var(--muted);
}
.dot.ok {
  background: var(--ok);
}
.dot.warn {
  background: var(--warn);
}
.dot.bad {
  background: var(--bad);
}
.entry-status {
  font-size: 13px;
  min-width: 90px;
}
.num {
  font-variant-numeric: tabular-nums;
  min-width: 48px;
}
</style>
