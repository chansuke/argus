// Typed lint for every TS package and app. The import rules carry what used to be
// comments: the phone bundle never loads Tauri, and apps reach the API only through
// @argus/client.
import js from "@eslint/js";
import tseslint from "typescript-eslint";

const tauri = {
  group: ["@tauri-apps/api", "@tauri-apps/api/*", "@argus/client/tauri", "**/transport-tauri"],
  message: "The phone bundle must not load Tauri. Import the transport from `@argus/client/ws`, or keep Tauri code in `transport-tauri.ts` and the desktop app.",
};
const generated = {
  group: ["@argus/client/src/*", "**/gen/api", "**/gen/api.ts"],
  message: "Import from `@argus/client`, which re-exports the generated API; `gen/` is regenerated from crates/argus-api.",
};

export default tseslint.config(
  { ignores: ["**/node_modules/**", "**/dist/**", "**/gen/**", "eslint.config.js"] },
  js.configs.recommended,
  ...tseslint.configs.strictTypeChecked,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
  },
  {
    // Shared by desktop and phone: everything but the Tauri transport.
    files: ["packages/argus-client/src/**/*.ts"],
    ignores: ["packages/argus-client/src/transport-tauri.ts"],
    rules: { "no-restricted-imports": ["error", { patterns: [tauri] }] },
  },
  {
    files: ["apps/mobile/**/*.{ts,tsx}"],
    rules: { "no-restricted-imports": ["error", { patterns: [tauri, generated] }] },
  },
  {
    files: ["apps/desktop/**/*.{ts,tsx}"],
    rules: { "no-restricted-imports": ["error", { patterns: [generated] }] },
  },
);
