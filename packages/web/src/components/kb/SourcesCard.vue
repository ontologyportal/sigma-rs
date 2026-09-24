<script setup lang="ts">
/** Which files came from where: one row per repo/URL/local upload the
 *  currently loaded constituents were fetched from. Collapsed to a
 *  name + count by default; the cog opens the full-detail dialog, and (for
 *  git/URL sources -- a local upload has no upstream to watch) the update
 *  button cycles how that source is watched for upstream changes. */
import { ref } from "vue";
import {
  useKBStore,
  type PendingUpdateFile,
  type SourceGroup,
  type UpdatePref,
} from "../../stores/kb";
import Card from "../Card.vue";
import SourceDetailsDialog from "./SourceDetailsDialog.vue";
import UpdatePreviewDialog from "./UpdatePreviewDialog.vue";

const kb = useKBStore();

const detailsOpen = ref(false);
const detailsGroup = ref<SourceGroup | null>(null);

function showDetails(g: SourceGroup) {
  detailsGroup.value = g;
  detailsOpen.value = true;
}

const updating = ref<Set<string>>(new Set());

const reviewOpen = ref(false);
const reviewFiles = ref<PendingUpdateFile[]>([]);
// The alert this review was opened from, if any -- dismissed once the
// review is acknowledged, so it doesn't linger alongside the applied/
// skipped decision it was superseded by. A manual "Update now" opens a
// review with no originating alert, so this stays null for that case.
const reviewAlertKey = ref<string | null>(null);

function openReview(
  files: PendingUpdateFile[],
  alertKey: string | null = null,
) {
  reviewFiles.value = files;
  reviewAlertKey.value = alertKey;
  reviewOpen.value = true;
}

function onReviewDone() {
  kb.finishReview(reviewFiles.value, reviewAlertKey.value);
}

async function updateNow(g: SourceGroup) {
  updating.value.add(g.key);
  try {
    const review = await kb.updateNow(g);
    if (review?.length) openReview(review);
  } finally {
    updating.value.delete(g.key);
  }
}

const PREF_LABEL: Record<UpdatePref, string> = {
  "auto-update": "Auto-update",
  "auto-check": "Auto-check",
  "no-check": "No check",
};
const PREF_TITLE: Record<UpdatePref, string> = {
  "auto-update":
    "Auto-update: fetches and applies a new version on every load. Click to switch to auto-check.",
  "auto-check":
    "Auto-check: alerts when a new version is found, without applying it. Click to switch to no-check.",
  "no-check":
    "No check: never looks for a new version. Click to switch to auto-update.",
};
</script>

<template>
  <Card title="Sources">
    <template #description>
      Every place the loaded constituents' text was fetched from.
    </template>

    <div v-if="kb.updateAlerts.length" class="src-alerts">
      <div v-for="a in kb.updateAlerts" :key="a.key" class="src-alert">
        <span>{{ a.message }}</span>
        <span class="src-alert-actions">
          <button
            v-if="a.review?.length"
            class="src-alert-review"
            type="button"
            @click="openReview(a.review!, a.key)"
          >
            Review
          </button>
          <button
            class="src-alert-dismiss"
            type="button"
            title="Dismiss"
            aria-label="Dismiss"
            @click="kb.dismissUpdateAlert(a.key)"
          >
            &times;
          </button>
        </span>
      </div>
    </div>

    <div v-if="!kb.sourceGroups.length" class="hint">
      No constituents loaded.
    </div>

    <details v-for="g in kb.sourceGroups" :key="g.key" class="src-report">
      <summary>
        <span class="src-label">{{ g.label }}</span>
        <span class="src-count">{{ g.files.length }}</span>
        <button
          v-if="g.origin.kind !== 'file'"
          class="src-pref"
          type="button"
          :class="`src-pref-${kb.prefFor(g.origin)}`"
          :title="PREF_TITLE[kb.prefFor(g.origin)]"
          @click.stop.prevent="kb.cycleUpdatePref(g.origin)"
        >
          {{ PREF_LABEL[kb.prefFor(g.origin)] }}
        </button>
        <button
          v-if="g.origin.kind !== 'file'"
          class="src-pref src-update-now"
          type="button"
          :disabled="updating.has(g.key)"
          title="Check for an update and apply it now"
          @click.stop.prevent="updateNow(g)"
        >
          {{ updating.has(g.key) ? "Updating…" : "Update now" }}
        </button>
        <button
          class="cog"
          type="button"
          title="Source details"
          aria-label="Source details"
          aria-haspopup="dialog"
          @click.stop.prevent="showDetails(g)"
        >
          &#9432;
        </button>
      </summary>
      <div class="src-rows">
        <span v-for="name in g.files" :key="name" class="src-row">
          {{ name }}
        </span>
      </div>
    </details>

    <SourceDetailsDialog v-model="detailsOpen" :group="detailsGroup" />
    <UpdatePreviewDialog
      v-model="reviewOpen"
      :files="reviewFiles"
      @done="onReviewDone"
    />
  </Card>
