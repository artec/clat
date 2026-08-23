// clat web e2e：宿主是真 clat serve（进程内 TestProvider，由
// global-setup 经 cargo test -- --ignored serve_e2e_host 拉起）。
const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  timeout: 120_000,
  // 宿主是有状态的（每个 key 一个会话历史）；串行 = 确定性。
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: { headless: true },
  globalSetup: require.resolve('./global-setup.js'),
});
