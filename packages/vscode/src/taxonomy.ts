// src/taxonomy.ts
//
// "SUMO: Show Taxonomy" -- a webview that renders a Mermaid graph
// of every upward taxonomy edge (subclass / instance /
// subrelation / subAttribute) reachable from a target symbol,
// plus the symbol's documentation.
//
// The server does the BFS once and ships the whole graph in one
// `sumo/taxonomy` response; the client is purely a rendering and
// navigation layer.  Back / Forward navigation tracks browsing
// history through the panel, and clicking a node (or a
// `&%SymbolName` reference inside the doc blurb) re-roots the
// graph on that symbol.
//
// The panel loads two vendored JS bundles from `vendor/`, staged there
// by scripts/sync-assets.mjs:
//
//   * mermaid            -- diagram rendering
//   * svg-pan-zoom       -- interactive pan/zoom on the generated SVG
//
// Both are loaded as `<script src="...">` via webview-URIs the
// extension resolves at panel-creation time; nothing the webview
// receives is evaluated as code.
//
// CSP note: `script-src` includes `'unsafe-inline'` because the
// rendered page has small inline event-handlers (`onclick="..."`).
// Mermaid itself does not require `'unsafe-eval'`.

import * as path from 'path';
import {
    ExtensionContext,
    OutputChannel,
    Uri,
    ViewColumn,
    window,
    workspace,
} from 'vscode';
import { LanguageClient } from 'vscode-languageclient/node';

// -- Wire types (must match crates/sumo-lsp/src/handlers/taxonomy.rs) --------

interface TaxonomyEdgeDto {
    from:     string;
    to:       string;
    relation: string;
}

interface DocEntryDto {
    language: string;
    text:     string;
}

interface TaxonomyResponse {
    symbol:        string;
    unknown:       boolean;
    documentation: DocEntryDto[];
    edges:         TaxonomyEdgeDto[];
}

interface HistoryState {
    canGoBack:    boolean;
    canGoForward: boolean;
}

// -- Public entry point -------------------------------------------------------

/**
 * Command handler for `sumo.showTaxonomy`.
 *
 * Resolves the target symbol in priority order:
 *   1. Explicit `argSymbol` argument (e.g. future callers that
 *      dispatch programmatically).
 *   2. The word under the cursor in the active editor.
 *
 * Falls through with a user-visible hint if neither yields a
 * symbol name.
 */
