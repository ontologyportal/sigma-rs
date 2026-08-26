// Ambient globals set by CDN <script> tags rather than imported as modules:
// Monaco's AMD loader (window.require/define/exports/module, window.monaco --
// see src/editor/monaco.ts and src/proof-graph.ts's UMD-global dance) and
// cytoscape's UMD self-registration (window.cytoscape). Untyped by design --
// these are third-party runtime values with no static shape known here.
export {};

declare global {
  interface Window {
    monaco?: any;
    require?: any;
    define?: any;
    exports?: any;
    module?: any;
    cytoscape?: any;
  }
}
