<script setup lang="ts">
import { SortSig } from "sigmakee/sdk";
import { computed } from "vue";

export interface ArgSource {
  /** The SUO-KIF axiom, e.g. "(domain MeasureFn 1 RealNumber)". */
  axiom?: string;
  /** File the axiom lives in, e.g. "Merge.kif". */
  file?: string;
  /** Link to the axiom (file + line, GitHub URL, …). */
  href?: string;
}

/** One signature slot as the SDK reports it, plus optional presentation
 *  extras the KB does not (yet) supply. */
export type RelationArg = SortSig & {
  /** Argument name, e.g. "agent"; falls back to "arg N". */
  name?: string;
  source?: ArgSource;
};

const props = withDefaults(
  defineProps<{
    /** Relation name, e.g. "MeasureFn". */
    name: string;
    /** Classes the relation is an instance of: ["BinaryFunction", "TotalValuedRelation"]. */
    kinds?: string[];
    /** Fixed arguments, in order. */
    args: RelationArg[];
    /** Return type. Omit for predicates. */
    range?: RelationArg;
    /** Instance of VariableArityRelation: trailing args repeat the last declared domain. */
    variableArity?: boolean;
    /** A worked example, e.g. "(MeasureFn 3 Meter)". */
    example?: string;
    /** Builds the href for a SUMO term link. */
    termHref?: (term: string) => string;
    /** Text for the "add a domain" link on undeclared args. Omit to hide. */
    fixHref?: string;
  }>(),
  {
    kinds: () => [],
    variableArity: false,
    termHref: (term: string) => `#${term}`,
  },
);

const undeclared = computed(() =>
  props.args.filter((a) => a.status === "undeclared"),
);
const inherited = computed(() =>
  props.args.filter((a) => a.status === "inherited"),
);
const lastDeclared = computed(() =>
  [...props.args].reverse().find((a) => a.status !== "undeclared"),
);

function argLabel(a: RelationArg, i: number) {
  return a.name ?? `arg ${i + 1}`;
}

function statusLabel(a: RelationArg) {
  switch (a.status) {
    case "inherited":
      return `inherited from ${a.inherited_from ?? "parent"}`;
    case "undeclared":
      return "undeclared";
    default:
      return "declared";
  }
}
</script>

