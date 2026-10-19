// AG-DOGFOOD-10（第九轮甲方）自举用例配置：A1 BELL-HOST-1 /
// A3 J1-EDGE-1 完全自举宿主（真 clat serve 二进制 + 真 PTY TUI + 真
// Chromium），不消费 global-setup 的共享门控宿主，运行时不支付那 12 个
// host 的启动成本，也不进入 `npm test`（playwright.config.js testDir=
// ./tests）的标准 CI 面——CI 不构建 clat 二进制，自举用例必须显式
// 本地/审核运行（需先 `cargo build -p clat`，可用 CLAT_E2E_BIN 覆盖路径）。
//
// 用法：npx playwright test -c playwright.ag10.config.js ag10-bell-host
//       npx playwright test -c playwright.ag10.config.js ag10-j1-edge
// （ag10-title-obs*.spec.js 诊断用例走 playwright.ag10-diag.config.js。）
const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './ag10',
  timeout: 300_000,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: { headless: true },
});
