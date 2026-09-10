<script setup>
import { computed, ref } from 'vue';
import { $, downloadText } from '../../../../dom.ts';
import { state } from '../../../../state.ts';
import { updateParams } from '../../../../router.ts';
import { lspRequest } from '../../../../editor/lsp-client.ts';
import {
  editDiagSummary, editDiagItems, onEditDiagClick,
  editStatusHtml, editPickerOptions, onEditPickerChange,
  editSaveHidden, editLogText, editLogIsError,
  editSaveStatusText, editSaveStatusIsError,
  editFileNameText, editFileNameTitle,
  editFullscreenOn, editFullscreenTitle, editFullscreenIconPath, onEditFullscreenClick,
  editSaveBusy, onEditSaveClick,
} from '../../../../tabs/edit.ts';
import {
  tptpPaneOpen, tptpStatus, tptpMenuOpen, tptpMenuPos,
  onEditTptpToggleClick, onTptpTranslationBtnClick,
} from '../../../../editor/tptp-pane.ts';
import {
  ghBadgeCount, ghBadgeHidden, ghBadgeStale, ghProposeTitle,
  ghPanelOpen, onGhProposeClick,
  ghChangesCountText, ghChangesListHtml,
  onGhChangesListChange, onGhChangesListInput, onGhChangesListClick,
  ghStatusText, ghStatusIsError, ghSubmitLabel, ghSubmitDisabled, ghSubmitBusy, onGhSubmitClick,
  ghResultHtml,
  ghDiffTitle, ghDiffMsg, ghDiffStatus, ghDiffTakeLabel, ghDiffTakeBusy,
  onGhDiffTakeClick, closeDiffDialog, onGhDiffDialogClose,
} from '../../../../tabs/contribute.ts';

const ghSubmitText = computed(() => ghSubmitBusy.value ? 'Submitting…' : ghSubmitLabel.value);

// -- The change list's own collapse/expand toggle -- nothing outside this
// component touches it (unlike `refreshChangeUi`/`openDiffDialog`, which
// tabs/contribute.ts keeps since kb.ts/boot.ts/edit.ts call into them).
const ghChangesListOpen = ref(false);
function onGhChangesToggleClick() {
  ghChangesListOpen.value = !ghChangesListOpen.value;
}

/** The `<select>`'s own change handler -- also syncs the URL, unlike
 *  `onEditPickerChange` alone (which `router.ts` also calls directly, where
 *  the URL is already the source of truth). */
function onEditPickerSelect() {
  onEditPickerChange();
  updateParams(state.editCurrentFile ? { file: state.editCurrentFile.name } : {});
}

// -- Open-file dialog: pick a loaded constituent, or start a new file --------
//
// Nothing outside this component opens this dialog.

const openDialogItems = ref([]);

function openEditOpenDialog() {
  openDialogItems.value = state.constituents.map((c) => ({ value: `${c.name}|${c.origin}`, name: c.name, origin: c.origin }));
  $('openDialog').showModal();
}
function onOpenFileClick(value) {
  $('openDialog').close();
  $('editPicker').value = value;
  onEditPickerChange();
  updateParams(state.editCurrentFile ? { file: state.editCurrentFile.name } : {});
}
function onEditCreateClick() {
  $('openDialog').close();
  $('editPicker').value = '__new__';
  onEditPickerChange();
  updateParams({});
}
function onOpenCancelClick() {
  $('openDialog').close();
}

// Delegates to Monaco's own format-document command rather than calling
// formatKif + applying the edit by hand — Monaco's command already runs it
// through the SAME registered provider (defineKifLanguage) and applies the
// result as one undoable edit, preserving cursor/scroll/undo history the way
// a hand-rolled setValue() would not.
function onEditFormatClick() {
  state.monacoEditor?.getAction('editor.action.formatDocument')?.run();
}

// -- Edit: download menu ------------------------------------------------------
//
// The download button opens a two-entry menu: the current buffer as KIF (a
// real download, independent of the in-browser OPFS/KB state), or the WHOLE
// knowledge base's TPTP translation (same `toTptpIndexed` dump the TPTP pane
// shows — an individual file's translation is meaningless without the rest
// of the KB's declarations, so there is no per-file TPTP option). Nothing
// outside this component opens it.

