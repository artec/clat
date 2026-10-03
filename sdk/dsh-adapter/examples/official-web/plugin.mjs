// Official packages are consumed unchanged; this file only composes them.
import WebRuntime from '@deepseek-ai/dsh-web'
import * as deepseek from '@deepseek-ai/dsh-web-search-deepseek'
import * as http from '@deepseek-ai/dsh-web-fetch-http'
import * as tools from '@deepseek-ai/dsh-tool-web'

export const officialWeb = {
  name: 'dsh-official-web',
  apply(ctx, config = {}) {
    new WebRuntime(ctx, WebRuntime.Config({}))
    deepseek.apply(ctx, deepseek.Config(config))
    http.apply(ctx, http.Config({}))
    tools.apply(ctx, tools.Config({ fetchMaxOutputChars: 100_000 }))
  },
}
