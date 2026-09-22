<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { navigate } from "../../router";
import { useShellStore } from "../../stores/shell.ts";
import { esc } from "../../utils/format";
import Card from "../Card.vue";
import WordNetEntry from "./WordNetEntry.vue";
import { SearchHit, WordNetMapping } from "sigmakee/sdk";
import Row from "../Row.vue";
import Col from "../Col.vue";

const props = defineProps<{
  /** The worker's `search` hits, newest query only. */
  hits: SearchHit[];
  /** The query the hits answer, for the count line and the WordNet narrowing. */
  query: string;
  /** Index of the keyboard-highlighted row, -1 for none. */
  selected: number;
  /** Appended to the count line when the hits came from the all-languages fallback. */
  langNote: string;
}>();

const listEl = ref<HTMLUListElement | null>(null);
const shell = useShellStore();

const relHits = computed(() =>
  props.hits.filter(
    (h) => !!h.kinds.find((k) => k == "predicate" || k == "function"),
  ),
);
const symHits = computed(() =>
  props.hits.filter(
    (h) => !h.kinds.includes("predicate") && !h.kinds.includes("function"),
  ),
);

watch(
  () => props.selected,
  async (i) => {
    if (i < 0) return;
    await nextTick();
    listEl.value?.children[i]?.scrollIntoView({ block: "nearest" });
  },
);

/** Plain-text breakdown of a search hit's rank score, one labeled
 *  contribution per line plus the total -- rendered as a native `title`
 *  tooltip on hover. */
function rankTooltip(hit: SearchHit): string {
  const lines = hit.rank_breakdown.map(
    (c) => `${c.label}: ${c.value >= 0 ? "+" : ""}${c.value.toFixed(1)}`,
  );
  lines.push(`= ${hit.rank.toFixed(1)}`);
  return lines.join("\n");
}

/** Kind labels for a result row. A WordNet hit whose SUMO anchor is not a
 *  symbol of the loaded KB has no kinds to show: WordNet knows the word,
 *  the loaded constituents do not define the term. */
function kindsText(hit: SearchHit): string {
  const kinds =
    hit.kinds.join(" · ") ||
    (hit.source === "wn" ? "not in loaded KB" : hit.source);
  return hit.sense ? `${kinds} · ${hit.sense}` : kinds;
}

/** Narrow a symbol's full WordNet mapping list down to the entries a search
 *  query actually matched -- a highly-connected symbol like `Canine` can
 *  carry 50+ mappings, and `DomesticDog` 170+, most of which are irrelevant
 *  to any one query. Matches only a whole lemma in the mapping's word list,
 *  case-insensitively -- "dog" matches the "dog" entry but not "sheep dog" or
 *  "domestic dog" -- so a substring hit inside a longer phrase doesn't drag
 *  in an unrelated mapping. A query that hits none of them (the symbol
 *  surfaced via its documentation or name instead) yields no inline mappings
 *  here -- the full list is still one click away on the man page's WordNet
 *  tab. */
function relevantWordnet(
  mappings: WordNetMapping[] | undefined,
  query: string,
): WordNetMapping[] {
  const q = query.trim().toLowerCase();
  if (!q || !mappings) return [];
  return mappings.filter((m) =>
    m.words
      .toLowerCase()
      .split(",")
      .some((w: string) => w.trim() === q),
  );
}

/** Render `&%Symbol` cross-reference markers as bold plain text (no link) --
 *  each row already links to the symbol. */
function boldifyDoc(text: unknown): string {
  return String(text)
    .split(/(&%[A-Za-z0-9_-]+)/)
    .map((part) => {
      const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
      return m ? `<b>${esc(m[1])}</b>` : esc(part);
    })
    .join("");
}
</script>

<template>
  <Card v-if="shell.effectiveLayout == 'comfortable'">
    <div class="hint count">
      {{ hits.length }} result{{ hits.length === 1 ? "" : "s" }} for
      <code>{{ query }}</code
      >{{ langNote }}
    </div>
    <ul ref="listEl" class="results">
      <li v-for="(h, i) in hits" :key="i" :class="{ selected: i === selected }">
        <a
          class="sym open"
          @click.prevent="navigate('browse', { q: query, sym: h.symbol })"
          >{{ h.symbol }}</a
        >
        <span class="kinds"
          >{{ kindsText(h) }} ·
          <span class="rank" :title="rankTooltip(h)"
            >rank {{ h.rank.toFixed(0) }}</span
          ></span
        >
        <div v-if="h.text" class="snippet" v-html="boldifyDoc(h.text)"></div>
        <WordNetEntry
          v-for="(m, j) in relevantWordnet(h.wordnet, query)"
          :key="j"
          :entry="m"
        />
      </li>
    </ul>
  </Card>

  <Row v-else-if="shell.effectiveLayout == 'classic'">
    <Col :span="6">
      <Card style="margin-right: 5px">
        <div class="hint count">
          {{ symHits.length }} symbol{{ symHits.length === 1 ? "" : "s" }} for
          <code>{{ query }}</code
          >{{ langNote }}
        </div>
        <ul ref="listEl" class="results">
          <li
            v-for="(h, i) in symHits"
            :key="i"
            :class="{ selected: i === selected }"
          >
            <a
              class="sym open"
              @click.prevent="navigate('browse', { q: query, sym: h.symbol })"
              >{{ h.symbol }}</a
            >
            <span class="kinds"
              >{{ kindsText(h) }} ·
              <span class="rank" :title="rankTooltip(h)"
                >rank {{ h.rank.toFixed(0) }}</span
              ></span
            >
            <div
              v-if="h.text"
              class="snippet"
              v-html="boldifyDoc(h.text)"
            ></div>
            <WordNetEntry
              v-for="(m, j) in relevantWordnet(h.wordnet, query)"
              :key="j"
              :entry="m"
            />
          </li>
        </ul>
      </Card>
    </Col>
    <Col :span="6">
      <Card style="margin-left: 5px">
        <div class="hint count">
          {{ relHits.length }} relation{{ relHits.length === 1 ? "" : "s" }} for
          <code>{{ query }}</code
          >{{ langNote }}
        </div>
        <ul ref="listEl" class="results">
          <li
            v-for="(h, i) in relHits"
            :key="i"
            :class="{ selected: i === selected }"
          >
            <a
              class="sym open"
              @click.prevent="navigate('browse', { q: query, sym: h.symbol })"
              >{{ h.symbol }}</a
            >
            <span class="kinds"
              >{{ kindsText(h) }} ·
              <span class="rank" :title="rankTooltip(h)"
                >rank {{ h.rank.toFixed(0) }}</span
              ></span
            >
            <div
              v-if="h.text"
              class="snippet"
              v-html="boldifyDoc(h.text)"
            ></div>
            <WordNetEntry
              v-for="(m, j) in relevantWordnet(h.wordnet, query)"
              :key="j"
              :entry="m"
            />
          </li>
        </ul>
      </Card>
    </Col>
  </Row>
</template>

<style scoped>
.col {
  display: flex;
  flex-direction: row;
}
.col .kinds {
  margin-left: 15px;
}
.kinds .rank {
  cursor: help;
  border-bottom: 1px dotted currentColor;
}
.count {
  margin-bottom: 6px;
}
.results li.selected {
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  border-radius: 6px;
}
.snippet {
  color: var(--muted);
  font-size: 13px;
}
</style>
