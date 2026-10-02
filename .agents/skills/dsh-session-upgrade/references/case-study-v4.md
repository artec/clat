# V4 对齐案例：DSH 0.2.0-rc.2

钉靶为 `639ed015397290b3745d163aafe02ffee4aa3f84`。本轮公开格式为 V4；
CLAT 新日志写 `session.v4.jsonl[.zstd]`，旧 V0/V2/V3 仍可只读打开并经
`/update` 的内存链一次性发布唯一 V4。全链失败不触碰源文件。

## 关键裁决

- `developer/message` 进已知词表和第五码表面节点，但 CLAT 不产此型。
  含该事件的 V4 会话只读，不能继续模型调用；否则会无声丢掉开发者
  消息和 deferred-tool 语义。非空 `agentPreset` 同理只读。
- `image/offload` 和 `workspace/changes` 为 envelope-only 已知座位，不产
  不投影。它们在上游冻结 V3 词表中已存在；只有
  `developer/message` 是 V4 新出生类型。
- V3 `tool/result` 的 user-role wrapper 搬为 V4 一等 tool-role 消息，
  `toolCallId`/可选 `isError` 在消息层；未知原消息、结果字段分别用
  `plugin:message:`、`plugin:result:` 保留。producer source 的
  `{kind:"plugin",plugin:"…"}` 改为冻结映射的直接 kind。

## 判别证据与边界

`tests/fixtures/dsh-session/gen-v4-fixtures.mts` 从真实 DSH
SessionStore append 路径和 released V4 codec 铸造原生日志，也把提交
的 V3 源送进上游 V3→V4 迁移器。`dsh_golden` 测试验证 CLAT 读取原生
V4，且本地转换逐事件等于上游产物。V3 真日志的协调器 resume→
`/update`→V4 可写腿另行钉住。

V4 审计确认三处容易误判的边界：第一，CLAT 本批增三个座位，但上游
冻结 V3 词表已含 `image/offload` 与 `workspace/changes`，不能把三者
都标成 V4 出生；第二，当前代的 `developer/message` 或非空
`agentPreset` 可读不代表可续写，打开时应无写租约、seed、checkpoint
等副作用，`/update` 仅给旧代；第三，旧 wire 形状驱动解析已接纳
V4 tool-role 结果，本批补 live 帧判别测试即可，无须新生产分支。

金样脚本固定随机消息 ID 和时间后，两个 zstd 文件连续重生的哈希
一致。验收时 `DSH_CHECKOUT` 单独武装 V4 写锁互斥；适配器测试仍
使用自身钉住的旧 DSH cohort。`gates.sh --full` 最终全绿；其固定
2691 端口测试须先确认端口空闲，不能从技能推导出停止用户进程的权限。

CLAT 仍不支持 seeded 子会话升级，也不补录历史 child catalog；此类
输入必须拒绝而不是猜测。适配器/npm 包和 DSH 宿主部署不属于该批。
