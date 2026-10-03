# pi.at.cn 本地生成、FTP 上传

当前部署方式仍可使用。首发不需要服务器端应用，也不需要部署 Node/Bun。
正式签名由负责人在本机执行；FTP 仅传输已经签好的静态公开文件。

## 准备首包

开发工作区已生成待正式签名的 macOS arm64 包：
`output/plg2/release-ready-licensed-aarch64-apple-darwin`。
它只有可执行文件、manifest 与许可证通知，不含测试签名或密钥。
先审阅该目录、源码与 `publishers/artec.html`。在仓库根执行（私钥
路径由负责人填写，私钥内容不传给开发者、不进入站点）：

```bash
npm --prefix market run sign-package -- \
  --package "$PWD/output/plg2/release-ready-licensed-aarch64-apple-darwin" \
  --publisher artec --public-key "$PWD/release/minisign.pub" \
  --minisign-key /secure/path/to/minisign.key
npm --prefix market run stage-package -- \
  --package "$PWD/output/plg2/release-ready-licensed-aarch64-apple-darwin" \
  --out "$PWD/output/plg2/production-proposal" \
  --target aarch64-apple-darwin --publisher-key-id artec-2026 \
  --review-url https://pi.at.cn/publishers/artec.html \
  --source-url https://github.com/artec/clat/tree/main/sdk/dsh-adapter/examples/official-web \
  --clat "$PWD/target/debug/clat"
```

`stage-package` 只产生提案。负责人复核后，将 proposal 中的 publisher /
package 记录合入 `market/index.source.json`；将 catalog 的单条记录添加
到现有 `market/catalog.json`，保留其他条目。将 `packages/*.clatpkg` 拷贝
到 `market/packages/`。不将测试工件或测试公钥上架。重复发布不得覆盖
已发布的同版本/目标平台工件。

```bash
npm --prefix market run build
npm --prefix market run release-index -- --minisign-key /secure/path/to/minisign.key
minisign -Vm market/dist/index.json -p release/minisign.pub -x market/dist/index.json.minisig
```

## 上传与验收

优先上传完整 `dist/` 到新的服务器版本目录，校验后在服务器切换网站根
目录，可保留上一版本用于回退。若当前只能覆盖网站目录：

1. 先上传不可变 `packages/` 工件与 `publishers/` 记录。
2. 再上传 HTML/CSS/JS，以及配套的 `index.json` / `index.json.minisig`；
   先用临时文件名上传完整文件，再重命名。两文件切换期间验签失败会
   拒绝新安装；有效旧缓存仍可使用，已装插件不受影响。
3. 最后上传 `catalog.json`，让 available 卡片在签名和工件就绪后可见。
4. 用 `curl -I https://pi.at.cn/catalog.json` 核对 CORS 头；在 PWA 实跑
   安装 → 搜索 fixture 或真实服务 → 卸载，再用 CLI market audit 核对。

`_headers` 不会被普通 FTP/Web 服务器自动识别。需在网站控制面板或
Web 服务器配置中为 `catalog.json`、`index.json`、`index.json.minisig`
与 `packages/*` 返回 `Access-Control-Allow-Origin: *`。这些文件完全公开，
不得启用带凭据的 CORS。索引及目录短缓存（300 秒），工件不可变长缓存。
当前线上响应显示为 Nginx。可在该静态网站的 `server` 中加入：

```nginx
add_header Access-Control-Allow-Origin "*" always;
```

若某个 `location` 自己定义了 `add_header`，它不会继承父级这些头，需在
该 location 同时添加 CORS 与既有安全响应头，并实测上述四类 URL。
Nginx 的响应头继承说明：
https://nginx.org/en/docs/http/ngx_http_headers_module.html 。

签名索引有效期七天，即使包不变也应到期前重新签名上传。不要删除历史
不可变包作为撤销手段；撤销通过签名索引状态记录完成。

## 后续自动化

若现有服务器支持 SSH/SFTP，推荐增加一个发布脚本：上传到版本目录 →
核对摘要 → 原子切换站点指向，失败时保留旧版本。SFTP 的 batch 模式与
rename 支持该流程：https://man.openbsd.org/sftp.1 。密钥保留在负责人
机器，使用系统 SSH 配置；不必迁移托管商，也不必让 CI 保存生产签名私钥。
