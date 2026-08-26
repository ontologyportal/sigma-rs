// cytoscape-node-html-label ships a `.d.ts` with no `export` statements, so
// TypeScript treats it as an ambient script rather than this module's types
// ("File ... is not a module"). This stands in as the module declaration;
// the actual shape is asserted locally in proof-graph.ts's `CyWithHtmlLabel`.
declare module 'cytoscape-node-html-label';
