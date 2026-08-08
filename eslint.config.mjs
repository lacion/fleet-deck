// Flat config (ESLint 9/10). Type-aware, maximally strict — but scoped to the TS
// surface so the ~18k lines of legacy .mjs are green on day one. Legacy JS is NOT
// linted during the transition; each module opts in when it converts to .ts (F1b).
//
// no-undef is turned OFF for TS blocks per the typescript-eslint FAQ: the compiler
// already resolves globals/types, and no-undef misfires on TS-only constructs.
import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import prettier from 'eslint-config-prettier';
import globals from 'globals';

export default tseslint.config(
  // ── Ignore generated, vendored, and not-yet-converted surfaces ──────────────
  {
    ignores: [
      'node_modules/**',
      'board/node_modules/**',
      'scripts/fleetd/fleetd.bundle.mjs',
      'scripts/fleetd/board-dist/**',
      'board/dist/**',
      // Legacy JS is unlinted for now; it converts to TS per pillar (F1b).
      '**/*.mjs',
      '**/*.cjs',
      '**/*.js',
      '**/*.jsx',
    ],
  },

  // ── Daemon / CLI / tests — Node + Bun, type-aware ───────────────────────────
  {
    files: ['scripts/**/*.{ts,mts,cts}', 'bin/**/*.{ts,mts,cts}', 'tests/**/*.{ts,mts,cts}'],
    extends: [
      js.configs.recommended,
      ...tseslint.configs.strictTypeChecked,
      ...tseslint.configs.stylisticTypeChecked,
    ],
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
      globals: { ...globals.node },
    },
    rules: {
      // The TS compiler owns undefined-symbol detection (incl. the `Bun` global).
      'no-undef': 'off',
    },
  },

  // ── Board — browser + React, type-aware ─────────────────────────────────────
  {
    files: ['board/src/**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      ...tseslint.configs.strictTypeChecked,
      ...tseslint.configs.stylisticTypeChecked,
    ],
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
      globals: { ...globals.browser },
    },
    rules: {
      'no-undef': 'off',
    },
  },

  // ── Prettier compatibility — MUST be last (turns off stylistic conflicts) ────
  prettier,
);
