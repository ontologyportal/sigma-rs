// `monaco-editor` ships types for its package entry, but not for the deep ESM
// paths this app imports to control what gets bundled (see services/monaco.ts:
// `editor.all.js` for the feature contributions, `editor.api.js` for the API
// surface). The API module is read through the package's own `MonacoNs` type
// at the import site; this side-effect module has no surface of its own.
declare module "monaco-editor/esm/vs/editor/editor.all.js";
