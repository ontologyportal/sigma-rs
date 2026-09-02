/**
 * Monaco: the lazy npm-package load, the `kif` and `tptp` languages, and the
 * diagnostic marker conversion. Shared by the Edit tab's IDE, the Ask/Tell
 * panes, and the read-only TPTP preview.
 */

import { formatKif } from 'sigmakee/sdk';
import { state } from '../state.ts';
import { lspOpenTag, lspRequest, lspSyncDocument, tagToUri } from './lsp-client.ts';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker.js?worker';

let monacoLoadPromise = null;

/** The in-flight (or settled) Monaco load, or null if it has not started. */
export function monacoLoading() {
  return monacoLoadPromise;
}

/** Load Monaco once, register the languages, and publish the namespace as
 *  `state.monaco` for everything that needs it after the fact.
 *
 *  Imports the slim editor API (no bundled typescript/json/html/css language
 *  contributions we never use) via a dynamic `import()`, so Vite code-splits
 *  it into its own chunk fetched only when this actually runs. */
export function loadMonaco() {
  if (monacoLoadPromise) return monacoLoadPromise;
  monacoLoadPromise = (async () => {
    self.MonacoEnvironment = { getWorker: () => new EditorWorker() };
    // `editor.api.js` alone is the bare API with NO feature contributions:
    // an editor built from it has no suggest controller (completion
    // providers register but nothing ever invokes them and there is no
    // widget), no hover controller, and no `editor.action.formatDocument` /
    // `editor.action.triggerSuggest` commands. `editor.all.js` registers
    // every editor-feature contribution while still excluding the heavy
    // language services (typescript/json/html/css) this app never uses.
    await import('monaco-editor/esm/vs/editor/editor.all.js');
    const monaco = await import('monaco-editor/esm/vs/editor/editor.api.js');
    defineKifLanguage(monaco);
    defineTptpLanguage(monaco);
    state.monaco = monaco;
    // Debug handle for automation / devtools poking (harmless in prod).
    (self as any).__monaco = monaco;
    return monaco;
  })();
  return monacoLoadPromise;
}

/** Monarch tokenizer mirroring `highlightKif`. */
const KIF_MONARCH = {
  defaultToken: '',
  tokenizer: {
    root: [
      { include: '@whitespace' },
      [/;.*$/, 'comment'],
      [/"(?:[^"\\]|\\.)*"/, 'string'],
      [/[?@][A-Za-z0-9_-]+/, 'variable'],
      [/-?\d+(?:\.\d+)?/, 'number'],
      [/<=>|=>/, 'keyword'],
      [/\(/, { token: 'delimiter.parenthesis', next: '@afterOpen' }],
      [/\)/, 'delimiter.parenthesis'],
      [/[A-Za-z_][A-Za-z0-9_-]*/, 'identifier'],
    ],
    afterOpen: [
      { include: '@whitespace' },
      [/\b(?:and|or|not|forall|exists|equal)\b/, { token: 'keyword', next: '@pop' }],
      [/[A-Za-z_][A-Za-z0-9_-]*/, { token: 'kif-function', next: '@pop' }],
      [/./, { token: '@rematch', next: '@pop' }],
    ],
    whitespace: [
      [/[ \t\r\n]+/, 'white'],
    ],
  },
};

/** LSP `CompletionItemKind` (protocol numbering) -> Monaco's enum (which
 *  numbers the same names differently). Covers the kinds sumo-lsp emits;
 *  anything else falls back to Text. */
function lspKindToMonaco(m, kind) {
  const K = m.languages.CompletionItemKind;
  return {
    1: K.Text, 2: K.Method, 3: K.Function, 6: K.Variable, 7: K.Class,
    8: K.Interface, 12: K.Value, 14: K.Keyword, 21: K.Constant,
  }[kind] ?? K.Text;
}

/**
 * A single unselectable, zero-effect item standing in for an empty
 * suggestion list. Monaco's suggest widget shows nothing at all for an
 * empty `suggestions` array from an automatic (typing-triggered)
 * invocation — indistinguishable from the request still being in flight.
 * Returning one placeholder item instead forces the widget open with an
 * explicit message, so "no matches" and "still waiting" are never confused.
 * `insertText: ''` makes accepting it (Enter/Tab) a harmless no-op.
 */
