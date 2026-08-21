// 本地 stub：真实包 @deepseek-ai/dsh-environment 从未发布到 npm
// （dsh-web-search-exa@0.0.1-rc.1 的 peer 引用是死链，rc.8 起改名
// @deepseek-ai/dsh-launch-environment）。等价于库内兜底：无环境层时
// 一切查询返回 undefined（插件代码随后回落 process.env 语义的空值）。
export function environmentOf() {
  return { get: () => undefined }
}
