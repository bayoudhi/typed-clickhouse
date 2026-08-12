// ESLint flat config for the workspace.
//
// Inlined when packages/eslint-config-custom was removed. The previous shared
// config targeted Next.js (eslint-config-next, eslint-config-turbo, a
// settings.next.rootDir block); no Next.js app survives the prune, so only the
// TypeScript rules that packages/lib and apps/cli-npm actually rely on are kept.
const eslintJs = require("@eslint/js");
const tseslint = require("typescript-eslint");
const prettierConfig = require("eslint-config-prettier");

module.exports = [
  {
    ignores: [
      "**/node_modules/**",
      "**/.tch/**", // Generated code
      "**/dist/**",
    ],
  },
  eslintJs.configs.recommended,
  ...tseslint.configs.recommended,
  { rules: prettierConfig.rules || {} },
  {
    rules: {
      // Prefix a variable or argument with _ to mark it intentionally unused.
      // This convention is documented in CLAUDE.md.
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
      "@typescript-eslint/no-explicit-any": "off",
    },
  },
];
