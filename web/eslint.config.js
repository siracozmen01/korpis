// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's lint rules: typescript-eslint's strictest type-checked sets, the rules of hooks,
// and FormatJS's, so every string the panel shows comes from its catalogue (question 4 of §10 of
// docs/11-surface-web.md): no literal text in the markup a person reads, and every message with
// a stable identifier and its English text. Generated code is left as generated; tests may
// write the text they look for.

import js from "@eslint/js";
import formatjs from "eslint-plugin-formatjs";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "src/gen", "node_modules"] },
  js.configs.recommended,
  ...tseslint.configs.strictTypeChecked,
  ...tseslint.configs.stylisticTypeChecked,
  {
    languageOptions: {
      parserOptions: {
        project: [
          "./tsconfig.json",
          "./tsconfig.node.json",
          "./tsconfig.sw.json",
          "./tsconfig.e2e.json",
        ],
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: { "react-hooks": reactHooks, formatjs },
    settings: { formatjs: { additionalFunctionNames: ["t"] } },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "@typescript-eslint/no-explicit-any": "error",
      "formatjs/no-literal-string-in-jsx": [
        "error",
        { props: { include: [["*", "{aria-label,alt,title,placeholder,label}"]] } },
      ],
      "formatjs/enforce-default-message": ["error", "literal"],
      "formatjs/enforce-id": ["error", { idInterpolationPattern: "[sha512:contenthash:base64:8]" }],
      "formatjs/enforce-placeholders": "error",
      "formatjs/no-multiple-whitespaces": "error",
      "formatjs/no-complex-selectors": "error",
    },
  },
  {
    files: ["**/*.test.ts", "**/*.test.tsx", "e2e/**"],
    rules: { "formatjs/no-literal-string-in-jsx": "off", "formatjs/enforce-id": "off" },
  },
  {
    files: ["eslint.config.js"],
    ...tseslint.configs.disableTypeChecked,
  },
);