const editDownloadMenuOpen = ref(false);
const editDownloadMenuPos = ref({ left: '0px', top: '0px' });

function setDownloadMenuOpen(on) {
  if (on) {
    // Fixed-position under the button (the menu can't live inside
    // .btn-group — overflow:hidden would clip it; see the template below).
    const r = $('editDownload').getBoundingClientRect();
    editDownloadMenuPos.value = { left: `${Math.round(r.left)}px`, top: `${Math.round(r.bottom + 4)}px` };
  }
  editDownloadMenuOpen.value = on;
}

// Close on any click OUTSIDE the button/menu (matching on the event target
// rather than relying on stopPropagation, which breaks under synthesized
// event sequences). The menu-item handlers below close it themselves. The
// component is always mounted (this app has one always-mounted root), so a
// plain top-level listener needs no mount/unmount bookkeeping.
document.addEventListener('click', (e) => {
  if (e.target instanceof Element && e.target.closest('#editDownload, #editDownloadMenu')) return;
  setDownloadMenuOpen(false);
});
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') setDownloadMenuOpen(false); });

function onDownloadKifClick() {
  setDownloadMenuOpen(false);
  if (!state.monacoEditor) return;
  downloadText(state.editCurrentFile ? state.editCurrentFile.name : 'untitled.kif', state.monacoEditor.getValue());
}

async function onDownloadTptpClick() {
  setDownloadMenuOpen(false);
  const prior = editStatusHtml.value;
  editStatusHtml.value = 'generating TPTP…';
  try {
    const r = await lspRequest('sumo/toTptp', {});
    downloadText('knowledge-base.tptp', r?.tptp ?? '');
    editStatusHtml.value = prior;
  } catch (e) {
    editStatusHtml.value = 'TPTP translation failed: ' + (e && e.message || e);
  }
}
</script>

