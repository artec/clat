// Isolated acceptance runner; these ephemeral keys never alter release trust.
import { spawn, spawnSync } from 'node:child_process';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

const repo = path.resolve(import.meta.dirname, '../..');
function option(name) {
  const index = process.argv.indexOf(name);
  if (index < 0 || !process.argv[index + 1]) throw new Error(`required: ${name}`);
  return process.argv[index + 1];
}
const source = path.resolve(option('--package'));
const clat = path.resolve(option('--clat'));
const target = option('--target');
const scratch = await mkdtemp(path.join(os.tmpdir(), 'clat-plugin-market-acceptance-'));
function run(command, args) {
  const result = spawnSync(command, args, { cwd: repo, stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.status})`);
}
let server;
try {
  const packageDir = path.join(scratch, 'package');
  await cp(source, packageDir, { recursive: true });
  // Any input signature remains untouched; only this private copy uses fixture keys.
  for (const file of ['clat-plugin.publisher.json', 'clat-plugin.minisig']) await rm(path.join(packageDir, file), { force: true });
  for (const role of ['publisher', 'index']) run('minisign', ['-G', '-W', '-p', path.join(scratch, `${role}.pub`), '-s', path.join(scratch, `${role}.key`)]);
  run('node', ['market/scripts/sign-package.mjs', '--package', packageDir, '--publisher', 'artec-fixture', '--public-key', path.join(scratch, 'publisher.pub'), '--minisign-key', path.join(scratch, 'publisher.key')]);
  const publication = path.join(scratch, 'publication');
  run('node', ['market/scripts/stage-package.mjs', '--package', packageDir, '--out', publication, '--target', target, '--publisher-key-id', 'fixture-only', '--review-url', 'https://example.com/fixture-review', '--source-url', 'https://github.com/artec/clat', '--clat', clat]);
  const index = JSON.parse(await readFile(path.join(publication, 'index.source.proposed.json'), 'utf8'));
  const now = Math.floor(Date.now() / 1000);
  index.market.generatedAtUnix = now; index.market.expiresAtUnix = now + 3600;
  const indexPath = path.join(publication, 'index.json');
  await writeFile(indexPath, JSON.stringify(index) + '\n');
  run('minisign', ['-S', '-s', path.join(scratch, 'index.key'), '-m', indexPath, '-x', indexPath + '.minisig', '-t', `CLAT plugin index cn.at.pi generated ${now}`]);
  server = http.createServer(async (request, response) => {
    try {
      if (request.url === '/fixture/v1/messages') {
        for await (const chunk of request) { void chunk; }
        if (request.headers['x-api-key'] !== 'private-fixture-key') { response.writeHead(401); response.end('{"error":{"message":"invalid fixture API key"}}'); return; }
        response.setHeader('Content-Type', 'application/json');
        response.end(JSON.stringify({ content: [{ type: 'web_search_tool_result', content: [{ type: 'web_search_result', url: 'https://example.com/clat', title: 'CLAT PLG-2 fixture' }] }] })); return;
      }
      if (!/^\/(index\.json(?:\.minisig)?|packages\/[A-Za-z0-9._-]+\.clatpkg)$/.test(request.url)) { response.writeHead(404); response.end(); return; }
      const body = await readFile(path.join(publication, request.url));
      response.setHeader('Content-Length', body.length); response.end(body);
    } catch { response.writeHead(500); response.end(); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/`;
  const env = { ...process.env, CLAT_PLG2_MARKET_URL: base, CLAT_PLG2_MARKET_PUBLIC_KEY: path.join(scratch, 'index.pub'), DEEPSEEK_SEARCH_BASE_URL: base + 'fixture/v1' };
  const child = spawn('npm', ['test', '--', 'tests/plugin-market.spec.js', 'tests/plugin-configuration.spec.js'], { cwd: path.join(repo, 'web/e2e'), env, stdio: 'inherit' });
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  if (code !== 0) throw new Error(`Playwright acceptance failed (${code})`);
} finally {
  if (server) await new Promise(resolve => server.close(resolve));
  await rm(scratch, { recursive: true, force: true });
}
