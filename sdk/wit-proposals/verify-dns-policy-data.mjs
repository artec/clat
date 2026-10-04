// Verify the public, byte-pinned IANA snapshot without network access.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
const data = new URL('./http-consumer-host/src/dns_authority/data/', import.meta.url)
const hashes = {
  'iana-ipv4.xml': 'cf24e11f41b7d42c68debe2d18b97cac815084ec413ebb3b244f704028a16f20',
  'iana-ipv6.xml': 'c17f4380ba84fb2160dae82ebfd8bd155a5853cfab624ed3a9fd251638a8be02',
  'special-ranges.json': '668bfdd820e06d3b905b627f2712256be54b68f01f05204da98494e6a5b53ac8',
}
const files = new Map()
for (const [name, expected] of Object.entries(hashes)) {
  const bytes = await readFile(new URL(name, data))
  assert.equal(createHash('sha256').update(bytes).digest('hex'), expected, name)
  files.set(name, bytes.toString('utf8'))
}
const table = JSON.parse(files.get('special-ranges.json'))
for (const family of ['ipv4', 'ipv6']) {
  const xml = files.get(`iana-${family}.xml`)
  // This pinned document uses literal address elements; strip only its footnote tags.
  const ranges = [...xml.matchAll(/<address>([\s\S]*?)<\/address>/g)]
    .flatMap(match => match[1].replace(/<[^>]+>/g, '').split(',').map(value => value.trim()))
  assert.deepEqual(table.ranges[family], ranges, `all ${family} assignments, including nested exceptions`)
  assert.equal(table.sources[family].sha256, hashes[`iana-${family}.xml`])
}
console.log('DNS_POLICY_DATA pinned hashes and complete registry extraction passed')
