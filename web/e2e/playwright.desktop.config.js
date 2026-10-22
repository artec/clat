const { defineConfig } = require('@playwright/test');
module.exports = defineConfig({
  testDir: './desktop', timeout: 30_000, workers: 1, retries: 0,
  reporter: [['list']], use: { headless: true },
});
