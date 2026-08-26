/**
 * Monaco: the CDN load, the `kif` and `tptp` languages, and the diagnostic
 * marker conversion. Shared by the Edit tab's IDE, the Ask/Tell panes, and the
 * read-only TPTP preview.
 */

import { formatKif } from 'sigmakee/sdk';
import { state } from '../state.ts';
import { call } from '../rpc.ts';

const MONACO_VERSION = '0.55.1';
const MONACO_CDN = `https://cdn.jsdelivr.net/npm/monaco-editor@${MONACO_VERSION}/min/vs`;

let monacoLoadPromise = null;

/** The in-flight (or settled) Monaco load, or null if it has not started —
 *  the Cytoscape loader waits on it before hiding the AMD globals. */
export function monacoLoading() {
  return monacoLoadPromise;
}

/** Load Monaco once, register the languages, and publish the namespace as
 *  `state.monaco` for everything that needs it after the fact. */
export function loadMonaco() {
  if (monacoLoadPromise) return monacoLoadPromise;
  monacoLoadPromise = new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = `${MONACO_CDN}/loader.js`;
    script.onload = () => {
      window.require.config({ paths: { vs: MONACO_CDN } });
      window.require(['vs/editor/editor.main'], () => {
        defineKifLanguage(window.monaco);
        defineTptpLanguage(window.monaco);
        state.monaco = window.monaco;
        resolve(window.monaco);
      }, reject);
    };
    script.onerror = () => reject(new Error(`failed to load Monaco from ${MONACO_CDN}`));
    document.head.appendChild(script);
  });
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

/** Monaco `CompletionItemKind` for a man-paged symbol's `ManKind` labels (see core's `ManKind::as_str`). */
function kindToCompletionKind(m, kinds) {
  const K = m.languages.CompletionItemKind;
  if (kinds.includes('class')) return K.Class;
  if (kinds.includes('relation')) return K.Interface;
  if (kinds.includes('function')) return K.Function;
  if (kinds.includes('predicate')) return K.Method;
  if (kinds.includes('instance')) return K.Value;
  if (kinds.includes('individual')) return K.Constant;
  return K.Text;
}

// Only the newest in-flight completion request may resolve into suggestions —
// typing several characters in a row fires one `search` per keystroke, and a
// slow early one must not clobber the list for what's on screen now.
let completionSeq = 0;

function isRelationPosition(model, position, wordStartColumn) {
  let line = position.lineNumber;
  while (line >= 1) {
    const text = model.getLineContent(line);
    let i = line === position.lineNumber ? wordStartColumn - 2 : text.length - 1;
    while (i >= 0 && /\s/.test(text[i])) i--;
    if (i >= 0) return text[i] === '(';
    line--;
  }
  return false;
}

/**
 * Real KB symbols only, via the same `search` the Home tab uses — this
 * replaces Monaco's default word-based suggestions (any string already
 * typed in the buffer), which has no notion of what's actually a SUMO term.
 */
function kifCompletionProvider(m) {
  return {
    provideCompletionItems(model, position) {
      const word = model.getWordUntilPosition(position);
      const prefix = word.word;
      if (!prefix) return { suggestions: [] };
      const range = {
        startLineNumber: position.lineNumber, startColumn: word.startColumn,
        endLineNumber: position.lineNumber, endColumn: word.endColumn,
      };
      const seq = ++completionSeq;
      const relationOnly = isRelationPosition(model, position, word.startColumn);
        return call('search', {
          query: prefix,
          limit: 50,
          kind: relationOnly ? 'relation' : undefined,
        })
        .then((r) => {
          if (seq !== completionSeq) return { suggestions: [] };
          const prefixLc = prefix.toLowerCase();
          const suggestions = r.hits
            .filter((h) => h.symbol.toLowerCase().startsWith(prefixLc))
            .map((h) => ({
              label: h.symbol,
              kind: kindToCompletionKind(m, h.kinds),
              detail: h.kinds.join(' · ') || h.source,
              insertText: h.symbol,
              range,
            }));
          return { suggestions };
        })
        .catch(() => ({ suggestions: [] }));
    },
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
  // Edit tab — both end up calling this same provider, so there's exactly one
  // implementation of "what formatting means" (formatKif, from the SDK).
  m.languages.registerDocumentFormattingEditProvider('kif', {
    provideDocumentFormattingEdits(model) {
      return [{ range: model.getFullModelRange(), text: formatKif(model.getValue()) }];
    },
  });
  m.languages.registerCompletionItemProvider('kif', kifCompletionProvider(m));
  m.editor.defineTheme('kif-light', {
    base: 'vs', inherit: true,
    rules: [
      { token: 'comment', foreground: '666666', fontStyle: 'italic' },
      { token: 'string', foreground: '1a7f37' },
      { token: 'number', foreground: '1a7f37' },
      { token: 'variable', foreground: '9a6700' },
      { token: 'keyword', foreground: '8250df', fontStyle: 'bold italic' },
      { token: 'kif-function', foreground: '2d6cdf' },
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