<template>
<div class="card">
  <div class="edit-toolbar">
    <div class="btn-group" role="toolbar" aria-label="Editor actions">
      <button
        id="editOpen"
        type="button"
        title="Open a file, or create a new one"
        aria-label="Open a file, or create a new one"
        @click="openEditOpenDialog"
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="currentColor"
          aria-hidden="true"
        >
          <path
            d="M1.75 1A1.75 1.75 0 0 0 0 2.75v10.5C0 14.216.784 15 1.75 15h12.5A1.75 1.75 0 0 0 16 13.25v-8.5A1.75 1.75 0 0 0 14.25 3H7.5a.25.25 0 0 1-.2-.1l-.9-1.2A1.75 1.75 0 0 0 5 1H1.75Z"
          />
        </svg>
      </button>
      <button
        id="editSave"
        type="button"
        title="Save to the in-browser knowledge base"
        aria-label="Save to the in-browser knowledge base"
        v-show="!editSaveHidden"
        :disabled="editSaveBusy"
        @click="onEditSaveClick"
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path
            d="M2.75 2.75h8.19a1 1 0 0 1 .7.3l1.31 1.3a1 1 0 0 1 .3.71v8.19h-10.5z"
          />
          <path d="M5 2.75V6.25h5V2.75M5 13.25V9.25h6v4" />
        </svg>
      </button>
      <button
        id="editFormat"
        type="button"
        title="Format document (fix indentation to match paren nesting)"
        aria-label="Format document"
        @click="onEditFormatClick"
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          aria-hidden="true"
        >
          <path d="M2 3h12M2 6.5h8M2 10h10M2 13.5h6" />
        </svg>
      </button>
      <button
        id="editDownload"
        type="button"
        aria-haspopup="menu"
        :aria-expanded="editDownloadMenuOpen"
        title="Download…"
        aria-label="Download this file or the TPTP translation"
        @click="setDownloadMenuOpen(!editDownloadMenuOpen)"
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="currentColor"
          aria-hidden="true"
        >
          <path
            d="M7.25 1.75a.75.75 0 0 1 1.5 0v6.69l2.22-2.22a.75.75 0 1 1 1.06 1.06l-3.5 3.5a.75.75 0 0 1-1.06 0l-3.5-3.5a.75.75 0 1 1 1.06-1.06l2.22 2.22ZM2.5 13a.75.75 0 0 0 0 1.5h11a.75.75 0 0 0 0-1.5Z"
          />
        </svg>
      </button>
      <button
        id="ghPropose"
        type="button"
        :aria-expanded="ghPanelOpen"
        aria-controls="ghPanel"
        :title="ghProposeTitle"
        aria-label="Submit changes to GitHub as a pull request"
        @click="onGhProposeClick"
      >
        <svg
          class="gh-mark"
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="currentColor"
          aria-hidden="true"
        >
          <path
            d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.012 8.012 0 0 0 16 8c0-4.42-3.58-8-8-8Z"
          />
        </svg>
        <span id="ghBadge" v-show="!ghBadgeHidden" :class="{ stale: ghBadgeStale }">{{ ghBadgeCount }}</span>
      </button>
      <button
        id="editFullscreen"
        type="button"
        :aria-pressed="editFullscreenOn"
        :title="editFullscreenTitle"
        aria-label="Toggle fullscreen editor"
        @click="onEditFullscreenClick"
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path :d="editFullscreenIconPath" />
        </svg>
      </button>
      <button
        id="editTptpToggle"
        type="button"
        :aria-pressed="tptpPaneOpen"
        aria-haspopup="menu"
        :aria-expanded="tptpMenuOpen"
        title="Split-screen views…"
        aria-label="Split-screen views"
        @click="onEditTptpToggleClick"
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <rect x="1" y="2.5" width="6" height="11" rx="1" />
          <rect x="9" y="2.5" width="6" height="11" rx="1" />
        </svg>
      </button>
    </div>
    <span id="editFileName" :title="editFileNameTitle">{{ editFileNameText }}</span>
    <span class="spacer"></span>
    <span id="editStatus" class="hint" v-html="editStatusHtml"></span>
    <select id="editPicker" hidden aria-hidden="true" @change="onEditPickerSelect">
      <option v-for="o in editPickerOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
    </select>
    <!-- Download menu for #editDownload. Lives OUTSIDE .btn-group (whose
       overflow:hidden would clip it, and whose `button + button`
       divider chain a nested div would break); fixed under the button
       when opened (see setDownloadMenuOpen). -->
    <div id="editDownloadMenu" class="dl-menu" role="menu" v-show="editDownloadMenuOpen" :style="editDownloadMenuPos">
      <button id="dlKifBtn" type="button" role="menuitem" @click="onDownloadKifClick">
        This file (KIF)
      </button>
      <button id="dlTptpBtn" type="button" role="menuitem" @click="onDownloadTptpClick">
        Whole knowledge base (TPTP)
      </button>
    </div>
    <!-- Split-screen menu for #editTptpToggle — same fixed-position
       pattern as the download menu above. -->
    <div id="editTptpMenu" class="dl-menu" role="menu" v-show="tptpMenuOpen" :style="tptpMenuPos">
      <button id="tptpTranslationBtn" type="button" role="menuitem" @click="onTptpTranslationBtnClick">
        TPTP Translation
      </button>
    </div>
  </div>
  <!-- Save feedback (success/error) lives right under the toolbar, not
     buried below the editor — the one place a user glances after
     clicking Save. -->
  <div id="editSaveStatus" class="hint" :style="{ color: editSaveStatusIsError ? 'var(--bad)' : '' }">{{ editSaveStatusText }}</div>

  <!-- Open-file dialog: pick a loaded constituent or start a new file.
     A new file starts unnamed — Save prompts for a filename the
     first time, rather than asking for one up front here. -->
  <dialog id="openDialog">
    <h3>Open file</h3>
    <ul id="openList" class="results">
      <li v-if="!openDialogItems.length" class="hint">no files loaded yet — create one below</li>
      <li v-for="c in openDialogItems" :key="c.value">
        <a class="open-file" @click="onOpenFileClick(c.value)">{{ c.name }}</a>
        <span class="hint origin">{{ c.origin }}</span>
      </li>
    </ul>
    <div style="text-align: right; margin-top: 12px">
      <button class="btn ghost" id="openCancel" type="button" @click="onOpenCancelClick">
        Cancel
      </button>
      <button class="btn" id="editCreate" type="button" @click="onEditCreateClick">
        + New file
      </button>
    </div>
  </dialog>

  <!-- Contribute the selected changes upstream as a branch + pull request -->
  <div id="ghPanel" class="settings" v-show="ghPanelOpen">
    <!-- Every tracked change, collapsed by default: the badge says how
       many there are, this says which. -->
    <div class="gh-changes">
      <button
        id="ghChangesToggle"
        type="button"
        :aria-expanded="ghChangesListOpen"
        aria-controls="ghChangesList"
        @click="onGhChangesToggleClick"
      >
        <span class="tri">{{ ghChangesListOpen ? '▾' : '▸' }}</span> Files changed
        <span id="ghChangesCount" class="hint">{{ ghChangesCountText }}</span>
      </button>
      <div id="ghChangesList" v-show="ghChangesListOpen" v-html="ghChangesListHtml"
        @change="onGhChangesListChange" @input="onGhChangesListInput" @click="onGhChangesListClick"></div>
    </div>
    <p class="hint" style="margin: 0 0 10px">
      Opens a pull request against <code>ontologyportal/sumo</code> with
      the ticked files, as one commit. You will be forked automatically
      if you lack push access.
    </p>
    <div class="settings-grid">
      <div>
        <label for="ghTitle">Pull request title</label>
        <input
          type="text"
          id="ghTitle"
          placeholder="Update Merge.kif"
        />
      </div>
    </div>
    <div style="margin-top: 10px">
      <label for="ghBody">Description</label>
      <textarea
        id="ghBody"
        rows="3"
        placeholder="What changed and why."
      ></textarea>
    </div>
    <div
      class="inline"
      style="justify-content: space-between; margin-top: 10px"
    >
      <span id="ghStatus" class="hint" :style="{ color: ghStatusIsError ? 'var(--bad)' : '' }">{{ ghStatusText }}</span>
      <div class="inline" style="gap: 8px">
        <button class="btn" id="ghSubmit" type="button" :disabled="ghSubmitDisabled || ghSubmitBusy" @click="onGhSubmitClick">
          {{ ghSubmitText }}
        </button>
      </div>
    </div>
    <div id="ghResult" class="hint" style="margin-top: 8px" v-html="ghResultHtml"></div>
  </div>

  <!-- Side-by-side upstream vs. local. Doubles as the conflict dialog:
     the only difference when upstream has moved is the wording and
     what "take upstream" costs. -->
  <dialog id="ghDiffDialog" @close="onGhDiffDialogClose">
    <h3 id="ghDiffTitle">{{ ghDiffTitle }}</h3>
    <p id="ghDiffMsg" class="hint">{{ ghDiffMsg }}</p>
    <div id="ghDiffPane"></div>
    <div class="dialog-actions">
      <span id="ghDiffStatus" class="hint">{{ ghDiffStatus }}</span>
      <span class="spacer"></span>
      <button class="btn ghost" id="ghDiffTake" type="button" :disabled="ghDiffTakeBusy" @click="onGhDiffTakeClick">
        {{ ghDiffTakeLabel }}
      </button>
      <button class="btn" id="ghDiffKeep" type="button" @click="closeDiffDialog">
        Keep mine
      </button>
    </div>
  </dialog>
