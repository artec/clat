#!/usr/bin/env node
import { serveClat } from '@artec/clat-dsh-adapter'
import { officialWeb } from './plugin.mjs'

const raw = process.env.CLAT_PLUGIN_CONFIG
if (raw !== undefined && Buffer.byteLength(raw) > 65_536) {
  throw new Error('CLAT_PLUGIN_CONFIG exceeds 64 KiB')
}
const config = raw === undefined || raw === '' ? {} : JSON.parse(raw)

await serveClat(officialWeb, {
  name: 'dsh-official-web',
  version: '0.2.0-rc.2',
  config,
  toolHints: { web_search: 'network', web_fetch: 'network' },
})
