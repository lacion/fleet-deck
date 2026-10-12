import { defineConfig } from '@playwright/test';

// Browser tests at a phone viewport (PLAN P4-5). They run offline against
// the built board (`bun run build`) served by `vite preview`. Parallel
// lanes set FD_E2E_PORT to a free port (DL-302).
const PORT = Number(process.env['FD_E2E_PORT'] ?? 4179);

export default defineConfig({
  testDir: 'e2e',
  outputDir: 'test-results',
  fullyParallel: true,
  forbidOnly: true,
  retries: 0,
  timeout: 30_000,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    browserName: 'chromium',
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    trace: 'retain-on-failure',
  },
  webServer: {
    command: `./node_modules/.bin/vite preview --host 127.0.0.1 --port ${PORT} --strictPort`,
    url: `http://127.0.0.1:${PORT}`,
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
