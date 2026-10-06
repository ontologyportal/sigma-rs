<script setup lang="ts">
import { computed } from "vue";
import {
  useRunHistory,
  type HistoryFeed,
  type ProofRun,
} from "../stores/runHistory";
import { fmtDate, fmtTime } from "../utils/format";

/** A tab's run history (`feed`), newest first: each run's query (or test
 *  name), how many assertions went with it, when, and badges for its
 *  backend and status. Picking one emits `pick` for the caller to act on. */
const props = defineProps<{ feed: HistoryFeed }>();
const emit = defineEmits<{ pick: [run: ProofRun] }>();

const history = useRunHistory(props.feed);

const today = new Date().toDateString();
/** Today's runs by time of day; older ones by date and time. */
function when(at: number): string {
  const d = new Date(at);
  return d.toDateString() === today
    ? fmtTime(d)
    : `${fmtDate(d)}, ${fmtTime(d)}`;
}

const runs = computed(() => history.runs);
</script>

<template>
  <div class="history">
    <div class="history-hd">
      <span class="hint">{{
        runs.length
          ? `${runs.length} run${runs.length === 1 ? "" : "s"}`
          : "No runs yet"
      }}</span>
      <button
        v-if="runs.length"
        class="linkish"
        type="button"
        @click="history.clear()"
      >
        Clear
      </button>
    </div>
    <ul class="runs">
      <li v-for="r in runs" :key="r.id">
        <button
          type="button"
          class="run"
          :title="
            feed === 'test'
              ? `Open ${r.title} in Ask/Tell`
              : 'Put this run back in the panes'
          "
          @click="emit('pick', r)"
        >
          <span class="run-main">
            <code class="goal">{{
              r.title ||
              r.goal ||
              (r.lang === "tptp" ? "TPTP problem" : r.query)
            }}</code>
            <span class="hint sub">
              <template v-if="r.told"
                >+{{ r.told }} assertion{{ r.told === 1 ? "" : "s" }} ·
              </template>
              <time :datetime="new Date(r.at).toISOString()">{{
                when(r.at)
              }}</time>
            </span>
          </span>
          <span class="badges">
            <span
              v-if="r.backend"
              class="backend"
              :title="`Proved with ${r.backend}`"
              >{{ r.backend }}</span
            >
            <span
              class="status"
              :class="r.status === 'Error' ? 'InputError' : r.status"
              >{{ r.status }}</span
            >
          </span>
        </button>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.history-hd {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 8px;
}
.runs {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.run {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 8px 10px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--bg);
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}
.run:hover {
  border-color: color-mix(in srgb, var(--accent) 45%, var(--line));
}
.run-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  flex: 1;
}
.goal {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 12px;
}
.sub {
  font-size: 12px;
}
.badges {
  flex: none;
  display: flex;
  align-items: center;
  gap: 6px;
}
/* The backend, as ProverChips shows it beside Prove. */
.backend {
  font-size: 12px;
  font-weight: 600;
  padding: 2px 9px;
  border-radius: 20px;
  border: 1px solid var(--line);
  background: var(--card);
  white-space: nowrap;
}
</style>