function placeholderSuggestion(m, range, label, typedWord) {
  return [{
    label,
    kind: m.languages.CompletionItemKind.Text,
    insertText: '',
    // Monaco fuzzy-filters provider results against what's already typed;
    // matching it against itself guarantees the placeholder always survives
    // that filter, however this word compares against `label`.
    filterText: typedWord,
    sortText: '',
    preselect: false,
    range,
  }];
}

/**
 * The LSP's context-aware completion, alongside (not replacing) the
 * KB-search provider above: it understands head vs argument position and
 * filters argument suggestions to the head's declared domain class, and its
 * items carry documentation popups. Only the Edit tab's live document has an
 * LSP identity; other kif models get nothing from this provider (the
 * KB-search one still serves them).
 *
 * The server returns the full candidate set (LSP clients filter locally);
 * to keep the worker round-trip payload sane we prefix-filter and cap here
 * before handing Monaco the list.
 */
function lspCompletionProvider(m) {
  const CAP = 200;
  return {
    async provideCompletionItems(model, position, _context, token) {
      const tag = lspOpenTag();
      if (!tag || state.monacoEditor?.getModel() !== model) return { suggestions: [] };
      const word = model.getWordUntilPosition(position);
      const range = {
        startLineNumber: position.lineNumber, startColumn: word.startColumn,
        endLineNumber: position.lineNumber, endColumn: word.endColumn,
      };
      try {
        // Sync FIRST: Monaco asks for completions on every keystroke, but the
        // server's copy of the document only refreshes on the (400ms) validate
        // debounce — classifying the cursor against that stale text yields a
        // non-completable context and an empty list, so the widget never
        // opens. The mid-typing guard makes this safe: a didChange with a
        // dangling paren updates the server's document rope (what completion
        // reads) without touching the KB.
        await lspSyncDocument(tag, model.getValue());
        const resp = await lspRequest<any>('textDocument/completion', {
          textDocument: { uri: tagToUri(tag) },
          position: { line: position.lineNumber - 1, character: position.column - 1 },
        });
        // Stale guard: Monaco cancels superseded invocations (new keystroke,
        // dismissed widget) — a result that arrives after cancellation is
        // computed against text that no longer exists, so drop it.
        if (token?.isCancellationRequested) return { suggestions: [] };
        // The server prefix-filters at the cursor and caps the list
        // (CompletionList with isIncomplete); older Array-shaped responses
        // are tolerated for completeness.
        const items = (Array.isArray(resp) ? resp : resp?.items) ?? [];
        const serverIncomplete = !Array.isArray(resp) && !!resp?.isIncomplete;
        console.debug('[lsp] completion:', word.word || '(no prefix)', '->', items.length, 'items');
        const suggestions = items.slice(0, CAP).map((i) => ({
          label: i.label,
          kind: lspKindToMonaco(m, i.kind),
          detail: i.detail,
          documentation: i.documentation?.value ? { value: i.documentation.value } : undefined,
          insertText: i.insertText ?? i.label,
          // Server-side relevance rank (the KB search index's ordering);
          // without it Monaco re-sorts by its own fuzzy score alone.
          sortText: i.sortText,
          range,
        }));
        // An incomplete (server-truncated) list must not be filtered locally
        // as the user types — symbols beyond the cap could never surface.
        // Propagating the flag makes Monaco re-query per keystroke, and the
        // narrower prefix re-filters server-side before the cap applies.
        if (suggestions.length === 0) {
          return { suggestions: placeholderSuggestion(m, range, 'No matches', word.word) };
        }
        return { suggestions, incomplete: serverIncomplete || items.length > CAP };
      } catch (e) {
        console.warn('[lsp] completion unavailable:', e);
        return { suggestions: placeholderSuggestion(m, range, 'Completion unavailable (see console)', word.word) };
      }
    },
  };
}

/**
 * The server's fixed token-type legend (`sumo-lsp`'s `semantic_tokens::TOKEN_TYPES`)
 * -- order matters, it's how the wire format's `typeIdx` resolves to a name.
 * No modifiers are emitted.
 */
const SEMANTIC_TOKEN_LEGEND = {
  tokenTypes: [
    'keyword', 'type', 'function', 'variable', 'string', 'number', 'comment', 'relation',
  ],
  tokenModifiers: [],
};

