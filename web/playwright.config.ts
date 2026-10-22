import { defineConfig } from '@playwright/test';

// Browser tests against the built board (`bun run build`) served by
// `vite preview` (PLAN P4-5, DL-140). Three projects: the phone runs every
// spec but the desktop one, the desktop runs the desktop layout, the terminal
// handover and the screenshots, the tablet only takes the screenshots
// (FD_SCREENS). Parallel lanes set FD_E2E_PORT to a free port (DL-302).
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
    trace: 'retain-on-failure',
  },
  projects: [
    {
      name: 'phone',
      testIgnore: ['**/desktop.spec.ts'],
      use: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true },
    },
    {
      name: 'desktop',
      testMatch: [
        '**/desktop.spec.ts',
        '**/handover.spec.ts',
        '**/screens.spec.ts',
        '**/gallery.spec.ts',
        '**/gallery-wall.spec.ts',
      ],
      use: { viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false },
    },
    {
      name: 'tablet',
      testMatch: ['**/screens.spec.ts'],
      use: { viewport: { width: 820, height: 1180 }, isMobile: false, hasTouch: true },
    },
  ],
  webServer: {
    command: `./node_modules/.bin/vite preview --host 127.0.0.1 --port ${PORT} --strictPort`,
    url: `http://127.0.0.1:${PORT}`,
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
