import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";

export default tseslint.config(
  {
    ignores: ["dist/", "node_modules/", "src-tauri/", ".llama.cpp/", "*.config.js", "*.config.ts"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      // Essential hooks rules — enforce rules-of-hooks and warn on
      // missing deps. The v7 React-Compiler rules (refs, set-state-in-effect)
      // are intentionally disabled: this project doesn't use the compiler and
      // patterns like `routeRef.current = route` are intentional.
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
    },
  },
  {
    // shadcn/ui files export variant helpers (e.g. `buttonVariants`) next to
    // their component. That is the upstream layout, so the fast-refresh
    // purity rule is not enforced there.
    files: ["src/components/ui/**"],
    rules: {
      "react-refresh/only-export-components": "off",
    },
  },
  {
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
);