export async function showTaxonomyCommand(
    context:   ExtensionContext,
    getClient: () => LanguageClient | undefined,
    output:    OutputChannel,
    argSymbol?: unknown,
): Promise<void> {
    let symbol: string | undefined =
        typeof argSymbol === 'string' && argSymbol.length > 0 ? argSymbol : undefined;

    if (!symbol) {
        const editor = window.activeTextEditor;
        if (!editor) {
            window.showInformationMessage(
                'Open a .kif file and place the cursor on a symbol to view its taxonomy.',
            );
            return;
        }
        const range = editor.document.getWordRangeAtPosition(editor.selection.active);
        if (!range) {
            window.showInformationMessage(
                'Place the cursor on a symbol before running "SUMO: Show Taxonomy".',
            );
            return;
        }
        symbol = editor.document.getText(range);
    }

    const panel = window.createWebviewPanel(
        'sumoTaxonomy',
        `Taxonomy: ${symbol}`,
        ViewColumn.Beside,
        {
            enableScripts: true,
            // Only the vendored bundles are reachable from the
            // webview.  Tightening these roots contains the blast
            // radius of a hypothetical script injection.
            localResourceRoots: [
                Uri.file(path.join(context.extensionPath, 'vendor', 'mermaid')),
                Uri.file(path.join(context.extensionPath, 'vendor', 'svg-pan-zoom')),
            ],
            retainContextWhenHidden: true,
        },
    );

    const mermaidUri = panel.webview.asWebviewUri(Uri.file(
        path.join(context.extensionPath, 'vendor', 'mermaid', 'mermaid.min.js'),
    ));
    const svgPanZoomUri = panel.webview.asWebviewUri(Uri.file(
        path.join(context.extensionPath, 'vendor', 'svg-pan-zoom', 'svg-pan-zoom.min.js'),
    ));

    // Panel-local navigation history.  Stays live until the panel
    // is disposed; re-opening the command spawns a fresh history.
    let history: string[] = [];
    let currentIndex = -1;

    const updateWebview = async (target: string, fromHistory = false): Promise<void> => {
        if (!fromHistory) {
            // Truncate any "forward" entries; a new navigation
            // invalidates them, same as a browser.
            if (currentIndex < history.length - 1) {
                history = history.slice(0, currentIndex + 1);
            }
            history.push(target);
            currentIndex++;
        }

        panel.title = `Taxonomy: ${target}`;
        panel.webview.html = renderLoadingHtml(target);

        // Yield once so the loading screen actually paints before we
        // block on the LSP round-trip.  25 ms is enough on all
        // common platforms without being user-visible.
        await new Promise(resolve => setTimeout(resolve, 25));

        const client = getClient();
        if (!client) {
            panel.webview.html = renderErrorHtml(
                target,
                'The sumo-lsp server is not running.  Check the "SUMO / KIF" output channel.',
            );
            return;
        }

        let response: TaxonomyResponse;
        try {
            response = await client.sendRequest<TaxonomyResponse>(
                'sumo/taxonomy',
                { symbol: target },
            );
        } catch (err) {
            output.appendLine(`[taxonomy] request for "${target}" failed: ${err}`);
            panel.webview.html = renderErrorHtml(
                target,
                `The server rejected the taxonomy request: ${String(err)}`,
            );
            return;
        }

        if (response.unknown) {
            panel.webview.html = renderErrorHtml(
                target,
                `Symbol "${target}" is not defined in the active knowledge base.`,
            );
            return;
        }

        const state: HistoryState = {
            canGoBack:    currentIndex > 0,
            canGoForward: currentIndex < history.length - 1,
        };
        panel.webview.html = generateTaxonomyHtml(
            response,
            mermaidUri.toString(),
            svgPanZoomUri.toString(),
            panel.webview.cspSource,
            state,
        );
    };

    panel.webview.onDidReceiveMessage((message) => {
        switch (message?.command) {
            case 'openTaxonomy':
                if (typeof message.symbol === 'string' && message.symbol.length > 0) {
                    void updateWebview(message.symbol);
                }
                return;
            case 'goBack':
                if (currentIndex > 0) {
                    currentIndex--;
                    void updateWebview(history[currentIndex], true);
                }
                return;
            case 'goForward':
                if (currentIndex < history.length - 1) {
                    currentIndex++;
                    void updateWebview(history[currentIndex], true);
                }
                return;
        }
    });

    await updateWebview(symbol);
}

// -- HTML generation ----------------------------------------------------------

function renderLoadingHtml(symbol: string): string {
    return `<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"></head>
<body style="font-family: var(--vscode-font-family); padding: 20px; color: var(--vscode-editor-foreground); background-color: var(--vscode-editor-background);">
  <h3>Loading taxonomy for ${escapeHtml(symbol)}&hellip;</h3>
</body>
</html>`;
}

function renderErrorHtml(symbol: string, message: string): string {
    return `<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"></head>
<body style="font-family: var(--vscode-font-family); padding: 20px; color: var(--vscode-editor-foreground); background-color: var(--vscode-editor-background);">
  <h3>Taxonomy: ${escapeHtml(symbol)}</h3>
  <p>${escapeHtml(message)}</p>
</body>
</html>`;
}

/**
 * Build the taxonomy page: nav bar + doc blurb + Mermaid graph.
 *
 * Mermaid and svg-pan-zoom are loaded from vendored bundles via
 * webview-URIs.  The Mermaid graph is emitted as plain text
 * inside `.mermaid`; the boot script calls `mermaid.run()` to
 * render it, then hands the resulting SVG to svg-pan-zoom.
 */
