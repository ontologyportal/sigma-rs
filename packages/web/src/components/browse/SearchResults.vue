<script setup lang="ts">
import { navigate } from "../../router";
import { esc } from "../../utils/format";
import Card from "../Card.vue";

defineProps<{
  /** The worker's `search` hits, newest query only. */
  hits: any[];
  /** The query the hits answer, for the count line and the WordNet narrowing. */
  query: string;
  /** Appended to the count line when the hits came from the all-languages fallback. */
  langNote: string;
}>();

/** Plain-text breakdown of a search hit's rank score, one labeled
 *  contribution per line plus the total -- rendered as a native `title`
 *  tooltip on hover. */
function rankTooltip(hit: any): string {
  const lines = hit.rank_breakdown.map(
    (c: any) => `${c.label}: ${c.value >= 0 ? "+" : ""}${c.value.toFixed(1)}`,
  );
  lines.push(`= ${hit.rank.toFixed(1)}`);
  return lines.join("\n");
}

function kindsText(hit: any): string {
  const kinds = hit.kinds.join(" · ") || hit.source;
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
function relevantWordnet(mappings: any[] | undefined, query: string): any[] {
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
  <Card>
    <div class="hint" style="margin-bottom: 6px">
      {{ hits.length }} result{{ hits.length === 1 ? "" : "s" }} for
      <code>{{ query }}</code
      >{{ langNote }}
    </div>
    <ul class="results">
      <li v-for="(h, i) in hits" :key="i">
        <a
          class="sym open"
          @click.prevent="navigate('browse', { sym: h.symbol })"
          >{{ h.symbol }}</a
        >
        <span class="kinds"
          >{{ kindsText(h) }} ·
          <span class="rank" :title="rankTooltip(h)"
            >rank {{ h.rank.toFixed(0) }}</span
          ></span
        >
        <div v-if="h.text" class="snippet" v-html="boldifyDoc(h.text)"></div>
        <div
          v-for="(m, j) in relevantWordnet(h.wordnet, query)"
          :key="j"
          class="wn-entry"
        >
          <div>"{{ m.words }}" ({{ m.pos }}) — {{ m.mapping }} mapping</div>
          <div class="hint">{{ m.gloss }}</div>
        </div>
      </li>
    </ul>
  </Card>
</template>

<style scoped>
.kinds .rank {
  cursor: help;
  border-bottom: 1px dotted currentColor;
}
.snippet {
  color: var(--muted);
  font-size: 13px;
}
/* WordNet mapping entries inline beneath a search-result snippet. */
.wn-entry {
  font-size: 13px;
  margin-top: 4px;
  padding-left: 8px;
  border-left: 2px solid var(--line);
}
.wn-entry + .wn-entry {
  margin-top: 8px;
}
</style>