</template>

<style scoped>
/* Mirrors WordNetDiagnosticsCard's .wn-report/.wn-rows pattern: one
   collapsible section per source, a pill count in the summary, files
   listed below as a wrapping run rather than a table (there's no
   per-file detail to align into columns here). Closed by default -- the
   summary row alone (name + count) is the card's normal resting state. */
.src-report {
  border-top: 1px solid var(--line);
  padding: 8px 0;
}
.src-report:first-of-type {
  border-top: none;
}
.src-report summary {
  cursor: pointer;
  font-size: 13px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
}
.src-label {
  flex-shrink: 0;
}
.src-count {
  display: inline-block;
  min-width: 15px;
  padding: 0 5px;
  margin-left: auto;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 700;
  line-height: 15px;
  text-align: center;
  background: color-mix(in srgb, var(--accent) 20%, transparent);
  color: var(--accent);
}
/* The shared .cog button is sized for a 41px-tall action row elsewhere
   (Ask/Tell, Audit); shrunk here to sit inline with a summary row. */
.src-report .cog {
  height: 22px;
  padding: 0 6px;
  font-size: 12px;
  border-radius: 5px;
}
/* The update-preference cycle button: its own small pill (not `.btn`, to
   avoid fighting that class's cascade over this size), color-coded by state
   so the row communicates its watch mode without reading the label. */
.src-pref {
  font: inherit;
  height: 22px;
  padding: 0 8px;
  font-size: 11px;
  font-weight: 600;
  line-height: 1;
  cursor: pointer;
  border: 1px solid var(--line);
  border-radius: 5px;
  background: var(--bg);
  color: var(--muted);
}
.src-pref:hover {
  border-color: var(--accent);
  color: var(--fg);
}
.src-pref-auto-update {
  border-color: color-mix(in srgb, var(--accent) 50%, var(--line));
  color: var(--accent);
}
.src-pref-auto-check {
  color: var(--fg);
}
/* "Update now" is a plain action, not a state pill -- it never gets the
   preference colors above, just the base `.src-pref` look plus a disabled
   state while the check/apply it triggered is in flight. */
.src-update-now:disabled {
  cursor: default;
  opacity: 0.6;
}

/* One dismissible line per pending update-check result, above the source
   list -- a plain warning-tinted row rather than the app's single hardcoded
   promote-toast, since these are per-source and can stack. */
.src-alerts {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 10px;
}
.src-alert {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 6px 10px;
  border-radius: 6px;
  font-size: 12px;
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.src-alert-actions {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  gap: 8px;
}
.src-alert-review {
  font: inherit;
  font-size: 11px;
  font-weight: 700;
  line-height: 1;
  cursor: pointer;
  border: 1px solid var(--accent);
  border-radius: 5px;
  padding: 3px 8px;
  background: none;
  color: var(--accent);
}
.src-alert-review:hover {
  background: color-mix(in srgb, var(--accent) 15%, transparent);
}
.src-alert-dismiss {
  flex-shrink: 0;
  border: none;
  background: none;
  color: inherit;
  font-size: 15px;
  line-height: 1;
  cursor: pointer;
  padding: 0 2px;
  opacity: 0.7;
}
.src-alert-dismiss:hover {
  opacity: 1;
}

.src-rows {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
}
.src-row {
  padding: 2px 8px;
  border-radius: 5px;
  background-color: var(--line);
  font-family: var(--mono);
  font-size: 12px;
}
</style>
