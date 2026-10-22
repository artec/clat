import { existsSync, mkdirSync, readdirSync } from 'node:fs';
import { dirname, isAbsolute, relative, resolve, sep, win32 } from 'node:path';
import * as tar from 'tar';

export function safeExtract(archive: string, destination: string): void {
  // Validate the complete inventory before any write; reject links and special files.
  // node-tar preserves executable modes and parses PAX/GNU headers for existing CI bundles.
  if (existsSync(destination) && readdirSync(destination).length) throw new Error('archive destination must be empty');
  let total = 0;
  tar.t({ file: archive, sync: true, strict: true, preservePaths: true, onReadEntry(entry) {
    const path = entry.path;
    const local = relative(resolve(destination), resolve(destination, path));
    if (isAbsolute(path) || win32.isAbsolute(path) || path.includes('\\')
      || path.split('/').includes('..') || local === '..' || local.startsWith('..' + sep)) {
      throw new Error('archive path escapes extraction directory');
    }
    if (!['File', 'Directory'].includes(entry.type)) throw new Error('links and special archive entries are forbidden');
    total += entry.size;
    if (total > 2 * 1024 ** 3) throw new Error('archive is too large');
  } });
  mkdirSync(destination, { recursive: true });
  tar.x({ file: archive, cwd: destination, sync: true, strict: true, preservePaths: false });
}

export function createArchive(app: string, destination: string): void {
  mkdirSync(dirname(destination), { recursive: true });
  tar.c({ file: destination, cwd: dirname(app), gzip: true, sync: true, portable: true }, [relative(dirname(app), app)]);
}
