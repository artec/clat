#!/usr/bin/env node
// Repo-only runner for the demo plugin (excluded from the npm tarball via
// package.json "files"). The Rust e2e test spawns this over MCP stdio.
import { serveDemoPlugin } from '../dist/src/demo.js'

serveDemoPlugin().catch(error => {
  console.error('[clat-dsh-adapter:demo]', error)
  process.exit(1)
})
