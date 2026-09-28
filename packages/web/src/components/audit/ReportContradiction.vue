<script setup lang="ts">
import { computed, ref } from "vue";
import type { AuditStep } from "sigmakee/sdk";
import BaseDialog from "../BaseDialog.vue";
import BusyButton from "../BusyButton.vue";
import {
  createSumoIssue,
  findSumoIssue,
  type IssueRef,
} from "../../api/github";
import { SUMO } from "../../constants";
import { GitOrigin, originId } from "../../models/Origin";
import { lspSyncedText } from "../../services/lsp";
import { useAuthStore } from "../../stores/auth";
import { useChangesStore } from "../../stores/changes";
import { useKBStore } from "../../stores/kb";
import { useProverStore } from "../../stores/prover";
import { useShellStore } from "../../stores/shell";
import {
  axiomDiagnostics,
  buildContradictionIssue,
  contradictionFingerprint,
  fingerprintMarker,
  reportability,
} from "../../utils/contradictionReport";
import { errMsg } from "../../utils/format";

/** "Report to SUMO" for one audit contradiction: offered only when every
 *  cited axiom comes from an unmodified file of the official SUMO repository,
 *  and files a GitHub issue there carrying the full proof. */
const props = defineProps<{
  steps: AuditStep[];
  prose?: string;
  /** Display name of the engine that found it ("SUPr", "Vampire"). */
  backend: string;
}>();

const kb = useKBStore();
const changes = useChangesStore();
const auth = useAuthStore();
const prover = useProverStore();
const shell = useShellStore();

/** Loaded from the default SUMO repo, with no saved local edit and no unsaved
 *  editor buffer reconciled into the live KB in its place. */
function isUpstream(file: string): boolean {
  const c = kb.byFile(file);
  if (!c || c.origin.kind !== "sumo" || !(c.origin as GitOrigin).isDefault)
    return false;
  if (changes.record(c.name, "sumo")) return false;
  const live = lspSyncedText(file);
  return live === null || live === c.text;
}

const rep = computed(() => reportability(props.steps, isUpstream));

const open = ref(false);
const preparing = ref(false);
const creating = ref(false);
const title = ref("");
const body = ref("");
const existing = ref<IssueRef | null>(null);
const created = ref<IssueRef | null>(null);
const error = ref("");

/** The commit the default SUMO group was last synced at, when recorded. */
function loadedCommit(): string | null {
  const c = rep.value.files.map((f) => kb.byFile(f)).find(Boolean);
  return c ? (kb.updateBaselines[originId(c.origin)] ?? null) : null;
}

async function start() {
  open.value = true;
  preparing.value = true;
  error.value = "";
  existing.value = null;
  created.value = null;
  try {
    const fingerprint = await contradictionFingerprint(props.steps);
    const commit = loadedCommit();
    const v = shell.version;
    const cfg = prover.config();
    const issue = buildContradictionIssue({
      steps: props.steps,
      prose: props.prose,
      fingerprint,
      commit,
      blobBase: `https://github.com/${SUMO.owner}/${SUMO.repo}/blob/${commit ?? SUMO.branch}`,
      context: [
        `Found by: ${props.backend} (Sigma consistency audit)`,
        `Time limit: ${cfg.timeLimitSecs ?? "?"}s`,
        `Sigma: ${v ? `${v.version} (build ${v.build}, ${v.commit})` : "unknown"}`,
        `Page: ${location.origin}${location.pathname}`,
      ],
      diagnostics: axiomDiagnostics(
        props.steps,
        kb.diagnostics,
        (f) => kb.byFile(f)?.text ?? null,
      )
        .flatMap((a) => a.diagnostics)
        .filter((d) => d.severity === "error" || d.severity === "warning"),
    });
    title.value = issue.title;
    body.value = issue.body;
    existing.value = await findSumoIssue(fingerprintMarker(fingerprint)).catch(
      () => null,
    );
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    preparing.value = false;
  }
}

async function submit() {
  if (!auth.token) {
    auth.openLoginDialog();
    return;
  }
  creating.value = true;
  error.value = "";
  try {
    created.value = await createSumoIssue(title.value.trim(), body.value);
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    creating.value = false;
  }
}

const reportLabel = computed(() =>
  existing.value ? "Report anyway" : "Create issue",
);
</script>

<template>
  <button
    v-if="rep.ok"
    class="btn ghost report-btn"
    type="button"
    title="Open an issue on the SUMO repository with this contradiction's full proof"
    @click="start"
  >
    Report to SUMO
  </button>
  <span v-else class="hint">
    Not reportable to SUMO: {{ rep.blockers.join("; ") }}.
  </span>

  <BaseDialog
    v-model="open"
    title="Report contradiction to SUMO"
    width="min(820px, 94vw)"
  >
    <p class="hint">
      Opens an issue on
      <a
        :href="`https://github.com/${SUMO.owner}/${SUMO.repo}/issues`"
        target="_blank"
        rel="noopener"
        >{{ SUMO.owner }}/{{ SUMO.repo }}</a
      >
      as {{ auth.user ? `@${auth.user.login}` : "your GitHub account" }}.
    </p>
    <p v-if="preparing" class="hint">Preparing report…</p>
    <template v-else-if="created">
      <p>
        Opened
        <a :href="created.url" target="_blank" rel="noopener"
          >#{{ created.number }}</a
        >
        on {{ SUMO.owner }}/{{ SUMO.repo }}. Thanks!
      </p>
    </template>
    <template v-else-if="title">
      <p v-if="existing" class="notice">
        This contradiction was already reported:
        <a :href="existing.url" target="_blank" rel="noopener"
          >#{{ existing.number }} {{ existing.title }}</a
        >
        ({{ existing.state }}).
      </p>
      <label for="report-title">Title</label>
      <input id="report-title" v-model="title" type="text" />
      <label>Body</label>
      <pre class="body">{{ body }}</pre>
    </template>
    <p v-if="error" class="hint bad">{{ error }}</p>
    <template #actions>
      <span class="spacer"></span>
      <button class="btn ghost" type="button" @click="open = false">
        {{ created ? "Close" : "Cancel" }}
      </button>
      <template v-if="!created && !preparing && title">
        <button
          v-if="!auth.signedIn"
          class="btn"
          type="button"
          @click="auth.openLoginDialog()"
        >
          Log in with GitHub
        </button>
        <BusyButton
          v-else
          :busy="creating"
          :label="reportLabel"
          busy-label="Creating…"
          :ghost="!!existing"
          :disabled="!title.trim()"
          @click="submit"
        />
      </template>
    </template>
  </BaseDialog>
</template>

<style scoped>
button.report-btn {
  height: 32px;
  font-size: 13px;
}
p {
  margin: 0 0 10px;
}
label {
  display: block;
  font-size: 12px;
  color: var(--muted);
  margin: 10px 0 4px;
}
.body {
  max-height: min(46vh, 420px);
  overflow: auto;
  margin: 0;
  padding: 10px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--bg);
  font-size: 12px;
  white-space: pre-wrap;
  word-break: break-word;
}
.notice {
  padding: 8px 10px;
  border-left: 3px solid var(--warn);
  background: color-mix(in srgb, var(--warn) 10%, transparent);
  border-radius: 4px;
}
</style>
