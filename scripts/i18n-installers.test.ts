import { expect, test } from 'bun:test';
import { bashResources, powershellResources, updateInstallers } from './i18n-installers';
import {readFileSync} from 'node:fs';

function shellText(locale: string, key: string, parameters: string[]) {
  const script = bashResources() + '\nclat_install_locale="$1"; shift; text "$@"';
  const result = Bun.spawnSync(['bash', '-c', script, 'fixture', locale, key, ...parameters],
    { stdin: 'ignore', stdout: 'pipe', stderr: 'pipe' });
  expect(result.exitCode).toBe(0);
  return result.stdout.toString();
}
test('user installers derive their translations from the authority catalogs', () => {
  updateInstallers(true);
  expect(powershellResources()).toContain('zh-Hant');
  expect(powershellResources()).not.toContain('Invoke-Expression');
});
test('standalone shell installer translates three languages and never interprets parameter text', () => {
  expect(shellText('en', 'installer.done', [])).toBe('done');
  expect(shellText('zh-Hans', 'installer.done', [])).toBe('完成');
  expect(shellText('zh-Hant', 'installer.installed', ['bin', '{dest}$(whoami)', 'dest', '/原始路径'])).toBe('已將 {dest}$(whoami) 安裝至 /原始路径');
});
test('PowerShell installer retains one complete program after generated resources',()=>{
  const source=readFileSync(new URL('../install.ps1',import.meta.url),'utf8').replaceAll('\r\n','\n');
  for(const name of ['Get-Target','Install-Binary','Install-FromRelease','Install-FromSource'])expect(source.match(new RegExp('^function '+name+'\\b','gm'))).toHaveLength(1);
  expect(source).toContain("zh(-Hans|-Hant|-CN|-SG|-TW|-HK|-MO)?)$') {");
  expect(source.indexOf('$problem = Text')).toBeLessThan(source.indexOf('function Get-Target'));
});