/**
 * KB-aware highlighting from the LSP's `textDocument/semanticTokens/full`
 * (`crates/lsp/src/handlers/semantic_tokens.rs`): a symbol classified as a
 * class colors as `type`, a function as `function`, and a predicate or any
 * other non-function relation as `relation` -- distinctions the Monarch
 * tokenizer above can't make since it never
 * consults the KB. Only the Edit tab's live document has an LSP identity;
 * other kif models get no semantic tokens (Monarch's lexical highlighting
 * still applies to them, and to comments/strings/delimiters here too --
 * semantic tokens only cover the LSP's classified subset and layer on top).
 */
function lspSemanticTokensProvider(_m) {
  return {
    getLegend() {
      return SEMANTIC_TOKEN_LEGEND;
    },
    async provideDocumentSemanticTokens(model, _lastResultId, token) {
      const tag = lspOpenTag();
      if (!tag || state.monacoEditor?.getModel() !== model) return null;
      try {
        const resp = await lspRequest<any>('textDocument/semanticTokens/full', {
          textDocument: { uri: tagToUri(tag) },
        });
        if (token?.isCancellationRequested || !resp?.data) return null;
        return { data: new Uint32Array(resp.data), resultId: resp.resultId };
      } catch (e) {
        console.warn('[lsp] semantic tokens unavailable:', e);
        return null;
      }
    },
    releaseDocumentSemanticTokens() {},
  };
}

function defineKifLanguage(m) {
  if (m.languages.getLanguages().some((l) => l.id === 'kif')) return;
  m.languages.register({ id: 'kif' });
  m.languages.setMonarchTokensProvider('kif', KIF_MONARCH);
  m.languages.setLanguageConfiguration('kif', {
    brackets: [['(', ')']],
    autoClosingPairs: [{ open: '(', close: ')' }, { open: '"', close: '"' }],
    // Match the tokenizer's notion of a symbol so word selection and
    // getWordAtPosition return whole SUMO terms, hyphens included — the Monaco
    // default stops at "-" and would hand back a fragment.
    wordPattern: /[A-Za-z_][A-Za-z0-9_-]*/g,
  });
  // Registering this is what makes Monaco's OWN "Format Document" command
  // (right-click menu, Shift+Alt+F) work, not just the toolbar button in the
  // Edit tab — both end up calling this same provider. The Edit tab's live
  // constituent formats through the LSP's `textDocument/formatting` (the
  // core round-trip formatter: canonical layout, comments preserved and
  // re-flowed); scratch buffers and the Ask/Tell panes — models the LSP has
  // never seen — fall back to the JS `formatKif` reflow.
  m.languages.registerDocumentFormattingEditProvider('kif', {
    async provideDocumentFormattingEdits(model) {
      const local = () => [{ range: model.getFullModelRange(), text: formatKif(model.getValue()) }];
      const tag = lspOpenTag();
      if (!tag || state.monacoEditor?.getModel() !== model) return local();
      try {
        const edits = await lspRequest<any[]>('textDocument/formatting', {
          textDocument: { uri: tagToUri(tag) },
          options: { tabSize: 2, insertSpaces: true },
        });
        // Empty edits = the server declined (parse errors); null = no doc.
        if (!edits?.length) return local();
        return edits.map((e) => ({
          range: {
            startLineNumber: e.range.start.line + 1,
            startColumn: e.range.start.character + 1,
            endLineNumber: e.range.end.line + 1,
            endColumn: e.range.end.character + 1,
          },
          text: e.newText,
        }));
      } catch (e) {
        console.warn('[lsp] formatting unavailable, using local formatter:', e);
        return local();
      }
    },
  });
  m.languages.registerCompletionItemProvider('kif', lspCompletionProvider(m));
  // KB-aware semantic highlighting (class/predicate/function distinctions
  // Monarch can't make) — see `lspSemanticTokensProvider`.
  m.languages.registerDocumentSemanticTokensProvider('kif', lspSemanticTokensProvider(m));
  // Symbol hover cards (kind, parents, documentation) from the LSP — only
  // for the Edit tab's live document; other kif models have no LSP identity.
  m.languages.registerHoverProvider('kif', {
    async provideHover(model, position) {
      const tag = lspOpenTag();
      if (!tag || state.monacoEditor?.getModel() !== model) return null;
      try {
        const h = await lspRequest<any>('textDocument/hover', {
          textDocument: { uri: tagToUri(tag) },
          position: { line: position.lineNumber - 1, character: position.column - 1 },
        });
        const value = h?.contents?.value;
        return value ? { contents: [{ value }] } : null;
      } catch (e) {
        console.warn('[lsp] hover unavailable:', e);
        return null;
      }
    },
  });
  // `type` / `function` / `relation` have no Monarch-lexical equivalent
  // (Monarch never consults the KB, so it can't tell a class symbol, a
  // function, and a predicate/relation apart) -- these rules exist only to
  // color the LSP semantic tokens of those names; `kif-function` above
  // stays for Monarch's own narrower "after an open paren" functor
  // highlight.
  m.editor.defineTheme('kif-light', {
    base: 'vs', inherit: true,
    rules: [
      { token: 'comment', foreground: '666666', fontStyle: 'italic' },
      { token: 'string', foreground: '1a7f37' },
      { token: 'number', foreground: '1a7f37' },
      { token: 'variable', foreground: '9a6700' },
      { token: 'keyword', foreground: '8250df', fontStyle: 'bold italic' },
      { token: 'kif-function', foreground: '2d6cdf' },
      { token: 'function', foreground: '2d6cdf' },
      { token: 'relation', foreground: 'c2266d' },
      { token: 'type', foreground: '0b7285' },
      { token: 'delimiter.parenthesis', foreground: '666666', fontStyle: 'bold' },
    ],
    colors: {},
  });
  m.editor.defineTheme('kif-dark', {
    base: 'vs-dark', inherit: true,
    rules: [
      { token: 'comment', foreground: '9aa0a6', fontStyle: 'italic' },
      { token: 'string', foreground: '4ac26b' },
      { token: 'number', foreground: '4ac26b' },
      { token: 'variable', foreground: 'e3b341' },
      { token: 'keyword', foreground: 'd2a8ff', fontStyle: 'bold italic' },
      { token: 'kif-function', foreground: '6ea8ff' },
      { token: 'function', foreground: '6ea8ff' },
      { token: 'relation', foreground: 'f78fb3' },
      { token: 'type', foreground: '66d9e8' },
      { token: 'delimiter.parenthesis', foreground: '9aa0a6', fontStyle: 'bold' },
    ],
    colors: {},
  });
}

