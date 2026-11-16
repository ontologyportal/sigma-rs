<script setup lang="ts">
/** Controlled English -> SUO-KIF via CELT, against the loaded KB and WordNet
 *  lexicon. Each sentence is shown with its KIF or the reason it was
 *  refused. The input is mirrored to `?q=` so a translation can be shared. */
import { ref, shallowRef } from "vue";
import type { CeltTranslation } from "sigmakee/sdk";
import { call } from "../services/sigma";
import { updateParams } from "../router";
import { useTabQuery } from "../composables/useTabQuery";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import CopyButton from "../components/CopyButton.vue";

const text = ref("");
const busy = ref(false);
const translation = shallowRef<CeltTranslation | null>(null);
const error = ref("");

const { query, onQuery, str } = useTabQuery(["translate"]);

onQuery((q) => {
  text.value = str(q.q);
  if (text.value.trim()) void run(text.value);
  else clear();
});

function clear() {
  translation.value = null;
  error.value = "";
}

async function run(input: string) {
  busy.value = true;
  clear();
  try {
    const { translation: t } = await call("translateCelt", { text: input });
    if (!t.available)
      error.value = t.error || "The WordNet-SUMO lexicon is not loaded.";
    else translation.value = t;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    busy.value = false;
  }
}

async function translate() {
  const input = text.value.trim();
  if (!input || busy.value) return;
  // A changed query re-runs through `onQuery`; an unchanged one would not.
  if (input === str(query.value.q)) await run(input);
  else await updateParams({ q: input });
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    void translate();
  }
}
</script>

<template>
  <Card
    title="Translate"
    description="Convert controlled English sentences into SUO-KIF using the loaded knowledge base and WordNet lexicon."
  >
    <label class="input-label" for="translate-input">English</label>
    <textarea
      id="translate-input"
      v-model="text"
      rows="4"
      spellcheck="true"
      placeholder="Every dog is an animal. Is Rex a dog?"
      :disabled="busy"
      @keydown="onKeydown"
    />
    <div class="inline between actions">
      <span class="hint">Ctrl/Cmd+Enter to translate</span>
      <BusyButton
        :busy="busy"
        label="Translate"
        busy-label="Translating..."
        :disabled="!text.trim()"
        @click="translate"
      />
    </div>
  </Card>

  <Card v-if="error">
    <p class="bad" role="alert">{{ error }}</p>
  </Card>

  <Card v-else-if="translation">
    <p v-if="!translation.sentences.length" class="hint">
      No sentence was recognized.
    </p>
    <ol class="sentences">
      <li v-for="(s, i) in translation.sentences" :key="`${s.offset}-${i}`">
        <div class="inline tight center">
          <span class="kind" :class="s.kif ? 'ok' : 'refused'">{{
            s.kif ? s.kind : "not translated"
          }}</span>
          <span class="source">{{ s.text }}</span>
        </div>
        <div v-if="s.kif" class="kif">
          <code>{{ s.kif }}</code>
          <CopyButton :text="s.kif" title="Copy KIF" />
        </div>
        <p v-else class="bad reason">{{ s.reason }}</p>
      </li>
    </ol>
  </Card>
</template>

<style scoped>
.input-label {
  margin-top: 12px;
}
.actions {
  margin-top: 8px;
}
.sentences {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  gap: 14px;
}
.kind {
  padding: 2px 8px;
  border-radius: 20px;
  font-size: 11px;
  font-weight: 700;
  text-transform: capitalize;
  white-space: nowrap;
}
.kind.ok {
  background: color-mix(in srgb, var(--ok) 20%, transparent);
  color: var(--ok);
}
.kind.refused {
  background: color-mix(in srgb, var(--bad) 18%, transparent);
  color: var(--bad);
}
.kif {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 8px;
  margin-top: 6px;
  padding: 8px 10px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--bg);
}
.kif code {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  font-size: 13px;
}
.reason {
  margin: 6px 0 0;
  font-size: 13px;
}
</style>
