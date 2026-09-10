<script setup>
import { ref } from 'vue';
import { call } from '../../../../rpc.ts';
import { state } from '../../../../state.ts';
import { updateParams } from '../../../../router.ts';
import {
  diagFilter, diagFilterOpen, diagSummaryHtml, diagFilterBadge,
  diagFilterSelects, diagPageItems, diagEmptyHint, diagPager,
  renderDiagnostics, toggleDiagFilterPanel,
} from '../../../../tabs/diagnostics.ts';
import { markStatsStale } from '../../../../tabs/home-stats.ts';

const revalidating = ref(false);

async function revalidate() {
  revalidating.value = true;
  try {
    state.diagnostics = (await call('validate')).diagnostics;
    markStatsStale();
    renderDiagnostics();
  } finally {
    revalidating.value = false;
  }
}

// Nothing outside this component calls these -- `renderDiagnostics`/
// `applyDiagRouteParams` (tabs/diagnostics.ts) are the only entry points
// `kb.ts`/`router.ts` need.
/** Mirror the active filters (and page, when not the first) into the address
 *  bar so a filtered/paginated view is shareable. `p` is 1-based in the URL —
 *  friendlier to read/type than the internal 0-based index. */
function syncDiagUrl() {
  updateParams({
    file: diagFilter.file,
    sev:  diagFilter.severity,
    kind: diagFilter.kind,
    code: diagFilter.code,
    p:    diagFilter.page ? diagFilter.page + 1 : null,
  });
}

function setDiagFilter(dim, value) {
  diagFilter[dim] = value;
  // Picking a type invalidates a code chosen under a different type — clear
  // it rather than leave a stale, now-impossible combination active.
  if (dim === 'kind') diagFilter.code = '';
  diagFilter.page = 0;
  renderDiagnostics();
  syncDiagUrl();
}

function diagPagerPrev() {
  diagFilter.page -= 1;
  renderDiagnostics();
  syncDiagUrl();
}
function diagPagerNext() {
  diagFilter.page += 1;
  renderDiagnostics();
  syncDiagUrl();
}
</script>

<template>
<div class="card">
  <div class="inline" style="justify-content: space-between">
    <div id="diagSummary" class="hint" v-html="diagSummaryHtml"></div>
    <div class="inline" style="gap: 8px">
      <button
        class="btn ghost"
        id="diagFilterBtn"
        type="button"
        :aria-expanded="diagFilterOpen"
        aria-controls="diagFilterPanel"
        title="Filter diagnostics"
        @click="toggleDiagFilterPanel()"
      >
        <svg
          viewBox="0 0 16 16"
          width="13"
          height="13"
          fill="currentColor"
          aria-hidden="true"
        >
          <path
            d="M1 2.75A.75.75 0 0 1 1.75 2h12.5a.75.75 0 0 1 .6 1.2L10 9.65v4.6a.75.75 0 0 1-1.14.64l-2.5-1.5A.75.75 0 0 1 6 12.75V9.65L1.15 3.2A.75.75 0 0 1 1 2.75Z"
          />
        </svg>
        Filter<span id="diagFilterBadge">{{ diagFilterBadge }}</span>
      </button>
      <button class="btn" id="revalidate" type="button" :disabled="revalidating" @click="revalidate">
        {{ revalidating ? 'Working…' : 'Re-validate' }}
      </button>
    </div>
  </div>
  <div id="diagFilterPanel" class="settings" v-show="diagFilterOpen">
    <div class="diag-filter-stack">
      <div>
        <label for="diagFileFilter">File</label>
        <select id="diagFileFilter" :value="diagFilter.file" @change="setDiagFilter('file', $event.target.value)">
          <option v-for="o in diagFilterSelects.file" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
      </div>
      <div>
        <label for="diagSevFilter">Severity</label>
        <select id="diagSevFilter" :value="diagFilter.severity" @change="setDiagFilter('severity', $event.target.value)">
          <option v-for="o in diagFilterSelects.severity" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
      </div>
      <div>
        <label for="diagKindFilter">Type</label>
        <select id="diagKindFilter" :value="diagFilter.kind" @change="setDiagFilter('kind', $event.target.value)">
          <option v-for="o in diagFilterSelects.kind" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
      </div>
      <div>
        <label for="diagCodeFilter">Code</label>
        <select id="diagCodeFilter" :value="diagFilter.code" @change="setDiagFilter('code', $event.target.value)">
          <option v-for="o in diagFilterSelects.code" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
      </div>
    </div>
  </div>
  <div id="diagList" style="margin-top: 10px">
    <div v-if="diagEmptyHint" class="hint">{{ diagEmptyHint }}</div>
    <div v-for="item in diagPageItems" :key="item.i" class="diag" :data-i="item.i" :data-sev="item.sev">
      <div class="diag-head">
        <span class="sev" :class="item.sev">{{ item.sev }}</span>
        <span v-html="item.locHtml"></span>
        <span class="code">[{{ item.kind }}/{{ item.code }}]</span>
        <span v-html="item.ghHtml"></span>
        <span class="msg">{{ item.message }}</span>
      </div>
    </div>
  </div>
  <div id="diagPager" class="diag-pager" v-if="diagPager">
    <button class="btn ghost" id="diagPrev" type="button" :disabled="diagPager.page === 0" @click="diagPagerPrev">‹ Prev</button>
    <span class="hint">{{ diagPager.from }}–{{ diagPager.to }} of {{ diagPager.total }} · page {{ diagPager.page + 1 }} of {{ diagPager.pageCount }}</span>
    <button class="btn ghost" id="diagNext" type="button" :disabled="diagPager.page >= diagPager.pageCount - 1" @click="diagPagerNext">Next ›</button>
  </div>
</div>
</template>
