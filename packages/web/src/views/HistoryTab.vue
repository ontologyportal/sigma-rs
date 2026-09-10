<script setup>
import { computed } from 'vue';
import { SUMO } from '../../../../constants.ts';
import { updateParams } from '../../../../router.ts';
import {
  historyFile, historyStatus, historyCommits, historyError,
  historyPickerOptions, loadHistory,
} from '../../../../tabs/history.ts';

// Nothing outside this component calls these -- `ensureHistory` (the
// tab-enter hook `router.ts` calls) only needs `loadHistory` itself.
function refreshHistory() {
  return loadHistory(historyFile.value, { force: true });
}
function selectHistoryFile(file) {
  updateParams({ file });
  loadHistory(file);
}
const historyAllCommitsUrl = computed(() => historyFile.value
  ? `https://github.com/${SUMO.owner}/${SUMO.repo}/commits/${SUMO.ref}/${encodeURI(historyFile.value)}`
  : '');

function commitMsg(c) {
  return (c.commit?.message || '(no message)').split('\n')[0];
}
function commitWho(c) {
  return c.commit?.author?.name || c.author?.login || 'unknown';
}
function commitWhen(c) {
  const iso = c.commit?.author?.date;
  return iso ? new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : '';
}
function commitSha(c) {
  return (c.sha || '').slice(0, 7);
}

function onPickerChange(e) {
  selectHistoryFile(e.target.value);
}
</script>

<template>
<div class="card">
  <div class="inline" style="justify-content: space-between">
    <div style="min-width: 260px">
      <label for="historyPicker">File</label>
      <select id="historyPicker" :value="historyFile" @change="onPickerChange">
        <option v-if="!historyPickerOptions().length" value="">(no SUMO-sourced files loaded)</option>
        <option v-for="f in historyPickerOptions()" :key="f" :value="f">{{ f }}</option>
      </select>
    </div>
    <div class="inline" style="gap: 8px">
      <span id="historyStatus" class="hint">{{ historyStatus }}</span>
      <button class="btn" id="historyRefresh" type="button" @click="refreshHistory">
        Refresh
      </button>
    </div>
  </div>
  <p class="hint" style="margin: 8px 0 0">
    Commit history from <code>ontologyportal/sumo</code> via the public
    GitHub API (no sign-in; unauthenticated requests are rate-limited to
    60/hour).
  </p>
</div>
<div id="historyList">
  <div v-if="historyError" class="card hint" style="color:var(--bad)">{{ historyError }}</div>
  <div v-else-if="historyFile && !historyCommits.length && !historyStatus" class="card hint">
    No commits found for <code>{{ historyFile }}</code>.
  </div>
  <div v-else-if="historyCommits.length" class="card">
    <ol class="timeline">
      <li v-for="c in historyCommits" :key="c.sha">
        <div class="commit-msg"><a :href="c.html_url || '#'" target="_blank" rel="noopener">{{ commitMsg(c) }}</a></div>
        <div class="commit-meta">{{ commitWho(c) }}<template v-if="commitWhen(c)"> · {{ commitWhen(c) }}</template> · <span class="sha">{{ commitSha(c) }}</span></div>
      </li>
    </ol>
    <div class="hint" style="margin-top:12px; padding-top:10px; border-top:1px solid var(--line)">
      Showing the {{ historyCommits.length }} most recent —
      <a :href="historyAllCommitsUrl" target="_blank" rel="noopener">full commit history for {{ historyFile }} on GitHub ↗</a>
    </div>
  </div>
</div>
</template>
