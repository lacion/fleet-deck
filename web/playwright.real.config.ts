import { defineConfig } from '@playwright/test';

// M2 P4-5: the MVP flows in a phone browser against real processes (one deck
// and two stations with the offline fake agent). The deck serves the built
// board (`bun run build`), so build the board first.
export default defineConfig({
  testDir: 'e2e-real',
  outputDir: 'test-results/real',
  globalSetup: './e2e-real/setup.ts',
  fullyParallel: false,
  workers: 1,
  forbidOnly: true,
  retries: 0,
  timeout: 120_000,
  expect: { timeout: 20_000 },
  reporter: [['list']],
  use: {
    browserName: 'chromium',
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    // The deck's board certificate is self-signed (A13); the phone accepts
    // it once, as a person does on first use.
    ignoreHTTPSErrors: true,
    trace: 'retain-on-failure',
  },
});
