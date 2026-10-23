import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import type { Runner } from './common';

// Same simple two-icon composition as Tauri's default DMG. Paths are argv,
// never interpolated into AppleScript source (spaces and quotes stay literal).
const FINDER_LAYOUT = `on run argv
  set imageAlias to POSIX file (item 1 of argv) as alias
  set appName to item 2 of argv
  tell application "Finder"
    set dmgWindow to make new Finder window
    set target of dmgWindow to imageAlias
    set imageFolder to target of dmgWindow
    tell dmgWindow
      set current view to icon view
      set toolbar visible to false
      set statusbar visible to false
      set bounds to {400, 200, 1060, 600}
    end tell
    set viewOptions to icon view options of dmgWindow
    set arrangement of viewOptions to not arranged
    set icon size of viewOptions to 128
    set text size of viewOptions to 16
    set position of item appName of imageFolder to {180, 170}
    set position of item "Applications" of imageFolder to {480, 170}
    update imageFolder without registering applications
    delay 2
    close dmgWindow
  end tell
end run`;

export function makeDmg(app: string, destination: string, product: string, execute: Runner): void {
  mkdirSync(dirname(destination), { recursive: true });
  const folder = mkdtempSync(join(tmpdir(), 'clat-desktop-dmg-'));
  const image = join(folder, 'image-root'), mount = join(folder, 'mounted');
  const writable = join(folder, 'layout.dmg');
  let mounted = false;
  try {
    mkdirSync(image); mkdirSync(mount);
    execute(['ditto', app, join(image, basename(app))]);
    symlinkSync('/Applications', join(image, 'Applications'));
    execute(['hdiutil', 'create', '-volname', product, '-fs', 'HFS+', '-srcfolder', image, '-ov', '-format', 'UDRW', writable]);
    let failure: unknown;
    try {
      // Even a partially failed attach must get a detach attempt.
      mounted = true;
      execute(['hdiutil', 'attach', writable, '-mountpoint', mount, '-noautoopen', '-readwrite']);
      execute(['osascript', '-e', FINDER_LAYOUT, realpathSync(mount), basename(app)], { timeout: 60000 });
      if (!existsSync(join(mount, '.DS_Store'))) throw new Error('Finder did not save the DMG layout');
      execute(['sync']);
    } catch (error) { failure = error; }
    try { execute(['hdiutil', 'detach', mount]); mounted = false; }
    catch (error) { failure = new AggregateError(failure ? [failure, error] : [error], 'DMG detach failed; retained temporary image: ' + folder); }
    if (failure) throw failure;
    execute(['hdiutil', 'convert', writable, '-format', 'UDZO', '-ov', '-o', destination]);
  } finally {
    // Never recursively remove a still-mounted volume after a detach failure.
    if (!mounted) rmSync(folder, { recursive: true, force: true });
  }
}
