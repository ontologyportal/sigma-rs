// src/askWebview.ts
//
// Webview panel that shows the result of a `sumo/ask` request.
//
// MVP scope: one panel, three sections (status / proof / raw
// transcript).  No history, no in-place re-querying, no cancellation
// -- those layer on cleanly once the kernel protocol grows
// streaming support.  The panel is disposed when the user closes it;
// the next ask spawns a fresh one.

import {
    Uri,
    ViewColumn,
    WebviewPanel,
    window,
} from 'vscode';

import type { AskResult } from './kernelClient';

/**
 * Show an ask-result panel for `result`.  `query` is the conjecture
 * that was sent; echoed in the header for context.
 */
export function showAskResult(query: string, result: AskResult): WebviewPanel {
    const panel = window.createWebviewPanel(
        'sumoAsk',
        `Ask: ${truncate(query, 40)}`,
        ViewColumn.Beside,
        {
            enableScripts: false,      // the page is fully static
            localResourceRoots: [],    // defence in depth
            retainContextWhenHidden: true,
        },
    );
    panel.webview.html = renderHtml(query, result);
    return panel;
}

function renderHtml(query: string, result: AskResult): string {
    const { cls, icon } = verdictStyle(result.status);
    const proof = result.proofKif.length === 0
        ? '<p class="muted">(no proof steps — the prover did not emit a proof section, or the conjecture was disproved / timed out)</p>'
        : `<ol class="proof">${result.proofKif.map(s => `<li><code>${escapeHtml(s)}</code></li>`).join('')}</ol>`;

    const bindings = result.bindings.length === 0
        ? ''
        : `<section>
             <h3>Bindings</h3>
             <ul>${result.bindings.map(b => `<li><code>${escapeHtml(b)}</code></li>`).join('')}</ul>
           </section>`;

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy"
        content="default-src 'none'; style-src 'unsafe-inline';">
  <style>
    body {
      font-family: var(--vscode-font-family);
      color: var(--vscode-editor-foreground);
      background-color: var(--vscode-editor-background);
      padding: 16px 24px;
      line-height: 1.5;
    }
    h1 {
      font-size: 1.4em;
      margin-top: 0;
      border-bottom: 1px solid var(--vscode-panel-border);
      padding-bottom: 6px;
    }
    h3 {
      color: var(--vscode-textLink-foreground);
      margin-top: 24px;
      margin-bottom: 6px;
    }
    code, pre {
      font-family: var(--vscode-editor-font-family);
      font-size: var(--vscode-editor-font-size);
    }
    pre {
      background-color: var(--vscode-textBlockQuote-background);
      border-left: 4px solid var(--vscode-textBlockQuote-border);
      padding: 10px 12px;
      overflow-x: auto;
      white-space: pre-wrap;
    }
    .query {
      background-color: var(--vscode-textBlockQuote-background);
      border-left: 4px solid var(--vscode-textBlockQuote-border);
      padding: 10px 12px;
      font-family: var(--vscode-editor-font-family);
    }
    .verdict {
      display: inline-block;
      padding: 4px 12px;
      border-radius: 4px;
      font-weight: 600;
      font-size: 1.1em;
      margin: 0 0 12px 0;
    }
    .verdict-proved       { background: #1a6b2e; color: #fff; }
    .verdict-disproved    { background: #a37500; color: #fff; }
    .verdict-consistent   { background: #1a6b2e; color: #fff; }
    .verdict-inconsistent { background: #8a2020; color: #fff; }
    .verdict-timeout      { background: #a37500; color: #fff; }
    .verdict-unknown      { background: #555;    color: #fff; }
    .proof li { margin-bottom: 4px; }
    .proof code { white-space: pre; }
    .muted { opacity: 0.65; font-style: italic; }
    details { margin-top: 16px; }
    details summary {
      cursor: pointer;
      color: var(--vscode-textLink-foreground);
      padding: 4px 0;
    }
  </style>
</head>
<body>
  <h1>Ask</h1>
  <div class="query"><code>${escapeHtml(query)}</code></div>
  <p class="verdict verdict-${cls}">${icon} ${escapeHtml(result.status)}</p>

  ${bindings}

  <section>
    <h3>Proof</h3>
    ${proof}
  </section>

  <details>
    <summary>Raw Vampire output</summary>
    <pre>${escapeHtml(result.raw || '(empty)')}</pre>
  </details>
</body>
</html>`;
}

function verdictStyle(status: string): { cls: string; icon: string } {
    switch (status) {
        case 'Proved':       return { cls: 'proved',       icon: '✓' };
        case 'Disproved':    return { cls: 'disproved',    icon: '✗' };
        case 'Consistent':   return { cls: 'consistent',   icon: '≈' };
        case 'Inconsistent': return { cls: 'inconsistent', icon: '⊥' };
        case 'Timeout':      return { cls: 'timeout',      icon: '⏱' };
        case 'Unknown':
        default:             return { cls: 'unknown',      icon: '?' };
    }
}

function truncate(s: string, max: number): string {
    if (s.length <= max) { return s; }
    return s.slice(0, max - 1) + '…';
}

function escapeHtml(s: string): string {
    return s
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

// Unused-import guard so `Uri` isn't stripped when the file is
// later extended to load CSS / JS via webview-URIs.
void (null as unknown as Uri);