/** Monarch tokenizer for the read-only TPTP preview pane. Reuses the `kif-*`
 * themes' token names (comment/string/number/keyword/kif-function/delimiter)
 * so the light/dark toggle at `setTheme` applies here too, with no separate
 * theme to keep in sync. */
const TPTP_MONARCH = {
  defaultToken: '',
  tokenizer: {
    root: [
      [/%.*$/, 'comment'],
      [/"(?:[^"\\]|\\.)*"/, 'string'],
      [/-?\d+(?:\.\d+)?/, 'number'],
      [/\b(?:fof|tff|cnf|thf)\b/, 'keyword'],
      [/\b(?:axiom|hypothesis|conjecture|negated_conjecture|type|plain)\b/, 'keyword'],
      [/[!?]\[/, 'keyword'],
      [/\b[A-Z][A-Za-z0-9_]*\b/, 'variable'],
      [/\b[a-z][A-Za-z0-9_]*\b/, 'kif-function'],
      [/[()[\],.]/, 'delimiter.parenthesis'],
    ],
  },
};

function defineTptpLanguage(m) {
  if (m.languages.getLanguages().some((l) => l.id === 'tptp')) return;
  m.languages.register({ id: 'tptp' });
  m.languages.setMonarchTokensProvider('tptp', TPTP_MONARCH);
}

const SEVERITY_TO_MONACO = { error: 'Error', warning: 'Warning', info: 'Info', hint: 'Hint' };

/** Diagnostics (from `validateFormula`, buffer-relative line/col) → Monaco markers. */
export function diagsToMarkers(diags) {
  const m = state.monaco;
  return diags.map((d) => ({
    startLineNumber: Math.max(1, d.line || 1),
    startColumn:     Math.max(1, d.col || 1),
    endLineNumber:   Math.max(1, d.end_line || d.line || 1),
    endColumn:       Math.max(1, (d.end_col || d.col || 1) + 1),
    message:         `[${d.kind}/${d.code}] ${d.message}`,
    severity:        m.MarkerSeverity[SEVERITY_TO_MONACO[d.severity]] || m.MarkerSeverity.Info,
  }));
}
