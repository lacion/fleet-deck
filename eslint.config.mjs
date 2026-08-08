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
      // strictTypeChecked bans EVERY non-string operand in a template literal,
      // integers included. The daemon templates primary-key ids constantly
      // (`plan #${p.plan_id}`, `question #${qid}`) — a number stringifies
      // losslessly with no `[object Object]`/`"null"` hazard, which is exactly
      // what the rest of this rule guards against. Re-allow numbers only; any /
      // boolean / nullish / never / regexp stay banned (their defaults).
      '@typescript-eslint/restrict-template-expressions': ['error', { allowNumber: true }],
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
      // Same rationale as the daemon block: integer ids in templates are safe.
      '@typescript-eslint/restrict-template-expressions': ['error', { allowNumber: true }],
    },
  },

  // ── Prettier compatibility — MUST be last (turns off stylistic conflicts) ────
  prettier,
);