<template>
  <section class="sig">
    <header class="sig-head">
      <ul v-if="kinds.length" class="sig-kinds">
        <li v-for="k in kinds" :key="k">
          <a class="xref" :data-sym="k" :href="termHref(k)">{{ k }}</a>
        </li>
      </ul>
    </header>

    <div class="sig-line">
      <code class="sig-code">
        <span class="sig-name">{{ name }}</span
        >(<template v-for="(a, i) in args" :key="i"
          ><span v-if="i > 0">, </span
          ><span v-if="a.subclass" class="sig-sub" title="subclass of"
            >&sub;</span
          ><a
            v-if="a.type"
            class="xref"
            :data-sym="a.type"
            :href="termHref(a.type)"
            >{{ a.type }}</a
          ><span v-else class="sig-missing" title="no domain declared"
            >?</span
          ></template
        ><span v-if="variableArity">, &hellip;</span>)<template
          v-if="range?.type"
        >
          <span class="sig-arrow"> &rarr; </span
          ><span v-if="range.subclass" class="sig-sub" title="subclass of"
            >&sub;</span
          ><a
            class="xref"
            :data-sym="range.type"
            :href="termHref(range.type)"
            >{{ range.type }}</a
          ></template
        >
      </code>
    </div>

    <table class="sig-table">
      <thead>
        <tr>
          <th scope="col">Arg</th>
          <th scope="col">Type</th>
          <th scope="col" class="sig-right">Source</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="(a, i) in args"
          :key="i"
          :class="{ 'is-undeclared': a.status === 'undeclared' }"
        >
          <td class="sig-arg">
            <span class="sig-idx">{{ i + 1 }}</span>
            <span :class="a.name ? 'sig-argname' : 'sig-argname is-fallback'">{{
              argLabel(a, i)
            }}</span>
          </td>
          <td>
            <template v-if="a.type">
              <div class="sig-type">
                <span v-if="a.subclass" class="sig-sub">&sub;</span>
                <span class="sig-muted">{{
                  a.subclass ? "subclass of" : "instance of"
                }}</span>
                <a class="xref" :data-sym="a.type" :href="termHref(a.type)">{{
                  a.type
                }}</a>
              </div>
              <code v-if="a.source?.axiom" class="sig-axiom">{{
                a.source.axiom
              }}</code>
            </template>
            <div v-else class="sig-type sig-warn-text">
              No domain declared for this argument.
              <a v-if="fixHref" :href="fixHref">Add one</a>
            </div>
          </td>
          <td class="sig-right">
            <span class="sig-status" :data-status="a.status">{{
              statusLabel(a)
            }}</span>
            <a v-if="a.source?.file" class="sig-file" :href="a.source.href">{{
              a.source.file
            }}</a>
          </td>
        </tr>

        <tr v-if="variableArity && lastDeclared?.type">
          <td class="sig-arg">
            <span class="sig-idx">{{ args.length + 1 }}&hellip;</span>
          </td>
          <td>
            <div class="sig-type">
              <span v-if="lastDeclared.subclass" class="sig-sub">&sub;</span>
              <span class="sig-muted">{{
                lastDeclared.subclass ? "subclass of" : "instance of"
              }}</span>
              <a
                class="xref"
                :data-sym="lastDeclared.type"
                :href="termHref(lastDeclared.type)"
                >{{ lastDeclared.type }}</a
              >
            </div>
            <span class="sig-note"
              >Variable arity: further arguments follow the last declared
              domain</span
            >
          </td>
          <td class="sig-right">
            <span class="sig-status" data-status="implied">implied</span>
          </td>
        </tr>

        <tr v-if="range?.type">
          <td class="sig-arg">
            <span class="sig-idx sig-arrow">&rarr;</span>
            <span class="sig-argname">range</span>
          </td>
          <td>
            <div class="sig-type">
              <span v-if="range.subclass" class="sig-sub">&sub;</span>
              <span class="sig-muted">{{
                range.subclass ? "subclass of" : "instance of"
              }}</span>
              <a
                class="xref"
                :data-sym="range.type"
                :href="termHref(range.type)"
                >{{ range.type }}</a
              >
            </div>
            <code v-if="range.source?.axiom" class="sig-axiom">{{
              range.source.axiom
            }}</code>
          </td>
          <td class="sig-right">
            <span class="sig-status" :data-status="range.status">{{
              statusLabel(range)
            }}</span>
            <a
              v-if="range.source?.file"
              class="sig-file"
              :href="range.source.href"
              >{{ range.source.file }}</a
            >
          </td>
        </tr>
      </tbody>
    </table>

    <p
      v-if="inherited.length || range?.status === 'inherited'"
      class="sig-note-row"
    >
      Slots marked
      <span class="sig-status" data-status="inherited">inherited</span> come
      from a <code>subrelation</code> parent's declaration and apply to
      {{ name }} unchanged.
    </p>

    <p v-if="undeclared.length" class="sig-warn" role="status">
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path
          d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"
        />
        <line x1="12" y1="9" x2="12" y2="13" />
        <line x1="12" y1="17" x2="12.01" y2="17" />
      </svg>
      <span>
        {{
          undeclared.length === 1
            ? "One argument has"
            : `${undeclared.length} arguments have`
        }}
        no domain declaration. Axioms using {{ name }} won't be type-checked on
        {{ undeclared.length === 1 ? "it" : "them" }}.
      </span>
    </p>
  </section>
</template>

<style scoped>
/* Palette follows the app theme (assets/styles.css); override the --sig-*
   tokens on a parent to restyle. */
.sig {
  --sig-bg: transparent;
  --sig-well: var(--card);
  --sig-border: var(--line);
  --sig-text: var(--fg);
  --sig-text-2: var(--fg);
  --sig-muted: var(--muted);
  --sig-faint: var(--muted);
  --sig-link: var(--accent);
  --sig-link-hover: var(--accent);
  --sig-accent: var(--op);
  --sig-ok-bg: color-mix(in srgb, var(--ok) 15%, transparent);
  --sig-ok-fg: var(--ok);
  --sig-inherit-bg: color-mix(in srgb, var(--accent) 15%, transparent);
  --sig-inherit-fg: var(--accent);
  --sig-warn-bg: color-mix(in srgb, var(--warn) 15%, transparent);
  --sig-warn-fg: var(--warn);
  --sig-mono: var(--mono);

  display: flex;
  flex-direction: column;
  gap: 16px;
  color: var(--sig-text);
  background: var(--sig-bg);
  font-size: 14px;
  line-height: 1.4;
}

