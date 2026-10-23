import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parseCatalog } from './i18n-check';

const root = resolve(import.meta.dir, '..');
const quote = (s: string) => "'" + s.replaceAll("'", "'\\''") + "'";
const psQuote = (s: string) => "'" + s.replaceAll("'", "''") + "'";
export function installerCatalogs() {
  const manifest = JSON.parse(readFileSync(resolve(root, 'locales/manifest.json'), 'utf8'));
  return Object.fromEntries(manifest.languages.map((l: {id:string}) => [l.id,
    Object.fromEntries(Object.entries(parseCatalog(readFileSync(resolve(root, `locales/${l.id}.json`), 'utf8')))
      .filter(([key]) => key.startsWith('installer.')))])) as Record<string, Record<string, string>>;
}
export function bashResources(catalogs = installerCatalogs()): string {
  const cases = Object.entries(catalogs).flatMap(([locale, values]) => Object.entries(values)
    .map(([key, value]) => `    ${locale}:${key}) value=${quote(value)} ;;`)).join('\n');
  return "# Generated from locales/*.json by bun scripts/i18n-installers.ts. Do not edit.\ntext() {\n  local key=\"$1\" value name parameter prefix tail index found\n  shift\n  local -a parameters=(\"$@\")\n  case \"$clat_install_locale:$key\" in\n" + cases + "\n    *) value=\"$key\" ;;\n  esac\n  while [[ \"$value\" == *'{'*'}'* ]]; do\n    prefix=${value%%\\{*}; tail=${value#*\\{}; name=${tail%%\\}*}\n    printf '%s' \"$prefix\"\n    found=0; index=0\n    while [ \"$index\" -lt \"${#parameters[@]}\" ]; do\n      if [ \"${parameters[$index]}\" = \"$name\" ]; then\n        index=$((index + 1)); printf '%s' \"${parameters[$index]}\"; found=1; break\n      fi\n      index=$((index + 2))\n    done\n    [ \"$found\" = 1 ] || printf '{%s}' \"$name\"\n    value=${tail#*\\}}\n  done\n  printf '%s' \"$value\"\n}";
}
export function powershellResources(catalogs = installerCatalogs()): string {
  const languages = Object.entries(catalogs).map(([locale, values]) => `${psQuote(locale)} = @{\n` + Object.entries(values)
    .map(([key, value]) => `  ${psQuote(key)} = ${psQuote(value)}`).join('\n') + '\n}').join('\n');
  return `# Generated from locales/*.json by bun scripts/i18n-installers.ts. Do not edit.\n$script:InstallTexts = @{\n${languages}\n}\nfunction Text($key, $parameters = @{}) {\n    $value = $script:InstallTexts[$script:InstallLocale][$key]\n    if ($null -eq $value) { $value = $script:InstallTexts['en'][$key] }\n    return [regex]::Replace($value, '\\{([A-Za-z][A-Za-z0-9_]*)\\}', { param($match)\n        $name = $match.Groups[1].Value\n        if ($parameters.ContainsKey($name)) { [string]$parameters[$name] } else { $match.Value }\n    })\n}`;
}
export function updateInstallers(check: boolean) {
  for (const [file, resources] of [['install.sh', bashResources()], ['install.ps1', powershellResources()]]) {
    const path = resolve(root, file!);
    const source = readFileSync(path, 'utf8').replaceAll('\r\n', '\n');
    if(!/^# BEGIN GENERATED LOCALES$/m.test(source)||!/^# END GENERATED LOCALES$/m.test(source))throw new Error(`${file}: generated locale markers missing`);
    const updated = source.replace(/# BEGIN GENERATED LOCALES\n[\s\S]*?# END GENERATED LOCALES/,
      () => '# BEGIN GENERATED LOCALES\n' + resources + '\n# END GENERATED LOCALES');
    if (source === updated) continue;
    if (check) throw new Error(`${file}: language resources are stale; run bun scripts/i18n-installers.ts`);
    writeFileSync(path, updated);
  }
}
if (import.meta.main) updateInstallers(process.argv.includes('--check'));
