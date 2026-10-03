// Include installed package copyright notices with the standalone executable.
import { readFile, readdir } from 'node:fs/promises'
import path from 'node:path'

export async function notices(root) {
  const lock = JSON.parse(await readFile(path.join(root, 'package-lock.json'), 'utf8'))
  const sections = ['Standalone DSH web bundle: third-party copyright notices.\nThe installed dependency set is included conservatively, including build/test dependencies.']
  const directories = new Map([[path.resolve(root, '../..'), '@artec/clat-dsh-adapter']])
  for (const [relative, metadata] of Object.entries(lock.packages)) {
    if (relative.startsWith('node_modules/')) directories.set(path.join(root, relative), `${relative.slice(13)} ${metadata.version || ''}`)
  }
  for (const [directory, name] of directories) {
    const files = (await readdir(directory)).filter(file => /^(LICENSE|LICENCE|COPYING|NOTICE)(\.|$)/i.test(file)).sort()
    for (const file of files) sections.push(`\n===== ${name}: ${file} =====\n${await readFile(path.join(directory, file), 'utf8')}`)
  }
  return sections.join('\n') + '\n'
}
