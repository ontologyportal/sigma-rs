// Workspace-wide ESLint (flat config): TypeScript + Vue across packages/.
// Run with `npm run lint`; the pre-commit hook runs the same command.

import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import pluginVue from "eslint-plugin-vue";

export default tseslint.config(
  {
    ignores: [
      ".claude/",
      "**/node_modules/",
      "**/.vscode-test/",
      "**/.wrangler/",
      "**/vendor/",
      "**/*.min.js",
      "crates/",
      "**/dist/",
      "**/dist-*/",
      "**/worker-dist/",
      "**/out/",
      "packages/vampire/",
      "packages/web/public/",
      "packages/vscode/server/",
      "packages/sigmakee/dist/",
      "target/",
      "eval-out/",
      "wikipedia-kb/",
      "docs/",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...pluginVue.configs["flat/recommended"],
  {
    files: ["**/*.vue"],
    languageOptions: {
      parserOptions: { parser: tseslint.parser },
    },
  },
  {
    languageOptions: {
      parserOptions: { tsconfigRootDir: import.meta.dirname },
      globals: { ...globals.browser, ...globals.node, ...globals.worker },
    },
    rules: {
      // The worker's wire shapes are untyped by design (see services/sigma.ts).
      "@typescript-eslint/no-explicit-any": "off",
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_", caughtErrors: "none" },
      ],
      "no-empty": ["error", { allowEmptyCatch: true }],
      // Components are named by file (BrowseTab.vue, Card.vue); the
      // multi-word rule exists to avoid clashes with native elements, which
      // PascalCase usage already prevents.
      "vue/multi-word-component-names": "off",
      // Optional TS props are undefined by design; no default is needed.
      "vue/require-default-prop": "off",
      // Highlighted formulas (utils/highlight-*.ts) are rendered with v-html
      // on purpose; the text is escaped by the highlighter.
      "vue/no-v-html": "off",
      // Formatting is prettier's job; prettier picks single quotes when the
      // value contains double quotes, so allow that here too.
      "vue/html-quotes": ["warn", "double", { avoidEscape: true }],
      "vue/max-attributes-per-line": "off",
      "vue/singleline-html-element-content-newline": "off",
      "vue/html-self-closing": "off",
      "vue/html-indent": "off",
      "vue/attributes-order": "off",
      "vue/first-attribute-linebreak": "off",
      "vue/html-closing-bracket-newline": "off",
      "vue/multiline-html-element-content-newline": "off",
    },
  },
);