function generateTaxonomyHtml(
    response:      TaxonomyResponse,
    mermaidUri:    string,
    svgPanZoomUri: string,
    cspSource:     string,
    history:       HistoryState,
): string {
    const targetLang =
        workspace.getConfiguration('sumo').get<string>('documentation.language')
        || 'EnglishLanguage';

    // Prefer the configured language; fall back to any available
    // documentation entry rather than showing nothing.
    const docEntry =
        response.documentation.find(d => d.language === targetLang)
        ?? response.documentation[0];
    const rawDoc = docEntry?.text ?? 'No documentation found in workspace.';

    // Escape first, then linkify `&%Symbol` references.  Order
    // matters: after escape, `&` has become `&amp;`, so the regex
    // matches `&amp;%...`.  The capture group is restricted to
    // alnum + `_` + `-`, so emitting it raw into the inline
    // `onclick='openSymbol("...")'` handler is injection-safe.
    const linkified = escapeHtml(rawDoc).replace(
        /&amp;%([A-Za-z0-9_-]+)/g,
        (_m, sym) => `<a href="#" onclick="openSymbol('${sym}'); return false;">${sym}</a>`,
    );

    // Collect every node reachable from the root.  The root is
    // unconditionally a node even when it has no parents (edges
    // list is empty).
    const nodeSet = new Set<string>([response.symbol]);
    for (const e of response.edges) {
        nodeSet.add(e.from);
        nodeSet.add(e.to);
    }

    // Emit Mermaid source.  IDs are sanitised because Mermaid's
    // node-ID grammar is stricter than SUMO's symbol grammar
    // (hyphens are legal in SUMO but would tokenise as minus in
    // Mermaid).  Labels are the original names with quotes
    // replaced -- Mermaid doesn't support escaping inside `"..."`.
    const lines: string[] = [
        'graph TD',
        'classDef default fill:#2d2d2d,stroke:#555,stroke-width:1px,color:#fff;',
        'classDef target fill:#0e639c,stroke:#007acc,stroke-width:2px,color:#fff;',
    ];
    for (const node of nodeSet) {
        const cls = node === response.symbol ? 'target' : 'default';
        const id  = mermaidId(node);
        lines.push(`${id}["${escapeMermaidLabel(node)}"]:::${cls}`);
        lines.push(`click ${id} callOpenSymbol`);
    }
    for (const edge of response.edges) {
        lines.push(
            `${mermaidId(edge.from)} -->|${escapeMermaidLabel(edge.relation)}| ${mermaidId(edge.to)}`,
        );
    }
    const mermaidGraph = lines.join('\n');

    // Mermaid click handlers get the raw node-ID back, not the
    // display label.  The webview side maps ID -> original symbol
    // via the embedded `nodeIdToSymbol` table below so
    // round-tripping through sanitisation doesn't lose the
    // original name.
    const idMap: Record<string, string> = {};
    for (const node of nodeSet) {
        idMap[mermaidId(node)] = node;
    }
    const idMapJson = JSON.stringify(idMap);

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline' ${cspSource}; style-src 'unsafe-inline'; img-src ${cspSource} data:;">
  <script src="${mermaidUri}"></script>
  <script src="${svgPanZoomUri}"></script>
  <style>
    body {
      font-family: var(--vscode-font-family);
      padding: 16px;
      color: var(--vscode-editor-foreground);
      background-color: var(--vscode-editor-background);
      display: flex;
      flex-direction: column;
      height: 95vh;
      margin: 0;
    }
    h1 {
      color: var(--vscode-textLink-foreground);
      border-bottom: 1px solid var(--vscode-panel-border);
      padding-bottom: 5px;
      margin-top: 4px;
    }
    .nav-buttons { margin-bottom: 12px; display: flex; gap: 8px; }
    .nav-btn {
      background-color: var(--vscode-button-background);
      color: var(--vscode-button-foreground);
      border: none;
      padding: 6px 12px;
      cursor: pointer;
      font-family: inherit;
    }
    .nav-btn:hover:not(:disabled) { background-color: var(--vscode-button-hoverBackground); }
    .nav-btn:disabled {
      opacity: 0.5; cursor: default;
      background-color: var(--vscode-button-secondaryBackground);
      color: var(--vscode-button-secondaryForeground);
    }
    .doc-block {
      margin-bottom: 12px;
      padding: 10px;
      background-color: var(--vscode-textBlockQuote-background);
      border-left: 4px solid var(--vscode-textBlockQuote-border);
      max-height: 25vh;
      overflow-y: auto;
    }
    .doc-block a {
      color: var(--vscode-textLink-foreground);
      text-decoration: none;
    }
    .doc-block a:hover { text-decoration: underline; }
    .mermaid {
      flex-grow: 1;
      overflow: hidden;
      border: 1px solid var(--vscode-panel-border);
      min-height: 200px;
    }
    .mermaid svg { height: 100%; width: 100%; }
  </style>
</head>
<body>
  <div class="nav-buttons">
    <button class="nav-btn" onclick="goBack()" ${history.canGoBack ? '' : 'disabled'}>&larr; Back</button>
    <button class="nav-btn" onclick="goForward()" ${history.canGoForward ? '' : 'disabled'}>Forward &rarr;</button>
  </div>
  <h1>Taxonomy: ${escapeHtml(response.symbol)}</h1>
  <div class="doc-block">${linkified}</div>
  <div class="mermaid">
${mermaidGraph}
  </div>

  <script>
    const vscode = acquireVsCodeApi();
    const nodeIdToSymbol = ${idMapJson};

    // Match the editor's light/dark theme so rendered nodes
    // aren't legible only against one background.
    const theme = document.body.classList.contains('vscode-dark') ? 'dark' : 'default';
    mermaid.initialize({ startOnLoad: false, theme: theme, securityLevel: 'loose' });

    // Inline handlers installed on the generated HTML.
    window.openSymbol = (sym) => vscode.postMessage({ command: 'openTaxonomy', symbol: sym });
    // Mermaid "click NODE callOpenSymbol" invokes this with the
    // node-ID string.  Map it back to the original symbol so the
    // server can look it up.
    window.callOpenSymbol = (nodeId) => {
      const sym = nodeIdToSymbol[nodeId] || nodeId;
      vscode.postMessage({ command: 'openTaxonomy', symbol: sym });
    };
    window.goBack    = () => vscode.postMessage({ command: 'goBack' });
    window.goForward = () => vscode.postMessage({ command: 'goForward' });

    (async () => {
      try {
        await mermaid.run();
        const svg = document.querySelector('.mermaid svg');
        if (svg) {
          svg.style.width = '100%';
          svg.style.height = '100%';
          svg.style.maxWidth = 'none';
          const panZoom = svgPanZoom(svg, {
            zoomEnabled: true,
            controlIconsEnabled: true,
            fit: true,
            center: true,
            minZoom: 0.1,
          });
          panZoom.resize(); panZoom.fit(); panZoom.center();
          window.addEventListener('resize', () => {
            panZoom.resize(); panZoom.fit(); panZoom.center();
          });
        }
      } catch (e) {
        console.error('Mermaid/PanZoom error:', e);
        const container = document.querySelector('.mermaid');
        if (container) {
          container.innerHTML =
            '<p style="padding:10px">Failed to render graph: ' +
            (e && e.message ? e.message : String(e)) + '</p>';
        }
      }
    })();
  </script>
</body>
</html>`;
}

// -- Escaping helpers ---------------------------------------------------------

function escapeHtml(s: string): string {
    return s
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

/**
 * Convert a SUMO symbol name into a Mermaid-safe node ID.
 * SUMO identifiers are typically alnum + underscore, but we
 * defensively strip anything else (hyphens, dots, ...) that
 * Mermaid would otherwise reject as syntax.
 */
function mermaidId(name: string): string {
    const cleaned = name.replace(/[^A-Za-z0-9_]/g, '_');
    // Prefix if empty or starts with a digit -- Mermaid IDs must
    // begin with a letter/underscore.
    if (cleaned.length === 0 || /^[0-9]/.test(cleaned)) {
        return '_' + cleaned;
    }
    return cleaned;
}

/**
 * Neutralise characters that would break a `"..."` label in
 * Mermaid source.  Double quotes are swapped to a visually
 * similar unicode variant; Mermaid has no real escape syntax.
 */
function escapeMermaidLabel(s: string): string {
    return s.replace(/"/g, '\u201C');
}
