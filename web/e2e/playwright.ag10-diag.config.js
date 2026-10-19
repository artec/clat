// AG-DOGFOOD-10（第九轮甲方）诊断用例配置：A2 TITLE-OBS-1 的观测记录/
// 失败现场捕获用例消费 global-setup 的共享 success 宿主（与标准面同型），
// 但不属于标准 CI 面（诊断仪器，按需显式运行，证据留存于运行日志）。
//
// 用法：npx playwright test -c playwright.ag10-diag.config.js ag10-title-obs
const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './ag10',
  timeout: 300_000,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: { headless: true },
  globalSetup: require.resolve('./global-setup.js'),
});
