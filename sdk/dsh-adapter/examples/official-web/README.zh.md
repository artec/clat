# DSH 官方 web 四件套配方

[English](README.md)

原样组合官方 web 服务、DeepSeek 搜索 provider、HTTP 抓取 provider 与模型工具，
全部钉在 DSH 0.2.0-rc.2。本目录是仓库私有配方，不是已发布 npm 包。需要
本地构建的适配器；已发布的 0.1.0-rc.2 尚无本批补齐的静态接口。

先在 `../..` 执行 `npm ci --ignore-scripts && npm run build`，再在这里运行：

```bash
npm ci --ignore-scripts
npm test
npm run package -- --out /absolute/new/official-web-package
```

Node 22.19+ 与 Bun 仅供作者构建。产物是独立 MCP 可执行文件及 manifest；
接收者通过 `clat plugin install` 安装，以私有 `--config-file` 配置密钥，或
在 `mcp.json` 配置可执行文件绝对路径与 `DEEPSEEK_API_KEY`，不需要写脚本。

打包拒绝覆盖已有输出，复用适配器的包元数据处理，关闭 `.env`/bunfig 自动
加载，并在发布目录前检查真实握手。上游源码不改。完整安装、权限与验收
边界见 [使用指南](../../../../docs/web.md)。

打包目录附带 LICENSES.txt，收录已安装依赖的许可证通知。
