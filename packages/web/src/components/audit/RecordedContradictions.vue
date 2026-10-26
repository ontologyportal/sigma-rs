<script setup lang="ts">
/** The contradictions recorded for this knowledge base -- found by local
 *  audits or imported from the contradiction sources -- each replayable
 *  (its cited axioms audited together) or expandable to its derivation.
 *  The list is the engine's: a finding disappears on its own once one of
 *  its cited axioms is edited or unloaded. */
import { computed, nextTick, ref, watch } from "vue";
import type { AuditFinding } from "sigmakee/sdk";
import { useContradictionsStore } from "../../stores/contradictions";
import { fmtTime } from "../../utils/format";
import { navigate } from "../../router";
import Card from "../Card.vue";
import SourceLoc from "../SourceLoc.vue";
import ContradictionCard from "./ContradictionCard.vue";

defineProps<{
  /** A run is in progress: replay is unavailable. */
  busy: boolean;
}>();
const emit = defineEmits<{ replay: [finding: AuditFinding] }>();

const store = useContradictionsStore();

const expanded = ref(new Set<string>());
function toggle(key: string) {
  const next = new Set(expanded.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expanded.value = next;
}

// Arriving from a source row or an alert: expand and scroll to the finding.
const rows = ref(new Map<string, HTMLElement>());
function setRow(key: string, el: unknown) {
  if (el instanceof HTMLElement) rows.value.set(key, el);
  else rows.value.delete(key);
}
watch(
  () => store.focusKey,
  async (key) => {
    if (!key) return;
    expanded.value = new Set([...expanded.value, key]);
    await nextTick();
    rows.value.get(key)?.scrollIntoView({ block: "center" });
    store.focusKey = null;
  },
  { immediate: true },
);

/** A finding's origin as a short label. */
function originLabel(origin: string): string {
  if (origin === "native") return "SUPr";
  if (origin === "vampire") return "Vampire";
  if (origin === "eprover") return "E";
  if (origin === "report") return "shared report";
  const github = /^github:([^#]+)#(\d+)$/.exec(origin);
  if (github) return `${github[1]} #${github[2]}`;
  if (origin.startsWith("github:")) return origin.slice("github:".length);
  if (origin.startsWith("url:")) {
    try {
      const u = new URL(origin.slice("url:".length));
      return `${u.host}${u.pathname}`;
    } catch {
      return origin.slice("url:".length);
    }
  }
  const file = /^file:(.*)@\d+$/.exec(origin);
  if (file) return file[1];
  if (origin.startsWith("import:")) return origin.slice("import:".length);
  return origin;
}

/** An imported finding links back to the issue it came from. */
function originUrl(origin: string): string | null {
  const github = /^github:([^#]+)#(\d+)$/.exec(origin);
  return github ? `https://github.com/${github[1]}/issues/${github[2]}` : null;
}

const count = computed(() => store.findings.length);

async function clearAll() {
  if (
    window.confirm(
      "Forget every recorded contradiction? They are recorded again when an audit finds them or a source reports them.",
    )
  )
    await store.clearFindings();
}
</script>

<template>
  <Card title="Recorded contradictions">
    <template #description>
      {{
        count
          ? `${count} contradiction${count === 1 ? "" : "s"} of this knowledge base, from audits and the configured sources. Each stays recorded until one of its axioms changes.`
          : "Contradictions found by audits, or reported by the configured sources, are recorded here until one of their axioms changes."
      }}
    </template>
    <template #header>
      <span class="inline tight">
        <button
          class="btn ghost small"
          type="button"
          title="Add or check contradiction sources"
          @click="navigate('kb', { view: 'contradictions' })"
        >
          Sources
        </button>
        <button
          v-if="count"
          class="btn ghost small"
          type="button"
          @click="clearAll"
        >
          Clear all
        </button>
      </span>
    </template>

    <div v-if="!count" class="hint">None recorded.</div>
    <div
      v-for="(f, i) in store.findings"
      :key="f.key"
      :ref="(el) => setRow(f.key, el)"
      class="finding"
    >
      <div class="finding-hd">
        <span class="finding-axioms">
          <SourceLoc
            v-for="c in f.culprits"
            :key="c.sid"
            :file="c.file ?? '?'"
            :line="c.line ?? 0"
            variant="loc"
          />
        </span>
        <span class="hint finding-meta">
          <a
            v-if="originUrl(f.origin)"
            :href="originUrl(f.origin)!"
            target="_blank"
            rel="noopener"
            >{{ originLabel(f.origin) }}</a
          >
          <template v-else>{{ originLabel(f.origin) }}</template>
          ·
          <time
            :datetime="new Date(f.found_at_ms).toISOString()"
            :title="new Date(f.found_at_ms).toLocaleString()"
            >{{ fmtTime(new Date(f.found_at_ms)) }}</time
          >
          <span
            v-if="!f.complete"
            title="Some proof steps cite no source axiom; the recorded axioms may not be its whole support."
            >· partial provenance</span
          >
        </span>
        <span class="finding-actions">
          <button
            class="btn ghost small"
            type="button"
            :disabled="busy"
            title="Audit these axioms together to re-derive the contradiction"
            @click="emit('replay', f)"
          >
            Replay
          </button>
          <button class="btn ghost small" type="button" @click="toggle(f.key)">
            {{ expanded.has(f.key) ? "Hide proof" : "Show proof" }}
          </button>
        </span>
      </div>
      <ContradictionCard
        v-if="expanded.has(f.key)"
        :contradiction="{ ...f.contradiction, foundAt: f.found_at_ms }"
        :number="i + 1"
        :backend="originLabel(f.origin)"
        :time-limit-secs="0"
        :collapsed="false"
      />
    </div>
  </Card>
</template>

<style scoped>
.finding {
  border-top: 1px solid var(--line);
  padding: 8px 0;
}
.finding:first-of-type {
  border-top: none;
}
.finding-hd {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 12px;
  font-size: 13px;
}
.finding-axioms {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 10px;
  min-width: 0;
}
.finding-meta {
  font-variant-numeric: tabular-nums;
}
.finding-actions {
  margin-left: auto;
  display: flex;
  gap: 6px;
}
</style>
