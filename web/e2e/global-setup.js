// 拉起 clat e2e 宿主（门控 cargo 测试），握手后交给 specs；
// teardown 写 stop 文件让宿主优雅收尾（关停 serve、清理临时目录）。
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const REPO_ROOT = path.resolve(__dirname, '../..');
const HOSTS = ['run-command', 'long-stream', 'success', 'compact-slow'];
if (process.env.CLAT_LIVE_GLM_E2E === '1') HOSTS.push('live-glm');
const STARTUP_TIMEOUT_MS = 300_000; // 含 cargo 增量编译
const SHUTDOWN_TIMEOUT_MS = 60_000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function infoPath(key, stateDir) {
  return path.join(stateDir, `.serve-${key}.json`);
}

function stopPath(key, stateDir) {
  return path.join(stateDir, `.stop-${key}`);
}

async function globalSetup() {
  // Each Playwright invocation owns its handshake files. A fixed directory
  // under web/e2e let a second local/CI invocation delete the first one's
  // live host metadata midway through its specs.
  const stateDir = fs.mkdtempSync(path.join(os.tmpdir(), 'clat-web-e2e-'));
  process.env.CLAT_E2E_RUN_DIR = stateDir;
  for (const key of HOSTS) {
    fs.rmSync(infoPath(key, stateDir), { force: true });
    fs.rmSync(stopPath(key, stateDir), { force: true });
  }

  const cargoArgs = ['test'];
  if (process.env.CLAT_E2E_RELEASE === '1') cargoArgs.push('--release');
  cargoArgs.push('--lib', '--', '--ignored', 'serve_e2e_host', '--nocapture');
  const child = spawn(
    'cargo',
    cargoArgs,
    // CLAT_E2E_HOST=1 是宿主驻留的武装开关（CI 的 --ignored 门控面
    // 不带此变量——宿主瞬过，不挂 CI）。
    {
      cwd: REPO_ROOT,
      stdio: 'inherit',
      env: { ...process.env, CLAT_E2E_HOST: '1', CLAT_E2E_RUN_DIR: stateDir },
    },
  );

  try {
    const deadline = Date.now() + STARTUP_TIMEOUT_MS;
    for (const key of HOSTS) {
      while (!fs.existsSync(infoPath(key, stateDir))) {
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
  } catch (error) {
    child.kill('SIGKILL');
    fs.rmSync(stateDir, { recursive: true, force: true });
    throw error;
  }

  return async () => {
    try {
      for (const key of HOSTS) fs.writeFileSync(stopPath(key, stateDir), '');
      const exitDeadline = Date.now() + SHUTDOWN_TIMEOUT_MS;
      while (child.exitCode === null && Date.now() < exitDeadline) await sleep(300);
      if (child.exitCode === null) child.kill('SIGKILL');
    } finally {
      fs.rmSync(stateDir, { recursive: true, force: true });
    }
  };
}

module.exports = globalSetup;
