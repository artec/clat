// 拉起 clat e2e 宿主（门控 cargo 测试），握手后交给 specs；
// teardown 写 stop 文件让宿主优雅收尾（关停 serve、清理临时目录）。
const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');

const REPO_ROOT = path.resolve(__dirname, '../..');
const HOSTS = ['run-command', 'long-stream'];
const STARTUP_TIMEOUT_MS = 300_000; // 含 cargo 增量编译
const SHUTDOWN_TIMEOUT_MS = 60_000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function infoPath(key) {
  return path.join(__dirname, `.serve-${key}.json`);
}

function stopPath(key) {
  return path.join(__dirname, `.stop-${key}`);
}

async function globalSetup() {
  for (const key of HOSTS) {
    fs.rmSync(infoPath(key), { force: true });
    fs.rmSync(stopPath(key), { force: true });
  }

  const child = spawn(
    'cargo',
    ['test', '--lib', '--', '--ignored', 'serve_e2e_host', '--nocapture'],
    // CLAT_E2E_HOST=1 是宿主驻留的武装开关（CI 的 --ignored 门控面
    // 不带此变量——宿主瞬过，不挂 CI）。
    { cwd: REPO_ROOT, stdio: 'inherit', env: { ...process.env, CLAT_E2E_HOST: '1' } },
  );

  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  for (const key of HOSTS) {
    while (!fs.existsSync(infoPath(key))) {
      if (Date.now() > deadline) {
        child.kill('SIGKILL');
        throw new Error(`e2e host "${key}" did not start within ${STARTUP_TIMEOUT_MS}ms`);
      }
      if (child.exitCode !== null) {
        throw new Error(`e2e host process exited early (code ${child.exitCode})`);
      }
      await sleep(500);
    }
  }

  return async () => {
    for (const key of HOSTS) fs.writeFileSync(stopPath(key), '');
    const exitDeadline = Date.now() + SHUTDOWN_TIMEOUT_MS;
    while (child.exitCode === null && Date.now() < exitDeadline) await sleep(300);
    if (child.exitCode === null) child.kill('SIGKILL');
    for (const key of HOSTS) {
      fs.rmSync(stopPath(key), { force: true });
      fs.rmSync(infoPath(key), { force: true });
    }
  };
}

module.exports = globalSetup;