.sig a {
  color: var(--sig-link);
  text-decoration: none;
}
.sig a:hover {
  color: var(--sig-link-hover);
  text-decoration: underline;
}

.sig-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.sig-title {
  margin: 0;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--sig-muted);
}
.sig-kinds {
  display: flex;
  gap: 8px;
  margin: 0;
  padding: 0;
  list-style: none;
  flex-wrap: wrap;
}
.sig-kinds a {
  display: inline-block;
  font-size: 11px;
  font-weight: 600;
  padding: 3px 8px;
  border-radius: 4px;
  background: var(--sig-border);
  color: var(--sig-text-2);
}

.sig-line {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border-radius: 8px;
  background: var(--sig-well);
  border: 1px solid var(--sig-border);
}
.sig-code {
  flex: 1 1 auto;
  font-family: var(--sig-mono);
  font-size: 16px;
  line-height: 1.5;
  overflow-x: auto;
  white-space: nowrap;
}
.sig-name {
  font-weight: 600;
}
.sig-sub,
.sig-arrow {
  color: var(--sig-accent);
}
.sig-missing {
  color: var(--sig-warn-fg);
  font-weight: 700;
}

.sig-table {
  width: 100%;
  border-collapse: collapse;
}
.sig-table th {
  padding: 0 6px 6px;
  text-align: left;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--sig-faint);
  border-bottom: 1px solid var(--sig-border);
}
.sig-table td {
  padding: 12px 6px;
  vertical-align: top;
  border-bottom: 1px solid var(--sig-border);
}
.sig-table tbody tr:last-child td {
  border-bottom: 0;
}
.sig-arg {
  width: 96px;
  white-space: nowrap;
}
.sig-idx {
  font-family: var(--sig-mono);
  font-size: 13px;
  color: var(--sig-faint);
  margin-right: 8px;
}
.sig-argname.is-fallback {
  color: var(--sig-muted);
}
.sig-type {
  display: flex;
  gap: 6px;
  align-items: baseline;
  flex-wrap: wrap;
}
.sig-muted {
  color: var(--sig-muted);
}
.sig-axiom,
.sig-note {
  display: block;
  margin-top: 4px;
  font-size: 12px;
  color: var(--sig-faint);
}
.sig-axiom {
  font-family: var(--sig-mono);
}
.sig-right {
  text-align: right;
  width: 120px;
}
.sig-right .sig-status,
.sig-right .sig-file {
  display: block;
  margin-left: auto;
  width: fit-content;
}
.sig-status {
  font-size: 11px;
  font-weight: 600;
  padding: 2px 7px;
  border-radius: 4px;
  background: var(--sig-border);
  color: var(--sig-text-2);
  white-space: nowrap;
}
.sig-status[data-status="declared"] {
  background: var(--sig-ok-bg);
  color: var(--sig-ok-fg);
}
.sig-status[data-status="inherited"] {
  background: var(--sig-inherit-bg);
  color: var(--sig-inherit-fg);
}
.sig-status[data-status="undeclared"] {
  background: var(--sig-warn-bg);
  color: var(--sig-warn-fg);
}
.sig-note-row {
  margin: 0;
  font-size: 13px;
  color: var(--sig-muted);
}
.sig-note-row code {
  font-family: var(--sig-mono);
}
.sig-file {
  margin-top: 4px;
  font-size: 12px;
  color: var(--sig-muted);
}
.is-undeclared .sig-idx {
  color: var(--sig-warn-fg);
}
.sig-warn-text {
  color: var(--sig-warn-fg);
}

.sig-warn {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  margin: 0;
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--sig-warn-bg);
  color: var(--sig-warn-fg);
  font-size: 13px;
}
.sig-warn svg {
  flex: none;
  margin-top: 1px;
}
.sig-warn a {
  color: inherit;
  text-decoration: underline;
}
</style>
