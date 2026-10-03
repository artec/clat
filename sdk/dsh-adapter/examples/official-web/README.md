# Official DSH web quartet recipe

[中文](README.zh.md)

Compose the unchanged official web service, DeepSeek search provider, HTTP
fetch provider and model-visible tools, pinned to DSH 0.2.0-rc.2. This is a
private repository recipe, not a published npm package. It uses the local
adapter build; the published 0.1.0-rc.2 lacks the newly required static seams.

Build the adapter first (`npm ci --ignore-scripts && npm run build` in `../..`),
then run here:

```bash
npm ci --ignore-scripts
npm test
npm run package -- --out /absolute/new/official-web-package
```

Node 22.19+ and Bun are author tools only. The compiled artifact is a standalone
MCP executable plus manifest. Recipients can install it with `clat plugin
install` and provide a key using a private `--config-file`, or configure its
absolute executable path and `DEEPSEEK_API_KEY` in `mcp.json`.

The package command refuses an existing output, uses the adapter's shared
metadata bundler, disables `.env`/bunfig autoloading, and smoke-tests the binary
before publication. No upstream sources are changed. See the complete
[setup, permission and validation guide](../../../../docs/web.md).