</div>
<div class="card" id="editPane">
  <div id="editorContainer" data-placeholder="Loading editor…"></div>
  <div id="tptpPane" v-show="tptpPaneOpen">
    <div class="tptp-pane-header">
      <span>TPTP — whole knowledge base</span>
      <span id="tptpStatus" class="hint">{{ tptpStatus }}</span>
    </div>
    <div id="tptpContainer" data-placeholder="Generating…"></div>
  </div>
</div>
<details class="card" id="editDiagnostics" open>
  <summary>
    <span>Problems</span>
    <span id="editDiagSummary">{{ editDiagSummary }}</span>
  </summary>
  <div id="editDiagList" aria-live="polite">
    <div v-if="!editDiagItems.length" id="editDiagEmpty" class="hint">This file has no errors or warnings.</div>
    <button v-for="d in editDiagItems" :key="d.i" class="edit-diag" type="button" :data-sev="d.sev" @click="onEditDiagClick(d.i)">
      <span class="sev" :class="d.sev">{{ d.sev }}</span>
      <span class="edit-diag-loc">{{ d.loc }}</span>
      <span class="edit-diag-msg">{{ d.message }} <span class="edit-diag-code">[{{ d.kind }}/{{ d.code }}]</span></span>
    </button>
  </div>
</details>
<div id="editLog" class="hint" :style="{ color: editLogIsError ? 'var(--bad)' : '' }">{{ editLogText }}</div>
</template>
