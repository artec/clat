#!/usr/bin/env node
// 作者视角的全部改造量：把现成插件的导出喂给 serveClat。
// 插件本体（@deepseek-ai/dsh-web-search-exa，npm 上的真实发布物）
// 一行未改。
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '@deepseek-ai/dsh-web-search-exa'

serveClat({ apply, Config, inject, name }, { name: 'web-search-exa', version: '0.0.0' })
  .catch(error => {
    console.error('[exa-acceptance]', error)
    process.exit(1)
  })
