<script setup lang="ts">
import { $ } from '../../../../dom.ts';
import { updateParams } from '../../../../router.ts';
import {
  browseViewHtml, browseHomeVisible, showSearchClear,
  onSearchSubmit, onSearchInput, onClearClick, runSearch, openManPage,
} from '../../../../tabs/browse.ts';
import {
  stat, statNote, onTileDocClick, onTileRelationsClick,
} from '../../../../tabs/home-stats.ts';

// Nothing outside this component calls these.
function onWordnetOnlyChange() {
  const q = $('q').value.trim();
  if (q) runSearch(q);
}

function onBrowseViewClick(e) {
  const link = e.target.closest('.open');
  if (link) {
    e.preventDefault();
    updateParams({ sym: link.dataset.sym });
    openManPage(link.dataset.sym);
  }
}
</script>

<template>
<div class="card">
  <form id="searchForm" @submit="onSearchSubmit">
    <label for="q"
      >Search symbols &amp; documentation
      <span class="hint kbd-hint"
        >(press <kbd>/</kbd> anywhere)</span
      ></label
    >
    <div class="search-box">
      <input
        type="search"
        id="q"
        placeholder="e.g. Human, Process, part…"
        autocomplete="off"
        @input="onSearchInput"
      />
      <button
        type="button"
        id="qClear"
        class="search-clear"
        aria-label="Clear search"
        v-show="showSearchClear"
        @click="onClearClick"
      >
        &times;
      </button>
    </div>
  </form>
  <details id="advancedSearch" style="margin-top: 10px">
    <summary class="hint">Advanced search</summary>
    <label class="check" style="margin-top: 8px">
      <input type="checkbox" id="searchWordnetOnly" @change="onWordnetOnlyChange" /> WordNet only
      <span class="hint"
        >— show only WordNet synonym-expansion hits (requires the
        WordNet lexicon; see the
        <a class="jump" data-tab="kb">Knowledge base</a> tab)</span
      >
    </label>
  </details>
</div>
<div id="browseView" v-html="browseViewHtml" @click="onBrowseViewClick"></div>
<div id="browseHome" v-show="browseHomeVisible">
  <div class="card">
    <h2 class="welcome-h">SUMO in your browser</h2>
    <p class="hint" style="margin: 6px 0 0">
      The <code>sigmakee-rs</code> native prover compiled to
      WebAssembly. Search above to explore the ontology — try
      <a class="try-q">Human</a>, <a class="try-q">Process</a> or
      <a class="try-q">part</a> — manage what is loaded under
      <a class="jump" data-tab="kb">Knowledge base</a>, prove things
      under <a class="jump" data-tab="prover">Ask/Tell</a>, and edit or
      contribute changes under <a class="jump" data-tab="edit">Edit</a>.
      Everything runs locally: <strong>no server</strong>.
    </p>
  </div>
  <div class="stats">
    <div class="stat">
      <div class="stat-n" id="statFiles">{{ stat.files }}</div>
      <div class="stat-l">constituent files</div>
    </div>
    <div class="stat">
      <div class="stat-n" id="statSymbols">{{ stat.symbols }}</div>
      <div class="stat-l">symbols</div>
    </div>
    <div class="stat">
      <div class="stat-n" id="statAxioms">{{ stat.axioms }}</div>
      <div class="stat-l">axioms</div>
    </div>
    <div class="stat">
      <div class="stat-n" id="statRules">{{ stat.rules }}</div>
      <div class="stat-l">rules</div>
    </div>
    <div class="stat">
      <div class="stat-n" id="statClasses">{{ stat.classes }}</div>
      <div class="stat-l">classes</div>
    </div>
    <div class="stat">
      <div class="stat-n" id="statInstances">{{ stat.instances }}</div>
      <div class="stat-l">instances</div>
    </div>
    <div
      class="stat clickable"
      id="tileRelations"
      title="Predicates vs functions"
      @click="onTileRelationsClick($event.currentTarget)"
    >
      <div class="stat-n" id="statRelations">{{ stat.relations }}</div>
      <div class="stat-l">relations</div>
    </div>
    <div
      class="stat clickable"
      id="tileDoc"
      :title="stat.docTooltip || 'Coverage by language'"
      @click="onTileDocClick($event.currentTarget)"
    >
      <div class="stat-n" id="statDoc">{{ stat.doc }}</div>
      <div class="meter"><div id="statDocBar" :style="{ width: stat.docPct + '%' }"></div></div>
      <div class="stat-l">symbols documented</div>
    </div>
    <div class="stat">
      <div class="stat-n stat-date" id="statCommit" :title="stat.commitTooltip">{{ stat.commit }}</div>
      <div class="stat-l">last upstream commit</div>
    </div>
  </div>
  <div id="statNote" class="hint">{{ statNote }}</div>
</div>
</template>
